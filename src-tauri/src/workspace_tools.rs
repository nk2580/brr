use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
};
use tokio::sync::Notify;

const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_DIFF: usize = 1024 * 1024;
const MAX_DIR_ENTRIES: usize = 2000;
const MAX_TERMINALS: usize = 4;
const MAX_TERMINAL_BUFFER: usize = 1024 * 1024;
const MAX_TERMINAL_INPUT: usize = 16 * 1024;
const MAX_TERMINAL_REPLY: usize = 64 * 1024;

#[derive(Default)]
pub struct WorkspaceTools {
    terminals: Mutex<HashMap<String, Terminal>>,
    file_write_lock: Mutex<()>,
}

struct Terminal {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    output: Arc<Mutex<Output>>,
    ready: Arc<Notify>,
    exit_code: Option<u32>,
}

struct Output {
    chunks: VecDeque<(u64, Vec<u8>)>,
    bytes: usize,
    next: u64,
    truncated: bool,
    ended: bool,
}

struct OutputSnapshot {
    chunks: Vec<Value>,
    cursor: u64,
    truncated: bool,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            next: 1,
            truncated: false,
            ended: false,
        }
    }
}

impl WorkspaceTools {
    pub async fn dispatch(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "directory.list" => directory_list(&params),
            "file.read" => file_read(&params),
            "file.write" => self.file_write(&params),
            "git.status" => git_status(&params),
            "git.diff" => git_diff(&params),
            "terminal.create" => self.terminal_create(&params),
            "terminal.read" => self.terminal_read(&params).await,
            "terminal.write" => self.terminal_write(&params).await,
            "terminal.resize" => self.terminal_resize(&params),
            "terminal.close" => self.terminal_close(&params).await,
            _ => Err(format!("Unknown workspace tool: {method}")),
        }
    }

    pub fn shutdown(&self) {
        let mut terminals = self.terminals.lock().unwrap_or_else(|e| e.into_inner());
        for (_, mut terminal) in terminals.drain() {
            let _ = terminal.child.kill();
            terminal.ready.notify_waiters();
        }
    }

    fn file_write(&self, params: &Value) -> Result<Value, String> {
        let _guard = self
            .file_write_lock
            .lock()
            .map_err(|_| "Workspace file-write state unavailable")?;
        file_write(params)
    }

    fn terminal_create(&self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let mut sessions = self
            .terminals
            .lock()
            .map_err(|_| "Terminal state unavailable")?;
        if sessions.len() >= MAX_TERMINALS {
            return Err("The terminal session limit has been reached".into());
        }
        let cols = dimension(params, "cols", 80);
        let rows = dimension(params, "rows", 24);
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Cannot open terminal: {e}"))?;
        let shell = login_shell();
        let mut cmd = CommandBuilder::new(shell.clone());
        cmd.cwd(root);
        cmd.env("TERM", "xterm-256color");
        #[cfg(windows)]
        {
            let shell_name = Path::new(&shell)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if shell_name == "powershell" || shell_name == "pwsh" {
                cmd.args(["-NoLogo", "-NoExit"]);
            } else {
                cmd.args(["/Q", "/K"]);
            }
        }
        #[cfg(unix)]
        cmd.arg("-l");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Cannot start terminal: {e}"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Cannot read terminal: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Cannot write terminal: {e}"))?;
        let output = Arc::new(Mutex::new(Output::default()));
        let read_output = output.clone();
        let ready = Arc::new(Notify::new());
        let read_ready = ready.clone();
        thread::Builder::new()
            .name("brr-terminal-output".into())
            .spawn(move || pump_terminal(reader, read_output, read_ready))
            .map_err(|e| format!("Cannot start terminal reader: {e}"))?;
        let id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            id.clone(),
            Terminal {
                master: pair.master,
                writer: Arc::new(Mutex::new(writer)),
                child,
                output,
                ready,
                exit_code: None,
            },
        );
        Ok(json!({"id":id}))
    }

    async fn terminal_read(&self, params: &Value) -> Result<Value, String> {
        let id = string(params, "id")?;
        let wait_ms = params
            .get("waitMs")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(1000);
        let ready = self
            .terminals
            .lock()
            .map_err(|_| "Terminal state unavailable")?
            .get(id)
            .ok_or("Unknown terminal session")?
            .ready
            .clone();
        // Subscribe before inspecting the buffer so output arriving between the
        // snapshot and await cannot leave the reader asleep until its timeout.
        let notified = ready.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let snapshot = self.terminal_snapshot(params)?;
        if wait_ms == 0
            || snapshot["exited"] == true
            || snapshot["chunks"]
                .as_array()
                .is_some_and(|chunks| !chunks.is_empty())
        {
            return Ok(snapshot);
        }
        let _ = tokio::time::timeout(std::time::Duration::from_millis(wait_ms), notified).await;
        self.terminal_snapshot(params)
    }

    fn terminal_snapshot(&self, params: &Value) -> Result<Value, String> {
        let id = string(params, "id")?;
        let after = params.get("after").and_then(Value::as_u64).unwrap_or(0);
        let mut sessions = self
            .terminals
            .lock()
            .map_err(|_| "Terminal state unavailable")?;
        let terminal = sessions.get_mut(id).ok_or("Unknown terminal session")?;
        if terminal.exit_code.is_none() {
            if let Some(status) = terminal
                .child
                .try_wait()
                .map_err(|e| format!("Cannot inspect terminal: {e}"))?
            {
                terminal.exit_code = Some(status.exit_code());
            }
        }
        let output = terminal
            .output
            .lock()
            .map_err(|_| "Terminal output unavailable")?;
        let snapshot = output_snapshot(&output, after);
        let exited = terminal.exit_code.is_some()
            && output.ended
            && snapshot.cursor >= output.next.saturating_sub(1);
        Ok(
            json!({"chunks":snapshot.chunks,"cursor":snapshot.cursor,"exited":exited,"exitCode":terminal.exit_code,"truncated":snapshot.truncated}),
        )
    }

    async fn terminal_write(&self, params: &Value) -> Result<Value, String> {
        let id = string(params, "id")?;
        let data = string(params, "data")?.as_bytes().to_vec();
        if data.len() > MAX_TERMINAL_INPUT {
            return Err("Terminal input is too large".into());
        }
        let writer = self
            .terminals
            .lock()
            .map_err(|_| "Terminal state unavailable")?
            .get(id)
            .ok_or("Unknown terminal session")?
            .writer
            .clone();
        // A PTY can stop accepting input. Keep that wait off the async executor
        // and outside the registry lock so reads, resize and close remain usable.
        let written = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            let mut writer = writer.lock().map_err(|_| "Terminal writer unavailable")?;
            writer
                .write_all(&data)
                .map_err(|e| format!("Cannot write terminal input: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("Cannot flush terminal input: {e}"))?;
            Ok(data.len())
        })
        .await
        .map_err(|e| format!("Terminal writer task failed: {e}"))??;
        Ok(json!({"written":written}))
    }

    fn terminal_resize(&self, params: &Value) -> Result<Value, String> {
        let id = string(params, "id")?;
        let mut sessions = self
            .terminals
            .lock()
            .map_err(|_| "Terminal state unavailable")?;
        let terminal = sessions.get_mut(id).ok_or("Unknown terminal session")?;
        terminal
            .master
            .resize(PtySize {
                rows: dimension(params, "rows", 24),
                cols: dimension(params, "cols", 80),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Cannot resize terminal: {e}"))?;
        Ok(json!({"resized":true}))
    }

    async fn terminal_close(&self, params: &Value) -> Result<Value, String> {
        let id = string(params, "id")?;
        let terminal = self
            .terminals
            .lock()
            .map_err(|_| "Terminal state unavailable")?
            .remove(id);
        if let Some(mut terminal) = terminal {
            terminal.ready.notify_waiters();
            tokio::task::spawn_blocking(move || {
                let _ = terminal.child.kill();
            })
            .await
            .map_err(|e| format!("Terminal close task failed: {e}"))?;
        }
        Ok(json!({"closed":true}))
    }
}

fn pump_terminal(mut reader: Box<dyn Read + Send>, output: Arc<Mutex<Output>>, ready: Arc<Notify>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut out = output.lock().unwrap_or_else(|e| e.into_inner());
                let bytes = buf[..n].to_vec();
                out.bytes += n;
                let seq = out.next;
                out.chunks.push_back((seq, bytes));
                out.next += 1;
                while out.bytes > MAX_TERMINAL_BUFFER {
                    if let Some((_, removed)) = out.chunks.pop_front() {
                        out.bytes -= removed.len();
                        out.truncated = true;
                    } else {
                        break;
                    }
                }
                drop(out);
                ready.notify_waiters();
            }
        }
    }
    output.lock().unwrap_or_else(|e| e.into_inner()).ended = true;
    ready.notify_waiters();
}

