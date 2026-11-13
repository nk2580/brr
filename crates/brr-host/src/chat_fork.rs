//! Journaled history branching. Inherited runtime history is context, not new authority.
use crate::{
    agent_host::AgentHost,
    worktrees::{unique_branch, ResolvedFolder, SessionWorkspace, Workspace},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

#[path = "chat_recovery.rs"]
mod recovery;

fn field<'a>(p: &'a Value, k: &str) -> Result<&'a str, String> {
    p[k].as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{k} is required"))
}

#[derive(Clone)]
struct RootMappingEntry {
    destination: PathBuf,
    branch: String,
    project_path: String,
    selected_path: PathBuf,
}

type RootMapping = HashMap<PathBuf, RootMappingEntry>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchRootSidecar {
    source_root: PathBuf,
    destination: PathBuf,
    branch: String,
    project_path: String,
    selected_path: PathBuf,
    setup_complete: bool,
}

fn branch_sidecar_path(root: &Path, operation_id: &str, branch: &str) -> PathBuf {
    root.join("chat-branch-staging")
        .join("operations")
        .join(operation_id)
        .join(format!("{}.json", branch.replace('/', "_")))
}

fn save_branch_sidecar(
    root: &Path,
    operation_id: &str,
    sidecar: &BranchRootSidecar,
) -> Result<(), String> {
    let path = branch_sidecar_path(root, operation_id, &sidecar.branch);
    let parent = path.parent().ok_or("Invalid branch sidecar path")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        uuid::Uuid::new_v4(),
        std::process::id()
    ));
    fs::write(
        &temporary,
        serde_json::to_vec(sidecar).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    fs::File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        error.to_string()
    })?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn read_branch_sidecars(root: &Path, operation_id: &str) -> Result<Vec<BranchRootSidecar>, String> {
    let directory = root
        .join("chat-branch-staging")
        .join("operations")
        .join(operation_id);
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut sidecars = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(entry.path()).map_err(|error| error.to_string())?;
        let sidecar = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "Branch sidecar {} is corrupt: {error}",
                entry.path().display()
            )
        })?;
        sidecars.push(sidecar);
    }
    Ok(sidecars)
}

