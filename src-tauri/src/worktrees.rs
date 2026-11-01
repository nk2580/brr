use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, time::timeout};

const WORKTRUNK_TIMEOUT: Duration = Duration::from_secs(120);
static BRANCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub project_path: String,
    pub path: String,
    pub branch: String,
    pub cwd: String,
}

#[derive(Clone, Debug)]
pub struct WorktreeManager {
    home: PathBuf,
    #[cfg(test)]
    executable: Option<PathBuf>,
}

impl WorktreeManager {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            #[cfg(test)]
            executable: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_executable(executable: PathBuf, home: PathBuf) -> Self {
        Self {
            home,
            executable: Some(executable),
        }
    }

    pub async fn prepare(&self, cwd: &str) -> Result<Workspace, String> {
        let selected = fs::canonicalize(cwd)
            .map_err(|e| format!("Cannot open selected project folder: {e}"))?;
        if !selected.is_dir() {
            return Err("The selected project path is not a directory".into());
        }
        let root = git_output(&selected, &["rev-parse", "--show-toplevel"]).await?;
        let root = fs::canonicalize(root.trim())
            .map_err(|e| format!("Cannot resolve Git project root: {e}"))?;
        let _common_dir = git_output(&root, &["rev-parse", "--git-common-dir"]).await?;
        git_output(&root, &["rev-parse", "--verify", "HEAD"])
            .await
            .map_err(|_| "This Git repository has no commit yet. Create and commit the project’s initial files before opening a worktree.".to_string())?;
        let relative = selected
            .strip_prefix(&root)
            .map_err(|_| "The selected folder is outside its Git project root".to_string())?;

        let executable = self.find_worktrunk()?;
        self.check_approvals(&executable, &root).await?;
        fs::create_dir_all(&self.home)
            .map_err(|e| format!("Cannot create Brr worktree directory: {e}"))?;
        let branch = unique_branch();
        let output = run_worktrunk(
            &executable,
            &root,
            &[
                "switch",
                "--create",
                &branch,
                "--base=@",
                "--no-cd",
                "--format=json",
            ],
        )
        .await?;
        let json: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
            format!(
                "Worktrunk returned invalid JSON ({e}). Output: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })?;
        let reported_path = json_path(&json).ok_or_else(|| {
            format!(
                "Worktrunk did not return a worktree path. Output: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })?;
        let path = if reported_path.is_absolute() {
            reported_path
        } else {
            root.join(reported_path)
        };
        let path = fs::canonicalize(&path).map_err(|e| {
            format!("Worktrunk created or reported {} but it cannot be opened: {e}. The path was retained; inspect it manually.", path.display())
        })?;
        let worktree_root = git_output(&path, &["rev-parse", "--show-toplevel"]).await?;
        let worktree_root = fs::canonicalize(worktree_root.trim())
            .map_err(|e| format!("Cannot resolve created worktree root: {e}"))?;
        let worktree_common =
            git_output(&worktree_root, &["rev-parse", "--git-common-dir"]).await?;
        let source_common = git_output(&root, &["rev-parse", "--git-common-dir"]).await?;
        if canonical_git_dir(&worktree_root, worktree_common.trim())?
            != canonical_git_dir(&root, source_common.trim())?
        {
            return Err(format!("Worktrunk reported {}, but it is not linked to the selected repository. The path was retained.", path.display()));
        }
        let actual_branch = git_output(&path, &["branch", "--show-current"]).await?;
        if actual_branch.trim() != branch {
            return Err(format!("Worktrunk created {} on unexpected branch {:?}; expected {branch}. The path was retained.", path.display(), actual_branch.trim()));
        }
        if path == root || root.starts_with(&path) {
            return Err(format!(
                "Worktrunk created an overlapping worktree at {}; the path was retained.",
                path.display()
            ));
        }
        let worktree_cwd = fs::canonicalize(worktree_root.join(relative)).map_err(|e| {
            format!(
                "Created worktree {} does not contain the selected project subfolder: {e}",
                worktree_root.display()
            )
        })?;
        Ok(Workspace {
            project_path: selected.to_string_lossy().into_owned(),
            path: worktree_root.to_string_lossy().into_owned(),
            branch,
            cwd: worktree_cwd.to_string_lossy().into_owned(),
        })
    }

    pub fn save(&self, thread_id: &str, workspace: &Workspace) -> Result<(), String> {
        validate_thread_id(thread_id)?;
        let session_dir = self.home.join("worktree-sessions");
        fs::create_dir_all(&session_dir)
            .map_err(|e| format!("Cannot create worktree session marker directory: {e}"))?;
        let marker = session_dir.join(thread_id);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(file) => file
                .sync_all()
                .map_err(|e| format!("Cannot persist worktree session marker: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Cannot create worktree session marker: {e}")),
        }
        #[cfg(unix)]
        fs::File::open(&session_dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("Cannot persist worktree session marker directory: {e}"))?;
        #[cfg(unix)]
        fs::File::open(&self.home)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("Cannot persist worktree session directory: {e}"))?;

        fs::create_dir_all(self.home.join("worktrees"))
            .map_err(|e| format!("Cannot create workspace metadata directory: {e}"))?;
        let destination = self
            .home
            .join("worktrees")
            .join(format!("{thread_id}.json"));
        let temp = self
            .home
            .join("worktrees")
            .join(format!(".{thread_id}.{}.tmp", unique_suffix()));
        let bytes =
            serde_json::to_vec(workspace).map_err(|e| format!("Cannot encode workspace: {e}"))?;
        fs::write(&temp, bytes).map_err(|e| format!("Cannot write workspace metadata: {e}"))?;
        if let Err(e) = fs::rename(&temp, &destination) {
            let _ = fs::remove_file(&temp);
            return Err(format!("Cannot save workspace metadata: {e}"));
        }
        Ok(())
    }

    pub fn get(&self, thread_id: &str) -> Result<Option<Workspace>, String> {
        validate_thread_id(thread_id)?;
        let file = self
            .home
            .join("worktrees")
            .join(format!("{thread_id}.json"));
        match fs::read(file) {
            Ok(data) => serde_json::from_slice(&data)
                .map(Some)
                .map_err(|e| format!("Workspace metadata is corrupt: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match fs::metadata(self.home.join("worktree-sessions").join(thread_id)) {
                    Ok(metadata) if metadata.is_file() => Err("Saved Worktree metadata is missing for this managed thread. Restore the workspace metadata from backup; Brr will not treat this thread as a legacy workspace or fall back to the source project.".into()),
                    Ok(_) => Err("Saved Worktree session marker is invalid. Restore the workspace metadata from backup before continuing.".into()),
                    Err(marker_error) if marker_error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(marker_error) => Err(format!("Cannot read worktree session marker: {marker_error}")),
                }
            }
            Err(e) => Err(format!("Cannot read workspace metadata: {e}")),
        }
    }

    pub async fn validate(&self, workspace: &Workspace) -> Result<(), String> {
        let root = fs::canonicalize(&workspace.path).map_err(|_| "The saved worktree is missing. Restore it with Worktrunk before continuing; Brr will not fall back to the source project.".to_string())?;
        let cwd = fs::canonicalize(&workspace.cwd).map_err(|_| "The saved worktree folder is missing. Restore it with Worktrunk before continuing; Brr will not fall back to the source project.".to_string())?;
        let actual_root = fs::canonicalize(
            git_output(&root, &["rev-parse", "--show-toplevel"])
                .await?
                .trim(),
        )
        .map_err(|e| format!("Cannot resolve saved worktree root: {e}"))?;
        if root != actual_root || !cwd.starts_with(&root) {
            return Err(
                "The saved workspace paths do not identify a folder inside the saved Git worktree"
                    .into(),
            );
        }
        let branch = git_output(&root, &["branch", "--show-current"]).await?;
        if branch.trim() != workspace.branch {
            return Err(format!("The saved worktree branch changed (expected {}, found {}). Restore the expected worktree with Worktrunk before continuing.", workspace.branch, branch.trim()));
        }
        let root_common = canonical_git_dir(
            &root,
            git_output(&root, &["rev-parse", "--git-common-dir"])
                .await?
                .trim(),
        )?;
        let source = fs::canonicalize(&workspace.project_path)
            .map_err(|e| format!("Cannot open saved source project: {e}"))?;
        let source_root = fs::canonicalize(
            git_output(&source, &["rev-parse", "--show-toplevel"])
                .await?
                .trim(),
        )
        .map_err(|e| format!("Cannot resolve saved source project root: {e}"))?;
        let source_common = canonical_git_dir(
            &source_root,
            git_output(&source_root, &["rev-parse", "--git-common-dir"])
                .await?
                .trim(),
        )?;
        if root_common != source_common {
            return Err("The saved worktree is no longer linked to its source repository".into());
        }
        Ok(())
    }

    fn find_worktrunk(&self) -> Result<PathBuf, String> {
        #[cfg(test)]
        if let Some(path) = &self.executable {
            return Ok(path.clone());
        }
        let name = if cfg!(windows) { "git-wt.exe" } else { "wt" };
        if let Some(path) = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        }) {
            return Ok(path);
        }
        #[cfg(target_os = "macos")]
        for path in ["/opt/homebrew/bin/wt", "/usr/local/bin/wt"] {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(path);
            }
        }
        Err("Worktrunk is required to create isolated project folders. Install Worktrunk and make `wt` available on PATH.".into())
    }

    async fn check_approvals(&self, executable: &Path, root: &Path) -> Result<(), String> {
        let output = run_worktrunk(
            executable,
            root,
            &["config", "approvals", "list", "--format=json"],
        )
        .await?;
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
            format!("Could not read Worktrunk approval state ({e}). Update Worktrunk to a supported version and inspect `wt config approvals list` before continuing.")
        })?;
        match value.get("state").and_then(Value::as_str) {
            Some("no_commands" | "approved") => Ok(()),
            Some("approval_required") => Err("Worktrunk has hooks awaiting approval. Review them with `wt config approvals list` and approve them with `wt config approvals add`; Brr will not bypass hook approvals.".into()),
            _ => Err("Worktrunk returned an unsupported Worktrunk approval state. Update Worktrunk and inspect `wt config approvals list` before continuing.".into()),
        }
    }
}

