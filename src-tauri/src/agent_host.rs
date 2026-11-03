use crate::{
    agent_protocol,
    agent_workers::WorkerState,
    bridge::{EventSink, RuntimeRpc},
    policy,
    worktrees::WorktreeManager,
};
use brr_agent_core::{
    AgentStore, DelegationNode, MemoryAccess, ProfileCandidate, SessionBinding, TaskAction,
    TaskInput, TaskSettings, WorkflowInput, WorkspacePermission,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) struct AgentHost {
    pub(crate) store: AgentStore,
    pub(crate) chat: crate::chat_state::ChatStore,
    pub(crate) foreground: crate::chat_service::ForegroundState,
    pub(crate) projects: crate::projects::ProjectStore,
    pub(crate) rpc: RuntimeRpc,
    pub(crate) worktrees: WorktreeManager,
    pub(crate) root: PathBuf,
    pub(crate) events: EventSink,
    pub(crate) workers: WorkerState,
    pub(crate) questions: crate::questions::QuestionState,
    browser_handler: std::sync::RwLock<Option<crate::workspace_bridge::BrowserToolHandler>>,
}

impl AgentHost {
    pub async fn new(
        root: PathBuf,
        rpc: RuntimeRpc,
        worktrees: WorktreeManager,
        events: EventSink,
        store: AgentStore,
        projects: crate::projects::ProjectStore,
    ) -> Result<Arc<Self>, String> {
        let chat = crate::chat_state::ChatStore::open(root.join("chat")).await?;
        let host = Arc::new(Self {
            chat,
            foreground: Default::default(),
            store,
            projects,
            rpc,
            worktrees,
            root,
            events,
            workers: WorkerState::new(),
            questions: crate::questions::QuestionState::new(),
            browser_handler: std::sync::RwLock::new(None),
        });
        sqlx::query("CREATE TABLE IF NOT EXISTS host_learning_evidence(source_id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,processed INTEGER NOT NULL DEFAULT 0,source_role TEXT NOT NULL DEFAULT 'user')").execute(host.store.pool()).await.map_err(|e|e.to_string())?;
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('host_learning_evidence')")
                .fetch_all(host.store.pool())
                .await
                .map_err(|e| e.to_string())?;
        if !columns.iter().any(|c| c == "source_role") {
            sqlx::query("ALTER TABLE host_learning_evidence ADD COLUMN source_role TEXT NOT NULL DEFAULT 'user'").execute(host.store.pool()).await.map_err(|e|e.to_string())?;
        }
        sqlx::query("CREATE TABLE IF NOT EXISTS host_task_source_workspaces(operation_id TEXT PRIMARY KEY,source_thread_id TEXT NOT NULL,workspace_project TEXT NOT NULL)").execute(host.store.pool()).await.map_err(|e| e.to_string())?;
        host.init_workers().await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS question_observed_turns(thread_id TEXT NOT NULL,turn_id TEXT NOT NULL,reconciled INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(thread_id,turn_id))")
            .execute(host.store.pool()).await.map_err(|e|e.to_string())?;
        crate::intelligent_ui::init(&host).await?;
        Ok(host)
    }

    pub(crate) fn changed(&self) {
        (self.events)(json!({"method":"brr/agents/changed","params":{}}));
        (self.events)(json!({"method":"brr/tasks/changed","params":{}}));
        (self.events)(json!({"method":"brr/tickets/changed","params":{}}));
    }

    pub(crate) async fn require_account(&self) -> Result<(), String> {
        let account = self
            .rpc
            .call("account/read", json!({"refreshToken":false}))
            .await?;
        if policy::is_chatgpt_account(&account) {
            Ok(())
        } else {
            Err("Sign in with ChatGPT to use an agent.".into())
        }
    }