fn map_path(path: &str, mappings: &RootMapping) -> String {
    let original = Path::new(path);
    mappings
        .iter()
        .filter_map(|(source, destination)| {
            original.strip_prefix(source).ok().map(|relative| {
                let mapped = if relative.as_os_str().is_empty() {
                    destination.destination.clone()
                } else {
                    destination.destination.join(relative)
                };
                (source.components().count(), mapped)
            })
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, mapped)| mapped.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

fn remap_workspace(source: &SessionWorkspace, mappings: &RootMapping) -> SessionWorkspace {
    let folders = source
        .folders
        .iter()
        .map(|folder| ResolvedFolder {
            source: folder.source.clone(),
            path: map_path(&folder.path, mappings),
        })
        .collect::<Vec<_>>();
    let mut worktrees = source
        .worktrees
        .iter()
        .map(|workspace| {
            let source_root = PathBuf::from(&workspace.path);
            if let Some(destination) = mappings.get(&source_root) {
                Workspace {
                    project_path: workspace.project_path.clone(),
                    path: destination.destination.to_string_lossy().into_owned(),
                    branch: destination.branch.clone(),
                    cwd: map_path(&workspace.cwd, mappings),
                }
            } else {
                workspace.clone()
            }
        })
        .collect::<Vec<_>>();
    let saved_roots = source
        .worktrees
        .iter()
        .map(|workspace| PathBuf::from(&workspace.path))
        .collect::<HashSet<_>>();
    for (source_root, destination) in mappings {
        if !saved_roots.contains(source_root) {
            worktrees.push(Workspace {
                project_path: destination.project_path.clone(),
                path: destination.destination.to_string_lossy().into_owned(),
                branch: destination.branch.clone(),
                cwd: map_path(&destination.selected_path.to_string_lossy(), mappings),
            });
        }
    }
    let mut roots = source
        .roots
        .iter()
        .map(|root| map_path(root, mappings))
        .collect::<Vec<_>>();
    roots.extend(folders.iter().map(|folder| folder.path.clone()));
    roots.extend(worktrees.iter().map(|workspace| workspace.path.clone()));
    roots.sort();
    roots.dedup();
    SessionWorkspace {
        cwd: map_path(&source.cwd, mappings),
        roots,
        folders,
        worktrees,
        scope_project_id: source.scope_project_id.clone(),
        project_revision: source.project_revision,
    }
}

fn session_branch_workspace(
    source: &SessionWorkspace,
    root_mappings: &RootMapping,
) -> SessionWorkspace {
    remap_workspace(source, root_mappings)
}

async fn create_snapshot_worktree(
    worktrees: &crate::worktrees::WorktreeManager,
    app_root: PathBuf,
    operation_id: String,
    source_root: PathBuf,
    staging_parent: PathBuf,
    project_path: String,
    selected_path: PathBuf,
) -> Result<RootMappingEntry, String> {
    let snapshot =
        crate::chat_branch::Snapshot::capture(&source_root, &staging_parent).map_err(|error| {
            format!(
                "Cannot capture saved source worktree {}: {error}",
                source_root.display()
            )
        })?;
    snapshot.verify_unchanged(&source_root).map_err(|error| {
        format!(
            "Saved source worktree changed during snapshot capture at {}: {error}",
            source_root.display()
        )
    })?;
    let head = snapshot.head().to_owned();
    let branch = unique_branch();
    let root = source_root.clone();
    let branch_name = branch.clone();
    let sidecar = BranchRootSidecar {
        source_root: source_root.clone(),
        destination: PathBuf::new(),
        branch: branch.clone(),
        project_path: project_path.clone(),
        selected_path: selected_path.clone(),
        setup_complete: false,
    };
    let sidecar_root = app_root.clone();
    let sidecar_operation = operation_id.clone();
    let manager = worktrees
        .native()
        .map_err(|error| format!("Cannot load WorkTrunk for branch creation: {error}"))?;
    let destination = tokio::task::spawn_blocking(move || {
        manager.create_snapshot(&root, &branch_name, &head, move |destination| {
            let mut pending = sidecar;
            pending.destination = destination.to_path_buf();
            save_branch_sidecar(&sidecar_root, &sidecar_operation, &pending)
                .map_err(std::io::Error::other)?;
            snapshot
                .materialize(destination)
                .map_err(std::io::Error::other)
                .map_err(Into::into)
        })
    })
    .await
    .map_err(|error| format!("Branch worktree setup task failed: {error}"))?
    .map_err(|error| {
        format!("Branch worktree setup failed; any partial snapshot was retained for inspection: {error:#}")
    })?;
    let destination = fs::canonicalize(&destination).map_err(|error| {
        format!(
            "Created branch worktree {} was retained but cannot be opened: {error}",
            destination.display()
        )
    })?;
    save_branch_sidecar(
        &app_root,
        &operation_id,
        &BranchRootSidecar {
            source_root: source_root.clone(),
            destination: destination.clone(),
            branch: branch.clone(),
            project_path: project_path.clone(),
            selected_path: selected_path.clone(),
            setup_complete: true,
        },
    )?;
    if destination == source_root
        || destination.starts_with(&source_root)
        || source_root.starts_with(&destination)
    {
        return Err(format!(
            "WorkTrunk created an overlapping branch path at {}; it was retained for inspection",
            destination.display()
        ));
    }
    Ok(RootMappingEntry {
        destination,
        branch,
        project_path,
        selected_path,
    })
}

fn git_root(path: &Path) -> Result<Option<PathBuf>, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .map_err(|error| format!("Cannot inspect Git root for {}: {error}", path.display()))?;
    if !output.status.success() {
        return Ok(None);
    }
    let root = String::from_utf8(output.stdout).map_err(|error| {
        format!(
            "Git returned a non-UTF-8 root for {}: {error}",
            path.display()
        )
    })?;
    fs::canonicalize(root.trim()).map(Some).map_err(|error| {
        format!(
            "Cannot canonicalize Git root for {}: {error}",
            path.display()
        )
    })
}

impl AgentHost {
    pub(crate) async fn create_chat_branch(self: &Arc<Self>, p: Value) -> Result<Value, String> {
        let source = field(&p, "threadId")?.to_owned();
        self.require_foreground(&source).await?;
        let _guard = self.questions.thread_guard(&source).await;
        let binding = self.store.session_binding(&source).await.ok();
        if binding
            .as_ref()
            .is_some_and(|b| !matches!(b.kind.as_str(), "agent" | "baseline"))
        {
            return Err("Open a regular agent chat to branch this workflow's result".into());
        }
        let source_workspace = self
            .worktrees
            .get_session(&source)?
            .ok_or("Saved workspace metadata is unavailable; restore it before branching")?;
        self.worktrees.validate_session(&source_workspace).await?;
        let sidebar_project_id = self.projects.membership(&source).await?;
        let loaded = self
            .rpc
            .call(
                "thread/read",
                json!({"threadId":source,"includeTurns":true}),
            )
            .await?;
        let mut turns = loaded["thread"]["turns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if loaded["thread"]["historyMode"] == "paginated" {
            turns.clear();
            let mut cursor: Option<String> = None;
            loop {
                let mut args =
                    json!({"threadId":source,"limit":100,"sortDirection":"asc","itemsView":"full"});
                if let Some(c) = cursor {
                    args["cursor"] = json!(c)
                }
                let page = self.rpc.call("thread/turns/list", args).await?;
                turns.extend(page["data"].as_array().cloned().unwrap_or_default());
                cursor = page["nextCursor"].as_str().map(str::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
        }
        if turns.iter().any(|t| t["status"] == "inProgress") {
            return Err("Wait for the source chat to finish before branching".into());
        }
        let boundary = field(&p, "turnId")?;
        let Some(index) = turns.iter().position(|t| t["id"] == boundary) else {
            return Err("That turn is no longer available".into());
        };
        if turns[index]["status"] != "completed" {
            return Err("Choose a completed turn to branch".into());
        }
        let editing = p["before"].as_bool().unwrap_or(false);
        let all_submissions = self
            .chat
            .dispatch_storage("submissions.list", json!({"threadId":source}))
            .await?;
        let retained_turns = &turns[..if editing { index } else { index + 1 }];
        let retained_submissions = all_submissions
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry["status"] == "accepted"
                    && retained_turns
                        .iter()
                        .any(|turn| turn["id"] == entry["turnId"])
            })
            .map(|entry| {
                json!({
                    "id":entry["id"],
                    "attachmentIds":entry["attachmentIds"].as_array().cloned().unwrap_or_default()
                })
            })
            .collect::<Vec<_>>();
        let editing_draft = if editing {
            let opening = all_submissions
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| {
                    entry["status"] == "accepted"
                        && entry["turnId"] == boundary
                        && entry["delivery"] != "steer"
                })
                .ok_or(
                    "Cannot edit this turn because its original submitted document is unavailable",
                )?;
            Some(json!({
                "sourceSubmissionId":opening["id"],
                "document":opening["document"],
                "settings":opening["settings"],
                "attachmentIds":opening["attachmentIds"].as_array().cloned().unwrap_or_default()
            }))
        } else {
            None
        };
        let operation_id = p["operationId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Ok(existing) = self
            .chat
            .dispatch_storage("operations.get", json!({"id":operation_id}))
            .await
        {
            if existing["sourceThreadId"] != source {
                return Err("Operation ID belongs to another branch".into());
            }
            if existing["phase"] == "ready" {
                return Ok(existing["result"].clone());
            }
            return Err(format!(
                "Branch setup {} is retained at phase {}. Inspect its saved paths before retrying.",
                operation_id, existing["phase"]
            ));
        }
        let mut journal = self
            .chat
            .dispatch_storage(
                "operations.put",
                json!({
                    "id":operation_id,
                    "kind":"branch",
                    "sourceThreadId":source,
                    "turnId":boundary,
                    "before":editing,
                    "phase":"preparing",
                    "sourceWorkspace":source_workspace.clone(),
                    "sidebarProjectId":sidebar_project_id.clone(),
                    "agentKind":binding.as_ref().map(|b| b.kind.as_str()).unwrap_or("baseline"),
                    "agentId":binding.as_ref().and_then(|b| b.agent_id.clone()),
                    "learningEligible":binding.as_ref().map(|b| b.learning_eligible).unwrap_or(true),
                    "retainedSubmissions":retained_submissions,
                    "editingDraft":editing_draft
                }),
            )
            .await?;
        self.pause_chat(&source, "Queue paused while creating a branch")
            .await;
        let result: Result<Value, String> = async {
            let kind = binding
                .as_ref()
                .map(|b| b.kind.as_str())
                .unwrap_or("baseline");
            let workspace = self
                .create_branch_workspace(&operation_id, &mut journal, &source_workspace)
                .await?;
            let source_thread_id = source.clone();
            let mut args = json!({
                "threadId":source_thread_id,
                "cwd":workspace.cwd,
                "runtimeWorkspaceRoots":workspace.roots,
                "modelProvider":"openai",
                "approvalPolicy":"on-request",
                "developerInstructions":crate::agent_protocol::instructions(kind),
                "config":crate::agent_host::workspace_config(kind,&workspace),
                "excludeTurns":false,
                "brrCopyHistory":true
            });
            args[if editing {
                "beforeTurnId"
            } else {
                "lastTurnId"
            }] = json!(boundary);
            let fork = self.rpc.call("thread/fork", args).await?;
            let id = field(&fork["thread"], "id")?.to_owned();
            journal = self
                .chat
                .dispatch_storage(
                    "operations.update",
                    json!({
                        "id":operation_id,
                        "expectedRevision":journal["revision"],
                        "phase":"history_forked",
                        "threadId":id,
                        "forkResult":fork,
                        "workspace":workspace,
                        "workspaceComplete":true
                    }),
                )
                .await?;
            recovery::finish_branch(self, &operation_id, &mut journal, fork).await
        }
        .await;
        if let Err(error) = &result {
            let _ = self
                .chat
                .dispatch_storage(
                    "operations.update",
                    json!({
                        "id":operation_id,
                        "expectedRevision":journal["revision"],
                        "phase":"needs_attention",
                        "error":error
                    }),
                )
                .await;
        }
        result
    }

    async fn create_branch_workspace(
        &self,
        operation_id: &str,
        journal: &mut Value,
        source: &SessionWorkspace,
    ) -> Result<SessionWorkspace, String> {
        if source.folders.is_empty() && source.worktrees.is_empty() {
            let cwd = self
                .root
                .join("sessions")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&cwd, fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("Cannot secure branch scratch directory: {error}"))?;
            }
            let cwd = fs::canonicalize(cwd).map_err(|error| error.to_string())?;
            let branch = SessionWorkspace {
                cwd: cwd.to_string_lossy().into_owned(),
                roots: vec![cwd.to_string_lossy().into_owned()],
                folders: vec![],
                worktrees: vec![],
                scope_project_id: source.scope_project_id.clone(),
                project_revision: source.project_revision,
            };
            *journal = self
                .chat
                .dispatch_storage(
                    "operations.update",
                    json!({"id":operation_id,"expectedRevision":journal["revision"],"phase":"workspace_ready","workspace":branch,"workspaceComplete":true}),
                )
                .await?;
            return Ok(branch);
        }

        let mut source_roots = HashMap::<PathBuf, (String, PathBuf)>::new();
        for workspace in &source.worktrees {
            let root = fs::canonicalize(&workspace.path).map_err(|error| {
                format!(
                    "Cannot open saved source worktree {}; it was retained: {error}",
                    workspace.path
                )
            })?;
            source_roots.entry(root).or_insert_with(|| {
                (
                    workspace.project_path.clone(),
                    PathBuf::from(&workspace.cwd),
                )
            });
        }
        // A valid saved session normally records each Git root in worktrees.
        // Also discover roots carried by saved folders/roots so a partial legacy
        // snapshot cannot silently turn a Git folder into shared source access.
        for path in source
            .folders
            .iter()
            .map(|folder| folder.path.as_str())
            .chain(source.roots.iter().map(String::as_str))
            .chain(std::iter::once(source.cwd.as_str()))
        {
            let path = Path::new(path);
            let Some(root) = git_root(path)? else {
                continue;
            };
            let selected_path = fs::canonicalize(path).map_err(|error| {
                format!(
                    "Cannot open saved project folder {}: {error}",
                    path.display()
                )
            })?;
            let project_path = source
                .folders
                .iter()
                .find(|folder| Path::new(&folder.path).starts_with(&root))
                .map(|folder| folder.source.clone())
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            source_roots
                .entry(root)
                .or_insert_with(|| (project_path, selected_path));
        }
        let mut ordered_roots = source_roots.into_iter().collect::<Vec<_>>();
        ordered_roots.sort_by(|left, right| left.0.cmp(&right.0));
        if ordered_roots.is_empty() {
            let branch = remap_workspace(source, &RootMapping::new());
            self.worktrees.validate_session(&branch).await?;
            *journal = self
                .chat
                .dispatch_storage(
                    "operations.update",
                    json!({"id":operation_id,"expectedRevision":journal["revision"],"phase":"workspace_ready","workspace":branch}),
                )
                .await?;
            return Ok(branch);
        }
        let stage = self.root.join("chat-branch-staging");
        fs::create_dir_all(&stage)
            .map_err(|error| format!("Cannot create branch staging directory: {error}"))?;
        let manager = self
            .worktrees
            .native()
            .map_err(|error| format!("Cannot load WorkTrunk for branch creation: {error}"))?;
        for (root, _) in &ordered_roots {
            let commands = manager.inspect(root).map_err(|error| {
                format!(
                    "Cannot inspect setup hooks for {}: {error:#}",
                    root.display()
                )
            })?;
            let denied = commands
                .iter()
                .filter(|command| !command.approved)
                .map(|command| command.template.as_str())
                .collect::<Vec<_>>();
            if !denied.is_empty() {
                return Err(format!(
                    "Unapproved setup hooks require approval before branching {}: {}",
                    root.display(),
                    denied.join(", ")
                ));
            }
        }
        let mut mappings = RootMapping::new();
        for (source_root, (project_path, selected_path)) in ordered_roots {
            let mapping = create_snapshot_worktree(
                &self.worktrees,
                self.root.clone(),
                operation_id.to_owned(),
                source_root.clone(),
                stage.clone(),
                project_path,
                selected_path,
            )
            .await?;
            mappings.insert(source_root, mapping);
            let partial = remap_workspace(source, &mappings);
            *journal = self
                .chat
                .dispatch_storage(
                    "operations.update",
                    json!({
                        "id":operation_id,
                        "expectedRevision":journal["revision"],
                        "phase":"workspace_preparing",
                        "workspace":partial,
                        "workspaceComplete":false
                    }),
                )
                .await?;
        }
        let branch = session_branch_workspace(source, &mappings);
        self.worktrees
            .validate_session(&branch)
            .await
            .map_err(|error| {
                format!("Created branch workspace was retained for inspection: {error}")
            })?;
        *journal = self
            .chat
            .dispatch_storage(
                "operations.update",
                json!({
                    "id":operation_id,
                    "expectedRevision":journal["revision"],
                    "phase":"workspace_ready",
                    "workspace":branch,
                    "workspaceComplete":true
                }),
            )
            .await?;
        Ok(branch)
    }
}