fn output_snapshot(output: &Output, after: u64) -> OutputSnapshot {
    let first_available = output
        .chunks
        .front()
        .map(|(seq, _)| *seq)
        .unwrap_or(output.next);
    let mut chunks = Vec::new();
    let mut bytes = 0;
    let mut cursor = after;
    for (seq, data) in output.chunks.iter().filter(|(seq, _)| *seq > after) {
        if bytes + data.len() > MAX_TERMINAL_REPLY {
            break;
        }
        bytes += data.len();
        cursor = *seq;
        chunks.push(json!({"seq":seq,"data":data}));
    }
    OutputSnapshot {
        chunks,
        cursor,
        truncated: output.truncated && after < first_available.saturating_sub(1),
    }
}

fn login_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    }
    #[cfg(unix)]
    {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if cfg!(target_os = "macos") {
                    "/bin/zsh".into()
                } else {
                    "/bin/sh".into()
                }
            })
    }
}

fn dimension(params: &Value, key: &str, default: u16) -> u16 {
    params
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(default as u64)
        .clamp(2, 300) as u16
}

fn string<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} must be a nonempty string"))
}

fn root_param(params: &Value) -> Result<PathBuf, String> {
    let root = PathBuf::from(string(params, "root")?);
    let canonical =
        fs::canonicalize(root).map_err(|e| format!("Cannot access workspace root: {e}"))?;
    if !canonical.is_dir() {
        return Err("Workspace root must be a directory".into());
    }
    Ok(canonical)
}

