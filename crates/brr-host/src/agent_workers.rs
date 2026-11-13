use crate::agent_host::AgentHost;
use brr_agent_core::{
    MemoryAccess, QuestionRequest, SessionBinding, Task, TaskRun, WorkspacePermission,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, watch, Notify},
    task::{JoinHandle, JoinSet},
};

const WORKER_ID: &str = "brr-background";
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_LEARNING_SOURCES: usize = 40;
const MAX_LEARNING_PAYLOAD_BYTES: usize = 14_000;

#[derive(Clone)]
pub(crate) struct WorkerState {
    inner: Arc<WorkerInner>,
}

struct WorkerInner {
    sender: mpsc::UnboundedSender<Value>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    stop: watch::Sender<bool>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    active_runs: Mutex<HashSet<String>>,
    approvals: Mutex<HashMap<String, ApprovalLease>>,
    workspace_guards: Mutex<HashMap<String, Vec<String>>>,
    workspace_changed: Notify,
    workspace_reservations: Mutex<HashMap<String, Vec<String>>>,
    approved_ticket_projects: Mutex<HashMap<String, crate::tickets::ApprovedTicketProject>>,
    task_control_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    learning_retry_after: Mutex<i64>,
    checked_waiting_runs: Mutex<HashSet<String>>,
    started: AtomicBool,
}

#[derive(Clone)]
struct ApprovalLease {
    task_id: String,
    run_id: String,
    lease_token: String,
}

#[derive(Clone, Debug)]
struct LearningSource {
    reference: String,
    wire: Value,
}

