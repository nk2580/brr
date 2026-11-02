use crate::agent_host::AgentHost;
use brr_agent_core::{MemoryAccess, SessionBinding, Task, TaskRun};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, watch},
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
    workspace_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    workspace_guards: Mutex<HashMap<String, tokio::sync::OwnedMutexGuard<()>>>,
    workspace_reservations: Mutex<HashMap<String, String>>,
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
                workspace_locks: Mutex::new(HashMap::new()),
                workspace_guards: Mutex::new(HashMap::new()),
                workspace_reservations: Mutex::new(HashMap::new()),
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
            .workspace_reservations
            .lock()
            .unwrap()
            .remove(run_id);
    }

    fn excluded_workspaces(&self) -> Vec<String> {
        self.inner
            .workspace_reservations
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    async fn reserve_workspace(&self, run_id: &str, task: &Task) -> Result<(), String> {
        let Some(cwd) = task.cwd.as_deref() else {
            return Ok(());
        };
        if self
            .inner
            .workspace_reservations
            .lock()
            .unwrap()
            .get(run_id)
            .is_some_and(|reserved| reserved == cwd)
        {
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
        let Some(cwd) = task.cwd.as_deref() else {
            return Ok(());
        };
        let cwd_owned = cwd.to_owned();
        let is_git = tokio::task::spawn_blocking(move || {
            worktrunk::git::Repository::at_fresh(&cwd_owned).is_ok()
        })
        .await
        .map_err(|error| error.to_string())?;
        if is_git {
            return Ok(());
        }
        self.inner
            .workspace_reservations
            .lock()
            .unwrap()
            .insert(run_id.to_owned(), cwd.to_owned());
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
        let Some(cwd) = task.cwd.as_deref() else {
            return Ok(());
        };
        self.reserve_workspace(run_id, task).await?;
        let is_reserved = self
            .inner
            .workspace_reservations
            .lock()
            .unwrap()
            .get(run_id)
            .is_some_and(|reserved| reserved == cwd);
        if !is_reserved {
            return Ok(());
        }
        let lock = {
            let mut locks = self.inner.workspace_locks.lock().unwrap();
            locks
                .entry(cwd.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = lock.lock_owned().await;
        self.inner
            .workspace_guards
            .lock()
            .unwrap()
            .insert(run_id.to_owned(), guard);
        Ok(())
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

    pub(crate) fn start(self: &Arc<Self>) {
        self.workers.start(Arc::downgrade(self));
    }

    pub(crate) fn notify(self: &Arc<Self>, event: Value) {
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
        self.workers.acquire_workspace(&run.id, &task).await?;
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
            if let Err(error) = recover_workers(&host, &workers, &mut tasks).await {
                (host.events)(json!({"method":"brr/worker/error","params":{"message":error}}));
            }
        }
        if let Err(error) = host.store.tick_workflows(epoch_seconds()).await {
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
    recover_waiting_runs(host, workers, tasks).await?;
    let unresolved = host.store.unresolved_runs().await?;
    for run in unresolved.into_iter().filter(|run| run.state == "running") {
        let task = host.store.get_task(&run.task_id).await?;
        workers.reserve_workspace(&run.id, &task).await?;
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
    workers.reserve_workspace(&run.id, &task).await?;
    workers.acquire_workspace(&run.id, &task).await?;
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

    for run in unresolved.into_iter().filter(|run| run.state == "running") {
        let task = host.store.get_task(&run.task_id).await?;
        workers.reserve_workspace(&run.id, &task).await?;
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
        workers.reserve_workspace(&run.id, &claimed_task).await?;
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
    workers.acquire_workspace(&run.id, &task).await?;
    let has_thread = run.thread_id.is_some();
    let run = if has_thread {
        host.store.recover_run(&run.id, &run.lease_token).await?
    } else {
        host.require_account().await?;
        let profile = host.store.read_profile(&task.agent_id).await?;
        let session = host
            .open_session(
                Some(task.agent_id.clone()),
                "task",
                task.model.as_deref().unwrap_or(&profile.model),
                task.cwd.as_deref(),
                false,
                Some(run.id.clone()),
            )
            .await?;
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
    let params = json!({
        "threadId":thread_id,
        "clientUserMessageId":client_message_id,
        "model":task.model,
        "effort":task.effort,
        "input":[{"type":"text","text":prompt,"text_elements":[]}]
    });
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

fn has_unknown_effects(turn: &Value) -> bool {
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

fn turn_has_client_message(turn: &Value, client_message_id: &str) -> bool {
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

async fn thread_turns(host: &AgentHost, thread_id: &str) -> Result<Vec<Value>, String> {
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
        .open_session(agent_id.clone(), "learning", &model, None, true, None)
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

#[cfg(test)]
mod tests {
    use super::*;

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
            vec![task.cwd.clone().unwrap()]
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
        let workspace = task.cwd.clone().unwrap();
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
}