async fn run_worktrunk(
    executable: &Path,
    root: &Path,
    args: &[&str],
) -> Result<std::process::Output, String> {
    let mut command = Command::new(executable);
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let result = timeout(WORKTRUNK_TIMEOUT, command.output()).await
        .map_err(|_| "Worktrunk timed out after two minutes; check Worktrunk and hook state, then inspect any reported paths before retrying.".to_string())?
        .map_err(|e| format!("Could not start Worktrunk: {e}. Install Worktrunk and make `wt` available on PATH."))?;
    if !result.status.success() {
        let retained = serde_json::from_slice::<Value>(&result.stdout)
            .ok()
            .and_then(|v| json_path(&v));
        let suffix = retained.map(|p| format!(" A worktree path was reported at {}; it was retained and must be inspected manually.", p.display())).unwrap_or_default();
        return Err(format!(
            "Worktrunk failed: {}{}{}",
            String::from_utf8_lossy(&result.stderr).trim(),
            suffix,
            if result.stderr.is_empty() {
                String::from_utf8_lossy(&result.stdout).to_string()
            } else {
                String::new()
            }
        ));
    }
    Ok(result)
}

async fn git_output(path: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args).kill_on_drop(true);
    let output = timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| "Git command timed out after ten seconds".to_string())?
        .map_err(|e| format!("Could not run Git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("Git returned invalid output: {e}"))
}

fn canonical_git_dir(base: &Path, value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    fs::canonicalize(if path.is_absolute() {
        path
    } else {
        base.join(path)
    })
    .map_err(|e| format!("Cannot resolve Git common directory: {e}"))
}

fn json_path(value: &Value) -> Option<PathBuf> {
    value
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("worktree")
                .and_then(|v| v.get("path"))
                .and_then(Value::as_str)
        })
        .map(PathBuf::from)
}