    pub(crate) async fn control_task_with_run(
        &self,
        task_id: &str,
        action: TaskAction,
        expected_run_id: Option<&str>,
    ) -> Result<brr_agent_core::Task, String> {
        let _control = self.workers.task_control_guard(task_id).await;
        let old = self.store.get_task(task_id).await?;
        if expected_run_id.is_some_and(|expected| {
            old.run_id.as_deref() != Some(expected)
                || matches!(old.status.as_str(), "succeeded" | "failed" | "cancelled")
        }) {
            return Ok(old);
        }
        if matches!(action, TaskAction::Pause | TaskAction::Cancel) {
            if let Some(run_id) = old.run_id.as_ref() {
                let run = self.store.get_run(run_id).await?;
                if let (Some(thread), Some(turn)) = (run.thread_id, run.turn_id) {
                    let turns = crate::agent_workers::thread_turns(self, &thread)
                        .await
                        .map_err(|error| format!("Could not verify whether the task turn is active. The runtime did not confirm interruption; the task remains active: {error}"))?;
                    let turn_status = turns
                        .iter()
                        .find(|candidate| candidate["id"].as_str() == Some(turn.as_str()))
                        .and_then(|candidate| candidate["status"].as_str())
                        .ok_or("Could not verify whether the task turn is active. The runtime did not confirm interruption; the task remains active")?;
                    if matches!(turn_status, "inProgress" | "in_progress") {
                        self.rpc
                            .call("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
                            .await
                            .map_err(|error| format!("The runtime did not confirm interruption; the task remains active: {error}"))?;
                    } else if !matches!(turn_status, "completed" | "interrupted" | "failed") {
                        return Err("Could not verify whether the task turn is active. The runtime did not confirm interruption; the task remains active".into());
                    }
                }
            }
        }
        let updated = self.store.control_task(task_id, action.clone()).await?;
        if action == TaskAction::Cancel {
            if let Some(thread_id) = old.thread_id.as_deref() {
                self.store
                    .close_questions(thread_id, None, "task cancelled by user")
                    .await?;
                self.questions_changed(Some(thread_id));
            }
            if let Some(run_id) = old.run_id.as_deref() {
                self.workers.forget_run(run_id);
            }
        }
        Ok(updated)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let result = match method {
            "agents.workspace.inspect" => {
                check_fields(&params, &["cwd"])?;
                let folder = canonical_folder(required(&params, "cwd")?)?;
                let git = git_folder(&folder).await?;
                let commands = if git {
                    self.worktrees.inspect(&folder).await?
                } else {
                    vec![]
                };
                json!({"git":git,"commands":commands})
            }
            "agents.list" => {
                check_fields(&params, &[])?;
                json!({"data":self.store.list_profiles().await?})
            }
            "agents.read" => {
                check_fields(&params, &["agentId"])?;
                json!(
                    self.store
                        .read_profile(required(&params, "agentId")?)
                        .await?
                )
            }
            "agents.interview" => {
                check_fields(&params, &["model", "effort"])?;
                self.require_account().await?;
                self.open_session(
                    None,
                    "interview",
                    required(&params, "model")?,
                    None,
                    None,
                    true,
                    None,
                    None,
                )
                .await?
            }
            "agents.edit" => {
                check_fields(&params, &["agentId"])?;
                self.require_account().await?;
                let profile = self
                    .store
                    .read_profile(required(&params, "agentId")?)
                    .await?;
                self.open_session(
                    Some(profile.id),
                    "profile",
                    &profile.model,
                    None,
                    None,
                    true,
                    None,
                    None,
                )
                .await?
            }
            "agents.drafts" => {
                check_fields(&params, &["threadId"])?;
                json!({"data":self.store.list_profile_drafts(optional(&params,"threadId")).await?})
            }
            "agents.approve" => {
                check_fields(&params, &["draftId", "hash"])?;
                let id = required(&params, "draftId")?;
                let hash = required(&params, "hash")?;
                self.store
                    .approve_profile(id, hash, &format!("user-review:{id}:{hash}"))
                    .await?;
                json!({"agent":self.store.apply_profile(id,hash,&format!("profile-apply:{id}:{hash}")).await?})
            }
            "agents.reject" => {
                check_fields(&params, &["draftId", "hash"])?;
                let changed=sqlx::query("UPDATE profile_drafts SET status='rejected' WHERE id=? AND hash=? AND status='proposed'")
                    .bind(required(&params,"draftId")?).bind(required(&params,"hash")?).execute(self.store.pool()).await.map_err(|e|e.to_string())?;
                if changed.rows_affected() != 1 {
                    return Err("Draft changed or has already been reviewed".into());
                }
                json!({})
            }
            "agents.open" => {
                check_fields(
                    &params,
                    &["agentId", "model", "effort", "cwd", "projectId", "readOnly"],
                )?;
                self.require_account().await?;
                let profile = self
                    .store
                    .read_profile(required(&params, "agentId")?)
                    .await?;
                self.open_session(
                    Some(profile.id),
                    "agent",
                    optional(&params, "model").unwrap_or(&profile.model),
                    optional(&params, "cwd"),
                    optional(&params, "projectId"),
                    params["readOnly"].as_bool().unwrap_or(false),
                    None,
                    None,
                )
                .await?
            }
            "agents.sessions" => {
                check_fields(&params, &["agentId"])?;
                let bindings = self
                    .store
                    .list_bindings(Some(required(&params, "agentId")?))
                    .await?;
                let mut data = Vec::new();
                for b in bindings
                    .into_iter()
                    .filter(|b| b.kind == "agent" || b.kind == "profile")
                {
                    match self.rpc.call("thread/read",json!({"threadId":b.thread_id,"includeTurns":false})).await {
                        Ok(mut response)=>{response["thread"]["brr"]=json!(b); data.push(response["thread"].clone());},
                        Err(_)=>data.push(json!({"id":b.thread_id,"cwd":b.cwd,"preview":"Saved agent conversation","brr":b,"turns":[]}))
                    }
                }
                json!({"data":data})
            }
            "memory.projects" => {
                check_fields(&params, &[])?;
                json!({"data":self.projects.list().await?})
            }
            "memory.list" | "memory.search" => {
                check_fields(&params, &["agentId", "projectId", "query"])?;
                let access = self.user_access(&params).await?;
                let data = if method == "memory.search" {
                    self.store
                        .search_memory(&access, required(&params, "query")?)
                        .await?
                } else {
                    self.store.list_memory(&access).await?
                };
                json!({"data":data,"sources":self.store.list_sources(&access,500).await?})
            }
            "memory.revise" => {
                check_fields(
                    &params,
                    &["agentId", "projectId", "id", "revision", "content"],
                )?;
                let access = self.user_access(&params).await?;
                json!(
                    self.store
                        .revise_memory_record(
                            &access,
                            required(&params, "id")?,
                            params["revision"].as_i64().ok_or("revision is required")?,
                            required(&params, "content")?
                        )
                        .await?
                )
            }
            "memory.forget" => {
                check_fields(&params, &["agentId", "projectId", "id"])?;
                let access = self.user_access(&params).await?;
                json!({"eventId":self.store.forget_memory(&access,required(&params,"id")?).await?})
            }
            "memory.undo" => {
                check_fields(&params, &["eventId", "agentId", "projectId"])?;
                let event = params["eventId"].as_i64().ok_or("eventId is required")?;
                self.store.undo_memory_event_for_user(event).await?;
                json!({})
            }
            "skills.list" => {
                check_fields(&params, &["agentId", "projectId"])?;
                let access = self.user_access(&params).await?;
                json!({"data":self.store.list_skills(&access).await?})
            }
            "skills.revoke" => {
                check_fields(&params, &["agentId", "projectId", "id"])?;
                let access = self.user_access(&params).await?;
                self.store
                    .revoke_skill(&access, required(&params, "id")?)
                    .await?;
                json!({})
            }
            "tasks.list" => {
                check_fields(&params, &["agentId", "after", "limit"])?;
                let data = self
                    .store
                    .list_tasks_page(
                        optional(&params, "agentId"),
                        optional(&params, "after"),
                        params["limit"].as_i64().unwrap_or(100),
                    )
                    .await?;
                let next = data.last().map(|t| t.id.clone());
                json!({"data":data,"nextCursor":next})
            }
            "tasks.create" => {
                check_fields(
                    &params,
                    &[
                        "agentId",
                        "title",
                        "instructions",
                        "acceptanceCriteria",
                        "cwd",
                        "projectId",
                        "model",
                        "effort",
                        "dependencies",
                        "context",
                        "sourceThreadId",
                        "operationId",
                    ],
                )?;
                let _creation = self.foreground.task_creation.lock().await;
                if let Some(operation_id) = optional(&params, "operationId") {
                    let reserved: Option<String> = sqlx::query_scalar(
                        "SELECT operation_id FROM chat_assignment_context WHERE operation_id=?",
                    )
                    .bind(operation_id)
                    .fetch_optional(&self.chat.pool)
                    .await
                    .map_err(|error| error.to_string())?;
                    if reserved.is_some() {
                        return Err(
                            "operationId is reserved by a chat task assignment; retry that assignment or use a new operation ID".into(),
                        );
                    }
                }
                self.create_task_with_workspace(params).await?
            }
            "tasks.control" => {
                check_fields(&params, &["taskId", "action"])?;
                let id = required(&params, "taskId")?;
                let action: TaskAction = parse(params["action"].clone())?;
                let updated = self.control_task_with_run(id, action, None).await?;
                json!(updated)
            }
            "tasks.proposals" => {
                check_fields(&params, &["agentId"])?;
                json!({"data":self.store.list_proposals(optional(&params,"agentId")).await?.into_iter().filter(|p|p.status=="proposed").collect::<Vec<_>>()})
            }
            "tasks.schedule" => {
                check_fields(
                    &params,
                    &["proposalId", "hash", "name", "expression", "timezone"],
                )?;
                json!(
                    self.store
                        .schedule_delegation(
                            required(&params, "proposalId")?,
                            required(&params, "hash")?,
                            required(&params, "name")?,
                            required(&params, "expression")?,
                            required(&params, "timezone")?
                        )
                        .await?
                )
            }
            "tasks.approve" => {
                check_fields(&params, &["proposalId", "hash"])?;
                json!({"data":self.store.approve_delegation(required(&params,"proposalId")?,required(&params,"hash")?).await?})
            }
            "workflows.list" => {
                check_fields(&params, &["agentId"])?;
                json!({"data":self.store.list_workflows(optional(&params,"agentId")).await?})
            }
            "workflows.create" => {
                check_fields(
                    &params,
                    &[
                        "name",
                        "agentId",
                        "title",
                        "instructions",
                        "acceptanceCriteria",
                        "cwd",
                        "projectId",
                        "model",
                        "effort",
                        "context",
                        "dependencies",
                        "expression",
                        "timezone",
                        "nodes",
                    ],
                )?;
                let mut task: TaskInput = parse(params.clone())?;
                self.validate_task(&mut task).await?;
                let nodes: Option<Vec<DelegationNode>> =
                    params.get("nodes").cloned().map(parse).transpose()?;
                let input = WorkflowInput {
                    name: required(&params, "name")?.into(),
                    task,
                    expression: required(&params, "expression")?.into(),
                    timezone: required(&params, "timezone")?.into(),
                    nodes,
                };
                json!(self.store.create_workflow(input).await?)
            }
            "workflows.control" => {
                check_fields(&params, &["workflowId", "paused"])?;
                json!(
                    self.store
                        .control_workflow(
                            required(&params, "workflowId")?,
                            params["paused"].as_bool().ok_or("paused is required")?
                        )
                        .await?
                )
            }
            "agents.settings" => {
                check_fields(&params, &[])?;
                json!(self.store.get_task_settings().await?)
            }
            "agents.settings.update" => {
                check_fields(&params, &["maxBackground", "maxPerAgent"])?;
                let settings: TaskSettings = parse(params)?;
                json!(self.store.update_task_settings(settings).await?)
            }
            "agents.events" => {
                check_fields(&params, &["after", "limit"])?;
                json!({"data":self.store.events(params["after"].as_i64().unwrap_or(0),params["limit"].as_i64().unwrap_or(100)).await?})
            }
            "agents.decisions" => {
                check_fields(&params, &[])?;
                json!({"data":self.store.list_proposals(None).await?.into_iter().filter(|p|p.status=="proposed").collect::<Vec<_>>(),"drafts":self.store.list_profile_drafts(None).await?.into_iter().filter(|d|d.status=="proposed").collect::<Vec<_>>()})
            }
            _ => return Err(format!("Unsupported Agents method: {method}")),
        };
        if !matches!(
            method,
            "agents.list"
                | "agents.read"
                | "agents.sessions"
                | "agents.drafts"
                | "memory.list"
                | "memory.search"
                | "skills.list"
                | "tasks.list"
                | "tasks.proposals"
                | "workflows.list"
                | "agents.settings"
                | "agents.events"
                | "agents.decisions"
        ) {
            self.changed();
        }
        Ok(result)
    }

    pub(crate) async fn user_access(&self, params: &Value) -> Result<MemoryAccess, String> {
        if let Some(id) = optional(params, "agentId") {
            self.store.read_profile(id).await?;
        }
        self.store
            .memory_access_for_user(optional(params, "agentId"), optional(params, "projectId"))
            .await
    }

    pub(crate) async fn validate_task(&self, input: &mut TaskInput) -> Result<(), String> {
        let profile = self.store.read_profile(&input.agent_id).await?;
        if input.model.is_none() {
            input.model = Some(profile.model);
        }
        if input.effort.is_none() {
            input.effort = Some(profile.effort);
        }
        if let Some(project_id) = input.project_id.as_deref() {
            let project = self
                .project_for_start(Some(project_id), None)
                .await?
                .ok_or("Project not found")?;
            if let Some(cwd) = input.cwd.as_deref() {
                let selected = canonical_folder(cwd)?;
                if !project.folders.contains(&selected) {
                    return Err("The selected task folder is not registered to its project".into());
                }
                input.cwd = Some(selected);
            } else {
                input.cwd = project.primary_folder;
            }
        } else if let Some(cwd) = input.cwd.as_ref() {
            input.cwd = Some(canonical_folder(cwd)?);
        }
        if input.title.trim().is_empty()
            || input.instructions.trim().is_empty()
            || input.acceptance_criteria.is_empty()
        {
            return Err("A task needs a title, instructions and acceptance criteria".into());
        }
        Ok(())
    }

    /// Resolve scope separately from execution paths. Ad-hoc folders never imply a project.
    pub(crate) async fn project_for_start(
        &self,
        project_id: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<Option<crate::projects::ProjectRecord>, String> {
        if let Some(id) = project_id {
            let project = self
                .projects
                .read(id)
                .await?
                .filter(|p| !p.hidden)
                .ok_or("Project does not exist")?;
            return Ok(Some(project));
        }
        cwd.map(|folder| {
            let folder = canonical_folder(folder)?;
            Ok(crate::projects::ProjectRecord {
                id: String::new(),
                name: "Workspace".into(),
                folders: vec![folder.clone()],
                primary_folder: Some(folder),
                revision: 0,
                model: None,
                effort: None,
                permission: None,
                hidden: false,
            })
        })
        .transpose()
    }

    pub(crate) async fn prepare_workspace(
        &self,
        project_id: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<crate::worktrees::SessionWorkspace, String> {
        let project = self.project_for_start(project_id, cwd).await?;
        let mut workspace = self.worktrees.prepare_session(project.as_ref()).await?;
        if project_id.is_none() {
            workspace.scope_project_id = None;
            workspace.project_revision = None;
        }
        Ok(workspace)
    }

    pub(crate) async fn open_session(
        &self,
        agent_id: Option<String>,
        kind: &str,
        model: &str,
        cwd: Option<&str>,
        project_id: Option<&str>,
        read_only: bool,
        task_run_id: Option<String>,
        workspace_permission: Option<WorkspacePermission>,
    ) -> Result<Value, String> {
        let workspace = if kind == "task" {
            if let (Some(project_id), Some(cwd)) = (project_id, cwd) {
                let mut project = self
                    .project_for_start(Some(project_id), None)
                    .await?
                    .ok_or("Task project no longer exists")?;
                let selected = canonical_folder(cwd)?;
                if !project.folders.contains(&selected) {
                    return Err(
                        "The selected task folder is no longer registered to its project".into(),
                    );
                }
                project.primary_folder = Some(selected);
                self.worktrees.prepare_session(Some(&project)).await?
            } else {
                self.prepare_workspace(project_id, cwd).await?
            }
        } else {
            self.prepare_workspace(project_id, cwd).await?
        };
        self.open_session_with_workspace(
            agent_id,
            kind,
            model,
            workspace,
            read_only,
            task_run_id,
            workspace_permission,
            project_id,
        )
        .await
    }

    pub(crate) async fn open_task_session_with_project_snapshot(
        &self,
        agent_id: Option<String>,
        model: &str,
        task_run_id: &str,
        workspace_permission: Option<WorkspacePermission>,
        project: &crate::projects::ProjectRecord,
    ) -> Result<Value, String> {
        let mut workspace = self.worktrees.prepare_session(Some(project)).await?;
        if project.id.is_empty() {
            workspace.scope_project_id = None;
            workspace.project_revision = None;
        }
        self.open_session_with_workspace(
            agent_id,
            "task",
            model,
            workspace,
            false,
            Some(task_run_id.to_owned()),
            workspace_permission,
            Some(&project.id),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn open_session_with_workspace(
        &self,
        agent_id: Option<String>,
        kind: &str,
        model: &str,
        workspace: crate::worktrees::SessionWorkspace,
        read_only: bool,
        task_run_id: Option<String>,
        workspace_permission: Option<WorkspacePermission>,
        project_id: Option<&str>,
    ) -> Result<Value, String> {
        let mut response = self.rpc.call("thread/start", json!({
            "cwd":workspace.cwd,"runtimeWorkspaceRoots":workspace.roots,"model":model,"modelProvider":"openai","approvalPolicy":"on-request",
            "sandbox":match workspace_permission.as_ref() { Some(WorkspacePermission::ReadOnly) => "read-only", Some(WorkspacePermission::WorkspaceWrite) => "workspace-write", None if read_only => "read-only", None => "workspace-write" },
            "developerInstructions":agent_protocol::instructions(kind),
            "dynamicTools":agent_protocol::tools(kind),"config":workspace_config(kind, &workspace),
            "ephemeral":kind=="learning"
        })).await.map_err(|e| format!("{e}. Prepared workspace retained at {}", workspace.cwd))?;
        let thread_id = response["thread"]["id"]
            .as_str()
            .ok_or("Runtime did not return a thread ID")?
            .to_owned();
        self.worktrees
            .save_session(&thread_id, &workspace)
            .map_err(|e| {
                format!(
                    "{e}. Thread {thread_id} and workspace {} were retained.",
                    workspace.cwd
                )
            })?;
        let binding = SessionBinding {
            thread_id: thread_id.clone(),
            agent_id,
            kind: kind.into(),
            project_id: workspace.scope_project_id.clone(),
            cwd: Some(workspace.cwd.clone()),
            task_run_id,
            learning_eligible: matches!(kind, "agent" | "baseline" | "task"),
        };
        self.store.bind_session(binding.clone()).await?;
        if matches!(kind, "agent" | "baseline") {
            self.register_fresh_chat(&thread_id);
        }
        if matches!(kind, "agent" | "baseline") {
            self.projects.set_membership(&thread_id, project_id).await?;
        }
        response["thread"]["brr"] = json!(binding);
        response["thread"]["projectId"] = json!(self.projects.membership(&thread_id).await?);
        response["thread"]["workspace"] = json!(workspace);
        if let Some(worktree) = workspace.worktrees.first() {
            response["thread"]["worktree"] = json!(worktree);
        }
        self.changed();
        Ok(response)
    }

    pub(crate) async fn annotate(&self, thread: &mut Value) {
        if let Some(id) = thread["id"].as_str().map(str::to_owned) {
            thread["projectId"] = json!(self.projects.membership(&id).await.unwrap_or(None));
            if let Ok(Some(workspace)) = self.worktrees.get_session(&id) {
                thread["workspace"] = json!(workspace);
            }
            if let Ok(binding) = self.store.session_binding(&id).await {
                thread["brr"] = json!(binding);
            }
        }
    }

    pub(crate) async fn bind_baseline(
        &self,
        thread: &Value,
        project_id: Option<String>,
    ) -> Result<(), String> {
        let id = required(thread, "id")?;
        self.store
            .bind_session(SessionBinding {
                thread_id: id.into(),
                agent_id: None,
                kind: "baseline".into(),
                project_id,
                cwd: optional(thread, "cwd").map(str::to_owned),
                task_run_id: None,
                learning_eligible: true,
            })
            .await?;
        Ok(())
    }

    pub(crate) async fn prepare_turn(
        &self,
        thread_id: &str,
        mut params: Value,
    ) -> Result<Value, String> {
        if let Some(workspace) = self.worktrees.get_session(thread_id)? {
            self.worktrees.validate_session(&workspace).await?;
        }
        let Ok(binding) = self.store.session_binding(thread_id).await else {
            return Ok(params);
        };
        let profile = match binding.agent_id.as_deref() {
            Some(id) => Some(self.store.read_profile(id).await?),
            None => None,
        };
        let access = self.tool_access(&binding).await?;
        let query = params["input"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if binding.learning_eligible && params["clientUserMessageId"].is_null() {
            // Small independent evidence units allow forgetting a fact without erasing a whole conversation.
            for chunk in query
                .split('\n')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(100)
            {
                let source_id = format!("{thread_id}:user:{}", uuid::Uuid::new_v4());
                self.store
                    .record_source(
                        &access,
                        &source_id,
                        1,
                        &redact_evidence(&format!("User: {}", chunk)),
                    )
                    .await?;
                sqlx::query(
                    "INSERT OR IGNORE INTO host_learning_evidence(source_id,thread_id) VALUES(?,?)",
                )
                .bind(&source_id)
                .bind(thread_id)
                .execute(self.store.pool())
                .await
                .map_err(|e| e.to_string())?;
            }
        }
        let can_recall = binding.learning_eligible || binding.kind == "learning";
        let memories = if can_recall {
            self.store.compact_context(&access, &query, 8000).await?
        } else {
            String::new()
        };
        let skills = if can_recall {
            let index=self.store.list_skills(&access).await?.into_iter().take(30).map(|s|json!({"id":s.id,"name":s.name,"description":s.description,"revision":s.revision})).collect::<Vec<_>>();
            serde_json::to_string(&index).map_err(|e| e.to_string())?
        } else {
            String::new()
        };
        let mut context =
            agent_protocol::additional_context(profile.as_ref(), &memories, &skills, &binding.kind);
        if can_recall {
            let sources = if binding.kind == "learning" {
                // The worker has already bounded this exact durable batch. Never truncate it again.
                let mut sources = Vec::new();
                for reference in self.learning_sources(thread_id).await? {
                    let (id, revision) = reference
                        .rsplit_once('@')
                        .ok_or("Invalid learning evidence reference")?;
                    let revision = revision.parse::<i64>().map_err(|e| e.to_string())?;
                    let source:Option<(String,i64,String,Option<String>,Option<String>,String)>=sqlx::query_as("SELECT source_id,revision,thread_id,agent_id,project_id,content FROM memory_sources WHERE source_id=? AND revision=? AND active=1").bind(id).bind(revision).fetch_optional(self.store.pool()).await.map_err(|e|e.to_string())?;
                    if let Some((id, revision, thread, agent, project, content)) = source {
                        sources.push(json!({"sourceId":format!("{id}@{revision}"),"revision":revision,"threadId":thread,"agentId":agent,"projectId":project,"content":content,"active":true}));
                    }
                }
                sources
            } else {
                revision_sources(self.store.list_sources(&access, 50).await?)
            };
            context["brr_source_index"] = json!({"kind":"untrusted","name":"brr_source_index","value":serde_json::to_string(&sources).map_err(|e|e.to_string())?});
        }
        params["additionalContext"] = context;
        self.prepare_assigned_chat_context(thread_id, &mut params)
            .await?;
        Ok(params)
    }

    pub(crate) async fn capture_tool_fence(&self, params: &mut Value) -> Result<(), String> {
        let binding = self
            .store
            .session_binding(required(params, "threadId")?)
            .await?;
        if binding.kind == "task" {
            let run = self
                .store
                .get_run(
                    binding
                        .task_run_id
                        .as_deref()
                        .ok_or("Task session has no run")?,
                )
                .await?;
            params["brrHostLease"] = json!(run.lease_token);
        }
        Ok(())
    }

    pub(crate) fn set_browser_handler(&self, handler: crate::workspace_bridge::BrowserToolHandler) {
        *self.browser_handler.write().unwrap() = Some(handler);
    }

    pub async fn handle_tool(&self, params: Value) -> Result<Value, String> {
        // Dedicated rendering path; UI output does not acquire memory authority.
        if required(&params, "tool")? == "brr_ui_render" {
            return crate::intelligent_ui::render(self, params).await;
        }
        let thread = required(&params, "threadId")?;
        let binding = self.store.session_binding(thread).await?;
        let _control = if binding.kind == "task" {
            let run = self
                .store
                .get_run(
                    binding
                        .task_run_id
                        .as_deref()
                        .ok_or("Task session has no run")?,
                )
                .await?;
            let guard = self.workers.task_control_guard(&run.task_id).await;
            let current = self.store.get_run(&run.id).await?;
            if current.state != "running"
                || params["brrHostLease"].as_str() != Some(current.lease_token.as_str())
            {
                return Err("The tool call belongs to a stale or stopped task lease".into());
            }
            Some(guard)
        } else {
            None
        };
        let tool = required(&params, "tool")?;
        if !agent_protocol::tools(&binding.kind)
            .iter()
            .any(|spec| spec["name"] == tool)
        {
            return Err("This harness tool is unavailable in this session".into());
        }
        let args = &params["arguments"];
        if !args.is_object() {
            return Err("Tool arguments must be an object".into());
        }
        if tool == "brr_browser" {
            let handler = self
                .browser_handler
                .read()
                .unwrap()
                .clone()
                .ok_or("The inline browser is unavailable in this host")?;
            return handler(thread.to_owned(), args.clone()).await;
        }
        let result = match tool {
            "brr_agent_propose" => {
                let candidate: ProfileCandidate = parse(args["candidate"].clone())?;
                json!(
                    self.store
                        .propose_profile(thread, binding.agent_id, candidate)
                        .await?
                )
            }
            "brr_agent_create" => {
                let id = required(args, "draftId")?;
                let hash = required(args, "hash")?;
                let draft = self
                    .store
                    .list_profile_drafts(Some(thread))
                    .await?
                    .into_iter()
                    .find(|d| d.id == id && d.hash == hash)
                    .ok_or("Draft does not belong to this interview")?;
                if draft.status != "approved" && draft.status != "applied" {
                    return Err("The user must review this exact candidate before creation".into());
                }
                json!(
                    self.store
                        .apply_profile(id, hash, &format!("profile-apply:{id}:{hash}"))
                        .await?
                )
            }
            "brr_memory_search" => {
                let access = self.tool_access(&binding).await?;
                json!({"data":self.store.search_memory(&access,required(args,"query")?).await?,"sources":revision_sources(self.store.list_sources(&access,50).await?)})
            }
            "brr_memory_remember" => {
                let access = self.tool_access(&binding).await?;
                json!(
                    self.store
                        .publish_memory(
                            &access,
                            parse(args["scope"].clone())?,
                            required(args, "kind")?,
                            required(args, "content")?,
                            &evidence_refs(args)?
                        )
                        .await?
                )
            }
            "brr_memory_forget" => {
                let access = self.tool_access(&binding).await?;
                json!({"eventId":self.store.forget_memory(&access,required(args,"id")?).await?})
            }
            "brr_memory_update" => {
                let access = self.tool_access(&binding).await?;
                json!(
                    self.store
                        .revise_memory(
                            &access,
                            required(args, "id")?,
                            args["revision"].as_i64().ok_or("revision is required")?,
                            required(args, "content")?,
                            &evidence_refs(args)?
                        )
                        .await?
                )
            }
            "brr_skill_publish" => {
                let access = self.tool_access(&binding).await?;
                let refs = evidence_refs(args)?;
                let mut demonstrated = false;
                for reference in &refs {
                    let id = reference.rsplit_once('@').unwrap().0;
                    let role: Option<String> = sqlx::query_scalar(
                        "SELECT source_role FROM host_learning_evidence WHERE source_id=?",
                    )
                    .bind(id)
                    .fetch_optional(self.store.pool())
                    .await
                    .map_err(|e| e.to_string())?;
                    demonstrated |= role.as_deref() == Some("tool_verified");
                }
                if !demonstrated {
                    return Err("A learned procedure needs verified successful tool evidence; a request or an assistant claim does not prove it worked".into());
                }
                json!(
                    self.store
                        .publish_skill(
                            &access,
                            parse(args["scope"].clone())?,
                            required(args, "name")?,
                            required(args, "description")?,
                            required(args, "content")?,
                            &evidence_refs(args)?
                        )
                        .await?
                )
            }
            "brr_skill_read" => {
                let access = self.tool_access(&binding).await?;
                let skill = self
                    .store
                    .list_skills(&access)
                    .await?
                    .into_iter()
                    .find(|s| s.id == args["id"])
                    .ok_or("Skill is unavailable in this scope or its evidence was revoked")?;
                json!(skill)
            }
            "brr_task_propose" => {
                let mut nodes: Vec<DelegationNode> = parse(args["tasks"].clone())?;
                for node in &mut nodes {
                    self.validate_task(&mut node.task).await?;
                }
                let parent = if let Some(run) = binding.task_run_id {
                    self.store.get_run(&run).await?.task_id
                } else {
                    let first = nodes.first().ok_or("A task graph cannot be empty")?;
                    self.store
                        .create_task_paused(TaskInput {
                            agent_id: binding
                                .agent_id
                                .unwrap_or_else(|| first.task.agent_id.clone()),
                            title: "Reviewed delegation".into(),
                            instructions:
                                "Coordinate the reviewed task graph and report its results.".into(),
                            acceptance_criteria: vec!["All approved child tasks succeed.".into()],
                            context: None,
                            cwd: binding.cwd,
                            project_id: binding.project_id,
                            model: None,
                            effort: None,
                            workspace_permission: None,
                            dependencies: vec![],
                        })
                        .await?
                        .id
                };
                json!(self.store.propose_delegation(&parent, nodes).await?)
            }
            "brr_task_complete" | "brr_task_block" | "brr_learning_done" => {
                self.tool_task(&binding, &params, args).await?
            }
            "brr_project_list"
            | "brr_ticket_list"
            | "brr_ticket_read"
            | "brr_ticket_create"
            | "brr_ticket_update"
            | "brr_ticket_comment"
            | "brr_ticket_run_propose" => {
                crate::tickets::runtime_tool(self, &binding, &params, args).await?
            }
            _ => return Err("Unsupported agent harness tool".into()),
        };
        self.changed();
        Ok(result)
    }
    pub(crate) async fn capture_item_source(
        &self,
        binding: &SessionBinding,
        event: &Value,
    ) -> Result<(), String> {
        if !binding.learning_eligible || event["method"] != "item/completed" {
            return Ok(());
        }
        let item = &event["params"]["item"];
        let kind = item["type"].as_str().unwrap_or("");
        if !matches!(
            kind,
            "agentMessage" | "commandExecution" | "fileChange" | "mcpToolCall"
        ) {
            return Ok(());
        }
        let Some(id) = item["id"].as_str() else {
            return Ok(());
        };
        let verified = match kind {
            "commandExecution" => item["status"] == "completed" && item["exitCode"] == 0,
            "fileChange" => item["status"] == "completed",
            "mcpToolCall" => {
                item["status"] == "completed"
                    && item["error"].is_null()
                    && item["result"]["isError"] != true
            }
            _ => false,
        };
        let role = if verified {
            "tool_verified"
        } else if kind == "agentMessage" {
            "assistant_unverified"
        } else {
            "tool_failed_or_unverified"
        };
        let source_id = format!("{}:item:{id}", binding.thread_id);
        let content = redact_evidence(&format!("Evidence role: {role}.\n{item}"));
        let access = self.store.memory_access(&binding.thread_id).await?;
        self.store
            .record_source(&access, &source_id, 1, &content)
            .await?;
        sqlx::query("INSERT OR IGNORE INTO host_learning_evidence(source_id,thread_id,source_role) VALUES(?,?,?)").bind(&source_id).bind(&binding.thread_id).bind(role).execute(self.store.pool()).await.map_err(|e|e.to_string())?;
        Ok(())
    }
}

pub(crate) fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}
pub(crate) fn optional<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|s| !s.trim().is_empty())
}
pub(crate) fn parse<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}
fn check_fields(value: &Value, fields: &[&str]) -> Result<(), String> {
    let obj = value.as_object().ok_or("Expected an object")?;
    if let Some(key) = obj.keys().find(|key| !fields.contains(&key.as_str())) {
        return Err(format!("Unsupported field: {key}"));
    }
    Ok(())
}
fn canonical_folder(cwd: &str) -> Result<String, String> {
    let path = std::fs::canonicalize(Path::new(cwd)).map_err(|e| e.to_string())?;
    if !path.is_dir() {
        return Err("Choose a directory".into());
    }
    Ok(path.to_string_lossy().into_owned())
}
async fn git_folder(cwd: &str) -> Result<bool, String> {
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move || worktrunk::git::Repository::at_fresh(&cwd).is_ok())
        .await
        .map_err(|e| e.to_string())
}
fn revision_sources(sources: Vec<brr_agent_core::MemorySource>) -> Vec<Value> {
    let mut remaining = 16000usize;
    sources
        .into_iter()
        .take_while(|s| {
            let size = s.content.chars().count() + 200;
            if size > remaining {
                return false;
            }
            remaining -= size;
            true
        })
        .map(|s| {
            let mut v = json!(s);
            v["sourceId"] = json!(format!("{}@{}", s.source_id, s.revision));
            v
        })
        .collect()
}
fn evidence_refs(args: &Value) -> Result<Vec<String>, String> {
    let refs: Vec<String> = parse(args["sourceIds"].clone())?;
    if refs.is_empty()
        || refs.iter().any(|s| {
            s.rsplit_once('@')
                .and_then(|(_, r)| r.parse::<i64>().ok())
                .is_none()
        })
    {
        return Err("Use the exact sourceId@revision references from the evidence index".into());
    }
    Ok(refs)
}

pub(crate) fn redact_evidence(text: &str) -> String {
    let mut content: String = text
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "sk-proj-",
                "sk-live-",
                "bearer ",
                "-----begin private key",
                "password=",
                "password\":",
                "api_key=",
                "api_key\":",
                "access_token\":",
                "refresh_token\":",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
            {
                "[credential-containing evidence redacted]".to_owned()
            } else {
                line.chars().take(6000).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(12000)
        .collect();
    // Bound serialized bytes, including Unicode and JSON escaping, before a source enters the queue.
    let mut truncated = false;
    while serde_json::to_vec(&content).is_ok_and(|v| v.len() > 7000) {
        let mut end = content.len() * 3 / 4;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
        truncated = true;
    }
    if truncated {
        content.push_str("\n[Evidence truncated to the bounded source size.]");
    }
    content
}

#[cfg(test)]
mod evidence_tests {
    use super::redact_evidence;
    #[test]
    fn source_capture_bounds_unicode_and_escaping_and_redacts_credentials() {
        let content = redact_evidence(&"🦀\"\\".repeat(12000));
        assert!(serde_json::to_vec(&content).unwrap().len() < 7200);
        assert!(content.contains("truncated"));
        assert!(!redact_evidence("api_key=synthetic-test-credential")
            .contains("synthetic-test-credential"));
    }
}

/// Runtime roots are supplied only by the native host, never by UI configuration.
pub(crate) fn workspace_config(
    kind: &str,
    workspace: &crate::worktrees::SessionWorkspace,
) -> Value {
    let mut config = agent_protocol::thread_config(kind);
    config["sandbox_workspace_write.writable_roots"] = json!(workspace.roots);
    config
}