#[cfg(test)]
mod branch_workspace_tests {
    use super::*;

    #[tokio::test]
    async fn native_branch_snapshot_maps_nested_cwd_and_keeps_non_git_folder_shared() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source-repo");
        let selected = root.join("packages/app");
        let shared = temp.path().join("shared-notes");
        fs::create_dir_all(&selected).unwrap();
        fs::create_dir_all(&shared).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(["-C", root.to_str().unwrap()])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        fs::write(root.join("packages/app/main.rs"), "committed\n").unwrap();
        fs::write(root.join("packages/app/remove.txt"), "remove me\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Brr Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "fixture",
        ]);
        fs::write(root.join("packages/app/main.rs"), "working copy\n").unwrap();
        fs::remove_file(root.join("packages/app/remove.txt")).unwrap();
        fs::write(root.join("packages/app/new.txt"), "untracked\n").unwrap();
        let index = fs::read(root.join(".git/index")).unwrap();
        let stage = temp.path().join("staging");
        fs::create_dir_all(&stage).unwrap();
        let manager = crate::worktrees::WorktreeManager::with_config(
            temp.path().join("engine"),
            worktrunk::config::UserConfig::default(),
            temp.path().join("approvals.toml"),
        );
        let source_root = root.canonicalize().unwrap();
        let selected = selected.canonicalize().unwrap();
        let shared = shared.canonicalize().unwrap();
        let source = SessionWorkspace {
            cwd: selected.to_string_lossy().into_owned(),
            roots: vec![
                source_root.to_string_lossy().into_owned(),
                selected.to_string_lossy().into_owned(),
                shared.to_string_lossy().into_owned(),
            ],
            folders: vec![
                ResolvedFolder {
                    source: selected.to_string_lossy().into_owned(),
                    path: selected.to_string_lossy().into_owned(),
                },
                ResolvedFolder {
                    source: shared.to_string_lossy().into_owned(),
                    path: shared.to_string_lossy().into_owned(),
                },
            ],
            worktrees: vec![Workspace {
                project_path: selected.to_string_lossy().into_owned(),
                path: source_root.to_string_lossy().into_owned(),
                branch: "main".into(),
                cwd: selected.to_string_lossy().into_owned(),
            }],
            scope_project_id: Some("stable-project-id".into()),
            project_revision: Some(9),
        };
        let mapping = create_snapshot_worktree(
            &manager,
            temp.path().to_path_buf(),
            "native-test-operation".into(),
            source_root.clone(),
            stage,
            selected.to_string_lossy().into_owned(),
            selected.clone(),
        )
        .await
        .unwrap();
        let branch = remap_workspace(&source, &HashMap::from([(source_root.clone(), mapping)]));
        manager.validate_session(&branch).await.unwrap();

        let branch_root = Path::new(&branch.worktrees[0].path);
        let branch_cwd = Path::new(&branch.cwd);
        assert_eq!(branch.cwd, branch.worktrees[0].cwd);
        assert!(branch_cwd.starts_with(branch_root));
        assert_eq!(
            branch.cwd,
            branch_root.join("packages/app").to_string_lossy()
        );
        assert_eq!(branch.worktrees[0].project_path, selected.to_string_lossy());
        assert_eq!(branch.folders[0].source, selected.to_string_lossy());
        assert_eq!(branch.folders[0].path, branch.cwd);
        assert_eq!(branch.folders[1].path, shared.to_string_lossy());
        assert_eq!(
            branch.scope_project_id.as_deref(),
            Some("stable-project-id")
        );
        assert_eq!(branch.project_revision, Some(9));
        assert_eq!(
            fs::read(branch_root.join("packages/app/main.rs")).unwrap(),
            b"working copy\n"
        );
        assert!(!branch_root.join("packages/app/remove.txt").exists());
        assert_eq!(
            fs::read(branch_root.join("packages/app/new.txt")).unwrap(),
            b"untracked\n"
        );
        assert_eq!(fs::read(root.join(".git/index")).unwrap(), index);
        assert_eq!(
            fs::read(root.join("packages/app/main.rs")).unwrap(),
            b"working copy\n"
        );
        assert_ne!(branch.worktrees[0].project_path, "stable-project-id");
    }

    #[test]
    fn remapping_preserves_scope_metadata_and_original_source_paths() {
        let source = SessionWorkspace {
            cwd: "/source/repo/packages/app".into(),
            roots: vec![
                "/source/repo".into(),
                "/source/repo/packages/app".into(),
                "/shared/notes".into(),
            ],
            folders: vec![
                ResolvedFolder {
                    source: "/original/repo/packages/app".into(),
                    path: "/source/repo/packages/app".into(),
                },
                ResolvedFolder {
                    source: "/shared/notes".into(),
                    path: "/shared/notes".into(),
                },
            ],
            worktrees: vec![Workspace {
                project_path: "/original/repo/packages/app".into(),
                path: "/source/repo".into(),
                branch: "source-branch".into(),
                cwd: "/source/repo/packages/app".into(),
            }],
            scope_project_id: Some("stable-project-id".into()),
            project_revision: Some(17),
        };
        let mapping = HashMap::from([(
            PathBuf::from("/source/repo"),
            RootMappingEntry {
                destination: PathBuf::from("/branch/repo"),
                branch: "branch-1".into(),
                project_path: "/original/repo/packages/app".into(),
                selected_path: PathBuf::from("/source/repo/packages/app"),
            },
        )]);

        let branch = remap_workspace(&source, &mapping);
        assert_eq!(
            branch.scope_project_id.as_deref(),
            Some("stable-project-id")
        );
        assert_eq!(branch.project_revision, Some(17));
        assert_eq!(branch.cwd, "/branch/repo/packages/app");
        assert!(branch.roots.contains(&"/branch/repo".into()));
        assert!(branch.roots.contains(&"/shared/notes".into()));
        assert_eq!(branch.folders[0].source, "/original/repo/packages/app");
        assert_eq!(branch.folders[0].path, "/branch/repo/packages/app");
        assert_eq!(branch.folders[1].path, "/shared/notes");
        assert_eq!(
            branch.worktrees[0].project_path,
            "/original/repo/packages/app"
        );
        assert_eq!(branch.worktrees[0].path, "/branch/repo");
        assert_eq!(branch.worktrees[0].cwd, "/branch/repo/packages/app");
        assert_ne!(branch.worktrees[0].project_path, "stable-project-id");
    }
}