fn unique_branch() -> String {
    format!("codex/brr-{}", unique_suffix())
}
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{}-{}-{}",
        std::process::id(),
        nanos,
        BRANCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn validate_thread_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid thread ID".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn creates_linked_isolated_worktree_and_preserves_selected_subfolder() {
        let temp = tempfile::tempdir().unwrap();
        let (root, selected) = committed_repo(temp.path());
        let tree = temp.path().join("created-tree");
        let wt = fake_wt(temp.path(), &root, &tree, "no_commands", false);
        let manager = WorktreeManager::with_executable(wt, temp.path().join("home"));
        let workspace = manager.prepare(selected.to_str().unwrap()).await.unwrap();
        assert_eq!(
            workspace.project_path,
            fs::canonicalize(&selected).unwrap().to_string_lossy()
        );
        assert_eq!(
            workspace.path,
            fs::canonicalize(&tree).unwrap().to_string_lossy()
        );
        assert_eq!(
            workspace.cwd,
            fs::canonicalize(tree.join("nested"))
                .unwrap()
                .to_string_lossy()
        );
        assert!(workspace.branch.starts_with("codex/brr-"));
        assert_ne!(workspace.path, workspace.project_path);
        fs::write(
            Path::new(&workspace.path).join("worktree-only.txt"),
            b"isolated",
        )
        .unwrap();
        assert!(!Path::new(&workspace.project_path)
            .parent()
            .unwrap()
            .join("worktree-only.txt")
            .exists());
        manager.validate(&workspace).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_hooks_requiring_approval_without_running_switch() {
        let temp = tempfile::tempdir().unwrap();
        let (root, selected) = committed_repo(temp.path());
        let tree = temp.path().join("must-not-exist");
        let wt = fake_wt(temp.path(), &root, &tree, "approval_required", false);
        let manager = WorktreeManager::with_executable(wt, temp.path().join("home"));
        let error = manager
            .prepare(selected.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("wt config approvals add"));
        assert!(!tree.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_unknown_approval_state_without_running_switch() {
        let temp = tempfile::tempdir().unwrap();
        let (root, selected) = committed_repo(temp.path());
        let tree = temp.path().join("must-not-exist");
        let wt = fake_wt(temp.path(), &root, &tree, "future_state", false);
        let manager = WorktreeManager::with_executable(wt, temp.path().join("home"));
        let error = manager
            .prepare(selected.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(
            error.contains("unsupported Worktrunk approval state"),
            "{error}"
        );
        assert!(!tree.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retains_worktree_reported_by_a_failed_switch() {
        let temp = tempfile::tempdir().unwrap();
        let (root, selected) = committed_repo(temp.path());
        let tree = temp.path().join("retained-tree");
        let wt = fake_wt(temp.path(), &root, &tree, "no_commands", true);
        let manager = WorktreeManager::with_executable(wt, temp.path().join("home"));
        let error = manager
            .prepare(selected.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("retained"), "{error}");
        assert!(tree.exists(), "a reported path must never be cleaned up");
        assert!(git_output(&tree, &["rev-parse", "--show-toplevel"])
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn missing_worktrunk_reports_install_guidance() {
        let temp = tempfile::tempdir().unwrap();
        let (root, selected) = committed_repo(temp.path());
        let manager = WorktreeManager::with_executable(
            temp.path().join("missing-wt"),
            temp.path().join("home"),
        );
        let error = manager
            .prepare(selected.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("Install Worktrunk"), "{error}");
        assert!(root.exists());
    }

    #[test]
    fn saves_atomically_reads_missing_and_surfaces_corrupt_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let manager = WorktreeManager::new(temp.path().to_path_buf());
        assert!(manager.get("thread-1").unwrap().is_none());
        let workspace = Workspace {
            project_path: "/source".into(),
            path: "/tree".into(),
            branch: "codex/brr-a".into(),
            cwd: "/tree".into(),
        };
        manager.save("thread-1", &workspace).unwrap();
        assert_eq!(manager.get("thread-1").unwrap().unwrap().path, "/tree");
        assert!(manager.save("../escape", &workspace).is_err());
        fs::write(temp.path().join("worktrees/thread-2.json"), b"{").unwrap();
        assert!(manager.get("thread-2").unwrap_err().contains("corrupt"));
    }

    #[test]
    fn missing_metadata_for_a_marked_thread_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let manager = WorktreeManager::new(temp.path().to_path_buf());
        let workspace = Workspace {
            project_path: "/source".into(),
            path: "/tree".into(),
            branch: "codex/brr-a".into(),
            cwd: "/tree".into(),
        };
        manager.save("thread-1", &workspace).unwrap();
        fs::remove_dir_all(temp.path().join("worktrees")).unwrap();
        let error = manager.get("thread-1").unwrap_err();
        assert!(error.contains("backup"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn validate_rejects_missing_worktree_without_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let (root, _) = committed_repo(temp.path());
        let workspace = Workspace {
            project_path: root.to_string_lossy().into_owned(),
            path: temp.path().join("gone").to_string_lossy().into_owned(),
            branch: "codex/brr-gone".into(),
            cwd: temp.path().join("gone").to_string_lossy().into_owned(),
        };
        let error = WorktreeManager::new(temp.path().join("home"))
            .validate(&workspace)
            .await
            .unwrap_err();
        assert!(error.contains("Restore it with Worktrunk"));
    }

    /// Run with BRR_WORKTRUNK_BIN pointing at a checksum-verified Worktrunk binary.
    /// This is intentionally ignored because it executes user-configured project hooks.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "manual smoke: provide BRR_WORKTRUNK_BIN; runs a temporary approved hook"]
    async fn real_worktrunk_honors_template_approval_hooks_and_isolation() {
        struct EnvRestore(Option<std::ffi::OsString>);
        impl Drop for EnvRestore {
            fn drop(&mut self) {
                if let Some(value) = self.0.take() {
                    std::env::set_var("XDG_CONFIG_HOME", value);
                } else {
                    std::env::remove_var("XDG_CONFIG_HOME");
                }
            }
        }

        let binary = std::env::var_os("BRR_WORKTRUNK_BIN")
            .expect("set BRR_WORKTRUNK_BIN to the verified Worktrunk executable");
        let temp = tempfile::tempdir().unwrap();
        let isolated_home = temp.path().join("home");
        let config_dir = isolated_home.join(".config/worktrunk");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            "worktree-path = \"{{ repo_path }}/.worktrees/{{ branch | sanitize }}\"\n",
        )
        .unwrap();
        let restore = EnvRestore(std::env::var_os("XDG_CONFIG_HOME"));
        std::env::set_var("XDG_CONFIG_HOME", isolated_home.join(".config"));

        let (root, selected) = committed_repo(temp.path());
        let marker = temp.path().join("hook-ran");
        fs::create_dir_all(root.join(".config")).unwrap();
        fs::write(
            root.join(".config/wt.toml"),
            format!("[pre-start]\nmarker = \"touch {}\"\n", marker.display()),
        )
        .unwrap();
        git(&root, &["add", ".config/wt.toml"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "add hook",
            ],
        );
        let dirty_file = root.join("source-dirty.txt");
        fs::write(&dirty_file, b"keep exactly").unwrap();

        let manager = WorktreeManager::with_executable(
            PathBuf::from(binary),
            isolated_home.join("brr-engine"),
        );
        let blocked = manager
            .prepare(selected.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(blocked.contains("wt config approvals add"));
        assert!(!marker.exists());

        let approved = std::process::Command::new(std::env::var_os("BRR_WORKTRUNK_BIN").unwrap())
            .arg("-C")
            .arg(&root)
            .args(["config", "approvals", "add", "--yes"])
            .env("XDG_CONFIG_HOME", isolated_home.join(".config"))
            .status()
            .unwrap();
        assert!(approved.success());

        let first = manager.prepare(selected.to_str().unwrap()).await.unwrap();
        assert!(
            Path::new(&first.path).starts_with(fs::canonicalize(root.join(".worktrees")).unwrap())
        );
        assert!(
            marker.exists(),
            "approved pre-start hook should finish before prepare returns"
        );
        assert_eq!(fs::read(&dirty_file).unwrap(), b"keep exactly");
        assert!(!Path::new(&first.path).join("source-dirty.txt").exists());

        let second = manager.prepare(selected.to_str().unwrap()).await.unwrap();
        assert_ne!(first.path, second.path);
        assert_ne!(first.branch, second.branch);
        manager.validate(&first).await.unwrap();
        manager.validate(&second).await.unwrap();
        drop(restore);
    }

    fn committed_repo(parent: &Path) -> (PathBuf, PathBuf) {
        let root = parent.join("source");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/.keep"), b"test").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "."]);
        git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        (root.clone(), root.join("nested"))
    }

    fn git(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[cfg(unix)]
    fn fake_wt(
        parent: &Path,
        root: &Path,
        tree: &Path,
        approval: &str,
        fail_after_create: bool,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let executable = parent.join("wt-test");
        let root = root.display().to_string();
        let tree = tree.display().to_string();
        let failure = if fail_after_create {
            "exit 1"
        } else {
            "exit 0"
        };
        let script = format!(
            "#!/bin/sh\nif [ \"$3\" = \"config\" ]; then echo '{{\"state\":\"{approval}\"}}'; exit 0; fi\nbranch=\"$5\"\ngit -C '{root}' worktree add '{tree}' -b \"$branch\" @ >/dev/null 2>&1 || exit 2\nprintf '{{\"path\":\"{tree}\"}}\\n'\n{failure}\n"
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        executable
    }
}