impl WorkerState {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let (stop, _) = watch::channel(false);
        Self {
            inner: Arc::new(WorkerInner {
                sender,
                receiver: Mutex::new(Some(receiver)),
                stop,
                handles: Mutex::new(Vec::new()),
                active_runs: Mutex::new(HashSet::new()),
                approvals: Mutex::new(HashMap::new()),
                workspace_guards: Mutex::new(HashMap::new()),
                workspace_changed: Notify::new(),
                workspace_reservations: Mutex::new(HashMap::new()),
                approved_ticket_projects: Mutex::new(HashMap::new()),
                task_control_locks: Mutex::new(HashMap::new()),
                learning_retry_after: Mutex::new(0),
                checked_waiting_runs: Mutex::new(HashSet::new()),
                started: AtomicBool::new(false),
            }),
        }
    }

    fn start(&self, host: std::sync::Weak<AgentHost>) {
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(receiver) = self.inner.receiver.lock().unwrap().take() else {
            return;
        };
        let mut stop = self.inner.stop.subscribe();
        let worker_state = self.clone();
        let handle = tokio::spawn(async move {
            worker_loop(host, worker_state, receiver, &mut stop).await;
        });
        self.inner.handles.lock().unwrap().push(handle);
    }

    fn notify(&self, event: Value) {
        let _ = self.inner.sender.send(event);
    }

    async fn stop(&self) {
        let _ = self.inner.stop.send(true);
        let handles = std::mem::take(&mut *self.inner.handles.lock().unwrap());
        for handle in handles {
            let _ = handle.await;
        }
    }

    fn release_workspace(&self, run_id: &str) {
        self.inner.workspace_guards.lock().unwrap().remove(run_id);
        self.inner
            .approved_ticket_projects
            .lock()
            .unwrap()
            .remove(run_id);
        self.inner
            .workspace_reservations
            .lock()
            .unwrap()
            .remove(run_id);
        self.inner.workspace_changed.notify_waiters();
    }

    pub(crate) fn forget_run(&self, run_id: &str) {
        self.release_workspace(run_id);
        self.inner.active_runs.lock().unwrap().remove(run_id);
        self.inner
            .checked_waiting_runs
            .lock()
            .unwrap()
            .remove(run_id);
        self.inner
            .approvals
            .lock()
            .unwrap()
            .retain(|_, approval| approval.run_id != run_id);
    }

    fn excluded_workspaces(&self) -> Vec<String> {
        self.inner
            .workspace_reservations
            .lock()
            .unwrap()
            .values()
            .flat_map(|roots| roots.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    async fn reserve_workspace(&self, run_id: &str, task: &Task) -> Result<(), String> {
        let Some(cwd) = task.cwd.as_deref() else {
            return Ok(());
        };
        let cwd_owned = cwd.to_owned();
        if tokio::task::spawn_blocking(move || {
            worktrunk::git::Repository::at_fresh(&cwd_owned).is_ok()
        })
        .await
        .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        self.reserve_workspace_roots(run_id, vec![cwd.to_owned()])
    }

    fn reserve_workspace_roots(&self, run_id: &str, roots: Vec<String>) -> Result<(), String> {
        let roots = canonical_workspace_roots(roots)?;
        if roots.is_empty() {
            return Ok(());
        }
        if self
            .inner
            .workspace_guards
            .lock()
            .unwrap()
            .contains_key(run_id)
        {
            return Ok(());
        }
        self.inner
            .workspace_reservations
            .lock()
            .unwrap()
            .insert(run_id.to_owned(), roots);
        Ok(())
    }

    async fn acquire_workspace(&self, run_id: &str, task: &Task) -> Result<(), String> {
        if self
            .inner
            .workspace_guards
            .lock()
            .unwrap()
            .contains_key(run_id)
        {
            return Ok(());
        }
        self.reserve_workspace(run_id, task).await?;
        let reservations = self
            .inner
            .workspace_reservations
            .lock()
            .unwrap()
            .get(run_id)
            .cloned()
            .unwrap_or_default();
        if reservations.is_empty() {
            return Ok(());
        }
        self.acquire_reserved_roots(run_id, reservations).await
    }

    async fn reserve_task_workspace(
        &self,
        host: &AgentHost,
        run_id: &str,
        task: &Task,
        thread_id: Option<&str>,
    ) -> Result<(), String> {
        let approved = if thread_id.is_none() {
            match crate::tickets::approved_ticket_project(host, task).await {
                Ok(approved) => approved,
                Err(error) => {
                    if let Ok(run) = host.store.get_run(run_id).await {
                        block_after_runtime_error(host, self, &run, &error).await;
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };
        let roots = if let Some(approved) = approved.as_ref() {
            project_shared_folders(&approved.workspace_project).await?
        } else {
            shared_task_folders(host, task, thread_id).await?
        };
        self.reserve_workspace_roots(run_id, roots)?;
        if let Some(approved) = approved {
            self.inner
                .approved_ticket_projects
                .lock()
                .unwrap()
                .insert(run_id.to_owned(), approved);
        }
        Ok(())
    }

    async fn acquire_task_workspace(
        &self,
        host: &AgentHost,
        run_id: &str,
        task: &Task,
        thread_id: Option<&str>,
    ) -> Result<(), String> {
        if self
            .inner
            .workspace_guards
            .lock()
            .unwrap()
            .contains_key(run_id)
        {
            return Ok(());
        }
        let approved = self
            .inner
            .approved_ticket_projects
            .lock()
            .unwrap()
            .get(run_id)
            .cloned();
        let roots = if let Some(approved) = approved {
            project_shared_folders(&approved.workspace_project).await?
        } else {
            shared_task_folders(host, task, thread_id).await?
        };
        self.reserve_workspace_roots(run_id, roots.clone())?;
        self.acquire_reserved_roots(run_id, roots).await
    }

    async fn acquire_reserved_roots(&self, run_id: &str, roots: Vec<String>) -> Result<(), String> {
        if self
            .inner
            .workspace_guards
            .lock()
            .unwrap()
            .contains_key(run_id)
        {
            return Ok(());
        }
        let roots = canonical_workspace_roots(roots)?;
        if roots.is_empty() {
            return Ok(());
        }
        loop {
            let notified = self.inner.workspace_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut active = self.inner.workspace_guards.lock().unwrap();
                if active.contains_key(run_id) {
                    return Ok(());
                }
                let conflict = active
                    .iter()
                    .any(|(owner, held)| owner != run_id && roots_overlap_any(&roots, held));
                if !conflict {
                    active.insert(run_id.to_owned(), roots.clone());
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    pub(crate) async fn task_control_guard(
        &self,
        task_id: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.inner.task_control_locks.lock().unwrap();
            locks
                .entry(task_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }
}

impl AgentHost {
    pub(crate) async fn init_workers(&self) -> Result<(), String> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_task_turns (
                run_id TEXT NOT NULL, turn_id TEXT NOT NULL, status TEXT NOT NULL,
                effects_unknown INTEGER NOT NULL DEFAULT 0, processed INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(run_id,turn_id)
            )",
        )
        .execute(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_task_progress (
                run_id TEXT PRIMARY KEY, no_progress_nudges INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_task_submissions (
                run_id TEXT NOT NULL, client_user_message_id TEXT NOT NULL,
                continuation INTEGER NOT NULL, dispatched INTEGER NOT NULL DEFAULT 0,
                turn_id TEXT, created_at INTEGER NOT NULL,
                PRIMARY KEY(run_id,client_user_message_id)
            )",
        )
        .execute(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_learning_jobs (
                id TEXT PRIMARY KEY, source_thread_id TEXT NOT NULL, source_turn_id TEXT NOT NULL,
                source_refs TEXT NOT NULL, agent_id TEXT, project_id TEXT, cwd TEXT,
                state TEXT NOT NULL, learning_thread_id TEXT, reason TEXT,
                turn_id TEXT, completion_turn_id TEXT,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                UNIQUE(source_thread_id,source_turn_id)
            )",
        )
        .execute(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        let has_continuation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('agent_learning_jobs') WHERE name='continuation_count'",
        )
        .fetch_one(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        if has_continuation_count == 0 {
            sqlx::query(
                "ALTER TABLE agent_learning_jobs ADD COLUMN continuation_count INTEGER NOT NULL DEFAULT 0",
            )
            .execute(self.store.pool())
            .await
            .map_err(|error| error.to_string())?;
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS agent_learning_pending ON agent_learning_jobs(state,created_at)",
        )
        .execute(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(crate) async fn tool_access(
        &self,
        binding: &SessionBinding,
    ) -> Result<MemoryAccess, String> {
        if binding.kind != "learning" {
            return self.store.memory_access(&binding.thread_id).await;
        }
        let source_thread: Option<String> = sqlx::query_scalar(
            "SELECT source_thread_id FROM agent_learning_jobs WHERE learning_thread_id=? AND state IN ('running','completion_proposed')",
        )
        .bind(&binding.thread_id)
        .fetch_optional(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        let source_thread = source_thread.ok_or("learning session has no active evidence job")?;
        self.store.memory_access(&source_thread).await
    }

    pub(crate) async fn learning_sources(
        &self,
        learning_thread_id: &str,
    ) -> Result<Vec<String>, String> {
        let refs: Option<String> = sqlx::query_scalar(
            "SELECT source_refs FROM agent_learning_jobs WHERE learning_thread_id=? AND state IN ('running','completion_proposed')",
        )
        .bind(learning_thread_id)
        .fetch_optional(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        refs.map(|raw| serde_json::from_str(&raw).map_err(|error| error.to_string()))
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    /// Returns the current task run for a durable question after checking its immutable bindings.
    /// The caller must hold this task's control guard.
    pub(crate) async fn activate_question_run(
        &self,
        question: &QuestionRequest,
    ) -> Result<Option<TaskRun>, String> {
        let task_id = question
            .task_id
            .as_deref()
            .ok_or("question is not bound to a task")?;
        let run_id = question
            .run_id
            .as_deref()
            .ok_or("question is not bound to a task run")?;
        let task = self.store.get_task(task_id).await?;
        let mut run = self.store.get_run(run_id).await?;
        let binding = self.store.session_binding(&question.thread_id).await?;
        if !question_run_binding_matches(question, &task, &run, &binding) {
            return Err("question does not match the immutable task run and thread binding".into());
        }
        if run.state == "running" {
            self.workers
                .reserve_task_workspace(self, &run.id, &task, run.thread_id.as_deref())
                .await?;
            self.workers
                .acquire_task_workspace(self, &run.id, &task, run.thread_id.as_deref())
                .await?;
            self.workers
                .inner
                .active_runs
                .lock()
                .unwrap()
                .insert(run.id.clone());
            return Ok(Some(run));
        }
        if run.state != "waiting_input" || task.status != "waiting_input" {
            return Ok(None);
        }
        let settings = self.store.get_task_settings().await?;
        let running: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM task_runs WHERE state='running'")
                .fetch_one(self.store.pool())
                .await
                .map_err(|error| error.to_string())?;
        let learning_running: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM agent_learning_jobs WHERE state IN ('running','completion_proposed'))",
        )
        .fetch_one(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        let capacity = (settings.max_background as i64).saturating_sub(i64::from(learning_running));
        if running >= capacity {
            return Ok(None);
        }
        run = match self
            .store
            .resume_run(&task.id, &run.id, &run.lease_token, WORKER_ID)
            .await
        {
            Ok(run) => run,
            Err(error) if error.contains("capacity is full") => return Ok(None),
            Err(error) => return Err(error),
        };
        self.workers
            .reserve_task_workspace(self, &run.id, &task, run.thread_id.as_deref())
            .await?;
        self.workers
            .acquire_task_workspace(self, &run.id, &task, run.thread_id.as_deref())
            .await?;
        self.workers
            .inner
            .checked_waiting_runs
            .lock()
            .unwrap()
            .remove(&run.id);
        self.workers
            .inner
            .active_runs
            .lock()
            .unwrap()
            .insert(run.id.clone());
        Ok(Some(run))
    }

    /// Attaches a host-dispatched answer follow-up to the existing task run.
    pub(crate) async fn attach_question_turn(
        &self,
        question: &QuestionRequest,
        run: &TaskRun,
        turn_id: &str,
    ) -> Result<(), String> {
        let task_id = question
            .task_id
            .as_deref()
            .ok_or("question is not bound to a task")?;
        if question.run_id.as_deref() != Some(run.id.as_str())
            || run.task_id != task_id
            || run.thread_id.as_deref() != Some(question.thread_id.as_str())
            || turn_id.trim().is_empty()
            || question
                .client_message_id
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err("question answer does not match the immutable task run binding".into());
        }
        let current = self.store.get_run(&run.id).await?;
        if current.state != "running"
            || current.lease_token != run.lease_token
            || current.thread_id.as_deref() != Some(question.thread_id.as_str())
        {
            return Err("question answer belongs to a stale task run lease".into());
        }
        let attached = self
            .store
            .attach_continuation(
                task_id,
                &run.id,
                &question.thread_id,
                turn_id,
                &run.lease_token,
            )
            .await?;
        persist_submission_turn(self, &run.id, turn_id).await?;
        record_turn(self, &attached, turn_id, "inProgress", false, false).await?;
        self.changed();
        Ok(())
    }

    async fn wait_for_question_input(&self, event: &Value) -> Result<(), String> {
        let params = &event["params"];
        let thread_id = params["threadId"]
            .as_str()
            .ok_or("question request has no thread id")?;
        let turn_id = params["turnId"]
            .as_str()
            .ok_or("question request has no turn id")?;
        let item_id = params["itemId"]
            .as_str()
            .ok_or("question request has no item id")?;
        let question = self
            .store
            .list_questions(Some(thread_id), false)
            .await?
            .into_iter()
            .find(|question| {
                question.item_id == item_id
                    && question.turn_id == turn_id
                    && question.kind == "blocking"
            });
        let Some(question) = question else {
            return Ok(());
        };
        if question.resolution != "pending" {
            return Ok(());
        }
        let (Some(task_id), Some(run_id)) =
            (question.task_id.as_deref(), question.run_id.as_deref())
        else {
            return Ok(());
        };
        let _control = self.workers.task_control_guard(task_id).await;
        let question = self.store.get_question(&question.id).await?;
        if question.resolution != "pending" {
            return Ok(());
        }
        let mut run = self.store.get_run(run_id).await?;
        if run.task_id != task_id || run.thread_id.as_deref() != Some(thread_id) {
            return Err("blocking question no longer matches its task run thread".into());
        }
        if run.state == "waiting_input" {
            self.workers
                .inner
                .active_runs
                .lock()
                .unwrap()
                .remove(run_id);
            return Ok(());
        }
        if run.state != "running"
            || run
                .turn_id
                .as_deref()
                .is_some_and(|attached| attached != turn_id)
        {
            return Ok(());
        }
        if run.turn_id.is_none() {
            run = self
                .store
                .attach_continuation(task_id, run_id, thread_id, turn_id, &run.lease_token)
                .await?;
            record_turn(self, &run, turn_id, "inProgress", false, false).await?;
        }
        self.store
            .wait_for_input(task_id, run_id, &run.lease_token)
            .await?;
        self.workers
            .inner
            .active_runs
            .lock()
            .unwrap()
            .remove(run_id);
        // Keep the workspace reservation and lock until the answer continuation ends.
        self.changed();
        Ok(())
    }

    pub(crate) fn start(self: &Arc<Self>) {
        self.workers.start(Arc::downgrade(self));
    }

    pub(crate) fn notify(&self, event: Value) {
        self.workers.notify(event);
    }

    pub(crate) async fn stop(&self) {
        self.workers.stop().await;
    }

    pub(crate) async fn before_approval(&self, request: &Value) -> Result<(), String> {
        let Some(thread_id) = request["params"]["threadId"].as_str() else {
            return Ok(());
        };
        let binding = match self.store.session_binding(thread_id).await {
            Ok(binding) if binding.kind == "task" => binding,
            _ => return Ok(()),
        };
        let Some(run_id) = binding.task_run_id.as_deref() else {
            return Ok(());
        };
        let run = self.store.get_run(run_id).await?;
        if run.state != "running" {
            return Ok(());
        }
        self.store
            .waiting_run(&run.task_id, &run.id, &run.lease_token)
            .await?;
        let request_id = request_id(request).ok_or("approval request id is missing")?;
        self.workers.inner.approvals.lock().unwrap().insert(
            request_id,
            ApprovalLease {
                task_id: run.task_id,
                run_id: run.id,
                lease_token: run.lease_token,
            },
        );
        self.workers
            .inner
            .checked_waiting_runs
            .lock()
            .unwrap()
            .remove(run_id);
        self.workers
            .inner
            .active_runs
            .lock()
            .unwrap()
            .remove(run_id);
        (self.events)(json!({"method":"brr/agents/changed","params":{}}));
        Ok(())
    }

    pub(crate) async fn resume_approval(&self, request: &Value) -> Result<(), String> {
        let Some(request_id) = request_id(request) else {
            return Ok(());
        };
        let lease = self
            .workers
            .inner
            .approvals
            .lock()
            .unwrap()
            .get(&request_id)
            .cloned();
        let Some(lease) = lease else {
            return Ok(());
        };
        let run = self
            .store
            .resume_run(&lease.task_id, &lease.run_id, &lease.lease_token, WORKER_ID)
            .await?;
        let task = self.store.get_task(&run.task_id).await?;
        self.workers
            .acquire_task_workspace(self, &run.id, &task, run.thread_id.as_deref())
            .await?;
        self.workers
            .inner
            .approvals
            .lock()
            .unwrap()
            .remove(&request_id);
        self.workers
            .inner
            .checked_waiting_runs
            .lock()
            .unwrap()
            .remove(&run.id);
        self.workers
            .inner
            .active_runs
            .lock()
            .unwrap()
            .insert(run.id);
        Ok(())
    }

    pub(crate) async fn tool_task(
        &self,
        binding: &SessionBinding,
        params: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        let tool = params["tool"].as_str().ok_or("tool name is required")?;
        if tool == "brr_learning_done" {
            return self.learning_done(binding, params, args).await;
        }
        let run_id = binding
            .task_run_id
            .as_deref()
            .ok_or("task session has no immutable run binding")?;
        let mut run = self.store.get_run(run_id).await?;
        if run.state == "running"
            && run.thread_id.as_deref() == Some(binding.thread_id.as_str())
            && run.turn_id.is_none()
        {
            let turn_id = params["turnId"].as_str().ok_or("turnId is required")?;
            self.store
                .attach_continuation(
                    &run.task_id,
                    &run.id,
                    &binding.thread_id,
                    turn_id,
                    &run.lease_token,
                )
                .await?;
            persist_submission_turn(self, run_id, params["turnId"].as_str().unwrap()).await?;
            run = self.store.get_run(run_id).await?;
        }
        validate_run_binding(binding, params, &run)?;
        match tool {
            "brr_task_complete" => {
                let result = required(args, "result")?;
                let evidence = args["evidence"]
                    .as_array()
                    .ok_or("completion evidence must be a non-empty array")?
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .filter(|item| !item.trim().is_empty())
                            .map(str::to_owned)
                            .ok_or_else(|| {
                                "completion evidence entries must be non-empty strings".to_string()
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.store
                    .propose_completion(&run.task_id, &run.id, &run.lease_token, result, evidence)
                    .await?;
                Ok(json!({"accepted":true,"awaiting":"authoritativeTurnSettlement"}))
            }
            "brr_task_block" => {
                let reason = required(args, "reason")?;
                self.store
                    .block_run(&run.task_id, &run.id, &run.lease_token, reason, true)
                    .await?;
                self.workers.release_workspace(&run.id);
                self.workers
                    .inner
                    .active_runs
                    .lock()
                    .unwrap()
                    .remove(&run.id);
                self.changed();
                Ok(json!({"blocked":true,"reason":reason}))
            }
            _ => Err(format!("Unsupported task tool: {tool}")),
        }
    }

    async fn learning_done(
        &self,
        binding: &SessionBinding,
        params: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        if binding.kind != "learning" || !args.as_object().is_some_and(|object| object.is_empty()) {
            return Err(
                "learning completion is only available without arguments in a learning session"
                    .into(),
            );
        }
        let thread_id = params["threadId"].as_str().ok_or("threadId is required")?;
        let turn_id = params["turnId"].as_str().ok_or("turnId is required")?;
        if thread_id != binding.thread_id {
            return Err("learning completion does not match its immutable session binding".into());
        }
        let changed = sqlx::query(
            "UPDATE agent_learning_jobs SET state='completion_proposed',turn_id=?,completion_turn_id=?,updated_at=? WHERE learning_thread_id=? AND state='running' AND (turn_id IS NULL OR turn_id=?)",
        )
        .bind(turn_id)
        .bind(turn_id)
        .bind(epoch_seconds())
        .bind(thread_id)
        .bind(turn_id)
        .execute(self.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        if changed.rows_affected() != 1 {
            return Err("learning completion requires the active attached turn".into());
        }
        Ok(json!({"accepted":true,"awaiting":"authoritativeTurnSettlement"}))
    }
}

async fn worker_loop(
    host: std::sync::Weak<AgentHost>,
    workers: WorkerState,
    mut events: mpsc::UnboundedReceiver<Value>,
    stop: &mut watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    let mut tasks = JoinSet::new();
    let mut startup = true;
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
            Some(event) = events.recv() => {
                if let Some(host) = host.upgrade() {
                    if let Err(error) = handle_notification(&host, &workers, &event).await {
                        (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
                    }
                } else {
                    break;
                }
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result {
                    if let Some(host) = host.upgrade() {
                        (host.events)(json!({"method":"brr/worker/error","params":{"message":error.to_string()}}));
                    }
                }
            }
            _ = tick.tick() => {}
        }
        let Some(host) = host.upgrade() else { break };
        if *stop.borrow() {
            break;
        }
        if startup {
            startup = false;
            if let Err(error) = host.reconcile_questions().await {
                (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
            }
            if let Err(error) = host.poll_question_deliveries(&mut tasks).await {
                (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
            }
            if let Err(error) = recover_workers(&host, &workers, &mut tasks).await {
                (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
            }
        }
        if let Err(error) = host.store.tick_workflows(epoch_seconds()).await {
            (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
        }
        if let Err(error) = host.poll_question_deliveries(&mut tasks).await {
            (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
        }
        if let Err(error) = poll_tasks(&host, &workers, &mut tasks).await {
            (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
        }
    }
}

async fn recover_workers(
    host: &Arc<AgentHost>,
    workers: &WorkerState,
    tasks: &mut JoinSet<()>,
) -> Result<(), String> {
    recover_question_runs(host, workers).await?;
    recover_waiting_runs(host, workers, tasks).await?;
    let unresolved = host.store.unresolved_runs().await?;
    for run in unresolved.into_iter().filter(|run| {
        run.state == "running" && !workers.inner.active_runs.lock().unwrap().contains(&run.id)
    }) {
        let task = host.store.get_task(&run.task_id).await?;
        workers
            .reserve_task_workspace(host, &run.id, &task, run.thread_id.as_deref())
            .await?;
        if !mark_active(workers, &run.id) {
            continue;
        }
        let host = Arc::downgrade(host);
        let workers = workers.clone();
        tasks.spawn(async move {
            if let Some(host) = host.upgrade() {
                if let Err(error) = recover_task_run(&host, &workers, &run).await {
                    block_after_runtime_error(&host, &workers, &run, &error).await;
                }
            }
        });
    }
    recover_learning(host, workers).await?;
    Ok(())
}

/// Reconciles runs owned by durable questions without starting a generic continuation.
async fn recover_question_runs(
    host: &Arc<AgentHost>,
    workers: &WorkerState,
) -> Result<HashSet<String>, String> {
    let requests = host.store.list_questions(None, true).await?;
    let mut by_run = HashMap::<String, Vec<QuestionRequest>>::new();
    for request in requests {
        if let Some(run_id) = request.run_id.as_ref() {
            by_run.entry(run_id.clone()).or_default().push(request);
        }
    }
    let mut protected = HashSet::new();
    for run in host.store.unresolved_runs().await? {
        let Some(questions) = by_run.get(&run.id) else {
            continue;
        };
        if !host.store.pending_run_questions(&run.id).await? {
            continue;
        }
        protected.insert(run.id.clone());
        if matches!(run.state.as_str(), "waiting_input" | "waiting_approval") {
            let _control = workers.task_control_guard(&run.task_id).await;
            // Delivery may have resumed this run while recovery waited for the task guard.
            let current = host.store.get_run(&run.id).await?;
            if !matches!(current.state.as_str(), "waiting_input" | "waiting_approval") {
                continue;
            }
            let task = host.store.get_task(&current.task_id).await?;
            workers
                .reserve_task_workspace(host, &run.id, &task, current.thread_id.as_deref())
                .await?;
            workers
                .acquire_task_workspace(host, &run.id, &task, current.thread_id.as_deref())
                .await?;
            workers.inner.active_runs.lock().unwrap().remove(&run.id);
            continue;
        }
        if run.state != "running" {
            continue;
        }
        let control = workers.task_control_guard(&run.task_id).await;
        // Answer activation may have resumed or claimed this run while recovery waited.
        let current = host.store.get_run(&run.id).await?;
        if current.state != "running"
            || workers.inner.active_runs.lock().unwrap().contains(&run.id)
            || !host.store.pending_run_questions(&run.id).await?
            || !mark_active(workers, &run.id)
        {
            continue;
        }
        let result = reconcile_question_run(host, workers, &current, questions).await;
        drop(control);
        if let Err(error) = result {
            workers.inner.active_runs.lock().unwrap().remove(&run.id);
            block_after_runtime_error(host, workers, &current, &error).await;
        }
    }
    Ok(protected)
}

async fn reconcile_question_run(
    host: &AgentHost,
    workers: &WorkerState,
    run: &TaskRun,
    questions: &[QuestionRequest],
) -> Result<(), String> {
    let task = host.store.get_task(&run.task_id).await?;
    workers
        .reserve_task_workspace(host, &run.id, &task, run.thread_id.as_deref())
        .await?;
    workers
        .acquire_task_workspace(host, &run.id, &task, run.thread_id.as_deref())
        .await?;
    let Some(thread_id) = run.thread_id.as_deref() else {
        return Err("question-bound task run has no saved runtime thread".into());
    };
    host.require_account().await?;
    host.rpc
        .call(
            "thread/resume",
            json!({
                "threadId":thread_id,
                "excludeTurns":true,
                "model":task.model,
                "modelProvider":"openai",
                "developerInstructions":crate::agent_protocol::instructions("task"),
                "config":crate::agent_protocol::thread_config("task")
            }),
        )
        .await?;
    let turns = thread_turns(host, thread_id).await?;
    let source_turn_id = run.turn_id.as_deref().or_else(|| {
        questions
            .iter()
            .max_by_key(|question| question.created_at)
            .map(|question| question.turn_id.as_str())
    });
    let Some(source_turn_id) = source_turn_id else {
        return Err("question-bound task run has no source turn".into());
    };
    let Some(source) = turns
        .iter()
        .find(|turn| turn["id"].as_str() == Some(source_turn_id))
    else {
        return Err("the question source turn is absent from runtime history; automatic continuation is unsafe".into());
    };
    let turn_status = source["status"].as_str().unwrap_or("unknown");
    let turn_id = source["id"]
        .as_str()
        .ok_or("question source turn has no runtime id")?;
    if run.turn_id.is_none() {
        let attached = host
            .store
            .attach_continuation(&task.id, &run.id, thread_id, turn_id, &run.lease_token)
            .await?;
        record_turn(
            host,
            &attached,
            turn_id,
            turn_status,
            has_unknown_effects(source),
            false,
        )
        .await?;
    }
    if matches!(turn_status, "inProgress" | "in_progress") {
        return Ok(());
    }
    let unknown = has_unknown_effects(source);
    record_turn(host, run, turn_id, turn_status, unknown, false).await?;
    match question_turn_action(true, turn_status, unknown) {
        QuestionTurnAction::Continue => Ok(()),
        QuestionTurnAction::BlockUnknown => {
            host.store
                .block_run(
                    &task.id,
                    &run.id,
                    &run.lease_token,
                    "The question source turn has an unresolved external effect or unknown status. Review it before continuing.",
                    true,
                )
                .await?;
            mark_turn_processed(host, &run.id, turn_id).await?;
            workers_release(host, workers, &run.id, &run.lease_token).await;
            host.changed();
            Ok(())
        }
        QuestionTurnAction::WaitForInput => {
            if host.store.pending_run_questions(&run.id).await? {
                host.store
                    .wait_for_input(&task.id, &run.id, &run.lease_token)
                    .await?;
                mark_turn_processed(host, &run.id, turn_id).await?;
                workers.inner.active_runs.lock().unwrap().remove(&run.id);
                // Keep workspace reservation and lock while the user's answer is pending.
                host.changed();
            }
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitingRecoveryAction {
    AwaitFreshApproval,
    ResumeSameRun,
    BlockUnknown,
}

async fn recover_waiting_runs(
    host: &Arc<AgentHost>,
    workers: &WorkerState,
    tasks: &mut JoinSet<()>,
) -> Result<(), String> {
    let waiting = host
        .store
        .unresolved_runs()
        .await?
        .into_iter()
        .filter(|run| run.state == "waiting_approval")
        .collect::<Vec<_>>();
    for run in waiting {
        if host.store.pending_run_questions(&run.id).await? {
            continue;
        }
        if workers
            .inner
            .checked_waiting_runs
            .lock()
            .unwrap()
            .contains(&run.id)
            || !mark_active(workers, &run.id)
        {
            continue;
        }
        match reconcile_waiting_run(host, workers, &run).await {
            Ok(Some(resumed)) => {
                workers
                    .inner
                    .checked_waiting_runs
                    .lock()
                    .unwrap()
                    .insert(run.id.clone());
                let host = Arc::downgrade(host);
                let workers = workers.clone();
                tasks.spawn(async move {
                    if let Some(host) = host.upgrade() {
                        if let Err(error) = recover_task_run(&host, &workers, &resumed).await {
                            block_after_runtime_error(&host, &workers, &resumed, &error).await;
                        }
                    }
                });
            }
            Ok(None) => {
                workers.inner.active_runs.lock().unwrap().remove(&run.id);
                workers
                    .inner
                    .checked_waiting_runs
                    .lock()
                    .unwrap()
                    .insert(run.id);
            }
            Err(error) => {
                workers.inner.active_runs.lock().unwrap().remove(&run.id);
                if !error.contains("capacity is full") {
                    workers
                        .inner
                        .checked_waiting_runs
                        .lock()
                        .unwrap()
                        .insert(run.id.clone());
                }
                (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
            }
        }
    }
    Ok(())
}

async fn reconcile_waiting_run(
    host: &AgentHost,
    workers: &WorkerState,
    run: &TaskRun,
) -> Result<Option<TaskRun>, String> {
    let task = host.store.get_task(&run.task_id).await?;
    workers
        .reserve_task_workspace(host, &run.id, &task, run.thread_id.as_deref())
        .await?;
    workers
        .acquire_task_workspace(host, &run.id, &task, run.thread_id.as_deref())
        .await?;
    let action = if let Some(thread_id) = run.thread_id.as_deref() {
        host.require_account().await?;
        host.rpc
            .call(
                "thread/resume",
                json!({
                    "threadId":thread_id,
                    "excludeTurns":true,
                    "model":task.model,
                    "modelProvider":"openai",
                    "developerInstructions":crate::agent_protocol::instructions("task"),
                    "config":crate::agent_protocol::thread_config("task")
                }),
            )
            .await?;
        let turns = thread_turns(host, thread_id).await?;
        if let Some(turn_id) = run.turn_id.as_deref() {
            if let Some(turn) = turns
                .iter()
                .find(|turn| turn["id"].as_str() == Some(turn_id))
            {
                waiting_recovery_action(
                    turn["status"].as_str().unwrap_or("unknown"),
                    has_unknown_effects(turn),
                )
            } else {
                WaitingRecoveryAction::BlockUnknown
            }
        } else if turns.is_empty() {
            WaitingRecoveryAction::ResumeSameRun
        } else {
            let latest = &turns[0];
            waiting_recovery_action(
                latest["status"].as_str().unwrap_or("unknown"),
                has_unknown_effects(latest),
            )
        }
    } else {
        WaitingRecoveryAction::ResumeSameRun
    };
    if action == WaitingRecoveryAction::AwaitFreshApproval {
        return Ok(None);
    }
    let resumed = host
        .store
        .resume_run(&run.task_id, &run.id, &run.lease_token, WORKER_ID)
        .await?;
    if action == WaitingRecoveryAction::BlockUnknown {
        host.store
            .block_run(
                &run.task_id,
                &run.id,
                &resumed.lease_token,
                "Approval recovery found an unresolved or ambiguous runtime effect. Review the thread before retrying.",
                true,
            )
            .await?;
        workers_release(host, workers, &run.id, &resumed.lease_token).await;
        host.changed();
        Ok(None)
    } else {
        Ok(Some(resumed))
    }
}

async fn poll_tasks(
    host: &Arc<AgentHost>,
    workers: &WorkerState,
    tasks: &mut JoinSet<()>,
) -> Result<(), String> {
    recover_waiting_runs(host, workers, tasks).await?;
    let question_runs = recover_question_runs(host, workers).await?;
    let unresolved = host.store.unresolved_runs().await?;
    let mut active = unresolved
        .iter()
        .filter(|run| run.state == "running")
        .map(|run| run.id.clone())
        .collect::<HashSet<_>>();
    workers
        .inner
        .active_runs
        .lock()
        .unwrap()
        .retain(|run_id| active.remove(run_id));

    for run in unresolved
        .into_iter()
        .filter(|run| run.state == "running" && !question_runs.contains(&run.id))
    {
        let task = host.store.get_task(&run.task_id).await?;
        workers
            .reserve_task_workspace(host, &run.id, &task, run.thread_id.as_deref())
            .await?;
        if mark_active(workers, &run.id) {
            let host = Arc::downgrade(host);
            let workers = workers.clone();
            tasks.spawn(async move {
                if let Some(host) = host.upgrade() {
                    if let Err(error) = recover_task_run(&host, &workers, &run).await {
                        block_after_runtime_error(&host, &workers, &run, &error).await;
                    }
                }
            });
        }
    }

    let settings = host.store.get_task_settings().await?;
    let mut active_count = host
        .store
        .unresolved_runs()
        .await?
        .iter()
        .filter(|run| run.state == "running")
        .count();
    let learning_running: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM agent_learning_jobs WHERE state IN ('running','completion_proposed'))",
    )
    .fetch_one(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;

    let task_capacity =
        (settings.max_background as usize).saturating_sub(usize::from(learning_running));
    while active_count < task_capacity {
        let excluded_cwds = workers.excluded_workspaces();
        let Some(run) = host
            .store
            .claim_ready_excluding_workspaces(WORKER_ID, &excluded_cwds)
            .await?
        else {
            break;
        };
        let claimed_task = host.store.get_task(&run.task_id).await?;
        workers
            .reserve_task_workspace(host, &run.id, &claimed_task, run.thread_id.as_deref())
            .await?;
        if host.store.pending_run_questions(&run.id).await? {
            workers
                .acquire_task_workspace(host, &run.id, &claimed_task, run.thread_id.as_deref())
                .await?;
            host.store
                .wait_for_input(&run.task_id, &run.id, &run.lease_token)
                .await?;
            host.changed();
            continue;
        }
        if !mark_active(workers, &run.id) {
            continue;
        }
        active_count += 1;
        let host = Arc::downgrade(host);
        let workers = workers.clone();
        tasks.spawn(async move {
            if let Some(host) = host.upgrade() {
                if let Err(error) = execute_claim(&host, &workers, &run).await {
                    block_after_runtime_error(&host, &workers, &run, &error).await;
                }
            }
        });
    }

    if active_count < settings.max_background as usize {
        start_pending_learning(host, workers).await?;
    }
    Ok(())
}

async fn shared_task_folders(
    host: &AgentHost,
    task: &Task,
    thread_id: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(thread_id) = thread_id {
        if let Some(snapshot) = saved_task_session(host, thread_id).await? {
            return Ok(snapshot
                .folders
                .into_iter()
                .filter_map(|folder| {
                    let source = PathBuf::from(&folder.source);
                    let resolved = PathBuf::from(&folder.path);
                    (canonical_path(&source).ok() == canonical_path(&resolved).ok())
                        .then(|| folder.path)
                })
                .collect());
        }
    }
    if let Some(project) = host.task_workspace_project(&task.id).await? {
        return project_shared_folders(&project).await;
    }
    if let Some(project_id) = task.project_id.as_deref() {
        let project = host
            .projects
            .read(project_id)
            .await?
            .ok_or("Task project no longer exists")?;
        return project_shared_folders(&project).await;
    }
    let Some(cwd) = task.cwd.as_deref() else {
        return Ok(Vec::new());
    };
    let path = canonical_path(Path::new(cwd))?;
    if worktrunk::git::Repository::at_fresh(&path).is_ok() {
        Ok(Vec::new())
    } else {
        Ok(vec![path.to_string_lossy().into_owned()])
    }
}

async fn project_shared_folders(
    project: &crate::projects::ProjectRecord,
) -> Result<Vec<String>, String> {
    let mut roots = Vec::new();
    for folder in &project.folders {
        let path = canonical_path(Path::new(folder))?;
        if worktrunk::git::Repository::at_fresh(&path).is_err() {
            roots.push(path.to_string_lossy().into_owned());
        }
    }
    Ok(roots)
}

fn canonical_path(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| {
        format!(
            "Cannot resolve shared task folder {}: {error}",
            path.display()
        )
    })
}

fn canonical_workspace_roots(roots: Vec<String>) -> Result<Vec<String>, String> {
    let mut paths = roots
        .into_iter()
        .map(|root| canonical_path(Path::new(&root)))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    let keep = paths
        .iter()
        .enumerate()
        .filter_map(|(i, path)| {
            if paths
                .iter()
                .enumerate()
                .any(|(j, other)| i != j && path.starts_with(other))
            {
                None
            } else {
                Some(path.to_string_lossy().into_owned())
            }
        })
        .collect();
    Ok(keep)
}

fn roots_overlap_any(left: &[String], right: &[String]) -> bool {
    left.iter().any(|a| {
        let a = Path::new(a);
        right.iter().any(|b| {
            let b = Path::new(b);
            a.starts_with(b) || b.starts_with(a)
        })
    })
}

fn mark_active(workers: &WorkerState, run_id: &str) -> bool {
    workers
        .inner
        .active_runs
        .lock()
        .unwrap()
        .insert(run_id.to_owned())
}

async fn execute_claim(
    host: &AgentHost,
    workers: &WorkerState,
    run: &TaskRun,
) -> Result<(), String> {
    let task = host.store.get_task(&run.task_id).await?;
    let has_thread = run.thread_id.is_some();
    let approved_ticket = if has_thread {
        None
    } else {
        let approved = workers
            .inner
            .approved_ticket_projects
            .lock()
            .unwrap()
            .get(&run.id)
            .cloned();
        let approved = match approved {
            Some(approved) => Some(approved),
            None => crate::tickets::approved_ticket_project(host, &task).await?,
        };
        if let Some(approved) = approved.as_ref() {
            crate::tickets::validate_approved_ticket_project(host, &task, approved).await?;
            workers
                .inner
                .approved_ticket_projects
                .lock()
                .unwrap()
                .entry(run.id.clone())
                .or_insert_with(|| approved.clone());
        }
        approved
    };
    workers
        .acquire_task_workspace(host, &run.id, &task, run.thread_id.as_deref())
        .await?;
    let run = if has_thread {
        host.store.recover_run(&run.id, &run.lease_token).await?
    } else {
        host.require_account().await?;
        let profile = host.store.read_profile(&task.agent_id).await?;
        let source_project = host.task_workspace_project(&task.id).await?;
        let session = if let Some(project) = source_project.as_ref() {
            host.open_task_session_with_project_snapshot(
                Some(task.agent_id.clone()),
                task.model.as_deref().unwrap_or(&profile.model),
                &run.id,
                task.workspace_permission.clone(),
                project,
            )
            .await?
        } else if let Some(approved) = approved_ticket.as_ref() {
            host.open_task_session_with_project_snapshot(
                Some(task.agent_id.clone()),
                task.model.as_deref().unwrap_or(&profile.model),
                &run.id,
                task.workspace_permission.clone(),
                &approved.workspace_project,
            )
            .await?
        } else {
            host.open_session(
                Some(task.agent_id.clone()),
                "task",
                task.model.as_deref().unwrap_or(&profile.model),
                task.cwd.as_deref(),
                task.project_id.as_deref(),
                false,
                Some(run.id.clone()),
                task.workspace_permission.clone(),
            )
            .await?
        };
        let thread_id = session["thread"]["id"]
            .as_str()
            .ok_or("task thread start returned no thread ID")?;
        host.store
            .bind_run_thread(&task.id, &run.id, thread_id, &run.lease_token)
            .await?
    };
    if has_thread {
        return Box::pin(recover_task_run(host, workers, &run)).await;
    }
    start_task_turn(host, workers, &run, &task, false).await
}

async fn recover_task_run(
    host: &AgentHost,
    workers: &WorkerState,
    run: &TaskRun,
) -> Result<(), String> {
    let run = host.store.recover_run(&run.id, &run.lease_token).await?;
    let task = host.store.get_task(&run.task_id).await?;
    let Some(thread_id) = run.thread_id.as_deref() else {
        return Box::pin(execute_claim(host, workers, &run)).await;
    };
    let _saved_workspace = saved_task_session(host, thread_id).await?;
    host.require_account().await?;
    host.rpc
        .call(
            "thread/resume",
            json!({
                "threadId":thread_id,
                "excludeTurns":true,
                "model":task.model,
                "modelProvider":"openai",
                "developerInstructions":crate::agent_protocol::instructions("task"),
                "config":crate::agent_protocol::thread_config("task")
            }),
        )
        .await?;
    let turns = thread_turns(host, thread_id).await?;
    let turn = if let Some(turn_id) = run.turn_id.as_deref() {
        turns
            .iter()
            .find(|turn| turn["id"].as_str() == Some(turn_id))
            .cloned()
            .ok_or("the persisted task turn is absent from runtime history; automatic replay is unsafe")?
    } else {
        let intent: Option<(String, i64, i64, Option<String>)> = sqlx::query_as(
            "SELECT client_user_message_id,continuation,dispatched,turn_id FROM agent_task_submissions WHERE run_id=? ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&run.id)
        .fetch_optional(host.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        if let Some((message_id, continuation, dispatched, recorded_turn)) = intent {
            let matched = if let Some(turn_id) = recorded_turn.as_deref() {
                turns
                    .iter()
                    .find(|turn| turn["id"].as_str() == Some(turn_id))
                    .cloned()
            } else {
                let matches = turns
                    .iter()
                    .filter(|turn| turn_has_client_message(turn, &message_id))
                    .cloned()
                    .collect::<Vec<_>>();
                match matches.len() {
                    0 => None,
                    1 => matches.into_iter().next(),
                    _ => return Err("multiple runtime turns contain the same task submission marker; automatic recovery is ambiguous".into()),
                }
            };
            if let Some(turn) = matched {
                turn
            } else if dispatched == 0 {
                start_task_turn_with_id(host, workers, &run, &task, continuation != 0, &message_id)
                    .await?;
                return Ok(());
            } else {
                return Err("runtime history does not contain the dispatched task submission marker; automatic replay is unsafe".into());
            }
        } else if let Some(latest) = turns.first() {
            let turn_id = latest["id"]
                .as_str()
                .ok_or("runtime history turn has no id")?;
            let processed: Option<i64> = sqlx::query_scalar(
                "SELECT processed FROM agent_task_turns WHERE run_id=? AND turn_id=?",
            )
            .bind(&run.id)
            .bind(turn_id)
            .fetch_optional(host.store.pool())
            .await
            .map_err(|error| error.to_string())?;
            if processed == Some(1) {
                start_task_turn(host, workers, &run, &task, true).await?;
                return Ok(());
            }
            return Err("runtime history has a turn without a durable task submission marker; automatic recovery is ambiguous".into());
        } else {
            start_task_turn(host, workers, &run, &task, false).await?;
            return Ok(());
        }
    };
    let turn_id = turn["id"]
        .as_str()
        .ok_or("runtime history turn has no id")?;
    if run.turn_id.is_none() {
        let processed: Option<i64> = sqlx::query_scalar(
            "SELECT processed FROM agent_task_turns WHERE run_id=? AND turn_id=?",
        )
        .bind(&run.id)
        .bind(turn_id)
        .fetch_optional(host.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        if processed == Some(1) {
            start_task_turn(host, workers, &run, &task, true).await?;
            return Ok(());
        }
    }
    if run.turn_id.as_deref() != Some(turn_id) {
        if run.turn_id.is_some() {
            return Err("runtime history does not contain the task's attached turn".into());
        }
        let attached = host
            .store
            .attach_continuation(&task.id, &run.id, thread_id, turn_id, &run.lease_token)
            .await?;
        persist_submission_turn(host, &run.id, turn_id).await?;
        record_turn(host, &attached, turn_id, "inProgress", false, false).await?;
        let mut attached = attached;
        attached.turn_id = Some(turn_id.to_owned());
        return settle_observed_turn(host, workers, &attached, &task, &turn).await;
    }
    settle_observed_turn(host, workers, &run, &task, &turn).await
}

async fn start_task_turn(
    host: &AgentHost,
    workers: &WorkerState,
    run: &TaskRun,
    task: &Task,
    continuation: bool,
) -> Result<(), String> {
    let client_message_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO agent_task_submissions(run_id,client_user_message_id,continuation,dispatched,created_at) VALUES(?,?,?,0,?)")
        .bind(&run.id).bind(&client_message_id).bind(i64::from(continuation)).bind(epoch_seconds())
        .execute(host.store.pool()).await.map_err(|error|error.to_string())?;
    start_task_turn_with_id(host, workers, run, task, continuation, &client_message_id).await
}

async fn start_task_turn_with_id(
    host: &AgentHost,
    _workers: &WorkerState,
    run: &TaskRun,
    task: &Task,
    continuation: bool,
    client_message_id: &str,
) -> Result<(), String> {
    let thread_id = run
        .thread_id
        .as_deref()
        .ok_or("task run is missing its persisted thread binding")?;
    let prompt = task_prompt(task, continuation);
    let mut params = json!({
        "threadId":thread_id,
        "clientUserMessageId":client_message_id,
        "model":task.model,
        "effort":task.effort,
        "approvalPolicy":"on-request",
        "input":[{"type":"text","text":prompt,"text_elements":[]}]
    });
    if let Some(permission) = task.workspace_permission.as_ref() {
        let roots = if let Some(snapshot) = saved_task_session(host, thread_id).await? {
            snapshot.roots
        } else {
            vec![host
                .store
                .session_binding(thread_id)
                .await?
                .cwd
                .ok_or("task session has no saved workspace")?]
        };
        params["sandboxPolicy"] = task_sandbox_override(permission, &roots);
    }
    let params = host.prepare_turn(thread_id, params).await?;
    sqlx::query("UPDATE agent_task_submissions SET dispatched=1 WHERE run_id=? AND client_user_message_id=?")
        .bind(&run.id).bind(&client_message_id).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
    let response = host.rpc.call("turn/start", params).await?;
    let turn_id = response["turn"]["id"]
        .as_str()
        .or_else(|| response["turnId"].as_str())
        .ok_or("runtime turn start returned no turn ID")?;
    sqlx::query(
        "UPDATE agent_task_submissions SET turn_id=? WHERE run_id=? AND client_user_message_id=?",
    )
    .bind(turn_id)
    .bind(&run.id)
    .bind(&client_message_id)
    .execute(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;
    let run = host
        .store
        .attach_continuation(&task.id, &run.id, thread_id, turn_id, &run.lease_token)
        .await?;
    record_turn(host, &run, turn_id, "inProgress", false, false).await?;
    host.changed();
    Ok(())
}

fn task_sandbox_override(permission: &WorkspacePermission, saved_roots: &[String]) -> Value {
    match permission {
        WorkspacePermission::ReadOnly => json!({"type":"readOnly","networkAccess":false}),
        WorkspacePermission::WorkspaceWrite => {
            json!({"type":"workspaceWrite","writableRoots":saved_roots,"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false})
        }
    }
}

async fn saved_task_session(
    host: &AgentHost,
    thread_id: &str,
) -> Result<Option<crate::worktrees::SessionWorkspace>, String> {
    match host.worktrees.get_session(thread_id) {
        Ok(Some(snapshot)) => {
            host.worktrees.validate_session(&snapshot).await?;
            Ok(Some(snapshot))
        }
        Ok(None) => Ok(None),
        Err(session_error) if session_error.contains("corrupt") => {
            // Older managed threads stored a single Workspace record. Preserve that
            // recovery path only when the old record parses and still validates.
            if let Some(legacy) = host.worktrees.get(thread_id)? {
                host.worktrees.validate(&legacy).await?;
                Ok(None)
            } else {
                Err(session_error)
            }
        }
        Err(error) => Err(error),
    }
}

fn task_prompt(task: &Task, continuation: bool) -> String {
    let prefix = if continuation {
        "Continue the assigned task from the existing conversation. Preserve completed work and do not repeat uncertain effects."
    } else {
        "Complete the assigned task and report evidence for every acceptance criterion."
    };
    let criteria = task
        .acceptance_criteria
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{prefix}\n\nTask: {}\n\nInstructions:\n{}\n\nAcceptance criteria:\n{}\n\nContext:\n{}",
        task.title,
        task.instructions,
        criteria,
        task.context.as_deref().unwrap_or("(none)")
    )
}

async fn settle_observed_turn(
    host: &AgentHost,
    workers: &WorkerState,
    run: &TaskRun,
    task: &Task,
    turn: &Value,
) -> Result<(), String> {
    let _control_guard = workers.task_control_guard(&task.id).await;
    let turn_id = turn["id"].as_str().ok_or("runtime turn has no id")?;
    let status = turn["status"].as_str().unwrap_or("unknown");
    if status == "inProgress" || status == "in_progress" {
        return Ok(());
    }
    let effects_unknown = has_unknown_effects(turn);
    record_turn(host, run, turn_id, status, effects_unknown, false).await?;
    let has_proposal = completion_proposed(host, &run.id, &run.task_id, &run.lease_token).await?;
    let pending_questions = host.store.pending_run_questions(&run.id).await?;
    match question_turn_action(pending_questions, status, effects_unknown) {
        QuestionTurnAction::WaitForInput => {
            host.store
                .wait_for_input(&task.id, &run.id, &run.lease_token)
                .await?;
            mark_turn_processed(host, &run.id, turn_id).await?;
            workers.inner.active_runs.lock().unwrap().remove(&run.id);
            // Keep the workspace reservation and lock across the user-input wait.
            host.changed();
            return Ok(());
        }
        QuestionTurnAction::BlockUnknown => {
            host.store
                .block_run(
                    &task.id,
                    &run.id,
                    &run.lease_token,
                    "The task turn ended with an unresolved external effect or unknown status. Review it before continuing.",
                    true,
                )
                .await?;
            mark_turn_processed(host, &run.id, turn_id).await?;
            workers_release(host, workers, &run.id, &run.lease_token).await;
            host.changed();
            return Ok(());
        }
        QuestionTurnAction::Continue => {}
    }
    if may_settle_success(status, has_proposal) {
        host.store
            .settle_run(&task.id, &run.id, turn_id, &run.lease_token, true)
            .await?;
        mark_turn_processed(host, &run.id, turn_id).await?;
        workers_release(host, workers, &run.id, &run.lease_token).await;
        host.changed();
        return Ok(());
    }
    if has_proposal {
        return Err("a task completion proposal was recorded, but runtime did not authoritatively complete that turn".into());
    }
    if matches!(status, "completed" | "interrupted" | "failed") {
        if status != "completed" && effects_unknown {
            host.store
                .block_run(
                    &task.id,
                    &run.id,
                    &run.lease_token,
                    "runtime ended while an external tool effect may be unresolved",
                    true,
                )
                .await?;
            mark_turn_processed(host, &run.id, turn_id).await?;
            workers_release(host, workers, &run.id, &run.lease_token).await;
            host.changed();
            return Ok(());
        }
        let previous_nudges: i64 = sqlx::query_scalar(
            "SELECT COALESCE((SELECT no_progress_nudges FROM agent_task_progress WHERE run_id=?),0)",
        )
        .bind(&run.id)
        .fetch_one(host.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some(next_nudges) =
            continuation_nudge_count(previous_nudges, has_verified_progress(turn))
        else {
            host.store
                .block_run(
                    &task.id,
                    &run.id,
                    &run.lease_token,
                    "The task made no verified tool or file progress across three continuation turns. Review progress and retry if more work is needed.",
                    false,
                )
                .await?;
            mark_turn_processed(host, &run.id, turn_id).await?;
            workers_release(host, workers, &run.id, &run.lease_token).await;
            host.changed();
            return Ok(());
        };
        sqlx::query("INSERT INTO agent_task_progress(run_id,no_progress_nudges) VALUES(?,?) ON CONFLICT(run_id) DO UPDATE SET no_progress_nudges=excluded.no_progress_nudges")
            .bind(&run.id).bind(next_nudges).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
        host.store
            .settle_turn(&task.id, &run.id, turn_id, &run.lease_token)
            .await?;
        mark_turn_processed(host, &run.id, turn_id).await?;
        let current = host.store.get_run(&run.id).await?;
        let current_task = host.store.get_task(&task.id).await?;
        start_task_turn(host, workers, &current, &current_task, true).await?;
    }
    Ok(())
}

async fn workers_release(
    host: &AgentHost,
    _workers: &WorkerState,
    run_id: &str,
    expected_lease_token: &str,
) {
    let Ok(current) = host.store.get_run(run_id).await else {
        return;
    };
    if current.lease_token != expected_lease_token || current.state == "running" {
        return;
    }
    host.workers.release_workspace(run_id);
    host.workers
        .inner
        .active_runs
        .lock()
        .unwrap()
        .remove(run_id);
    host.workers
        .inner
        .checked_waiting_runs
        .lock()
        .unwrap()
        .remove(run_id);
}

async fn completion_proposed(
    host: &AgentHost,
    run_id: &str,
    task_id: &str,
    lease: &str,
) -> Result<bool, String> {
    let proposed: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT proposed_result,proposed_evidence FROM task_runs WHERE id=? AND task_id=? AND lease_token=?",
    )
    .bind(run_id)
    .bind(task_id)
    .bind(lease)
    .fetch_optional(host.store.pool())
    .await
    .map_err(|error| error.to_string())?
    .ok_or("task run disappeared while checking its completion proposal")?;
    Ok(proposed.0.is_some() && proposed.1.is_some())
}

async fn record_turn(
    host: &AgentHost,
    run: &TaskRun,
    turn_id: &str,
    status: &str,
    effects_unknown: bool,
    processed: bool,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO agent_task_turns(run_id,turn_id,status,effects_unknown,processed) VALUES(?,?,?,?,?) ON CONFLICT(run_id,turn_id) DO UPDATE SET status=excluded.status,effects_unknown=MAX(agent_task_turns.effects_unknown,excluded.effects_unknown),processed=MAX(agent_task_turns.processed,excluded.processed)",
    )
    .bind(&run.id)
    .bind(turn_id)
    .bind(status)
    .bind(i64::from(effects_unknown))
    .bind(i64::from(processed))
    .execute(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn mark_turn_processed(host: &AgentHost, run_id: &str, turn_id: &str) -> Result<(), String> {
    sqlx::query("UPDATE agent_task_turns SET processed=1 WHERE run_id=? AND turn_id=?")
        .bind(run_id)
        .bind(turn_id)
        .execute(host.store.pool())
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn has_unknown_effects(turn: &Value) -> bool {
    turn["items"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            let kind = item["type"].as_str().unwrap_or("");
            let effectful = matches!(
                kind,
                "commandExecution" | "fileChange" | "mcpToolCall" | "webSearchCall"
            );
            effectful
                && (matches!(
                    item["status"].as_str(),
                    Some("inProgress" | "in_progress" | "running")
                ) || (item["status"].is_null()
                    && item["startedAtMs"].is_number()
                    && item["completedAtMs"].is_null()))
        })
    })
}

pub(crate) fn turn_has_client_message(turn: &Value, client_message_id: &str) -> bool {
    turn["items"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["type"].as_str() == Some("userMessage")
                && item["clientId"].as_str() == Some(client_message_id)
        })
    })
}

async fn persist_submission_turn(
    host: &AgentHost,
    run_id: &str,
    turn_id: &str,
) -> Result<(), String> {
    sqlx::query("UPDATE agent_task_submissions SET turn_id=? WHERE rowid=(SELECT rowid FROM agent_task_submissions WHERE run_id=? AND turn_id IS NULL ORDER BY created_at DESC LIMIT 1)")
        .bind(turn_id).bind(run_id).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
    Ok(())
}

pub(crate) async fn thread_turns(host: &AgentHost, thread_id: &str) -> Result<Vec<Value>, String> {
    let mut cursor: Option<String> = None;
    let mut turns = Vec::new();
    loop {
        let mut params = json!({
            "threadId":thread_id,
            "limit":100,
            "sortDirection":"desc",
            "itemsView":"full"
        });
        if let Some(cursor) = cursor.as_deref() {
            params["cursor"] = json!(cursor);
        }
        let page = host.rpc.call("thread/turns/list", params).await?;
        let data = page["data"]
            .as_array()
            .ok_or("runtime turn history page has no data")?;
        turns.extend(data.iter().cloned());
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(turns)
}

async fn block_after_runtime_error(
    host: &AgentHost,
    workers: &WorkerState,
    run: &TaskRun,
    error: &str,
) {
    let _control_guard = workers.task_control_guard(&run.task_id).await;
    let Ok(current) = host.store.get_run(&run.id).await else {
        return;
    };
    if !runtime_error_owner_is_current(run, &current) {
        return;
    }
    if host
        .store
        .block_run(
            &current.task_id,
            &current.id,
            &current.lease_token,
            &format!("Worker stopped safely: {error}"),
            true,
        )
        .await
        .is_ok()
    {
        workers_release(host, workers, &run.id, &current.lease_token).await;
        host.changed();
    }
}

fn runtime_error_owner_is_current(expected: &TaskRun, current: &TaskRun) -> bool {
    expected.id == current.id
        && expected.task_id == current.task_id
        && expected.lease_token == current.lease_token
        && current.state == "running"
}

fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

async fn handle_notification(
    host: &AgentHost,
    workers: &WorkerState,
    event: &Value,
) -> Result<(), String> {
    let method = event["method"].as_str().unwrap_or("");
    if method == "serverRequest/resolved" {
        if let Some(id) = request_id(&event["params"]) {
            workers.inner.approvals.lock().unwrap().remove(&id);
        }
        return Ok(());
    }
    if method == "item/tool/requestUserInput" {
        host.wait_for_question_input(event).await?;
        return Ok(());
    }
    if method == "item/completed" {
        if let Some(thread_id) = event["params"]["threadId"].as_str() {
            if let Ok(binding) = host.store.session_binding(thread_id).await {
                host.capture_item_source(&binding, event).await?;
            }
        }
        return Ok(());
    }
    if !matches!(method, "turn/started" | "turn/completed") {
        return Ok(());
    }
    let params = &event["params"];
    let Some(thread_id) = params["threadId"].as_str() else {
        return Ok(());
    };
    let binding = match host.store.session_binding(thread_id).await {
        Ok(binding) => binding,
        Err(_) => return Ok(()),
    };
    let Some(turn_id) = params["turn"]["id"].as_str() else {
        return Ok(());
    };
    if method == "turn/started" {
        if binding.kind == "task" {
            if let Some(run_id) = binding.task_run_id.as_deref() {
                let run = host.store.get_run(run_id).await?;
                if run.state == "running"
                    && run.thread_id.as_deref() == Some(thread_id)
                    && run.turn_id.is_none()
                {
                    let attached = host
                        .store
                        .attach_continuation(
                            &run.task_id,
                            &run.id,
                            thread_id,
                            turn_id,
                            &run.lease_token,
                        )
                        .await?;
                    record_turn(host, &attached, turn_id, "inProgress", false, false).await?;
                }
            }
        } else if binding.kind == "learning" {
            sqlx::query("UPDATE agent_learning_jobs SET turn_id=?,updated_at=? WHERE learning_thread_id=? AND state IN ('running','completion_proposed') AND (turn_id IS NULL OR turn_id=?)")
                .bind(turn_id).bind(epoch_seconds()).bind(thread_id).bind(turn_id)
                .execute(host.store.pool()).await.map_err(|error|error.to_string())?;
        }
        return Ok(());
    }

    if binding.learning_eligible && matches!(binding.kind.as_str(), "agent" | "baseline" | "task") {
        enqueue_learning(host, &binding, turn_id).await?;
    }

    if binding.kind == "task" {
        if let Some(run_id) = binding.task_run_id.as_deref() {
            let run = host.store.get_run(run_id).await?;
            if run.state != "running" || run.thread_id.as_deref() != Some(thread_id) {
                return Ok(());
            }
            let run = if run.turn_id.as_deref() == Some(turn_id) {
                run
            } else if run.turn_id.is_none() {
                host.store
                    .attach_continuation(
                        &run.task_id,
                        &run.id,
                        thread_id,
                        turn_id,
                        &run.lease_token,
                    )
                    .await?
            } else {
                return Ok(());
            };
            let task = host.store.get_task(&run.task_id).await?;
            let turn = event_turn_details(host, thread_id, turn_id, &params["turn"]).await?;
            settle_observed_turn(host, workers, &run, &task, &turn).await?;
        }
    } else if binding.kind == "learning" {
        finish_learning_turn(host, thread_id, turn_id, &params["turn"]).await?;
    }
    Ok(())
}

async fn event_turn_details(
    host: &AgentHost,
    thread_id: &str,
    turn_id: &str,
    event_turn: &Value,
) -> Result<Value, String> {
    let loaded = event_turn["itemsView"]
        .as_str()
        .is_some_and(|view| view == "full")
        || event_turn["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty());
    if loaded {
        return Ok(event_turn.clone());
    }
    let turns = thread_turns(host, thread_id).await?;
    turns
        .into_iter()
        .find(|turn| turn["id"].as_str() == Some(turn_id))
        .ok_or_else(|| "completed task turn is absent from runtime history".into())
}

async fn enqueue_learning(
    host: &AgentHost,
    binding: &SessionBinding,
    turn_id: &str,
) -> Result<(), String> {
    if enqueue_learning_jobs(host.store.pool(), binding, turn_id).await? > 0 {
        host.changed();
    }
    Ok(())
}

async fn enqueue_learning_jobs(
    pool: &SqlitePool,
    binding: &SessionBinding,
    turn_id: &str,
) -> Result<usize, String> {
    let rows: Vec<(String, i64, String, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT s.source_id,s.revision,s.thread_id,s.agent_id,s.project_id,s.content FROM host_learning_evidence e JOIN memory_sources s ON s.source_id=e.source_id WHERE e.thread_id=? AND e.processed=0 AND s.active=1 AND (s.thread_id=e.thread_id OR (? IS NOT NULL AND s.agent_id=?)) ORDER BY s.updated_at ASC,s.source_id",
    )
    .bind(&binding.thread_id)
    .bind(&binding.agent_id)
    .bind(&binding.agent_id)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    if rows.is_empty() {
        return Ok(0);
    }
    let available = rows
        .into_iter()
        .map(
            |(source_id, revision, thread_id, agent_id, project_id, content)| {
                let reference = format!("{source_id}@{revision}");
                LearningSource {
                    reference: reference.clone(),
                    wire: json!({
                        "sourceId":reference,
                        "revision":revision,
                        "threadId":thread_id,
                        "agentId":agent_id,
                        "projectId":project_id,
                        "content":content,
                        "active":true
                    }),
                }
            },
        )
        .collect::<Vec<_>>();
    let reserved_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,source_refs FROM agent_learning_jobs WHERE state IN ('pending','running','completion_proposed')",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let reserved: HashSet<String> = reserved_rows
        .into_iter()
        .flat_map(|(_, refs)| serde_json::from_str::<Vec<String>>(&refs).unwrap_or_default())
        .collect();
    let available = unreserved_sources(available, &reserved);
    if available.is_empty() {
        return Ok(0);
    }
    let mut inserted = 0;
    for (batch_index, batch) in source_batches(available)?.into_iter().enumerate() {
        let Some(first_ref) = batch.first() else {
            continue;
        };
        let refs = batch
            .iter()
            .map(|source| source.reference.as_str())
            .collect::<Vec<_>>();
        let source_refs = serde_json::to_string(&refs).map_err(|error| error.to_string())?;
        let batch_turn_id = format!("{turn_id}#batch:{}", first_ref.reference);
        let created_at = epoch_millis().saturating_add(batch_index as i64);
        sqlx::query("INSERT OR IGNORE INTO agent_learning_jobs(id,source_thread_id,source_turn_id,source_refs,agent_id,project_id,cwd,state,created_at,updated_at) VALUES(?,?,?,?,?,?,?,'pending',?,?)")
            .bind(uuid::Uuid::new_v4().to_string()).bind(&binding.thread_id).bind(batch_turn_id).bind(source_refs)
            .bind(&binding.agent_id).bind(&binding.project_id).bind(&binding.cwd)
            .bind(created_at).bind(epoch_seconds())
            .execute(pool).await.map_err(|error|error.to_string())?;
        inserted += 1;
    }
    Ok(inserted)
}

async fn start_pending_learning(host: &AgentHost, workers: &WorkerState) -> Result<(), String> {
    if *workers.inner.learning_retry_after.lock().unwrap() > epoch_seconds() {
        return Ok(());
    }
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM agent_learning_jobs WHERE state IN ('running','completion_proposed'))",
    )
    .fetch_one(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;
    if active {
        return Ok(());
    }
    let job: Option<(String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id,source_thread_id,agent_id,project_id,cwd FROM agent_learning_jobs WHERE state='pending' ORDER BY created_at,id LIMIT 1",
    )
    .fetch_optional(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;
    let Some((job_id, source_thread, agent_id, project_id, cwd)) = job else {
        return Ok(());
    };
    host.require_account().await?;
    let rate_limits = match host
        .rpc
        .call(
            "account/rateLimits/read",
            json!({"excludeResetCreditDetails":true}),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            defer_learning(
                host,
                workers,
                &job_id,
                &format!("Rate limit status is unavailable: {error}"),
            )
            .await?;
            return Ok(());
        }
    };
    if is_near_quota(&rate_limits) {
        defer_learning(
            host,
            workers,
            &job_id,
            "Learning is deferred because ChatGPT usage is at least 90% of a rate limit.",
        )
        .await?;
        return Ok(());
    }
    let model = if let Some(agent_id) = agent_id.as_deref() {
        host.store.read_profile(agent_id).await?.model
    } else {
        let source_model = host
            .rpc
            .call(
                "thread/read",
                json!({"threadId":source_thread,"includeTurns":false}),
            )
            .await
            .ok()
            .and_then(|thread| thread["thread"]["model"].as_str().map(str::to_owned));
        match source_model {
            Some(model) => model,
            None => match host
                .rpc
                .call("model/list", json!({"includeHidden":false}))
                .await
            {
                Ok(models) => match default_visible_model(&models) {
                    Some(model) => model,
                    None => {
                        defer_learning(
                            host,
                            workers,
                            &job_id,
                            "Learning is deferred because there is no available default ChatGPT model.",
                        )
                        .await?;
                        return Ok(());
                    }
                },
                Err(error) => {
                    defer_learning(
                        host,
                        workers,
                        &job_id,
                        &format!("Learning is deferred because model availability could not be checked: {error}"),
                    )
                    .await?;
                    return Ok(());
                }
            },
        }
    };
    let session = match host
        .open_session(
            agent_id.clone(),
            "learning",
            &model,
            None,
            None,
            true,
            None,
            None,
        )
        .await
    {
        Ok(session) => session,
        Err(error) => {
            sqlx::query("UPDATE agent_learning_jobs SET reason=?,updated_at=? WHERE id=? AND state='pending'")
                .bind(&error).bind(epoch_seconds()).bind(&job_id).execute(host.store.pool()).await.map_err(|e|e.to_string())?;
            return Err(error);
        }
    };
    let learning_thread = session["thread"]["id"]
        .as_str()
        .ok_or("learning thread start returned no thread ID")?;
    sqlx::query("UPDATE agent_learning_jobs SET state='running',learning_thread_id=?,agent_id=?,project_id=?,cwd=?,reason=NULL,updated_at=? WHERE id=? AND state='pending'")
        .bind(learning_thread).bind(&agent_id).bind(project_id).bind(cwd).bind(epoch_seconds()).bind(&job_id)
        .execute(host.store.pool()).await.map_err(|error|error.to_string())?;
    start_learning_turn(host, workers, learning_thread, false).await
}

async fn defer_learning(
    host: &AgentHost,
    workers: &WorkerState,
    job_id: &str,
    reason: &str,
) -> Result<(), String> {
    *workers.inner.learning_retry_after.lock().unwrap() = epoch_seconds() + 60;
    sqlx::query(
        "UPDATE agent_learning_jobs SET reason=?,updated_at=? WHERE id=? AND state='pending'",
    )
    .bind(reason)
    .bind(epoch_seconds())
    .bind(job_id)
    .execute(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;
    host.changed();
    Ok(())
}

async fn start_learning_turn(
    host: &AgentHost,
    _workers: &WorkerState,
    thread_id: &str,
    continuation: bool,
) -> Result<(), String> {
    if continuation {
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT continuation_count FROM agent_learning_jobs WHERE learning_thread_id=? AND state IN ('running','completion_proposed')",
        )
        .bind(thread_id)
        .fetch_optional(host.store.pool())
        .await
        .map_err(|error| error.to_string())?;
        let Some(next) = current.and_then(|count| continuation_nudge_count(count, false)) else {
            sqlx::query("UPDATE agent_learning_jobs SET state='blocked',reason='Learning did not complete after three turns without verified tool or file progress.',updated_at=? WHERE learning_thread_id=? AND state IN ('running','completion_proposed')")
                .bind(epoch_seconds()).bind(thread_id).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
            host.changed();
            return Ok(());
        };
        sqlx::query("UPDATE agent_learning_jobs SET continuation_count=?,updated_at=? WHERE learning_thread_id=? AND state IN ('running','completion_proposed')")
            .bind(next).bind(epoch_seconds()).bind(thread_id).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
    }
    let prompt = if continuation {
        "Continue the source review from this learning session. Preserve the exact supplied source references and finish by calling brr_learning_done. Do not repeat operations whose result is already in the conversation."
    } else {
        "Review only the source evidence attached to this learning session. Propose supported facts or demonstrated procedures with their exact sourceId@revision references, then call brr_learning_done."
    };
    let params = json!({
        "threadId":thread_id,
        "input":[{"type":"text","text":prompt,"text_elements":[]}]
    });
    let params = host.prepare_turn(thread_id, params).await?;
    let response = host.rpc.call("turn/start", params).await?;
    let turn_id = response["turn"]["id"]
        .as_str()
        .or_else(|| response["turnId"].as_str())
        .ok_or("runtime learning turn start returned no turn ID")?;
    sqlx::query("UPDATE agent_learning_jobs SET turn_id=?,completion_turn_id=NULL,state='running',updated_at=? WHERE learning_thread_id=? AND state IN ('running','completion_proposed')")
        .bind(turn_id).bind(epoch_seconds()).bind(thread_id).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
    Ok(())
}

async fn finish_learning_turn(
    host: &AgentHost,
    thread_id: &str,
    turn_id: &str,
    event_turn: &Value,
) -> Result<(), String> {
    let mut turn = event_turn.clone();
    if turn["itemsView"].as_str() != Some("full")
        && turn["items"].as_array().is_none_or(Vec::is_empty)
    {
        turn = event_turn_details(host, thread_id, turn_id, event_turn).await?;
    }
    let status = turn["status"].as_str().unwrap_or("unknown");
    if status == "inProgress" || status == "in_progress" {
        return Ok(());
    }
    let job: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT id,state,source_refs,source_thread_id,source_turn_id FROM agent_learning_jobs WHERE learning_thread_id=? AND (turn_id=? OR completion_turn_id=?)",
    )
    .bind(thread_id)
    .bind(turn_id)
    .bind(turn_id)
    .fetch_optional(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;
    let Some((job_id, state, source_refs, source_thread_id, source_turn_id)) = job else {
        return Ok(());
    };
    if status == "completed" && state == "completion_proposed" {
        let refs: Vec<String> =
            serde_json::from_str(&source_refs).map_err(|error| error.to_string())?;
        let mut tx = host
            .store
            .pool()
            .begin()
            .await
            .map_err(|error| error.to_string())?;
        for reference in refs {
            if let Some((source_id, _revision)) = reference.rsplit_once('@') {
                sqlx::query("UPDATE host_learning_evidence SET processed=1 WHERE thread_id=? AND source_id=?")
                    .bind(&source_thread_id)
                    .bind(source_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        sqlx::query("UPDATE agent_learning_jobs SET state='done',reason=NULL,updated_at=? WHERE id=? AND state='completion_proposed' AND completion_turn_id=?")
            .bind(epoch_seconds()).bind(&job_id).bind(turn_id).execute(&mut *tx).await.map_err(|error|error.to_string())?;
        tx.commit().await.map_err(|error| error.to_string())?;
        host.changed();
        if let Ok(binding) = host.store.session_binding(&source_thread_id).await {
            let original_turn = source_turn_id
                .split_once("#batch:")
                .map(|(turn, _)| turn)
                .unwrap_or(&source_turn_id);
            enqueue_learning(host, &binding, original_turn).await?;
        }
        return Ok(());
    }
    if status == "failed" && has_unknown_effects(&turn) {
        sqlx::query("UPDATE agent_learning_jobs SET state='blocked',reason='learning turn ended with an unresolved tool effect',updated_at=? WHERE id=?")
            .bind(epoch_seconds()).bind(&job_id).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
        return Ok(());
    }
    if has_verified_progress(&turn) {
        sqlx::query(
            "UPDATE agent_learning_jobs SET continuation_count=0 WHERE id=? AND state='running'",
        )
        .bind(&job_id)
        .execute(host.store.pool())
        .await
        .map_err(|error| error.to_string())?;
    }
    sqlx::query("UPDATE agent_learning_jobs SET state='running',turn_id=NULL,completion_turn_id=NULL,updated_at=? WHERE id=?")
        .bind(epoch_seconds()).bind(&job_id).execute(host.store.pool()).await.map_err(|error|error.to_string())?;
    start_learning_turn(host, &host.workers, thread_id, true).await?;
    Ok(())
}

async fn recover_learning(host: &AgentHost, workers: &WorkerState) -> Result<(), String> {
    if reset_ephemeral_learning_jobs(host.store.pool()).await? > 0 {
        host.changed();
    }
    let _ = workers;
    Ok(())
}

async fn reset_ephemeral_learning_jobs(pool: &SqlitePool) -> Result<u64, String> {
    let reset = sqlx::query("UPDATE agent_learning_jobs SET state='pending',learning_thread_id=NULL,turn_id=NULL,completion_turn_id=NULL,reason='Learning restarted in a new ephemeral session; source-backed proposals may be safely repeated.',updated_at=? WHERE state IN ('running','completion_proposed')")
        .bind(epoch_seconds())
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;
    Ok(reset.rows_affected())
}

fn may_settle_success(status: &str, proposal_present: bool) -> bool {
    status == "completed" && proposal_present
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuestionTurnAction {
    Continue,
    WaitForInput,
    BlockUnknown,
}

fn question_turn_action(
    has_pending_question: bool,
    status: &str,
    effects_unknown: bool,
) -> QuestionTurnAction {
    if !has_pending_question || matches!(status, "inProgress" | "in_progress") {
        return QuestionTurnAction::Continue;
    }
    if effects_unknown || !matches!(status, "completed" | "interrupted" | "failed") {
        return QuestionTurnAction::BlockUnknown;
    }
    QuestionTurnAction::WaitForInput
}

fn unreserved_sources(
    available: Vec<LearningSource>,
    reserved: &HashSet<String>,
) -> Vec<LearningSource> {
    let mut seen = HashSet::new();
    available
        .into_iter()
        .filter(|source| {
            !reserved.contains(&source.reference) && seen.insert(source.reference.clone())
        })
        .collect()
}

fn source_batches(sources: Vec<LearningSource>) -> Result<Vec<Vec<LearningSource>>, String> {
    let mut seen = HashSet::new();
    let sources = sources
        .into_iter()
        .filter(|source| seen.insert(source.reference.clone()))
        .collect::<Vec<_>>();
    let mut batches = Vec::new();
    let mut current = Vec::new();
    for source in sources {
        let mut candidate = current.clone();
        candidate.push(source.clone());
        let encoded_len = serde_json::to_vec(
            &candidate
                .iter()
                .map(|source| &source.wire)
                .collect::<Vec<_>>(),
        )
        .map_err(|error| error.to_string())?
        .len();
        if !current.is_empty()
            && (candidate.len() > MAX_LEARNING_SOURCES || encoded_len > MAX_LEARNING_PAYLOAD_BYTES)
        {
            batches.push(std::mem::take(&mut current));
        }
        current.push(source);
        let single_len = serde_json::to_vec(
            &current
                .iter()
                .map(|source| &source.wire)
                .collect::<Vec<_>>(),
        )
        .map_err(|error| error.to_string())?
        .len();
        if single_len > MAX_LEARNING_PAYLOAD_BYTES {
            return Err("a source payload exceeds the learning batch size limit".into());
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

fn continuation_nudge_count(current: i64, verified_progress: bool) -> Option<i64> {
    if verified_progress {
        Some(0)
    } else if current >= 3 {
        None
    } else {
        Some(current + 1)
    }
}

fn waiting_recovery_action(status: &str, effects_unknown: bool) -> WaitingRecoveryAction {
    if effects_unknown
        || !matches!(
            status,
            "inProgress" | "in_progress" | "interrupted" | "failed" | "completed"
        )
    {
        return WaitingRecoveryAction::BlockUnknown;
    }
    if matches!(status, "interrupted" | "failed" | "completed") {
        WaitingRecoveryAction::ResumeSameRun
    } else {
        WaitingRecoveryAction::AwaitFreshApproval
    }
}

fn has_verified_progress(turn: &Value) -> bool {
    turn["items"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            let kind = item["type"].as_str().unwrap_or("");
            let status = item["status"].as_str();
            match kind {
                "commandExecution" => {
                    status == Some("completed") && item["exitCode"].as_i64() == Some(0)
                }
                "fileChange" | "mcpToolCall" => status == Some("completed"),
                _ => false,
            }
        })
    })
}

fn is_near_quota(response: &Value) -> bool {
    response["rateLimits"]
        .as_object()
        .into_iter()
        .flat_map(|limits| limits.values())
        .filter_map(|limit| limit["usedPercent"].as_f64())
        .any(|used| used >= 90.0)
}

fn default_visible_model(response: &Value) -> Option<String> {
    let models = response["data"].as_array()?;
    let default = models
        .iter()
        .find(|model| model["isDefault"].as_bool() == Some(true))
        .or_else(|| models.first())?;
    default
        .get("id")
        .or_else(|| default.get("slug"))
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .map(str::to_owned)
}

fn request_id(request: &Value) -> Option<String> {
    request
        .get("id")
        .or_else(|| request.get("requestId"))
        .or_else(|| {
            request
                .get("params")
                .and_then(|params| params.get("requestId"))
        })
        .map(Value::to_string)
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn validate_run_binding(
    binding: &SessionBinding,
    params: &Value,
    run: &TaskRun,
) -> Result<(), String> {
    if binding.kind != "task"
        || run.thread_id.as_deref() != Some(binding.thread_id.as_str())
        || params["threadId"].as_str() != Some(binding.thread_id.as_str())
        || run.turn_id.as_deref() != params["turnId"].as_str()
        || run.state != "running"
    {
        return Err("task tool call does not match the active immutable run binding".into());
    }
    Ok(())
}

fn question_run_binding_matches(
    question: &QuestionRequest,
    task: &Task,
    run: &TaskRun,
    binding: &SessionBinding,
) -> bool {
    question.task_id.as_deref() == Some(task.id.as_str())
        && question.run_id.as_deref() == Some(run.id.as_str())
        && run.task_id == task.id
        && run.agent_id == task.agent_id
        && question
            .agent_id
            .as_deref()
            .is_none_or(|agent| agent == task.agent_id)
        && run.thread_id.as_deref() == Some(question.thread_id.as_str())
        && binding.kind == "task"
        && binding.thread_id == question.thread_id
        && binding.task_run_id.as_deref() == Some(run.id.as_str())
        && binding
            .agent_id
            .as_deref()
            .is_none_or(|agent| agent == run.agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approved_folderless_ticket_uses_private_scratch_with_project_scope() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_path = dir.path().join("runtime.py");
        std::fs::write(
            &runtime_path,
            r#"
import json,sys
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    m=json.loads(line)
    if 'method' not in m or 'id' not in m: continue
    method=m['method']
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    else: result={}
    send({'id':m['id'],'result':result})
"#,
        )
        .unwrap();
        let mut command = tokio::process::Command::new("python3");
        command.arg(&runtime_path).current_dir(dir.path());
        let engine = crate::bridge::Engine::connect(command, Arc::new(|_| {}))
            .await
            .unwrap();
        engine.agents.stop().await;
        let project = engine
            .agents
            .projects
            .create(crate::projects::ProjectInput {
                name: "Scratch ticket".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let draft = engine
            .agents
            .store
            .propose_profile(
                "scratch-ticket",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Scratch runner".into(),
                    description: "Runs scratch tickets".into(),
                    instructions: "Complete the approved ticket".into(),
                    soul: "Be careful".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        engine
            .agents
            .store
            .approve_profile(&draft.id, &draft.hash, "scratch-profile-review")
            .await
            .unwrap();
        let profile = engine
            .agents
            .store
            .apply_profile(&draft.id, &draft.hash, "scratch-profile-apply")
            .await
            .unwrap();
        let ticket = crate::tickets::request_stores(&engine.agents.store, &engine.agents.projects,
            "create", json!({"projectId":project.id,"title":"Scratch ticket","description":"Run privately",
                "acceptanceCriteria":["Keep project scope"],"agentId":profile.id,"operationId":"scratch-ticket-create"})
        ).await.unwrap();
        let proposal = crate::tickets::request_stores(&engine.agents.store, &engine.agents.projects,
            "proposeRun", json!({"ticketId":ticket["id"],"revision":ticket["revision"],"operationId":"scratch-ticket-propose"})
        ).await.unwrap();
        let task = engine
            .agents
            .store
            .approve_ticket_run(
                ticket["id"].as_str().unwrap(),
                proposal["hash"].as_str().unwrap(),
                brr_agent_core::TicketMutation {
                    operation_id: "scratch-ticket-approve".into(),
                    actor: brr_agent_core::TicketActor::human(),
                },
            )
            .await
            .unwrap();
        let approved = crate::tickets::approved_ticket_project(&engine.agents, &task)
            .await
            .unwrap()
            .unwrap();
        crate::tickets::validate_approved_ticket_project(&engine.agents, &task, &approved)
            .await
            .unwrap();
        assert_eq!(approved.reviewed.id, project.id);
        assert!(approved.selected_folder.is_none());
        assert!(approved.workspace_project.folders.is_empty());
        let workspace = engine
            .agents
            .worktrees
            .prepare_session(Some(&approved.workspace_project))
            .await
            .unwrap();
        assert!(std::path::Path::new(&workspace.cwd).is_dir());
        assert_eq!(workspace.roots, vec![workspace.cwd.clone()]);
        assert!(workspace.folders.is_empty());
        assert_eq!(
            workspace.scope_project_id.as_deref(),
            Some(project.id.as_str())
        );
        assert_eq!(workspace.project_revision, Some(project.revision));
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn queued_ticket_with_changed_project_is_blocked_before_any_runtime_thread() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_path = dir.path().join("runtime.py");
        std::fs::write(
            &runtime_path,
            r#"
import json,sys
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    m=json.loads(line)
    if 'method' not in m or 'id' not in m: continue
    method=m['method']
    with open('runtime-calls.txt','a') as f: f.write(method+'\n')
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='thread/start': result={'thread':{'id':'must-not-start','cwd':m.get('params',{}).get('cwd'),'turns':[]}}
    elif method=='turn/start':
        send({'id':m['id'],'error':{'code':-32000,'message':'fixture must not receive a ticket turn'}})
        continue
    else: result={}
    send({'id':m['id'],'result':result})
"#,
        )
        .unwrap();
        let mut command = tokio::process::Command::new("python3");
        command.arg(&runtime_path).current_dir(dir.path());
        let engine = crate::bridge::Engine::connect(command, Arc::new(|_| {}))
            .await
            .unwrap();
        engine.agents.stop().await;

        let first = dir.path().join("project-a");
        let second = dir.path().join("project-b");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let project = engine
            .agents
            .projects
            .create(crate::projects::ProjectInput {
                name: "A then B".into(),
                folders: vec![first.to_string_lossy().into_owned()],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let draft = engine
            .agents
            .store
            .propose_profile(
                "queued-ticket-test",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Queued ticket runner".into(),
                    description: "Runs ticket work".into(),
                    instructions: "Follow the reviewed ticket".into(),
                    soul: "Be careful".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        engine
            .agents
            .store
            .approve_profile(&draft.id, &draft.hash, "queued-ticket-profile-review")
            .await
            .unwrap();
        let profile = engine
            .agents
            .store
            .apply_profile(&draft.id, &draft.hash, "queued-ticket-profile-apply")
            .await
            .unwrap();
        let ticket = crate::tickets::request_stores(
            &engine.agents.store,
            &engine.agents.projects,
            "create",
            json!({
                "projectId": project.id,
                "title": "Wait for reviewed folder",
                "description": "Run in project A only",
                "acceptanceCriteria": ["Use the reviewed folder"],
                "agentId": profile.id,
                "operationId": "queued-ticket-create"
            }),
        )
        .await
        .unwrap();
        let proposal = crate::tickets::request_stores(
            &engine.agents.store,
            &engine.agents.projects,
            "proposeRun",
            json!({
                "ticketId": ticket["id"],
                "revision": ticket["revision"],
                "operationId": "queued-ticket-propose"
            }),
        )
        .await
        .unwrap();
        let task = engine
            .agents
            .store
            .approve_ticket_run(
                ticket["id"].as_str().unwrap(),
                proposal["hash"].as_str().unwrap(),
                brr_agent_core::TicketMutation {
                    operation_id: "queued-ticket-approve".into(),
                    actor: brr_agent_core::TicketActor::human(),
                },
            )
            .await
            .unwrap();

        let run = engine
            .agents
            .store
            .claim_ready("ticket-guard-test")
            .await
            .unwrap()
            .unwrap();
        let task_row = engine.agents.store.get_task(&task.id).await.unwrap();
        let workers = WorkerState::new();
        workers
            .reserve_task_workspace(&engine.agents, &run.id, &task_row, None)
            .await
            .unwrap();
        let mut changed_project = project;
        changed_project
            .folders
            .push(second.to_string_lossy().into_owned());
        engine.agents.projects.save(changed_project).await.unwrap();
        let roots = workers
            .inner
            .workspace_reservations
            .lock()
            .unwrap()
            .get(&run.id)
            .cloned()
            .unwrap_or_default();
        assert!(roots.contains(&first.canonicalize().unwrap().to_string_lossy().into_owned()));
        assert!(!roots.contains(
            &second
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ));

        let error = execute_claim(&engine.agents, &workers, &run)
            .await
            .unwrap_err();
        block_after_runtime_error(&engine.agents, &workers, &run, &error).await;
        assert!(error.contains("project configuration changed"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("runtime-calls.txt"))
                .unwrap_or_default()
                .lines()
                .filter(|method| *method == "thread/start")
                .count(),
            0
        );
        assert_eq!(
            engine.agents.store.get_task(&task.id).await.unwrap().status,
            "blocked"
        );
        engine.shutdown().await;
    }

    #[test]
    fn task_continuations_preserve_reviewed_workspace_permissions_at_saved_cwd() {
        assert_eq!(
            task_sandbox_override(
                &WorkspacePermission::ReadOnly,
                &["/worktrees/task-1".into()]
            ),
            json!({"type":"readOnly","networkAccess":false})
        );
        assert_eq!(
            task_sandbox_override(
                &WorkspacePermission::WorkspaceWrite,
                &["/worktrees/task-1".into(), "/shared/docs".into()]
            ),
            json!({"type":"workspaceWrite","writableRoots":["/worktrees/task-1","/shared/docs"],"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false})
        );
    }

    #[test]
    fn root_overlap_matches_only_equal_or_ancestor_paths() {
        assert!(roots_overlap_any(
            &["/workspace/project".into()],
            &["/workspace/project/docs".into()]
        ));
        assert!(!roots_overlap_any(
            &["/workspace/project-a".into()],
            &["/workspace/project-b".into()]
        ));
    }

    #[test]
    fn canonical_workspace_roots_remove_nested_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        std::fs::create_dir_all(&child).unwrap();
        let roots = canonical_workspace_roots(vec![
            temp.path().display().to_string(),
            child.display().to_string(),
        ])
        .unwrap();
        assert_eq!(
            roots,
            vec![temp.path().canonicalize().unwrap().display().to_string()]
        );
    }

    #[tokio::test]
    async fn overlapping_shared_folder_reservations_wait_until_prior_run_releases() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("project");
        let child = parent.join("docs");
        std::fs::create_dir_all(&child).unwrap();
        let state = WorkerState::new();
        state
            .reserve_workspace_roots("parent-run", vec![parent.display().to_string()])
            .unwrap();
        state
            .acquire_reserved_roots("parent-run", vec![parent.display().to_string()])
            .await
            .unwrap();
        let waiter = {
            let state = state.clone();
            let child = child.display().to_string();
            tokio::spawn(async move {
                state
                    .reserve_workspace_roots("child-run", vec![child.clone()])
                    .unwrap();
                state
                    .acquire_reserved_roots("child-run", vec![child])
                    .await
                    .unwrap();
            })
        };
        assert!(tokio::time::timeout(Duration::from_millis(40), async {
            while !waiter.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err());
        state.release_workspace("parent-run");
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        state.release_workspace("child-run");
    }

    #[tokio::test]
    async fn sibling_shared_folder_reservations_acquire_concurrently() {
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("project-a");
        let right = temp.path().join("project-b");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        let state = WorkerState::new();
        state
            .reserve_workspace_roots("left-run", vec![left.display().to_string()])
            .unwrap();
        state
            .acquire_reserved_roots("left-run", vec![left.display().to_string()])
            .await
            .unwrap();
        state
            .reserve_workspace_roots("right-run", vec![right.display().to_string()])
            .unwrap();
        tokio::time::timeout(
            Duration::from_millis(100),
            state.acquire_reserved_roots("right-run", vec![right.display().to_string()]),
        )
        .await
        .expect("unrelated sibling roots should not block each other")
        .unwrap();
        state.release_workspace("left-run");
        state.release_workspace("right-run");
    }

    use brr_agent_core::{AgentStore, ProfileCandidate};

    fn learning_source(reference: &str, content: &str) -> LearningSource {
        let (_, revision) = reference.rsplit_once('@').unwrap();
        let revision = revision.parse::<i64>().unwrap();
        LearningSource {
            reference: reference.into(),
            wire: json!({
                "sourceId":reference,
                "revision":revision,
                "threadId":"thread-test",
                "agentId":"agent-test",
                "projectId":null,
                "content":content,
                "active":true
            }),
        }
    }

    fn task_run(task_id: &str, run_id: &str, lease_token: &str, state: &str) -> TaskRun {
        TaskRun {
            id: run_id.into(),
            task_id: task_id.into(),
            agent_id: "agent-test".into(),
            worker_id: WORKER_ID.into(),
            lease_token: lease_token.into(),
            thread_id: None,
            turn_id: None,
            state: state.into(),
            created_at: 0,
            settled_at: None,
        }
    }

    #[test]
    fn earlier_async_question_can_activate_the_same_live_task_run() {
        let mut run = task_run("task-1", "run-1", "lease", "running");
        run.thread_id = Some("thread-1".into());
        run.turn_id = Some("answer-turn".into());
        let task = Task {
            id: "task-1".into(),
            agent_id: "agent-test".into(),
            title: "task".into(),
            instructions: "instructions".into(),
            acceptance_criteria: vec![],
            context: None,
            cwd: None,
            project_id: None,
            model: None,
            effort: None,
            workspace_permission: None,
            dependencies: vec![],
            status: "running".into(),
            reason: None,
            run_id: Some("run-1".into()),
            thread_id: Some("thread-1".into()),
            result: None,
            evidence: None,
            created_at: 0,
            updated_at: 0,
        };
        let binding = SessionBinding {
            thread_id: "thread-1".into(),
            agent_id: Some("agent-test".into()),
            kind: "task".into(),
            project_id: None,
            cwd: None,
            task_run_id: Some("run-1".into()),
            learning_eligible: false,
        };
        let question = QuestionRequest {
            id: "question-1".into(),
            thread_id: "thread-1".into(),
            turn_id: "source-turn".into(),
            item_id: "item-1".into(),
            agent_id: Some("agent-test".into()),
            task_id: Some("task-1".into()),
            run_id: Some("run-1".into()),
            kind: "async".into(),
            questions: vec![],
            runtime_generation: None,
            runtime_request_id: None,
            draft: Default::default(),
            answers: None,
            resolution: "answered".into(),
            delivery: "queued".into(),
            reason: None,
            client_message_id: Some("message-1".into()),
            created_at: 0,
            updated_at: 0,
        };
        assert!(question_run_binding_matches(
            &question, &task, &run, &binding
        ));
    }

    fn learning_batch_bytes(batch: &[LearningSource]) -> usize {
        serde_json::to_vec(&batch.iter().map(|source| &source.wire).collect::<Vec<_>>())
            .unwrap()
            .len()
    }

    #[test]
    fn task_completion_requires_a_proposal_and_authoritative_success() {
        assert!(!may_settle_success("completed", false));
        assert!(!may_settle_success("interrupted", true));
        assert!(!may_settle_success("failed", true));
        assert!(may_settle_success("completed", true));
    }

    #[test]
    fn pending_questions_park_safe_ended_turns_but_unknown_effects_still_block() {
        assert_eq!(
            question_turn_action(true, "completed", false),
            QuestionTurnAction::WaitForInput
        );
        assert_eq!(
            question_turn_action(true, "interrupted", false),
            QuestionTurnAction::WaitForInput
        );
        assert_eq!(
            question_turn_action(true, "failed", true),
            QuestionTurnAction::BlockUnknown
        );
        assert_eq!(
            question_turn_action(true, "inProgress", false),
            QuestionTurnAction::Continue
        );
        assert_eq!(
            question_turn_action(false, "completed", false),
            QuestionTurnAction::Continue
        );
    }

    #[test]
    fn approval_ids_match_json_rpc_key_representation() {
        assert_eq!(request_id(&json!({"id":9})).as_deref(), Some("9"));
        assert_eq!(
            request_id(&json!({"params":{"requestId":"approval-1"}})).as_deref(),
            Some("\"approval-1\"")
        );
    }

    #[test]
    fn learning_queue_excludes_sources_already_in_an_active_pass() {
        let available = vec![
            learning_source("one@1", "content"),
            learning_source("two@3", "content"),
        ];
        let reserved = HashSet::from(["one@1".to_owned()]);
        assert_eq!(
            unreserved_sources(available, &reserved)
                .iter()
                .map(|source| source.reference.as_str())
                .collect::<Vec<_>>(),
            vec!["two@3"]
        );
    }

    #[test]
    fn learning_batches_cover_all_sources_with_bounded_delivery_payloads() {
        let sources = (0..87)
            .map(|index| learning_source(&format!("thread:source-{index:03}@1"), "small"))
            .collect::<Vec<_>>();
        let batches = source_batches(sources.clone()).unwrap();
        let flattened = batches
            .iter()
            .flatten()
            .map(|source| source.reference.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            flattened,
            sources
                .iter()
                .map(|source| source.reference.clone())
                .collect::<Vec<_>>()
        );
        assert!(batches.len() > 2);
        assert!(batches.iter().all(|batch| {
            batch.len() <= MAX_LEARNING_SOURCES
                && learning_batch_bytes(batch) <= MAX_LEARNING_PAYLOAD_BYTES
        }));
    }

    #[test]
    fn learning_batch_limit_counts_source_bodies_not_only_reference_ids() {
        let sources = (0..40)
            .map(|index| {
                learning_source(
                    &format!("source-{index:02}@1"),
                    &format!("source evidence {index}: {}", "x".repeat(1_000)),
                )
            })
            .collect::<Vec<_>>();
        let batches = source_batches(sources.clone()).unwrap();

        assert!(batches.len() >= 3, "forty large bodies must be split");
        assert!(batches.iter().all(|batch| {
            batch.len() <= MAX_LEARNING_SOURCES
                && learning_batch_bytes(batch) <= MAX_LEARNING_PAYLOAD_BYTES
        }));
        assert_eq!(
            batches
                .iter()
                .flatten()
                .map(|source| source.reference.clone())
                .collect::<Vec<_>>(),
            sources
                .iter()
                .map(|source| source.reference.clone())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn learning_job_batches_persist_every_oldest_unprocessed_source() {
        let directory = tempfile::tempdir().unwrap();
        let store = brr_agent_core::AgentStore::open(directory.path())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE host_learning_evidence(source_id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,processed INTEGER NOT NULL DEFAULT 0,source_role TEXT NOT NULL DEFAULT 'user')")
            .execute(store.pool()).await.unwrap();
        sqlx::query("CREATE TABLE agent_learning_jobs(id TEXT PRIMARY KEY,source_thread_id TEXT NOT NULL,source_turn_id TEXT NOT NULL,source_refs TEXT NOT NULL,agent_id TEXT,project_id TEXT,cwd TEXT,state TEXT NOT NULL,learning_thread_id TEXT,reason TEXT,turn_id TEXT,completion_turn_id TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(source_thread_id,source_turn_id))")
            .execute(store.pool()).await.unwrap();
        for index in 0..87 {
            let source_id = format!("thread-1:source-{index:03}");
            sqlx::query("INSERT INTO memory_sources(source_id,revision,thread_id,agent_id,project_id,content,active,updated_at) VALUES(?,1,'thread-1','agent-1',NULL,'evidence',1,?)")
                .bind(&source_id).bind(index as i64).execute(store.pool()).await.unwrap();
            sqlx::query(
                "INSERT INTO host_learning_evidence(source_id,thread_id) VALUES(?,'thread-1')",
            )
            .bind(source_id)
            .execute(store.pool())
            .await
            .unwrap();
        }
        let binding = SessionBinding {
            thread_id: "thread-1".into(),
            agent_id: Some("agent-1".into()),
            kind: "task".into(),
            project_id: None,
            cwd: None,
            task_run_id: Some("run-1".into()),
            learning_eligible: true,
        };

        assert_eq!(
            enqueue_learning_jobs(store.pool(), &binding, "turn-1")
                .await
                .unwrap(),
            3
        );
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT source_refs FROM agent_learning_jobs ORDER BY created_at,id",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        let batches = rows
            .iter()
            .map(|row| serde_json::from_str::<Vec<String>>(row).unwrap())
            .collect::<Vec<_>>();
        let delivered = batches.iter().flatten().cloned().collect::<Vec<_>>();
        let expected = (0..87)
            .map(|index| format!("thread-1:source-{index:03}@1"))
            .collect::<Vec<_>>();
        assert_eq!(delivered, expected);
        assert!(batches
            .iter()
            .all(|batch| { batch.len() <= MAX_LEARNING_SOURCES }));
        let still_unprocessed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM host_learning_evidence WHERE thread_id='thread-1' AND processed=0",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(still_unprocessed, 87);
    }

    #[tokio::test]
    async fn durable_large_source_batches_bound_exact_model_payload_and_keep_all_refs() {
        let directory = tempfile::tempdir().unwrap();
        let store = brr_agent_core::AgentStore::open(directory.path())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE host_learning_evidence(source_id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,processed INTEGER NOT NULL DEFAULT 0,source_role TEXT NOT NULL DEFAULT 'user')")
            .execute(store.pool()).await.unwrap();
        sqlx::query("CREATE TABLE agent_learning_jobs(id TEXT PRIMARY KEY,source_thread_id TEXT NOT NULL,source_turn_id TEXT NOT NULL,source_refs TEXT NOT NULL,agent_id TEXT,project_id TEXT,cwd TEXT,state TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(source_thread_id,source_turn_id))")
            .execute(store.pool()).await.unwrap();
        for index in 0..40 {
            let source_id = format!("large-source-{index:02}");
            let content = format!("verified evidence {index}: {}", "x".repeat(1_000));
            sqlx::query("INSERT INTO memory_sources(source_id,revision,thread_id,agent_id,project_id,content,active,updated_at) VALUES(?,1,'thread-large','agent-large',NULL,?,1,?)")
                .bind(&source_id).bind(content).bind(index as i64).execute(store.pool()).await.unwrap();
            sqlx::query(
                "INSERT INTO host_learning_evidence(source_id,thread_id) VALUES(?,'thread-large')",
            )
            .bind(source_id)
            .execute(store.pool())
            .await
            .unwrap();
        }
        let binding = SessionBinding {
            thread_id: "thread-large".into(),
            agent_id: Some("agent-large".into()),
            kind: "task".into(),
            project_id: None,
            cwd: None,
            task_run_id: Some("run-large".into()),
            learning_eligible: true,
        };

        let count = enqueue_learning_jobs(store.pool(), &binding, "turn-large")
            .await
            .unwrap();
        assert!(count >= 3);
        let refs: Vec<String> = sqlx::query_scalar(
            "SELECT source_refs FROM agent_learning_jobs ORDER BY created_at,id",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        let batches = refs
            .iter()
            .map(|refs| serde_json::from_str::<Vec<String>>(refs).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            batches.iter().flatten().cloned().collect::<Vec<_>>(),
            (0..40)
                .map(|index| format!("large-source-{index:02}@1"))
                .collect::<Vec<_>>()
        );
        for batch in batches {
            let mut wire = Vec::new();
            for reference in &batch {
                let (id, revision) = reference.rsplit_once('@').unwrap();
                let revision = revision.parse::<i64>().unwrap();
                let source: (String, i64, String, Option<String>, Option<String>, String) =
                    sqlx::query_as("SELECT source_id,revision,thread_id,agent_id,project_id,content FROM memory_sources WHERE source_id=? AND revision=? AND active=1")
                        .bind(id).bind(revision).fetch_one(store.pool()).await.unwrap();
                wire.push(json!({
                    "sourceId":format!("{}@{}",source.0,source.1),
                    "revision":source.1,
                    "threadId":source.2,
                    "agentId":source.3,
                    "projectId":source.4,
                    "content":source.5,
                    "active":true
                }));
            }
            assert!(wire.len() <= MAX_LEARNING_SOURCES);
            assert!(serde_json::to_vec(&wire).unwrap().len() <= MAX_LEARNING_PAYLOAD_BYTES);
        }
    }

    #[tokio::test]
    async fn restart_requeues_ephemeral_learning_with_same_sources_and_nudge_budget() {
        let directory = tempfile::tempdir().unwrap();
        let store = brr_agent_core::AgentStore::open(directory.path())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE agent_learning_jobs(id TEXT PRIMARY KEY,source_thread_id TEXT NOT NULL,source_turn_id TEXT NOT NULL,source_refs TEXT NOT NULL,agent_id TEXT,project_id TEXT,cwd TEXT,state TEXT NOT NULL,learning_thread_id TEXT,reason TEXT,turn_id TEXT,completion_turn_id TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,continuation_count INTEGER NOT NULL DEFAULT 0,UNIQUE(source_thread_id,source_turn_id))")
            .execute(store.pool()).await.unwrap();
        let refs = vec![
            "thread:source-1@2".to_owned(),
            "thread:source-2@4".to_owned(),
        ];
        let encoded = serde_json::to_string(&refs).unwrap();
        sqlx::query("INSERT INTO agent_learning_jobs(id,source_thread_id,source_turn_id,source_refs,state,learning_thread_id,turn_id,completion_turn_id,created_at,updated_at,continuation_count) VALUES('job','source-thread','turn#batch:source-1',?,'completion_proposed','ephemeral-thread','turn-1','turn-1',1,1,2)")
            .bind(&encoded).execute(store.pool()).await.unwrap();

        assert_eq!(
            reset_ephemeral_learning_jobs(store.pool()).await.unwrap(),
            1
        );
        let recovered: (String, Option<String>, Option<String>, Option<String>, String, i64) = sqlx::query_as(
            "SELECT state,learning_thread_id,turn_id,completion_turn_id,source_refs,continuation_count FROM agent_learning_jobs WHERE id='job'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(recovered.0, "pending");
        assert_eq!(recovered.1, None);
        assert_eq!(recovered.2, None);
        assert_eq!(recovered.3, None);
        assert_eq!(recovered.4, encoded);
        assert_eq!(recovered.5, 2);
    }

    #[test]
    fn learning_continuations_stop_after_three_no_progress_turns_and_reset_on_progress() {
        assert_eq!(continuation_nudge_count(0, false), Some(1));
        assert_eq!(continuation_nudge_count(2, false), Some(3));
        assert_eq!(continuation_nudge_count(3, false), None);
        assert_eq!(continuation_nudge_count(3, true), Some(0));
    }

    #[test]
    fn waiting_approval_recovery_requires_interrupted_safe_history() {
        assert_eq!(
            waiting_recovery_action("inProgress", false),
            WaitingRecoveryAction::AwaitFreshApproval
        );
        assert_eq!(
            waiting_recovery_action("interrupted", false),
            WaitingRecoveryAction::ResumeSameRun
        );
        assert_eq!(
            waiting_recovery_action("failed", true),
            WaitingRecoveryAction::BlockUnknown
        );
    }

    #[test]
    fn delayed_runtime_error_cannot_block_or_release_a_resumed_run() {
        let old_worker = task_run("task-1", "run-1", "old-fence", "running");
        let resumed_owner = task_run("task-1", "run-1", "new-fence", "running");
        assert!(!runtime_error_owner_is_current(&old_worker, &resumed_owner));
        assert!(runtime_error_owner_is_current(
            &resumed_owner,
            &resumed_owner
        ));
    }

    #[tokio::test]
    async fn pause_resume_reclaim_keeps_workspace_reserved_from_delayed_old_worker_error() {
        use brr_agent_core::{AgentStore, TaskAction, TaskInput};

        let directory = tempfile::tempdir().unwrap();
        let store = AgentStore::open(directory.path()).await.unwrap();
        let profile = json!({
            "name":"worker",
            "description":"test",
            "instructions":"test",
            "soul":"test",
            "model":"gpt-5",
            "effort":"medium"
        });
        sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-test',1,?,0,0)")
            .bind(profile.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        let task = store
            .create_task(TaskInput {
                agent_id: "agent-test".into(),
                title: "hold workspace".into(),
                instructions: "hold workspace".into(),
                acceptance_criteria: vec!["evidence".into()],
                context: None,
                cwd: Some(directory.path().to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                effort: None,
                workspace_permission: None,
                dependencies: vec![],
            })
            .await
            .unwrap();
        let workers = WorkerState::new();
        let old = store.claim_ready(WORKER_ID).await.unwrap().unwrap();
        workers.acquire_workspace(&old.id, &task).await.unwrap();

        store
            .control_task(&task.id, TaskAction::Pause)
            .await
            .unwrap();
        store
            .control_task(&task.id, TaskAction::Resume)
            .await
            .unwrap();
        let resumed = store.claim_ready(WORKER_ID).await.unwrap().unwrap();
        assert_eq!(resumed.id, old.id);
        assert_ne!(resumed.lease_token, old.lease_token);

        let current = store.get_run(&old.id).await.unwrap();
        assert!(current.state == "running");
        assert!(!runtime_error_owner_is_current(&old, &current));
        assert_eq!(
            workers.excluded_workspaces(),
            vec![std::fs::canonicalize(task.cwd.as_ref().unwrap())
                .unwrap()
                .to_string_lossy()
                .into_owned()]
        );
    }

    #[test]
    fn learning_observes_quota_and_uses_an_available_default_model() {
        assert!(is_near_quota(
            &json!({"rateLimits":{"primary":{"usedPercent":90}}})
        ));
        assert!(!is_near_quota(
            &json!({"rateLimits":{"primary":{"usedPercent":89}}})
        ));
        assert_eq!(
            default_visible_model(
                &json!({"data":[{"id":"first"},{"id":"default","isDefault":true}]})
            ),
            Some("default".into())
        );
        assert_eq!(
            default_visible_model(&json!({"data":[{"slug":"fallback"}]})),
            Some("fallback".into())
        );
    }

    #[tokio::test]
    async fn approval_wait_retains_workspace_ownership_and_resume_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let state = WorkerState::new();
        let task = Task {
            id: "task".into(),
            agent_id: "agent".into(),
            title: "write file".into(),
            instructions: "write file".into(),
            acceptance_criteria: vec!["file exists".into()],
            context: None,
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            effort: None,
            workspace_permission: None,
            dependencies: Vec::new(),
            status: "running".into(),
            reason: None,
            run_id: Some("run-1".into()),
            thread_id: Some("thread-1".into()),
            result: None,
            evidence: None,
            created_at: 0,
            updated_at: 0,
        };
        state.acquire_workspace("run-1", &task).await.unwrap();
        let workspace = std::fs::canonicalize(task.cwd.as_ref().unwrap())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(state.excluded_workspaces(), vec![workspace.clone()]);

        // Admission reserves the next writer before dispatch. Do not depend on a
        // blocking Git-discovery thread being scheduled within a 30 ms timeout.
        state.reserve_workspace("run-2", &task).await.unwrap();

        let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
        let second_state = state.clone();
        let second_task = task.clone();
        let second = tokio::spawn(async move {
            second_state
                .acquire_workspace("run-2", &second_task)
                .await
                .unwrap();
            let _ = acquired_tx.send(());
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut acquired_rx)
                .await
                .is_err()
        );

        tokio::time::timeout(
            Duration::from_millis(30),
            state.acquire_workspace("run-1", &task),
        )
        .await
        .expect("resuming the same task should reuse its held guard")
        .unwrap();
        state.release_workspace("run-1");
        assert_eq!(state.excluded_workspaces(), vec![workspace]);
        tokio::time::timeout(Duration::from_secs(1), acquired_rx)
            .await
            .expect("next task should acquire the cwd after settlement")
            .unwrap();
        state.release_workspace("run-2");
        assert!(state.excluded_workspaces().is_empty());
        second.await.unwrap();
    }

    #[tokio::test]
    async fn ended_async_question_can_be_paused_and_resumed_without_interrupt_or_requeue() {
        let dir = tempfile::tempdir().unwrap();
        let store = AgentStore::open(dir.path().join("agents")).await.unwrap();
        store
            .update_task_settings(brr_agent_core::TaskSettings {
                max_background: 5,
                max_per_agent: 5,
            })
            .await
            .unwrap();
        let draft = store
            .propose_profile(
                "question-control-test",
                None,
                ProfileCandidate {
                    name: "Question worker".into(),
                    description: "Worker for question control regression".into(),
                    instructions: "Follow the task".into(),
                    soul: "Be precise".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, "question-control-review")
            .await
            .unwrap();
        let profile = store
            .apply_profile(&draft.id, &draft.hash, "question-control-apply")
            .await
            .unwrap();

        let runtime = dir.path().join("runtime.py");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            &runtime,
            r#"
import json,sys,os
history_path='runtime-history.json'
turns=json.load(open(history_path)) if os.path.exists(history_path) else {}
thread_count=len(turns)
def send(value): print(json.dumps(value),flush=True)
for line in sys.stdin:
    message=json.loads(line)
    if 'method' not in message or 'id' not in message: continue
    method=message['method']; p=message.get('params',{}); result={}
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='account/rateLimits/read': result={'rateLimits':{'primary':{'usedPercent':0}}}
    elif method=='thread/start':
        thread_count+=1
        thread='question-worker-thread-'+str(thread_count)
        turns[thread]=[]
        json.dump(turns,open(history_path,'w'))
        result={'thread':{'id':thread,'cwd':p.get('cwd'),'turns':[]}}
    elif method in ('thread/resume','thread/read'):
        result={'thread':{'id':p['threadId'],'turns':turns.get(p['threadId'],[])}}
    elif method=='thread/turns/list':
        result={'data':list(reversed(turns.get(p['threadId'],[]))),'nextCursor':None}
    elif method=='turn/start':
        thread=p['threadId']; turn_id='source-turn-'+str(len(turns.get(p['threadId'],[]))+1)
        turn={'id':turn_id,'status':'inProgress','items':[]}
        turns[thread].append(turn)
        send({'id':message['id'],'result':{'turn':{'id':turn_id}}})
        send({'method':'turn/started','params':{'threadId':thread,'turn':turn}})
        answer_followup=any(x.get('text','').startswith('User response to question request') for x in p.get('input',[]) if isinstance(x,dict))
        if not answer_followup:
            item={'id':'async-question-'+turn_id,'type':'agentMessage','delivery':'async','questions':[{'title':'Choose a destination','options':['Workspace']}]}
            turn['items'].append(item)
            send({'method':'item/completed','params':{'threadId':thread,'turnId':turn_id,'item':item}})
        turn['status']='completed'
        json.dump(turns,open(history_path,'w'))
        send({'method':'turn/completed','params':{'threadId':thread,'turn':turn}})
        continue
    elif method=='turn/interrupt':
        send({'id':message['id'],'error':{'code':-32602,'message':'no active turn to interrupt'}})
        continue
    send({'id':message['id'],'result':result})
"#,
        )
        .unwrap();
        let mut command = tokio::process::Command::new("python3");
        command.arg(&runtime).current_dir(dir.path());
        let events = Arc::new(Mutex::new(Vec::<Value>::new()));
        let event_log = events.clone();
        let engine = crate::bridge::Engine::connect(
            command,
            Arc::new(move |event| event_log.lock().unwrap().push(event)),
        )
        .await
        .unwrap();
        let task = engine
            .agents_request(
                "tasks.create",
                json!({"agentId":profile.id,"title":"Wait for answer","instructions":"Ask the user for a destination.","acceptanceCriteria":["Use the selected destination."],"cwd":workspace}),
            )
            .await
            .unwrap();
        let task_id = task["id"].as_str().unwrap();
        let wait_for_question = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let data = engine
                    .agents_request("tasks.list", json!({}))
                    .await
                    .unwrap();
                if data["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|task| task["id"] == task_id && task["status"] == "waiting_input")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if wait_for_question.is_err() {
            panic!(
                "completed question turn should park its task; tasks: {}; events: {:?}",
                engine
                    .agents_request("tasks.list", json!({}))
                    .await
                    .unwrap(),
                *events.lock().unwrap()
            );
        }
        let listed = engine
            .agents_request("tasks.list", json!({}))
            .await
            .unwrap();
        let task_thread = listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|listed_task| listed_task["id"] == task_id)
            .unwrap()["threadId"]
            .clone();

        engine
            .agents_request("tasks.control", json!({"taskId":task_id,"action":"pause"}))
            .await
            .unwrap();
        let resumed = engine
            .agents_request("tasks.control", json!({"taskId":task_id,"action":"resume"}))
            .await
            .unwrap();
        assert_eq!(resumed["status"], "waiting_input");
        let questions = engine
            .agents_request(
                "questions.list",
                json!({"threadId":task_thread,"pendingOnly":true}),
            )
            .await
            .unwrap();
        assert_eq!(questions["data"].as_array().unwrap().len(), 1);

        engine
            .agents_request("tasks.control", json!({"taskId":task_id,"action":"cancel"}))
            .await
            .unwrap();
        assert!(engine
            .agents_request(
                "questions.list",
                json!({"threadId":task_thread,"pendingOnly":true}),
            )
            .await
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .is_empty());

        let next = engine
            .agents_request(
                "tasks.create",
                json!({"agentId":profile.id,"title":"Reuse workspace","instructions":"Ask the user for a destination.","acceptanceCriteria":["Use the selected destination."],"cwd":workspace}),
            )
            .await
            .unwrap();
        let next_id = next["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let data = engine
                    .agents_request("tasks.list", json!({}))
                    .await
                    .unwrap();
                if data["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|task| task["id"] == next_id && task["status"] == "waiting_input")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("cancelling a question task releases its non-Git workspace");

        let tasks = engine
            .agents_request("tasks.list", json!({}))
            .await
            .unwrap();
        let next_task = tasks["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == next_id)
            .unwrap();
        let next_thread = next_task["threadId"].as_str().unwrap().to_owned();
        let next_run_id = next_task["runId"].as_str().unwrap().to_owned();
        let unanswered = engine
            .agents_request(
                "questions.list",
                json!({"threadId":next_thread,"pendingOnly":true}),
            )
            .await
            .unwrap();
        let question_id = unanswered["data"][0]["id"].as_str().unwrap();
        engine.shutdown().await;
        let durable_store = AgentStore::open(dir.path().join("agents")).await.unwrap();
        durable_store
            .resolve_question(
                question_id,
                Some(std::collections::BTreeMap::from([(
                    "question_1".to_owned(),
                    vec!["Workspace".to_owned()],
                )])),
            )
            .await
            .unwrap();
        let queued_run = durable_store.get_run(&next_run_id).await.unwrap();
        durable_store
            .resume_run(
                next_id,
                &queued_run.id,
                &queued_run.lease_token,
                "restart-recovery-test",
            )
            .await
            .unwrap();
        let mut restarted_command = tokio::process::Command::new("python3");
        restarted_command.arg(runtime).current_dir(dir.path());
        let restarted = crate::bridge::Engine::connect(restarted_command, Arc::new(|_| {}))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let all_questions = restarted
                    .agents_request(
                        "questions.list",
                        json!({"threadId":next_thread,"pendingOnly":false}),
                    )
                    .await
                    .unwrap();
                if all_questions["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|question| {
                        question["id"] == question_id && question["delivery"] == "delivered"
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("startup recovery and queued answer activation must complete without deadlock");

        let archive_workspace = dir.path().join("archive-workspace");
        std::fs::create_dir_all(&archive_workspace).unwrap();
        let archived_task = restarted
            .agents_request(
                "tasks.create",
                json!({"agentId":profile.id,"title":"Archive waiting question","instructions":"Ask the user for a destination.","acceptanceCriteria":["Use the selected destination."],"cwd":archive_workspace}),
            )
            .await
            .unwrap();
        let archived_id = archived_task["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let tasks = restarted
                    .agents_request("tasks.list", json!({}))
                    .await
                    .unwrap();
                if tasks["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|task| task["id"] == archived_id && task["status"] == "waiting_input")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("archive test task should reach its user-input wait");
        let tasks = restarted
            .agents_request("tasks.list", json!({}))
            .await
            .unwrap();
        let archive_thread = tasks["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == archived_id)
            .unwrap()["threadId"]
            .as_str()
            .unwrap()
            .to_owned();
        restarted
            .request("thread/archive", json!({"threadId":archive_thread}))
            .await
            .unwrap();
        let after_archive = restarted
            .agents_request("tasks.list", json!({}))
            .await
            .unwrap();
        assert!(
            after_archive["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| { task["id"] == archived_id && task["status"] == "cancelled" }),
            "archiving a parked question thread must cancel its scheduler-owned task"
        );
        let replacement = restarted
            .agents_request(
                "tasks.create",
                json!({"agentId":profile.id,"title":"Reuse archived workspace","instructions":"Ask the user for a destination.","acceptanceCriteria":["Use the selected destination."],"cwd":archive_workspace}),
            )
            .await
            .unwrap();
        let replacement_id = replacement["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let tasks = restarted
                    .agents_request("tasks.list", json!({}))
                    .await
                    .unwrap();
                if tasks["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|task| task["id"] == replacement_id && task["status"] == "waiting_input")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("archiving a question task releases its non-Git workspace");
        let before_retry = restarted
            .agents_request("tasks.list", json!({}))
            .await
            .unwrap();
        let old_run_task = before_retry["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == replacement_id)
            .unwrap();
        let old_thread = old_run_task["threadId"].as_str().unwrap().to_owned();
        let old_run_id = old_run_task["runId"].as_str().unwrap().to_owned();
        restarted
            .agents_request(
                "tasks.control",
                json!({"taskId":replacement_id,"action":"cancel"}),
            )
            .await
            .unwrap();
        restarted
            .agents_request(
                "tasks.control",
                json!({"taskId":replacement_id,"action":"retry"}),
            )
            .await
            .unwrap();
        let retried = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let tasks = restarted
                    .agents_request("tasks.list", json!({}))
                    .await
                    .unwrap();
                if let Some(task) = tasks["data"].as_array().unwrap().iter().find(|task| {
                    task["id"] == replacement_id
                        && task["status"] == "waiting_input"
                        && task["runId"]
                            .as_str()
                            .is_some_and(|run_id| run_id != old_run_id)
                }) {
                    break task.clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("retry should create a fresh question-owning run");
        restarted
            .request("thread/archive", json!({"threadId":old_thread}))
            .await
            .unwrap();
        let after_old_archive = restarted
            .agents_request("tasks.list", json!({}))
            .await
            .unwrap();
        assert!(
            after_old_archive["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| {
                    task["id"] == replacement_id
                        && task["runId"] == retried["runId"]
                        && task["status"] == "waiting_input"
                }),
            "archiving history from an older run must not cancel its successor"
        );
        tokio::time::timeout(Duration::from_secs(3), restarted.shutdown())
            .await
            .expect("worker shutdown must not hang while a question run owns its workspace");
    }
}
