use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
pub use worktrunk::embedded::SetupCommand;
use worktrunk::{config::UserConfig, embedded::SessionManager, git::Repository};

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

    fn native(&self) -> Result<SessionManager, String> {
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
        let workspace = workspace.clone();
        tokio::task::spawn_blocking(move || validate_workspace(&workspace))
            .await
            .map_err(|e| format!("Saved worktree validation failed: {e}"))?
    }
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
#[path = "worktrees_native_tests.rs"]
mod tests;
