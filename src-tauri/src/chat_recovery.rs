//! Explicit recovery for interrupted chat branch transactions.
use super::*;
use brr_agent_core::SessionBinding;
use serde_json::json;
use std::collections::HashSet;

impl AgentHost {
    pub(crate) async fn require_chat_ready(&self, thread_id: &str) -> Result<(), String> {
        let operations = self
            .chat
            .dispatch_storage("operations.list", json!({}))
            .await?;
        let operations = operations
            .as_array()
            .ok_or("Saved branch operation journal is malformed")?;
        for operation in operations
            .iter()
            .filter(|operation| operation["kind"] == "branch" && operation["phase"] != "ready")
        {
            let operation_id = operation["id"].as_str().unwrap_or("unknown");
            if operation["threadId"] == thread_id {
                return Err(format!(
                    "Chat is fenced by incomplete branch operation {operation_id}. Inspect or recover that branch before continuing."
                ));
            }
            let Some(source) = operation["sourceThreadId"].as_str() else {
                continue;
            };
            let mut candidate_cwds = operation["workspace"]["cwd"]
                .as_str()
                .map(str::to_owned)
                .into_iter()
                .collect::<Vec<_>>();
            for sidecar in super::read_branch_sidecars(&self.root, operation_id)? {
                if let Ok(relative) = sidecar.selected_path.strip_prefix(&sidecar.source_root) {
                    candidate_cwds.push(
                        sidecar
                            .destination
                            .join(relative)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            candidate_cwds.sort();
            candidate_cwds.dedup();
            for cwd in candidate_cwds {
                if runtime_has_fork(self, source, &cwd, Some(thread_id)).await? {
                    return Err(format!(
                        "Chat is fenced by an unrecorded runtime fork for branch operation {operation_id}. Recover that branch before continuing."
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn pending_chat_branches(&self) -> Result<Value, String> {
        let operations = self
            .chat
            .dispatch_storage("operations.list", json!({}))
            .await?;
        let mut pending = operations
            .as_array()
            .ok_or("Saved branch operation journal is malformed")?
            .iter()
            .filter(|operation| operation["kind"] == "branch" && operation["phase"] != "ready")
            .cloned()
            .collect::<Vec<_>>();
        for operation in &mut pending {
            if let Some(id) = operation["id"].as_str() {
                let sidecars = super::read_branch_sidecars(&self.root, id)?;
                let paths = sidecars
                    .iter()
                    .map(|sidecar| {
                        json!({
                            "sourceRoot":sidecar.source_root,
                            "destination":sidecar.destination,
                            "branch":sidecar.branch,
                            "setupComplete":sidecar.setup_complete
                        })
                    })
                    .collect::<Vec<_>>();
                if operation["workspace"]["cwd"].as_str().is_none() {
                    if let Some(path) = sidecars.first() {
                        let relative = path
                            .selected_path
                            .strip_prefix(&path.source_root)
                            .unwrap_or_else(|_| Path::new(""));
                        let cwd = if relative.as_os_str().is_empty() {
                            path.destination.clone()
                        } else {
                            path.destination.join(relative)
                        };
                        operation["cwd"] = json!(cwd);
                    }
                } else {
                    operation["cwd"] = operation["workspace"]["cwd"].clone();
                }
                operation["retainedPaths"] = json!(paths);
            }
        }
        Ok(json!({"data":pending}))
    }

    pub(crate) async fn recover_chat_branch(
        self: &Arc<Self>,
        params: Value,
    ) -> Result<Value, String> {
        let operation_id = field(&params, "operationId")?;
        let mut operation = self
            .chat
            .dispatch_storage("operations.get", json!({"id":operation_id}))
            .await?;
        if operation["kind"] != "branch" {
            return Err("That operation is not a chat branch".into());
        }
        if operation["phase"] == "ready" {
            return Ok(operation["result"].clone());
        }
        let source = operation["sourceThreadId"]
            .as_str()
            .ok_or("Branch journal is missing its source conversation")?
            .to_owned();
        let workspace: SessionWorkspace = match operation.get("workspace") {
            Some(value)
                if value.is_object()
                    && (operation["workspaceComplete"] == true
                        || (operation["workspaceComplete"].is_null()
                            && matches!(
                                operation["phase"].as_str(),
                                Some("workspace_ready" | "history_forked")
                            ))) =>
            {
                serde_json::from_value(value.clone()).map_err(|error| {
                    unresolved(
                        operation_id,
                        &format!("workspace snapshot is invalid: {error}"),
                    )
                })?
            }
            _ => recover_workspace_from_sidecars(self, operation_id, &mut operation).await?,
        };
        self.worktrees
            .validate_session(&workspace)
            .await
            .map_err(|error| {
                unresolved(
                    operation_id,
                    &format!("retained branch workspace is not ready: {error}"),
                )
            })?;

        let fork = match operation
            .get("forkResult")
            .filter(|result| result["thread"]["id"].as_str().is_some())
        {
            Some(result) => result.clone(),
            None => {
                let cwd = &workspace.cwd;
                let candidates = runtime_forks(self, &source, cwd).await.map_err(|error| {
                    unresolved(
                        operation_id,
                        &format!("cannot inspect runtime fork history: {error}"),
                    )
                })?;
                match candidates.as_slice() {
                    [candidate] => {
                        let id = candidate["id"].as_str().ok_or_else(|| {
                            unresolved(operation_id, "matching runtime fork has no thread ID")
                        })?;
                        self.rpc
                            .call("thread/read", json!({"threadId":id,"includeTurns":true}))
                            .await
                            .map_err(|error| {
                                unresolved(
                                    operation_id,
                                    &format!("cannot read matching runtime fork: {error}"),
                                )
                            })?
                    }
                    [] => {
                        return Err(unresolved(
                            operation_id,
                            "no runtime fork is recorded; it is unsafe to retry thread/fork automatically",
                        ));
                    }
                    _ => {
                        return Err(unresolved(
                            operation_id,
                            "multiple runtime forks match this branch; choose one after inspection",
                        ));
                    }
                }
            }
        };
        let thread = &fork["thread"];
        let id = thread["id"]
            .as_str()
            .ok_or_else(|| unresolved(operation_id, "fork evidence has no thread ID"))?
            .to_owned();
        if id == source
            || thread["forkedFromId"].as_str() != Some(source.as_str())
            || thread["cwd"].as_str() != Some(workspace.cwd.as_str())
        {
            return Err(unresolved(
                operation_id,
                "fork evidence does not match the recorded source conversation and destination folder",
            ));
        }
        if operation["forkResult"]["thread"]["id"].as_str().is_none() {
            operation = self
                .chat
                .dispatch_storage(
                    "operations.update",
                    json!({
                        "id":operation_id,
                        "expectedRevision":operation["revision"],
                        "phase":"history_forked",
                        "threadId":id,
                        "forkResult":fork,
                        "workspace":workspace
                    }),
                )
                .await?;
        }
        let result = finish_branch(self, operation_id, &mut operation, fork).await;
        if let Err(error) = &result {
            let _ = self
                .chat
                .dispatch_storage(
                    "operations.update",
                    json!({
                        "id":operation_id,
                        "expectedRevision":operation["revision"],
                        "phase":"needs_attention",
                        "error":error
                    }),
                )
                .await;
        }
        result
    }
}

async fn recover_workspace_from_sidecars(
    host: &AgentHost,
    operation_id: &str,
    operation: &mut Value,
) -> Result<SessionWorkspace, String> {
    let source: SessionWorkspace = serde_json::from_value(
        operation
            .get("sourceWorkspace")
            .cloned()
            .ok_or_else(|| unresolved(operation_id, "source workspace snapshot is missing"))?,
    )
    .map_err(|error| {
        unresolved(
            operation_id,
            &format!("source workspace snapshot is invalid: {error}"),
        )
    })?;
    let sidecars = super::read_branch_sidecars(&host.root, operation_id)
        .map_err(|error| unresolved(operation_id, &error))?;
    if sidecars.is_empty() || sidecars.iter().any(|sidecar| !sidecar.setup_complete) {
        return Err(unresolved(
            operation_id,
            "destination setup did not complete for every recorded Git root; inspect retained paths before continuing",
        ));
    }
    let mut expected_roots = HashSet::new();
    for workspace in &source.worktrees {
        expected_roots.insert(fs::canonicalize(&workspace.path).map_err(|error| {
            unresolved(
                operation_id,
                &format!(
                    "saved source root {} is unavailable: {error}",
                    workspace.path
                ),
            )
        })?);
    }
    for path in source
        .folders
        .iter()
        .map(|folder| folder.path.as_str())
        .chain(source.roots.iter().map(String::as_str))
        .chain(std::iter::once(source.cwd.as_str()))
    {
        if let Some(root) =
            super::git_root(Path::new(path)).map_err(|error| unresolved(operation_id, &error))?
        {
            expected_roots.insert(root);
        }
    }
    if expected_roots.is_empty() || sidecars.len() != expected_roots.len() {
        return Err(unresolved(
            operation_id,
            "retained destination records do not cover every distinct saved Git root",
        ));
    }
    let mut mappings = super::RootMapping::new();
    for sidecar in sidecars {
        let source_root = fs::canonicalize(&sidecar.source_root).map_err(|error| {
            unresolved(
                operation_id,
                &format!(
                    "saved source root {} is unavailable: {error}",
                    sidecar.source_root.display()
                ),
            )
        })?;
        let destination = fs::canonicalize(&sidecar.destination).map_err(|error| {
            unresolved(
                operation_id,
                &format!(
                    "retained destination {} is unavailable: {error}",
                    sidecar.destination.display()
                ),
            )
        })?;
        if destination == source_root
            || destination.starts_with(&source_root)
            || source_root.starts_with(&destination)
            || !sidecar.selected_path.starts_with(&source_root)
        {
            return Err(unresolved(
                operation_id,
                "retained destination overlaps its source or has an invalid selected folder",
            ));
        }
        if !expected_roots.contains(&source_root) || mappings.contains_key(&source_root) {
            return Err(unresolved(
                operation_id,
                "retained destination records contain an unexpected or duplicate Git root",
            ));
        }
        mappings.insert(
            source_root,
            super::RootMappingEntry {
                destination,
                branch: sidecar.branch,
                project_path: sidecar.project_path,
                selected_path: sidecar.selected_path,
            },
        );
    }
    if mappings.len() != expected_roots.len() {
        return Err(unresolved(
            operation_id,
            "retained destination records are incomplete",
        ));
    }
    let workspace = super::session_branch_workspace(&source, &mappings);
    host.worktrees
        .validate_session(&workspace)
        .await
        .map_err(|error| {
            unresolved(
                operation_id,
                &format!("retained branch workspace is not ready: {error}"),
            )
        })?;
    *operation = host
        .chat
        .dispatch_storage(
            "operations.update",
            json!({
                "id":operation_id,
                "expectedRevision":operation["revision"],
                "phase":"workspace_ready",
                "workspace":workspace,
                "workspaceComplete":true
            }),
        )
        .await?;
    Ok(workspace)
}

fn remap_edit_references(
    document: &mut Value,
    source: &SessionWorkspace,
    destination: &SessionWorkspace,
) {
    let mut roots = Vec::<(std::path::PathBuf, std::path::PathBuf)>::new();
    for original in &source.worktrees {
        if let Some(branch) = destination
            .worktrees
            .iter()
            .find(|candidate| candidate.project_path == original.project_path)
        {
            roots.push((original.path.clone().into(), branch.path.clone().into()));
        }
    }
    for original in &source.folders {
        if let Some(branch) = destination
            .folders
            .iter()
            .find(|candidate| candidate.source == original.source)
        {
            if original.path != branch.path {
                roots.push((original.path.clone().into(), branch.path.clone().into()));
            }
        }
    }
    roots.sort_by_key(|(root, _)| std::cmp::Reverse(root.components().count()));
    let Some(references) = document["references"].as_array_mut() else {
        return;
    };
    for reference in references {
        if !matches!(reference["type"].as_str(), Some("file" | "folder")) {
            continue;
        }
        for field in ["id", "path"] {
            let Some(value) = reference[field].as_str() else {
                continue;
            };
            let original = std::path::Path::new(value);
            if !original.is_absolute() {
                continue;
            }
            if let Some((_, mapped)) = roots.iter().find_map(|(old, new)| {
                original
                    .strip_prefix(old)
                    .ok()
                    .map(|relative| (old, new.join(relative)))
            }) {
                reference[field] = json!(mapped.to_string_lossy());
            }
        }
    }
}

pub(super) async fn finish_branch(
    host: &AgentHost,
    operation_id: &str,
    operation: &mut Value,
    mut fork: Value,
) -> Result<Value, String> {
    let source = field(operation, "sourceThreadId")?.to_owned();
    let workspace: SessionWorkspace = serde_json::from_value(
        operation
            .get("workspace")
            .cloned()
            .ok_or("Branch journal is missing its workspace snapshot")?,
    )
    .map_err(|error| format!("Branch workspace journal is invalid: {error}"))?;
    host.worktrees
        .validate_session(&workspace)
        .await
        .map_err(|error| format!("Branch workspace was retained for inspection: {error}"))?;
    let source_workspace: SessionWorkspace = serde_json::from_value(
        operation
            .get("sourceWorkspace")
            .cloned()
            .ok_or("Branch journal is missing its source workspace")?,
    )
    .map_err(|error| format!("Branch source workspace journal is invalid: {error}"))?;
    let id = field(&fork["thread"], "id")?.to_owned();
    let kind = operation["agentKind"].as_str().unwrap_or("baseline");
    let sidebar_project_id = operation["sidebarProjectId"].as_str();
    let binding = SessionBinding {
        thread_id: id.clone(),
        agent_id: operation["agentId"].as_str().map(str::to_owned),
        kind: kind.to_owned(),
        project_id: workspace.scope_project_id.clone(),
        cwd: Some(workspace.cwd.clone()),
        task_run_id: None,
        learning_eligible: operation["learningEligible"].as_bool().unwrap_or(true),
    };

    host.worktrees
        .save_session(&id, &workspace)
        .map_err(|error| {
            format!(
                "Thread {id} and branch workspace {} were retained: {error}",
                workspace.cwd
            )
        })?;
    host.store.bind_session(binding.clone()).await?;
    let active_sidebar_project = match sidebar_project_id {
        Some(project_id) => host
            .projects
            .read(project_id)
            .await?
            .filter(|project| !project.hidden)
            .map(|project| project.id),
        None => None,
    };
    host.projects
        .set_membership(&id, active_sidebar_project.as_deref())
        .await?;
    host.rpc
        .call(
            "thread/dynamicTools/replace",
            json!({"threadId":id,"tools":crate::agent_protocol::tools(kind)}),
        )
        .await?;

    let submissions = operation["retainedSubmissions"]
        .as_array()
        .ok_or_else(|| unresolved(operation_id, "retained attachment manifest is missing"))?;
    for entry in submissions {
        let owner = field(entry, "id")?;
        let ids = entry["attachmentIds"]
            .as_array()
            .ok_or_else(|| unresolved(operation_id, "retained attachment manifest is malformed"))?
            .iter()
            .filter_map(|id| id.as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        host.chat.copy_attachment_grants(owner, &id, &ids).await?;
        host.chat.stage_for_thread(&id, owner, &ids).await?;
    }

    let editing_draft = if operation
        .get("editingDraft")
        .is_some_and(|draft| draft.is_object())
    {
        let draft = &operation["editingDraft"];
        let owner = field(draft, "sourceSubmissionId")?;
        let attachment_ids = draft["attachmentIds"]
            .as_array()
            .ok_or_else(|| unresolved(operation_id, "edit attachment manifest is malformed"))?
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        host.chat
            .copy_attachment_grants(owner, &id, &attachment_ids)
            .await?;
        let mut document = draft["document"].clone();
        remap_edit_references(&mut document, &source_workspace, &workspace);
        document["attachmentIds"] = json!(attachment_ids);
        let expected = json!({
            "id":id,
            "threadId":id,
            "projectId":active_sidebar_project,
            "agentId":binding.agent_id,
            "document":document,
            "settings":draft["settings"]
        });
        match host
            .chat
            .dispatch_storage("drafts.get", json!({"id":id}))
            .await
        {
            Ok(saved) => {
                for key in ["threadId", "projectId", "agentId", "document", "settings"] {
                    if saved[key] != expected[key] {
                        return Err(unresolved(
                            operation_id,
                            "a conflicting draft already exists for the recovered thread",
                        ));
                    }
                }
                saved
            }
            Err(error) if error == "Draft not found" => {
                host.chat
                    .dispatch_storage(
                        "drafts.save",
                        json!({
                            "id":id,
                            "threadId":id,
                            "projectId":active_sidebar_project,
                            "agentId":binding.agent_id,
                            "document":document,
                            "settings":draft["settings"],
                            "expectedRevision":0
                        }),
                    )
                    .await?
            }
            Err(error) => return Err(error),
        }
    } else {
        Value::Null
    };

    let turn_id = field(operation, "turnId")?.to_owned();
    fork["thread"]["brr"] = json!(binding);
    fork["thread"]["projectId"] = json!(active_sidebar_project);
    fork["thread"]["workspace"] = json!(workspace);
    if let Some(worktree) = workspace.worktrees.first() {
        fork["thread"]["worktree"] = json!(worktree);
    }
    fork["thread"]["branchOf"] = json!({
        "threadId":source,
        "turnId":turn_id,
        "usesCurrentFiles":true,
        "sharedFolder":workspace.folders.iter().any(|folder| {
            source_workspace.folders.iter().any(|source_folder| {
                source_folder.source == folder.source && source_folder.path == folder.path
            })
        })
    });
    if !editing_draft.is_null() {
        fork["draft"] = editing_draft;
    }
    let lineage = fork["thread"]["branchOf"].clone();
    sqlx::query("INSERT OR REPLACE INTO chat_branch_lineage(thread_id,lineage) VALUES(?,?)")
        .bind(&id)
        .bind(lineage.to_string())
        .execute(&host.chat.pool)
        .await
        .map_err(|error| error.to_string())?;
    *operation = host
        .chat
        .dispatch_storage(
            "operations.update",
            json!({
                "id":operation_id,
                "expectedRevision":operation["revision"],
                "phase":"ready",
                "result":fork,
                "forkResult":fork,
                "workspace":workspace
            }),
        )
        .await?;
    host.chat_changed(Some(&id));
    Ok(operation["result"].clone())
}

fn unresolved(operation_id: &str, reason: &str) -> String {
    format!("Branch operation {operation_id} remains pending for inspection: {reason}")
}

async fn runtime_has_fork(
    host: &AgentHost,
    source: &str,
    cwd: &str,
    target_id: Option<&str>,
) -> Result<bool, String> {
    let candidates = runtime_forks(host, source, cwd).await?;
    Ok(candidates
        .iter()
        .any(|candidate| target_id.is_none_or(|target| candidate["id"].as_str() == Some(target))))
}

async fn runtime_forks(host: &AgentHost, source: &str, cwd: &str) -> Result<Vec<Value>, String> {
    let mut matches = Vec::new();
    let mut seen = HashSet::new();
    for archived in [false, true] {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        loop {
            let mut params = json!({
                "limit":100,
                "sortKey":"updated_at",
                "sortDirection":"desc",
                "archived":archived
            });
            if let Some(cursor_value) = &cursor {
                params["cursor"] = json!(cursor_value);
            }
            let page = host.rpc.call("thread/list", params).await?;
            let data = page["data"]
                .as_array()
                .ok_or("thread/list returned malformed branch recovery data")?;
            for thread in data {
                if thread["id"].as_str() == Some(source) {
                    continue;
                }
                if thread["forkedFromId"].as_str() == Some(source)
                    && thread["cwd"].as_str() == Some(cwd)
                {
                    let id = thread["id"]
                        .as_str()
                        .ok_or("thread/list returned a fork without an ID")?;
                    if seen.insert(id.to_owned()) {
                        matches.push(thread.clone());
                    }
                }
            }
            match page.get("nextCursor") {
                Some(Value::Null) => break,
                Some(Value::String(next)) if !next.is_empty() => {
                    if !seen_cursors.insert(next.clone()) {
                        return Err("thread/list repeated a recovery cursor".into());
                    }
                    cursor = Some(next.clone());
                }
                _ => return Err("thread/list returned an invalid recovery cursor".into()),
            }
        }
    }
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::Engine;
    use std::sync::Arc;
    use tokio::process::Command;

    async fn recovery_fixture(
        directory: &Path,
        candidate_count: usize,
    ) -> (Engine, String, SessionWorkspace) {
        let scratch = directory.join("branch-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let scratch = std::fs::canonicalize(scratch).unwrap();
        let workspace = SessionWorkspace {
            cwd: scratch.to_string_lossy().into_owned(),
            roots: vec![scratch.to_string_lossy().into_owned()],
            folders: vec![],
            worktrees: vec![],
            scope_project_id: None,
            project_revision: None,
        };
        let state = directory.join("runtime.json");
        let candidates = (0..candidate_count)
            .map(|index| {
                json!({
                    "id":format!("candidate-{index}"),
                    "cwd":workspace.cwd,
                    "forkedFromId":"source-chat",
                    "parentThreadId":null,
                    "turns":[]
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(&state, json!({"threads":candidates,"calls":[]}).to_string()).unwrap();
        let script = directory.join("runtime.py");
        std::fs::write(
            &script,
            r#"
import json,sys
state_path=sys.argv[1]
def load(): return json.load(open(state_path))
def save(v): json.dump(v,open(state_path,'w'))
def emit(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    method=m['method']; p=m.get('params',{}); state=load()
    state['calls'].append(method); save(state)
    result={}
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='initialize': result={'capabilities':{}}
    elif method=='thread/list': result={'data':state['threads'],'nextCursor':None}
    elif method=='thread/read':
        result={'thread':next(t for t in state['threads'] if t['id']==p['threadId'])}
    elif method=='thread/fork': result={'thread':{'id':'blind-retry'}}
    emit({'id':m['id'],'result':result})
"#,
        )
        .unwrap();
        let mut command = Command::new("python3");
        command.arg(script).arg(&state).current_dir(directory);
        let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
        let op = engine
            .agents
            .chat
            .dispatch_storage(
                "operations.put",
                json!({
                    "id":"branch-crash-gap",
                    "kind":"branch",
                    "sourceThreadId":"source-chat",
                    "turnId":"turn-1",
                    "before":false,
                    "phase":"workspace_ready",
                    "workspaceComplete":true,
                    "sourceWorkspace":workspace,
                    "workspace":workspace,
                    "sidebarProjectId":null,
                    "agentKind":"baseline",
                    "agentId":null,
                    "learningEligible":true,
                    "retainedSubmissions":[]
                }),
            )
            .await
            .unwrap();
        assert_eq!(op["phase"], "workspace_ready");
        (engine, state.to_string_lossy().into_owned(), workspace)
    }

    #[tokio::test]
    async fn crash_gap_adopts_unique_runtime_fork_and_fences_it_until_recovered() {
        let directory = tempfile::tempdir().unwrap();
        let (engine, state, workspace) = recovery_fixture(directory.path(), 1).await;
        assert!(
            engine
                .agents
                .require_chat_ready("source-chat")
                .await
                .is_ok(),
            "an incomplete destination branch must not freeze its source chat"
        );
        let fenced = engine.agents.require_chat_ready("candidate-0").await;
        assert!(fenced.unwrap_err().contains("unrecorded runtime fork"));

        let recovered = engine
            .agents
            .recover_chat_branch(json!({"operationId":"branch-crash-gap"}))
            .await
            .unwrap();
        assert_eq!(recovered["thread"]["projectId"], Value::Null);
        assert_eq!(recovered["draft"]["projectId"], Value::Null);
        assert_eq!(recovered["thread"]["id"], "candidate-0");
        assert_eq!(recovered["thread"]["cwd"], workspace.cwd);
        assert_eq!(recovered["thread"]["branchOf"]["threadId"], "source-chat");
        assert!(engine
            .agents
            .require_chat_ready("candidate-0")
            .await
            .is_ok());
        assert_eq!(
            engine
                .agents
                .chat
                .dispatch_storage("operations.get", json!({"id":"branch-crash-gap"}))
                .await
                .unwrap()["phase"],
            "ready"
        );
        assert_eq!(
            engine
                .agents
                .worktrees
                .get_session("candidate-0")
                .unwrap()
                .unwrap(),
            workspace
        );
        let state: Value = serde_json::from_slice(&std::fs::read(state).unwrap()).unwrap();
        assert_eq!(
            state["calls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|method| *method == "thread/fork")
                .count(),
            0,
            "recovery must never issue a second fork"
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn ambiguous_runtime_forks_remain_pending_without_guessing() {
        let directory = tempfile::tempdir().unwrap();
        let (engine, state, _) = recovery_fixture(directory.path(), 2).await;
        let error = engine
            .agents
            .recover_chat_branch(json!({"operationId":"branch-crash-gap"}))
            .await
            .unwrap_err();
        assert!(error.contains("multiple runtime forks match"));
        assert_eq!(
            engine
                .agents
                .chat
                .dispatch_storage("operations.get", json!({"id":"branch-crash-gap"}))
                .await
                .unwrap()["phase"],
            "workspace_ready"
        );
        let state: Value = serde_json::from_slice(&std::fs::read(state).unwrap()).unwrap();
        assert_eq!(
            state["calls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|method| *method == "thread/fork")
                .count(),
            0
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn recorded_fork_recovery_restores_edit_document_without_staging_it() {
        let directory = tempfile::tempdir().unwrap();
        let (engine, state, mut workspace) = recovery_fixture(directory.path(), 1).await;
        let source_root = directory.path().join("original-worktree");
        std::fs::create_dir_all(source_root.join("src")).unwrap();
        std::fs::write(source_root.join("src/example.rs"), "source file").unwrap();
        std::fs::create_dir_all(Path::new(&workspace.cwd).join("src")).unwrap();
        std::fs::write(
            Path::new(&workspace.cwd).join("src/example.rs"),
            "branch file",
        )
        .unwrap();
        let source_root = std::fs::canonicalize(source_root).unwrap();
        let mut source_workspace = SessionWorkspace {
            cwd: source_root.to_string_lossy().into_owned(),
            roots: vec![source_root.to_string_lossy().into_owned()],
            folders: vec![crate::worktrees::ResolvedFolder {
                source: source_root.to_string_lossy().into_owned(),
                path: source_root.to_string_lossy().into_owned(),
            }],
            worktrees: vec![],
            scope_project_id: None,
            project_revision: None,
        };
        workspace.folders = vec![crate::worktrees::ResolvedFolder {
            source: source_root.to_string_lossy().into_owned(),
            path: workspace.cwd.clone(),
        }];
        let sidebar_project = engine
            .agents
            .projects
            .create(crate::projects::ProjectInput {
                name: "Pending branch project".into(),
                folders: vec![workspace.cwd.clone()],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        workspace.scope_project_id = Some(sidebar_project.id.clone());
        workspace.project_revision = Some(sidebar_project.revision);
        source_workspace.scope_project_id = Some(sidebar_project.id.clone());
        source_workspace.project_revision = Some(sidebar_project.revision);
        let operation = engine
            .agents
            .chat
            .dispatch_storage("operations.get", json!({"id":"branch-crash-gap"}))
            .await
            .unwrap();
        let fork = json!({"thread":{"id":"candidate-0","cwd":workspace.cwd,"forkedFromId":"source-chat","turns":[]}});
        let mut document = json!({
            "version":1,
            "text":"Please inspect this symbol",
            "references":[{"type":"file","id":source_root.join("src/example.rs"),"name":"example.rs","path":source_root.join("src/example.rs"),"start":12,"end":31}],
            "quotes":[{"text":"old wording","start":0,"end":13}],
            "attachmentIds":[]
        });
        let imported = engine
            .agents
            .chat
            .dispatch_storage(
                "attachments.importBytes",
                json!({"ownerId":"original-submission","files":[{"name":"opening.txt","mime":"text/plain","data":"b3BlbmluZyBmaWxl"}]}),
            )
            .await
            .unwrap();
        let attachment_id = imported[0]["id"].as_str().unwrap().to_owned();
        document["attachmentIds"] = json!([attachment_id]);
        engine
            .agents
            .chat
            .dispatch_storage(
                "operations.update",
                json!({
                    "id":"branch-crash-gap",
                    "expectedRevision":operation["revision"],
                    "phase":"history_forked",
                    "threadId":"candidate-0",
                    "forkResult":fork,
                    "workspace":workspace,
                    "sourceWorkspace":source_workspace,
                    "sidebarProjectId":sidebar_project.id,
                    "workspaceComplete":true,
                    "editingDraft":{
                        "sourceSubmissionId":"original-submission",
                        "document":document,
                        "settings":{"model":"test-model","effort":"high","mode":"plan"},
                        "attachmentIds":[attachment_id]
                    }
                }),
            )
            .await
            .unwrap();

        engine
            .agents
            .projects
            .delete(&sidebar_project.id)
            .await
            .unwrap();

        let recovered = engine
            .agents
            .recover_chat_branch(json!({"operationId":"branch-crash-gap"}))
            .await
            .unwrap();
        assert_eq!(recovered["draft"]["document"]["text"], document["text"]);
        assert_eq!(
            recovered["draft"]["document"]["references"],
            json!([{"type":"file","id":Path::new(&workspace.cwd).join("src/example.rs"),"name":"example.rs","path":Path::new(&workspace.cwd).join("src/example.rs"),"start":12,"end":31}])
        );
        let resolved = engine.agents.chat_request("references.resolve", json!({
            "threadId":"candidate-0","type":"file","id":Path::new(&workspace.cwd).join("src/example.rs").to_string_lossy()
        })).await.unwrap();
        assert_eq!(
            resolved["absolutePath"],
            Path::new(&workspace.cwd)
                .join("src/example.rs")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(recovered["draft"]["document"]["quotes"], document["quotes"]);
        assert_eq!(
            recovered["draft"]["document"]["attachmentIds"],
            json!([attachment_id])
        );
        assert_eq!(recovered["draft"]["settings"]["mode"], "plan");
        let binding = engine
            .agents
            .store
            .session_binding("candidate-0")
            .await
            .unwrap();
        assert_eq!(
            binding.project_id.as_deref(),
            Some(sidebar_project.id.as_str()),
            "historical execution scope survives project deletion"
        );
        assert_eq!(
            engine
                .agents
                .projects
                .membership("candidate-0")
                .await
                .unwrap(),
            None,
            "a deleted project is not recreated as sidebar membership"
        );
        let saved = engine
            .agents
            .chat
            .dispatch_storage("drafts.get", json!({"id":"candidate-0"}))
            .await
            .unwrap();
        assert_eq!(saved, recovered["draft"]);
        assert_eq!(saved["projectId"], Value::Null);
        let attachments = engine
            .agents
            .chat
            .dispatch_storage("attachments.list", json!({"ownerId":"candidate-0"}))
            .await
            .unwrap();
        assert_eq!(attachments[0]["id"], attachment_id);
        assert!(
            !engine
                .agents
                .root
                .join("chat")
                .join("runtime")
                .join("candidate-0")
                .join("original-submission")
                .exists(),
            "edit draft grants are retained without runtime staging"
        );
        let retried = engine
            .agents
            .recover_chat_branch(json!({"operationId":"branch-crash-gap"}))
            .await
            .unwrap();
        assert_eq!(retried, recovered);
        let state: Value = serde_json::from_slice(&std::fs::read(state).unwrap()).unwrap();
        assert_eq!(
            state["calls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|method| *method == "thread/fork")
                .count(),
            0
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn complete_sidecar_roots_rebuild_workspace_after_journal_gap() {
        let directory = tempfile::tempdir().unwrap();
        let (engine, state, _) = recovery_fixture(directory.path(), 1).await;
        let source_root = directory.path().join("source-repo");
        std::fs::create_dir_all(&source_root).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-C", source_root.to_str().unwrap()])
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
        std::fs::write(source_root.join("file.txt"), "source\n").unwrap();
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
        let source_root = std::fs::canonicalize(source_root).unwrap();
        let source_workspace = SessionWorkspace {
            cwd: source_root.to_string_lossy().into_owned(),
            roots: vec![source_root.to_string_lossy().into_owned()],
            folders: vec![ResolvedFolder {
                source: source_root.to_string_lossy().into_owned(),
                path: source_root.to_string_lossy().into_owned(),
            }],
            worktrees: vec![],
            scope_project_id: Some("scope-id".into()),
            project_revision: Some(4),
        };
        let stage = directory.path().join("staging");
        std::fs::create_dir_all(&stage).unwrap();
        let manager = crate::worktrees::WorktreeManager::with_config(
            directory.path().join("worktrees"),
            worktrunk::config::UserConfig::default(),
            directory.path().join("approvals.toml"),
        );
        let sidebar_project = engine
            .agents
            .projects
            .create(crate::projects::ProjectInput {
                name: "Sidebar grouping".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        engine
            .agents
            .projects
            .set_membership("source-chat", Some(&sidebar_project.id))
            .await
            .unwrap();
        let mapping = super::super::create_snapshot_worktree(
            &manager,
            engine.agents.root.clone(),
            "branch-crash-gap".into(),
            source_root.clone(),
            stage,
            source_root.to_string_lossy().into_owned(),
            source_root.clone(),
        )
        .await
        .unwrap();
        let expected_cwd = mapping.destination.to_string_lossy().into_owned();
        let state_json: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
        let mut updated_state = state_json;
        updated_state["threads"][0]["cwd"] = json!(expected_cwd);
        std::fs::write(&state, updated_state.to_string()).unwrap();
        let listed = engine
            .agents
            .rpc
            .call(
                "thread/list",
                json!({"limit":100,"sortKey":"updated_at","sortDirection":"desc","archived":false}),
            )
            .await
            .unwrap();
        assert_eq!(listed["data"][0]["cwd"], expected_cwd);
        assert_eq!(
            runtime_forks(&engine.agents, "source-chat", &expected_cwd)
                .await
                .unwrap()
                .len(),
            1
        );
        let operation = engine
            .agents
            .chat
            .dispatch_storage("operations.get", json!({"id":"branch-crash-gap"}))
            .await
            .unwrap();
        engine
            .agents
            .chat
            .dispatch_storage(
                "operations.update",
                json!({
                    "id":"branch-crash-gap",
                    "expectedRevision":operation["revision"],
                    "phase":"workspace_preparing",
                    "sourceWorkspace":source_workspace,
                    "workspace":null,
                    "workspaceComplete":false,
                    "sidebarProjectId":sidebar_project.id
                }),
            )
            .await
            .unwrap();

        let pending = engine.agents.pending_chat_branches().await.unwrap();
        assert_eq!(
            pending["data"][0]["retainedPaths"][0]["destination"],
            expected_cwd
        );
        assert_eq!(
            pending["data"][0]["retainedPaths"][0]["setupComplete"],
            true
        );

        let recovered_result = engine
            .agents
            .recover_chat_branch(json!({"operationId":"branch-crash-gap"}))
            .await;
        let recovered = match recovered_result {
            Ok(result) => result,
            Err(error) => {
                let operation = engine
                    .agents
                    .chat
                    .dispatch_storage("operations.get", json!({"id":"branch-crash-gap"}))
                    .await
                    .unwrap();
                panic!(
                    "{error}; reconstructed cwd={}",
                    operation["workspace"]["cwd"]
                );
            }
        };
        assert_eq!(recovered["thread"]["cwd"], expected_cwd);
        assert_eq!(
            recovered["thread"]["workspace"]["scopeProjectId"],
            "scope-id"
        );
        assert_eq!(recovered["thread"]["workspace"]["projectRevision"], 4);
        let binding = engine
            .agents
            .store
            .session_binding("candidate-0")
            .await
            .unwrap();
        assert_eq!(binding.project_id.as_deref(), Some("scope-id"));
        assert_eq!(
            engine
                .agents
                .projects
                .membership("candidate-0")
                .await
                .unwrap()
                .as_deref(),
            Some(sidebar_project.id.as_str())
        );
        assert_eq!(
            engine
                .agents
                .chat
                .dispatch_storage("operations.get", json!({"id":"branch-crash-gap"}))
                .await
                .unwrap()["phase"],
            "ready"
        );
        engine.shutdown().await;
    }
}