fn relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
            && !value.is_empty()
    {
        return Err("Workspace path must be relative and cannot contain traversal".into());
    }
    if path
        .components()
        .any(|c| matches!(c, Component::Normal(v) if v == ".git"))
    {
        return Err("Git metadata is not accessible through workspace tools".into());
    }
    Ok(path.to_path_buf())
}

fn target(root: &Path, relative: &Path, allow_missing: bool) -> Result<PathBuf, String> {
    let candidate = root.join(relative);
    let canonical = match fs::canonicalize(&candidate) {
        Ok(p) => p,
        Err(e) if allow_missing && e.kind() == std::io::ErrorKind::NotFound => {
            let mut ancestor = candidate.as_path();
            let mut suffix = Vec::new();
            loop {
                if let Some(name) = ancestor.file_name() {
                    suffix.push(name.to_os_string());
                }
                ancestor = ancestor.parent().ok_or("Workspace path has no parent")?;
                match fs::canonicalize(ancestor) {
                    Ok(mut parent) => {
                        if !parent.starts_with(root) {
                            return Err("Workspace path escapes the selected project".into());
                        }
                        for component in suffix.iter().rev() {
                            parent.push(component);
                        }
                        break parent;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(format!("Cannot access workspace parent: {e}")),
                }
            }
        }
        Err(e) => return Err(format!("Cannot access workspace path: {e}")),
    };
    if !canonical.starts_with(root) {
        return Err("Workspace path escapes the selected project".into());
    }
    if git_metadata_root(root).is_some_and(|metadata| canonical.starts_with(metadata)) {
        return Err("Git metadata is not accessible through workspace tools".into());
    }
    Ok(canonical)
}

fn git_metadata_root(root: &Path) -> Option<PathBuf> {
    let marker = root.join(".git");
    if marker.is_dir() {
        return fs::canonicalize(marker).ok();
    }
    let contents = fs::read_to_string(&marker).ok()?;
    let target = PathBuf::from(contents.trim().strip_prefix("gitdir:")?.trim());
    let target = if target.is_absolute() {
        target
    } else {
        marker.parent()?.join(target)
    };
    fs::canonicalize(target).ok()
}

fn directory_list(params: &Value) -> Result<Value, String> {
    let root = root_param(params)?;
    let relative = relative_path(params.get("path").and_then(Value::as_str).unwrap_or(""))?;
    let dir = target(&root, &relative, false)?;
    if !dir.is_dir() {
        return Err("Workspace path is not a directory".into());
    }
    let mut entries = Vec::new();
    for item in fs::read_dir(dir).map_err(|e| format!("Cannot list directory: {e}"))? {
        let item = item.map_err(|e| format!("Cannot list directory entry: {e}"))?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let meta =
            fs::symlink_metadata(item.path()).map_err(|e| format!("Cannot inspect entry: {e}"))?;
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            "symlink"
        } else if ft.is_dir() {
            "directory"
        } else {
            "file"
        };
        let mut path = relative.join(&name).to_string_lossy().replace('\\', "/");
        if path == "." {
            path.clear();
        }
        entries.push((
            kind == "directory",
            json!({"name":name,"path":path,"kind":kind,"size":if ft.is_file(){meta.len()}else{0}}),
        ));
        if entries.len() >= MAX_DIR_ENTRIES {
            break;
        }
    }
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            a.1["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&b.1["name"].as_str().unwrap_or("").to_lowercase())
        })
    });
    Ok(json!({"entries":entries.into_iter().map(|(_,v)|v).collect::<Vec<_>>() }))
}

