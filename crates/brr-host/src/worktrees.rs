use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
pub use worktrunk::embedded::SetupCommand;
use worktrunk::{config::UserConfig, embedded::SessionManager, git::Repository};

static BRANCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub project_path: String,
    pub path: String,
    pub branch: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedFolder {
    pub source: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionWorkspace {
    pub cwd: String,
    pub roots: Vec<String>,
    pub folders: Vec<ResolvedFolder>,
    pub worktrees: Vec<Workspace>,
    pub scope_project_id: Option<String>,
    pub project_revision: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetupCommand {
    pub cwd: String,
    pub phase: String,
    pub name: Option<String>,
    pub template: String,
    pub approved: bool,
}

#[derive(Clone, Debug)]
pub struct WorktreeManager {
    home: PathBuf,
    #[cfg(test)]
    native_config: Option<(UserConfig, PathBuf)>,
}

impl WorktreeManager {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            #[cfg(test)]
            native_config: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_config(home: PathBuf, config: UserConfig, approvals: PathBuf) -> Self {
        Self {
            home,
            native_config: Some((config, approvals)),
        }
    }

    pub(crate) fn native(&self) -> Result<SessionManager, String> {
        #[cfg(test)]
        if let Some((config, approvals)) = &self.native_config {
            return Ok(SessionManager::new(config.clone(), approvals.clone()));
        }
        let config =
            UserConfig::load().map_err(|e| format!("Cannot load WorkTrunk settings: {e}"))?;
        let approvals = worktrunk::config::require_approvals_path().map_err(|e| e.to_string())?;
        Ok(SessionManager::new(config, approvals))
    }

    pub async fn inspect(&self, cwd: &str) -> Result<Vec<SetupCommand>, String> {
        let manager = self.clone();
        let cwd = cwd.to_owned();
        tokio::task::spawn_blocking(move || {
            let project = selected_folder(&cwd)?;
            manager
                .native()?
                .inspect(&project)
                .map_err(|e| format!("{e:#}"))
        })
        .await
        .map_err(|e| format!("WorkTrunk setup inspection failed: {e}"))?
    }

    pub async fn approve(&self, cwd: &str, templates: Vec<String>) -> Result<(), String> {
        let manager = self.clone();
        let cwd = cwd.to_owned();
        tokio::task::spawn_blocking(move || {
            let project = selected_folder(&cwd)?;
            manager
                .native()?
                .approve(&project, &templates)
                .map_err(|e| format!("{e:#}"))
        })
        .await
        .map_err(|e| format!("WorkTrunk setup approval failed: {e}"))?
    }

    pub async fn prepare(&self, cwd: &str) -> Result<Workspace, String> {
        let manager = self.clone();
        let cwd = cwd.to_owned();
        tokio::task::spawn_blocking(move || {
            let selected = selected_folder(&cwd)?;
            let repo = Repository::at_fresh(&selected).map_err(|e| format!("Cannot open Git project: {e:#}"))?;
            let root = repo.current_worktree().root_fresh().map_err(|e| format!("Cannot open Git worktree: {e:#}"))?;
            let root = fs::canonicalize(&root).map_err(|e| e.to_string())?;
            let relative = selected.strip_prefix(&root).map_err(|_| "Selected folder is outside its Git project")?;
            let branch = unique_branch();
            let path = manager.native()?.create(&root, &branch).map_err(|e| format!("{e:#}"))?;
            let path = fs::canonicalize(&path).map_err(|e| format!("Cannot open created worktree {}: {e}. The worktree was retained.", path.display()))?;
            let worktree_cwd = fs::canonicalize(path.join(relative)).map_err(|e| format!("Created worktree {} does not contain the selected project subfolder: {e}. The worktree was retained.", path.display()))?;
            if !worktree_cwd.starts_with(&path) || path == root || root.starts_with(&path) {
                return Err(format!("WorkTrunk returned an overlapping worktree at {}. The path was retained.", path.display()));
            }
            let workspace = Workspace {
                project_path: selected.to_string_lossy().into_owned(),
                path: path.to_string_lossy().into_owned(),
                branch,
                cwd: worktree_cwd.to_string_lossy().into_owned(),
            };
            validate_workspace(&workspace)?;
            Ok(workspace)
        }).await.map_err(|e| format!("Native WorkTrunk setup failed: {e}"))?
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
            Ok(data) => {
                let value: serde_json::Value = serde_json::from_slice(&data)
                    .map_err(|e| format!("Workspace metadata is corrupt: {e}"))?;
                if value.get("roots").is_some() {
                    let snapshot: SessionWorkspace = serde_json::from_value(value)
                        .map_err(|e| format!("Workspace metadata is corrupt: {e}"))?;
                    validate_session_snapshot(&snapshot)?;
                    Ok(snapshot.worktrees.into_iter().next())
                } else {
                    serde_json::from_value(value)
                        .map(Some)
                        .map_err(|e| format!("Workspace metadata is corrupt: {e}"))
                }
            }
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
        let workspace = workspace.clone();
        tokio::task::spawn_blocking(move || validate_workspace(&workspace))
            .await
            .map_err(|e| format!("Saved worktree validation failed: {e}"))?
    }

    pub async fn inspect_session(
        &self,
        project: Option<&crate::projects::ProjectRecord>,
    ) -> Result<Vec<SessionSetupCommand>, String> {
        let manager = self.clone();
        let folders = project.map(|p| p.folders.clone()).unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            let mut repos = HashMap::<String, PathBuf>::new();
            for folder in folders {
                let selected = selected_folder(&folder)?;
                if let Ok(repo) = Repository::at_fresh(&selected) {
                    let root = fs::canonicalize(
                        repo.current_worktree()
                            .root_fresh()
                            .map_err(|e| format!("Cannot read Git root: {e:#}"))?,
                    )
                    .map_err(|e| e.to_string())?;
                    let key = fs::canonicalize(repo.git_common_dir())
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .into_owned();
                    repos.entry(key).or_insert(root);
                }
            }
            let native = if repos.is_empty() {
                None
            } else {
                Some(manager.native()?)
            };
            let mut result = Vec::new();
            for root in repos.values() {
                for command in native
                    .as_ref()
                    .unwrap()
                    .inspect(root)
                    .map_err(|e| format!("Cannot inspect project setup hooks: {e:#}"))?
                {
                    result.push(SessionSetupCommand {
                        cwd: root.to_string_lossy().into_owned(),
                        phase: command.phase,
                        name: command.name,
                        template: command.template,
                        approved: command.approved,
                    });
                }
            }
            Ok(result)
        })
        .await
        .map_err(|e| format!("Session setup inspection failed: {e}"))?
    }

    pub async fn prepare_session(
        &self,
        project: Option<&crate::projects::ProjectRecord>,
    ) -> Result<SessionWorkspace, String> {
        let manager = self.clone();
        let project = project.cloned();
        tokio::task::spawn_blocking(move || {
            let selected: Vec<PathBuf> = project.as_ref().map(|p| p.folders.iter().map(|f| selected_folder(f)).collect()).transpose()?.unwrap_or_default();
            let mut repos = HashMap::<String, (PathBuf, Vec<(usize, PathBuf)>)>::new();
            let mut direct = Vec::new();
            for (i, folder) in selected.iter().enumerate() {
                if let Ok(repo) = Repository::at_fresh(folder) {
                    let root = fs::canonicalize(repo.current_worktree().root_fresh().map_err(|e| format!("Cannot read Git root: {e:#}"))?).map_err(|e| e.to_string())?;
                    let key = fs::canonicalize(repo.git_common_dir()).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
                    let relative = folder.strip_prefix(&root).map_err(|_| "Selected folder is outside its source Git worktree")?.to_path_buf();
                    repos.entry(key).or_insert_with(|| (root, Vec::new())).1.push((i, relative));
                } else { direct.push(i); }
            }
            // Inspect every Git repository before creating any worktree.
            let native = if repos.is_empty() { None } else { Some(manager.native()?) };
            for (root, _) in repos.values() {
                let commands = native.as_ref().unwrap().inspect(root).map_err(|e| format!("Cannot inspect project setup hooks: {e:#}"))?;
                let denied: Vec<_> = commands.iter().filter(|c| !c.approved).map(|c| c.template.as_str()).collect();
                if !denied.is_empty() { return Err(format!("Unapproved setup hooks require approval before preparing this session: {}", denied.join(", "))); }
            }
            let mut worktrees = Vec::new();
            let mut mapped: HashMap<usize, PathBuf> = HashMap::new();
            for (root, indexes) in repos.values() {
                let branch = unique_branch();
                let created = match native.as_ref().unwrap().create(root, &branch) {
                    Ok(path) => path,
                    Err(e) => return Err(if worktrees.is_empty() { format!("Cannot prepare Git worktree: {e:#}") } else { format!("Cannot prepare Git worktree: {e:#}. Already created worktrees were retained: {}", worktrees.iter().map(|w: &Workspace| w.path.as_str()).collect::<Vec<_>>().join(", ")) }),
                };
                let path = match fs::canonicalize(&created) { Ok(p) => p, Err(e) => return Err(format!("Cannot open created worktree {}: {e}; it was retained{}", created.display(), retained_suffix(&worktrees))) };
                let mut representative = None;
                for (i, relative) in indexes {
                    let cwd = path.join(relative);
                    mapped.insert(*i, cwd.clone());
                    representative.get_or_insert((*i, cwd));
                }
                if let Some((i, cwd)) = representative { worktrees.push(Workspace { project_path: selected[i].to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), branch, cwd: cwd.to_string_lossy().into_owned() }); }
            }
            let mut folders = Vec::new();
            for (i, source) in selected.iter().enumerate() {
                let path = mapped.get(&i).cloned().unwrap_or_else(|| source.clone());
                folders.push(ResolvedFolder { source: source.to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned() });
            }
            if selected.is_empty() {
                let scratch = manager.home.join("session-workspaces").join(unique_suffix());
                fs::create_dir_all(&scratch).map_err(|e| format!("Cannot create session scratch directory: {e}"))?;
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).map_err(|e| format!("Cannot secure session scratch directory: {e}"))?; }
                let cwd = fs::canonicalize(scratch).map_err(|e| e.to_string())?;
                return Ok(SessionWorkspace { cwd: cwd.to_string_lossy().into_owned(), roots: vec![cwd.to_string_lossy().into_owned()], folders, worktrees, scope_project_id: project.as_ref().map(|p| p.id.clone()), project_revision: project.as_ref().map(|p| p.revision) });
            }
            let primary_index = project.as_ref().and_then(|p| p.primary_folder.as_ref()).and_then(|primary| selected.iter().position(|f| f == Path::new(primary) || f.to_string_lossy().as_ref() == primary)).unwrap_or(0);
            let cwd = mapped.get(&primary_index).cloned().unwrap_or_else(|| selected[primary_index].clone());
            let mut roots: Vec<String> = folders.iter().map(|f| f.path.clone()).collect();
            roots.extend(worktrees.iter().map(|w| w.path.clone()));
            roots.sort(); roots.dedup();
            let snapshot = SessionWorkspace { cwd: cwd.to_string_lossy().into_owned(), roots, folders, worktrees, scope_project_id: project.as_ref().map(|p| p.id.clone()), project_revision: project.as_ref().map(|p| p.revision) };
            validate_session_snapshot(&snapshot).map_err(|e| if snapshot.worktrees.is_empty() { e } else { format!("{e}. Created worktrees were retained: {}", snapshot.worktrees.iter().map(|w| w.path.as_str()).collect::<Vec<_>>().join(", ")) })?;
            let _ = direct;
            Ok(snapshot)
        }).await.map_err(|e| format!("Session preparation failed: {e}"))?
    }

    pub fn save_session(&self, thread_id: &str, snapshot: &SessionWorkspace) -> Result<(), String> {
        validate_thread_id(thread_id)?;
        let dir = self.home.join("worktrees");
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create session metadata directory: {e}"))?;
        let sessions = self.home.join("worktree-sessions");
        fs::create_dir_all(&sessions)
            .map_err(|e| format!("Cannot create session marker directory: {e}"))?;
        let marker = sessions.join(thread_id);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(file) => file
                .sync_all()
                .map_err(|e| format!("Cannot persist session marker: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Cannot persist session marker: {e}")),
        }
        #[cfg(unix)]
        fs::File::open(&sessions)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())?;
        let dest = dir.join(format!("{thread_id}.json"));
        let tmp = dir.join(format!(".{thread_id}.{}.tmp", unique_suffix()));
        fs::write(
            &tmp,
            serde_json::to_vec(snapshot).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::File::open(&tmp)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &dest).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Cannot save session metadata: {e}")
        })?;
        #[cfg(unix)]
        fs::File::open(&dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_session(&self, thread_id: &str) -> Result<Option<SessionWorkspace>, String> {
        validate_thread_id(thread_id)?;
        let file = self
            .home
            .join("worktrees")
            .join(format!("{thread_id}.json"));
        match fs::read(file) {
            Ok(data) => {
                let value: serde_json::Value = serde_json::from_slice(&data).map_err(|e| format!("Session metadata is corrupt: {e}"))?;
                let snapshot: SessionWorkspace = if value.get("roots").is_some() {
                    serde_json::from_value(value).map_err(|e| format!("Session metadata is corrupt: {e}"))?
                } else {
                    let w: Workspace = serde_json::from_value(value).map_err(|e| format!("Session metadata is corrupt: {e}"))?;
                    SessionWorkspace { cwd:w.cwd.clone(), roots:vec![w.path.clone(),w.cwd.clone()],folders:vec![ResolvedFolder{source:w.project_path.clone(),path:w.cwd.clone()}],worktrees:vec![w],scope_project_id:None,project_revision:None }
                };
                self.validate_session_sync(&snapshot)?;
                Ok(Some(snapshot))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match fs::metadata(self.home.join("worktree-sessions").join(thread_id)) { Ok(_) => Err("Saved session metadata is missing. Restore the workspace metadata from backup before continuing.".into()), Err(m) if m.kind() == std::io::ErrorKind::NotFound => Ok(None), Err(m) => Err(m.to_string()) },
            Err(e) => Err(e.to_string()),
        }
    }

    pub async fn validate_session(&self, snapshot: &SessionWorkspace) -> Result<(), String> {
        let s = snapshot.clone();
        tokio::task::spawn_blocking(move || validate_session_snapshot(&s))
            .await
            .map_err(|e| e.to_string())?
    }
    fn validate_session_sync(&self, snapshot: &SessionWorkspace) -> Result<(), String> {
        validate_session_snapshot(snapshot)
    }
}

fn retained_suffix(worktrees: &[Workspace]) -> String {
    if worktrees.is_empty() {
        String::new()
    } else {
        format!(
            "; previously created worktrees were retained: {}",
            worktrees
                .iter()
                .map(|w| w.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
fn validate_session_snapshot(snapshot: &SessionWorkspace) -> Result<(), String> {
    let cwd = fs::canonicalize(&snapshot.cwd).map_err(|_| "Saved session folder is missing; restore it before continuing. Brr will not fall back to source folders.")?;
    for root in &snapshot.roots {
        let resolved = fs::canonicalize(root).map_err(|_| {
            format!("Saved session root is missing: {root}. Restore it before continuing.")
        })?;
        if !resolved.is_dir() {
            return Err(format!("Saved session root is not a directory: {root}"));
        }
        if resolved != Path::new(root) {
            return Err(format!(
                "Saved session root changed its canonical location: {root}"
            ));
        }
    }
    if cwd != Path::new(&snapshot.cwd) {
        return Err("Saved session starting folder changed its canonical location".into());
    }
    if !snapshot.roots.iter().any(|r| cwd.starts_with(r)) {
        return Err("Saved session cwd is outside its recorded roots".into());
    }
    for folder in &snapshot.folders {
        fs::canonicalize(&folder.path).map_err(|_| {
            format!(
                "Saved project folder is missing: {}. Restore it before continuing.",
                folder.path
            )
        })?;
    }
    for workspace in &snapshot.worktrees {
        validate_workspace(workspace)?;
    }
    Ok(())
}

fn selected_folder(cwd: &str) -> Result<PathBuf, String> {
    let selected =
        fs::canonicalize(cwd).map_err(|e| format!("Cannot open selected project folder: {e}"))?;
    if !selected.is_dir() {
        return Err("The selected project path is not a directory".into());
    }
    Ok(selected)
}

fn validate_workspace(workspace: &Workspace) -> Result<(), String> {
    let path = fs::canonicalize(&workspace.path).map_err(|_| "The saved worktree is missing. Restore it before continuing; Brr will not fall back to the source project.")?;
    let cwd = fs::canonicalize(&workspace.cwd).map_err(|_| "The saved worktree folder is missing. Restore it before continuing; Brr will not fall back to the source project.")?;
    let repo = Repository::at_fresh(&path)
        .map_err(|e| format!("Cannot open saved Git worktree: {e:#}"))?;
    let actual_root = repo
        .current_worktree()
        .root_fresh()
        .map_err(|e| format!("Cannot read saved Git worktree: {e:#}"))?;
    if fs::canonicalize(actual_root).map_err(|e| e.to_string())? != path || !cwd.starts_with(&path)
    {
        return Err("Saved paths no longer identify the conversation's worktree".into());
    }
    let branch = repo
        .current_worktree()
        .branch_fresh()
        .map_err(|e| e.to_string())?;
    if branch.as_deref() != Some(&workspace.branch) {
        return Err(format!(
            "The saved worktree branch changed. Restore {} before continuing.",
            workspace.branch
        ));
    }
    let source = Repository::at_fresh(&workspace.project_path)
        .map_err(|e| format!("Cannot open saved source project: {e:#}"))?;
    let source_common = fs::canonicalize(source.git_common_dir()).map_err(|e| e.to_string())?;
    let saved_common = fs::canonicalize(repo.git_common_dir()).map_err(|e| e.to_string())?;
    if source_common != saved_common {
        return Err("The saved worktree is no longer linked to its source repository".into());
    }
    Ok(())
}

pub(crate) fn unique_branch() -> String {
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
#[path = "worktrees_native_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "project_workspace_tests.rs"]
mod project_workspace_tests;