fn file_read(params: &Value) -> Result<Value, String> {
    let root = root_param(params)?;
    let relative = relative_path(string(params, "path")?)?;
    let path = target(&root, &relative, false)?;
    let meta = fs::metadata(&path).map_err(|e| format!("Cannot inspect file: {e}"))?;
    if !meta.is_file() {
        return Err("Workspace path is not a regular file".into());
    }
    if meta.len() > MAX_FILE {
        return Err("File exceeds the 2 MiB text editor limit".into());
    }
    let bytes = fs::read(&path).map_err(|e| format!("Cannot read file: {e}"))?;
    let content =
        String::from_utf8(bytes).map_err(|_| "Binary files cannot be opened in the text editor")?;
    let version = version(&meta, content.as_bytes());
    Ok(json!({"path":relative,"content":content,"version":version}))
}

fn file_write(params: &Value) -> Result<Value, String> {
    let root = root_param(params)?;
    let relative = relative_path(string(params, "path")?)?;
    let content = string_allow_empty(params, "content")?;
    if content.len() as u64 > MAX_FILE {
        return Err("File exceeds the 2 MiB text editor limit".into());
    }
    let expected = string(params, "expectedVersion")?;
    let path = target(&root, &relative, true)?;
    let old_meta = match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => Some(meta),
        Ok(_) => return Err("Only regular files can be saved".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Cannot inspect file: {e}")),
    };
    let current_version = if let Some(meta) = old_meta.as_ref() {
        if meta.len() > MAX_FILE {
            return Err("File exceeds the 2 MiB text editor limit".into());
        }
        let existing =
            fs::read(&path).map_err(|e| format!("Cannot read file before saving: {e}"))?;
        version(meta, &existing)
    } else {
        "missing".into()
    };
    if current_version != expected {
        return Err("File changed on disk since it was opened; reload before saving".into());
    }
    let parent = path.parent().ok_or("File path has no parent")?;
    let tmp = parent.join(format!(".brr-save-{}", uuid::Uuid::new_v4()));
    fs::write(&tmp, content.as_bytes()).map_err(|e| format!("Cannot write temporary file: {e}"))?;
    if let Some(meta) = old_meta.as_ref() {
        fs::set_permissions(&tmp, meta.permissions())
            .map_err(|e| format!("Cannot preserve file permissions: {e}"))?;
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Cannot save file atomically: {e}"));
    }
    let meta = fs::metadata(&path).map_err(|e| format!("Cannot inspect saved file: {e}"))?;
    let version = version(&meta, content.as_bytes());
    Ok(json!({"path":relative,"content":content,"version":version}))
}

fn string_allow_empty<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))
}

fn version(meta: &fs::Metadata, bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    meta.len().hash(&mut h);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mtime().hash(&mut h);
        meta.mtime_nsec().hash(&mut h);
    }
    #[cfg(windows)]
    {
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

fn git_status(params: &Value) -> Result<Value, String> {
    let root = root_param(params)?;
    let branch = git_output(&root, &["branch", "--show-current"])?
        .trim()
        .to_string();
    let bytes = git_command(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--renames",
        ],
    )?;
    let mut fields = bytes.split(|b| *b == 0).filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue;
        }
        let x = record[0] as char;
        let y = record[1] as char;
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        let renamed = x == 'R' || y == 'R' || x == 'C' || y == 'C';
        let old_path = if renamed {
            fields
                .next()
                .map(|p| String::from_utf8_lossy(p).into_owned())
        } else {
            None
        };
        files.push(json!({"path":path,"oldPath":old_path,"indexStatus":if x==' ' {Value::Null}else{json!(x.to_string())},"worktreeStatus":if y==' ' {Value::Null}else{json!(y.to_string())},"untracked":x=='?' && y=='?'}));
    }
    Ok(json!({"branch":branch,"files":files}))
}

fn git_diff(params: &Value) -> Result<Value, String> {
    let root = root_param(params)?;
    let relative = relative_path(string(params, "path")?)?;
    let path = target(&root, &relative, true)?;
    let rel = relative.to_string_lossy().into_owned();
    let status = git_output(&root, &["status", "--porcelain=v1", "--", &rel])?;
    let exists = path.is_file();
    if !exists
        && (path.exists()
            || !status
                .as_bytes()
                .get(..2)
                .is_some_and(|xy| xy.contains(&b'D')))
    {
        return Err("Diff path must be a regular file or a tracked deletion".into());
    }
    let untracked = status.starts_with("??");
    let staged = params
        .get("staged")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let output = if untracked {
        #[cfg(windows)]
        let null = "NUL";
        #[cfg(not(windows))]
        let null = "/dev/null";
        git_capture(
            &root,
            &[
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--",
                null,
                &rel,
            ],
        )?
    } else {
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
        if staged {
            args.push("--cached");
        }
        args.extend(["--", rel.as_str()]);
        git_capture(&root, &args)?
    };
    let text = String::from_utf8_lossy(&output);
    if text.contains("\0") || text.contains("Binary files ") || text.contains("GIT binary patch") {
        return Ok(
            json!({"patch":"Binary file changes are not shown as text diffs.","truncated":false}),
        );
    }
    let truncated = output.len() > MAX_DIFF;
    let mut end = output.len().min(MAX_DIFF);
    while end > 0 && std::str::from_utf8(&output[..end]).is_err() {
        end -= 1;
    }
    let patch = String::from_utf8_lossy(&output[..end]).into_owned();
    Ok(json!({"patch":patch,"truncated":truncated}))
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git_command(root, args)?)
        .trim_end()
        .to_string())
}
fn git_command(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_EXTERNAL_DIFF", "")
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|e| format!("Cannot run Git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Git failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}
fn git_capture(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_EXTERNAL_DIFF", "")
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|e| format!("Cannot run Git: {e}"))?;
    if !out.status.success() && out.status.code() != Some(1) {
        return Err(format!(
            "Git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn git(root: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }

    #[tokio::test]
    async fn reads_saves_and_rejects_stale_editor_versions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("note.txt"), "one").unwrap();
        let tools = WorkspaceTools::default();
        let read = tools
            .dispatch("file.read", json!({"root":dir.path(),"path":"note.txt"}))
            .await
            .unwrap();
        assert_eq!(read["content"], "one");
        let saved = tools.dispatch("file.write", json!({"root":dir.path(),"path":"note.txt","content":"two","expectedVersion":read["version"]})).await.unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "two"
        );
        let error = tools.dispatch("file.write", json!({"root":dir.path(),"path":"note.txt","content":"stale","expectedVersion":read["version"]})).await.unwrap_err();
        assert!(error.contains("changed on disk"));
        assert_eq!(saved["content"], "two");
    }

    #[tokio::test]
    async fn rejects_traversal_and_git_metadata_and_lists_directories_first() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("zdir")).unwrap();
        fs::write(dir.path().join("afile"), "x").unwrap();
        git(dir.path(), &["init", "-q"]);
        let tools = WorkspaceTools::default();
        assert!(tools
            .dispatch("file.read", json!({"root":dir.path(),"path":"../outside"}))
            .await
            .is_err());
        assert!(tools
            .dispatch("file.read", json!({"root":dir.path(),"path":".git/config"}))
            .await
            .is_err());
        let result = tools
            .dispatch("directory.list", json!({"root":dir.path(),"path":""}))
            .await
            .unwrap();
        assert_eq!(result["entries"][0]["kind"], "directory");
    }

    #[tokio::test]
    async fn git_status_preserves_paths_with_spaces_and_terminal_is_interactive_pty() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("file with spaces.txt"), "change\n").unwrap();
        let tools = WorkspaceTools::default();
        let status = tools
            .dispatch("git.status", json!({"root":dir.path()}))
            .await
            .unwrap();
        assert!(status["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "file with spaces.txt"));
        let diff = tools
            .dispatch(
                "git.diff",
                json!({"root":dir.path(),"path":"file with spaces.txt"}),
            )
            .await
            .unwrap();
        assert!(diff["patch"].as_str().unwrap().contains("+change"));
        let term = tools
            .dispatch(
                "terminal.create",
                json!({"root":dir.path(),"cols":80,"rows":24}),
            )
            .await
            .unwrap();
        let id = term["id"].as_str().unwrap();
        tools
            .dispatch(
                "terminal.write",
                json!({"id":id,"data":"printf 'PTY_OK\\n'\n"}),
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let output = tools
            .dispatch("terminal.read", json!({"id":id,"after":0}))
            .await
            .unwrap();
        let bytes: Vec<u8> = output["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| {
                c["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b.as_u64().unwrap() as u8)
            })
            .collect();
        assert!(String::from_utf8_lossy(&bytes).contains("PTY_OK"));
        let last_seq = output["chunks"].as_array().unwrap().last().unwrap()["seq"]
            .as_u64()
            .unwrap_or(0);
        assert_eq!(output["cursor"].as_u64(), Some(last_seq));
        tools.shutdown();
    }

    #[test]
    fn terminal_output_snapshot_reports_only_unseen_truncation_and_last_emitted_cursor() {
        let output = Output {
            chunks: VecDeque::from([(7, vec![b'x']), (8, vec![b'y'])]),
            bytes: 2,
            next: 9,
            truncated: true,
            ..Output::default()
        };
        let first = output_snapshot(&output, 0);
        assert_eq!(first.cursor, 8);
        assert!(first.truncated);
        let caught_up = output_snapshot(&output, 6);
        assert!(!caught_up.truncated);
        assert_eq!(caught_up.chunks.len(), 2);
    }

    #[cfg(unix)]
    struct TerminalCleanup(Arc<WorkspaceTools>);
    #[cfg(unix)]
    impl Drop for TerminalCleanup {
        fn drop(&mut self) {
            self.0.shutdown();
        }
    }
    #[cfg(unix)]
    fn terminal_fixture(command: CommandBuilder) -> (Arc<WorkspaceTools>, TerminalCleanup) {
        let tools = Arc::new(WorkspaceTools::default());
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let child = pair.slave.spawn_command(command).unwrap();
        let reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let output = Arc::new(Mutex::new(Output::default()));
        let read_output = output.clone();
        let ready = Arc::new(Notify::new());
        let read_ready = ready.clone();
        thread::spawn(move || pump_terminal(reader, read_output, read_ready));
        tools.terminals.lock().unwrap().insert(
            "echo".into(),
            Terminal {
                master: pair.master,
                writer: Arc::new(Mutex::new(writer)),
                child,
                output,
                ready,
                exit_code: None,
            },
        );
        let cleanup = TerminalCleanup(tools.clone());
        (tools, cleanup)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_read_waits_for_output_and_wakes_without_blocking_input() {
        use std::time::{Duration, Instant};
        let (tools, _cleanup) = terminal_fixture(CommandBuilder::new("/bin/cat"));

        let reading = tools.clone();
        let mut read = tokio::spawn(async move {
            reading
                .dispatch(
                    "terminal.read",
                    json!({"id":"echo","after":0,"waitMs":1000}),
                )
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(40), &mut read)
                .await
                .is_err(),
            "an idle terminal should wait for output, not require another poll"
        );
        let began = Instant::now();
        tokio::time::timeout(
            Duration::from_millis(250),
            tools.dispatch("terminal.write", json!({"id":"echo","data":"RESPONSIVE\n"})),
        )
        .await
        .unwrap()
        .unwrap();
        let result = tokio::time::timeout(Duration::from_millis(250), read)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let bytes: Vec<u8> = result["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| {
                c["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b.as_u64().unwrap() as u8)
            })
            .collect();
        assert!(String::from_utf8_lossy(&bytes).contains("RESPONSIVE"));
        eprintln!("native input-to-ready-output: {:?}", began.elapsed());
    }

    #[test]
    fn terminal_output_replies_are_bounded_and_resume_without_skipping_bytes() {
        let output = Output {
            chunks: (1..=40).map(|seq| (seq, vec![seq as u8; 4096])).collect(),
            bytes: 40 * 4096,
            next: 41,
            ..Output::default()
        };
        let mut after = 0;
        let mut recovered = Vec::new();
        while after < 40 {
            let result = output_snapshot(&output, after);
            let bytes: Vec<u8> = result
                .chunks
                .iter()
                .flat_map(|chunk| {
                    chunk["data"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|byte| byte.as_u64().unwrap() as u8)
                })
                .collect();
            assert!(bytes.len() <= MAX_TERMINAL_REPLY);
            assert!(!bytes.is_empty());
            recovered.extend(bytes);
            after = result.cursor;
        }
        assert_eq!(
            recovered,
            (1..=40)
                .flat_map(|seq| vec![seq as u8; 4096])
                .collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_drains_burst_output_before_reporting_process_exit() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "awk 'BEGIN {for (i=0;i<20000;i++) print \"burst\";}'"]);
        let (tools, _cleanup) = terminal_fixture(command);
        let started = std::time::Instant::now();
        let mut after = 0;
        let mut all = Vec::new();
        let mut replies = 0;
        loop {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                tools.dispatch(
                    "terminal.read",
                    json!({"id":"echo","after":after,"waitMs":1000}),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            let bytes: Vec<u8> = result["chunks"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|c| {
                    c["data"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|b| b.as_u64().unwrap() as u8)
                })
                .collect();
            assert!(bytes.len() <= MAX_TERMINAL_REPLY);
            assert_eq!(result["truncated"], false);
            all.extend(bytes);
            replies += 1;
            after = result["cursor"].as_u64().unwrap();
            if result["exited"] == true {
                break;
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
        }
        assert_eq!(
            String::from_utf8_lossy(&all).matches("burst").count(),
            20000
        );
        eprintln!(
            "native burst: {} bytes in {} replies over {:?}",
            all.len(),
            replies,
            started.elapsed()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_blocked_terminal_writer_does_not_block_resize_or_close() {
        use std::sync::mpsc;
        use std::time::Duration;
        struct BlockingWriter {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Write for BlockingWriter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                let _ = self.entered.send(());
                let _ = self.release.recv_timeout(Duration::from_secs(1));
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let tools = Arc::new(WorkspaceTools::default());
        let created = tools
            .dispatch("terminal.create", json!({"root":dir.path()}))
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        tools.terminals.lock().unwrap().get_mut(&id).unwrap().writer =
            Arc::new(Mutex::new(Box::new(BlockingWriter {
                entered: entered_tx,
                release: release_rx,
            })));
        let writing = tools.clone();
        let write_id = id.clone();
        let write = tokio::spawn(async move {
            writing
                .dispatch("terminal.write", json!({"id":write_id,"data":"x"}))
                .await
        });
        tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_millis(500)))
            .await
            .unwrap()
            .unwrap();
        let resizing = tools.clone();
        let resize_id = id.clone();
        let mut resize = tokio::spawn(async move {
            resizing
                .dispatch(
                    "terminal.resize",
                    json!({"id":resize_id,"cols":90,"rows":30}),
                )
                .await
        });
        let resized = tokio::time::timeout(Duration::from_millis(50), &mut resize).await;
        let resized_independently = resized.is_ok();
        let closing = tools.clone();
        let close_id = id.clone();
        let mut close = tokio::spawn(async move {
            closing
                .dispatch("terminal.close", json!({"id":close_id}))
                .await
        });
        // portable-pty allows a 200ms SIGHUP grace period before force-killing.
        let closed_independently = tokio::time::timeout(Duration::from_millis(400), &mut close)
            .await
            .is_ok();
        let _ = release_tx.send(());
        write.await.unwrap().unwrap();
        if !resized_independently {
            resize.await.unwrap().unwrap();
        }
        if !closed_independently {
            close.await.unwrap().unwrap();
        }
        tools.shutdown();
        assert!(
            resized_independently && closed_independently,
            "blocked input must not hold the terminal registry lock"
        );
    }

    #[tokio::test]
    async fn git_diff_can_show_deleted_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(
            dir.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("nested/deleted.txt"), "gone\n").unwrap_or_else(|_| {
            fs::create_dir(dir.path().join("nested")).unwrap();
            fs::write(dir.path().join("nested/deleted.txt"), "gone\n").unwrap();
        });
        git(dir.path(), &["add", "nested/deleted.txt"]);
        git(dir.path(), &["commit", "-qm", "add file"]);
        fs::remove_file(dir.path().join("nested/deleted.txt")).unwrap();
        let diff = WorkspaceTools::default()
            .dispatch(
                "git.diff",
                json!({"root":dir.path(),"path":"nested/deleted.txt"}),
            )
            .await
            .unwrap();
        assert!(diff["patch"].as_str().unwrap().contains("-gone"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_git_metadata_reached_through_an_in_root_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        symlink(dir.path().join(".git"), dir.path().join("metadata-link")).unwrap();
        let error = WorkspaceTools::default()
            .dispatch(
                "file.read",
                json!({"root":dir.path(),"path":"metadata-link/config"}),
            )
            .await
            .unwrap_err();
        assert!(error.contains("Git metadata"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_from_the_same_version_allow_only_one_save() {
        use std::sync::Barrier;
        let dir = tempfile::tempdir().unwrap();
        let original = vec![b'x'; 1_500_000];
        fs::write(dir.path().join("shared.txt"), &original).unwrap();
        let tools = Arc::new(WorkspaceTools::default());
        let read = tools
            .dispatch("file.read", json!({"root":dir.path(),"path":"shared.txt"}))
            .await
            .unwrap();
        let version = read["version"].as_str().unwrap().to_string();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for fill in [b'a', b'b'] {
            let tools = tools.clone();
            let barrier = barrier.clone();
            let root = dir.path().to_path_buf();
            let version = version.clone();
            let handle = tokio::runtime::Handle::current();
            workers.push(tokio::task::spawn_blocking(move || {
                barrier.wait();
                let content = String::from_utf8(vec![fill; 1_500_000]).unwrap();
                handle.block_on(tools.dispatch("file.write", json!({"root":root,"path":"shared.txt","content":content,"expectedVersion":version})))
            }));
        }
        barrier.wait();
        let mut results = Vec::new();
        for worker in workers {
            results.push(worker.await);
        }
        let successes = results
            .into_iter()
            .filter(|r| r.as_ref().unwrap().is_ok())
            .count();
        assert_eq!(successes, 1);
    }
}
