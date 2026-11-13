use crate::{policy, worktrees::WorktreeManager};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{oneshot, Mutex as AsyncMutex},
    task::JoinHandle,
};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
pub(crate) type EventSink = Arc<dyn Fn(Value) + Send + Sync>;
type SharedStdin = Arc<AsyncMutex<Option<ChildStdin>>>;

#[derive(Clone)]
pub(crate) struct RuntimeRpc {
    stdin: SharedStdin,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
}

impl RuntimeRpc {
    pub(crate) async fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err("The runtime disconnected before the answer was sent".into());
        }
        write_message(&self.stdin, &json!({"id":id,"result":result})).await
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err("Codex runtime disconnected. Reconnect to continue.".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(error) = write_message(
            &self.stdin,
            &json!({"id":id,"method":method,"params":params}),
        )
        .await
        {
            self.pending.lock().unwrap().remove(&id);
            return Err(format!("Runtime transport error: {error}"));
        }
        let response = await_raw_response(method, rx, Duration::from_secs(60)).await;
        self.pending.lock().unwrap().remove(&id);
        response
    }
}

pub struct Engine {
    stdin: SharedStdin,
    child: AsyncMutex<Child>,
    pending: Pending,
    approvals: Arc<Mutex<HashMap<String, Value>>>,
    next_id: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
    reader: AsyncMutex<Option<JoinHandle<()>>>,
    read_only_history: bool,
    worktrees: WorktreeManager,
    pub(crate) agents: Arc<crate::agent_host::AgentHost>,
    pub(crate) ui_sandboxes: crate::generated_ui_sandbox::SandboxManager,
}

#[cfg(test)]
#[path = "bridge_questions_tests.rs"]
mod question_tests;

impl Engine {
    pub(crate) async fn archive_project_chats(
        &self,
        path: &str,
    ) -> Result<crate::project_archive::ArchiveReport, String> {
        crate::project_archive::archive_project_chats(&EngineArchiveBackend { engine: self }, path)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn verify_live_agent_task(&self) -> Result<(), String> {
        self.agents.require_account().await?;
        let models = self
            .raw("model/list", json!({"includeHidden":false}))
            .await?;
        let models = models["data"].as_array().ok_or("No live models")?;
        let model = models
            .iter()
            .find(|m| m["isDefault"] == true)
            .or_else(|| models.first())
            .ok_or("No available model")?;
        let name = model["model"]
            .as_str()
            .or_else(|| model["id"].as_str())
            .ok_or("No model ID")?;
        let candidate=brr_agent_core::ProfileCandidate{name:"Acceptance verifier".into(),description:"Local synthetic acceptance".into(),instructions:"Work only in this assigned scratch folder. Do not browse, install packages, contact people, or invoke external service tools. For the task, write the requested file using a shell command and separately verify its bytes. Complete the task with brr_task_complete and concrete evidence. Synthetic acceptance preference: the user's preferred verification color is teal. Remember that fact with its exact source revision; do not store credentials. A learned skill is appropriate only if supported by successful tool evidence.".into(),soul:"Be precise and concise.".into(),model:name.into(),effort:"medium".into()};
        let draft = self
            .agents
            .store
            .propose_profile("acceptance-user", None, candidate)
            .await?;
        self.agents
            .store
            .approve_profile(&draft.id, &draft.hash, "acceptance-review")
            .await?;
        let agent = self
            .agents
            .store
            .apply_profile(&draft.id, &draft.hash, "acceptance-apply")
            .await?;
        let task=self.agents_request("tasks.create",json!({"agentId":agent.id,"title":"Verify local acceptance output","instructions":"Write acceptance.txt in this scratch working directory with exactly BRR_AGENT_ACCEPTED followed by a newline. Read it back and independently verify the exact bytes. Do not access any other folder, browse, install packages or call external service tools. The synthetic user's preferred verification color is teal; remember this as a synthetic user fact with exact source references. Finish with brr_task_complete only after verification, giving the command result as evidence.","acceptanceCriteria":["acceptance.txt contains exactly BRR_AGENT_ACCEPTED followed by a newline.","The exact bytes were independently verified."]})).await?;
        let id = task["id"].as_str().ok_or("No acceptance task ID")?;
        let task = tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                let task = self.agents.store.get_task(id).await?;
                if task.status == "succeeded" {
                    return Ok::<_, String>(task);
                }
                if matches!(task.status.as_str(), "blocked" | "failed") {
                    return Err(format!(
                        "Live acceptance task {}: {}",
                        task.status,
                        task.reason.unwrap_or_default()
                    ));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .map_err(|_| "Live acceptance task timed out")??;
        let binding = self
            .agents
            .store
            .session_binding(
                task.thread_id
                    .as_deref()
                    .ok_or("Missing live task thread")?,
            )
            .await?;
        let path =
            std::path::PathBuf::from(binding.cwd.ok_or("Missing task cwd")?).join("acceptance.txt");
        if std::fs::read(path).map_err(|e| e.to_string())? != b"BRR_AGENT_ACCEPTED\n" {
            return Err("Live output bytes do not match acceptance criteria".into());
        }
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let done: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM agent_learning_jobs WHERE state='done')",
                )
                .fetch_one(self.agents.store.pool())
                .await
                .map_err(|e| e.to_string())?;
                if done {
                    return Ok::<_, String>(());
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .map_err(|_| "Live automatic learning did not settle")??;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) async fn verify_agent_protocol(&self) -> Result<(), String> {
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let response=self.raw("thread/start",json!({"cwd":scratch.path(),"modelProvider":"openai","ephemeral":false,"approvalPolicy":"on-request","sandbox":"read-only","config":crate::agent_protocol::thread_config("task"),"developerInstructions":crate::agent_protocol::instructions("task"),"dynamicTools":crate::agent_protocol::tools("task")})).await?;
        let id = response["thread"]["id"]
            .as_str()
            .ok_or("Missing managed thread ID")?;
        let history = self
            .raw(
                "thread/turns/list",
                json!({"threadId":id,"limit":100,"sortDirection":"desc","itemsView":"full"}),
            )
            .await;
        match history {
            Ok(page) if page["data"].is_array() => {}
            // The real runtime deliberately materializes history on first user input.
            // This response also proves the typed pagination parameters were accepted.
            Err(error) if error.contains("not materialized yet") => {}
            Err(error) => return Err(error),
            _ => return Err("Managed task history paging is unavailable".into()),
        }
        let error=self.raw("turn/start",json!({"threadId":id,"input":[{"type":"text","text":"Authentication rejection check only.","text_elements":[]}],"additionalContext":crate::agent_protocol::additional_context(None,"A scoped fact","A skill index","task")})).await.unwrap_err();
        if !error.contains("ChatGPT") {
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn set_browser_handler(&self, handler: crate::workspace_bridge::BrowserToolHandler) {
        self.agents.set_browser_handler(handler);
    }

    pub async fn agents_request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.agents_request_with_expected_revision(method, params, None)
            .await
    }

    pub async fn agents_request_with_expected_revision(
        &self,
        method: &str,
        params: Value,
        expected_revision: Option<&str>,
    ) -> Result<Value, String> {
        if method.starts_with("questions.") {
            let result = self.agents.questions_request(method, params).await?;
            if matches!(method, "questions.answer" | "questions.dismiss") {
                self.agents
                    .notify(json!({"method":"brr/questions/dispatch","params":{}}));
            }
            return Ok(result);
        }
        let mut response = self
            .agents
            .request_with_expected_revision(method, params, expected_revision)
            .await?;
        if method == "agents.decisions" {
            response["questions"] = json!(self.agents.question_views(None, true).await?);
            response["runtimeRequests"] = json!(self
                .approvals
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>());
        }
        Ok(response)
    }

    pub(crate) async fn intelligent_ui_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        crate::intelligent_ui::request(self, method, params).await
    }

    pub(crate) async fn tickets_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        crate::tickets::request(Some(&self.agents), method, params).await
    }

    pub async fn connect(command: Command, events: EventSink) -> Result<Self, String> {
        let home = command
            .as_std()
            .get_current_dir()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("brr-engine-{}", std::process::id()))
            });
        let store = brr_agent_core::AgentStore::open(home.join("agents")).await?;
        let projects = crate::projects::ProjectStore::open(home.join("projects.sqlite")).await?;
        Self::connect_with_stores(command, events, store, projects).await
    }

    pub async fn connect_with_stores(
        command: Command,
        events: EventSink,
        store: brr_agent_core::AgentStore,
        projects: crate::projects::ProjectStore,
    ) -> Result<Self, String> {
        Self::connect_with_stores_mode(command, events, store, projects, false).await
    }

    pub(crate) async fn connect_quiesced_history_with_stores(
        command: Command,
        events: EventSink,
        store: brr_agent_core::AgentStore,
        projects: crate::projects::ProjectStore,
    ) -> Result<Self, String> {
        Self::connect_with_stores_mode(command, events, store, projects, true).await
    }

    async fn connect_with_stores_mode(
        mut command: Command,
        events: EventSink,
        store: brr_agent_core::AgentStore,
        projects: crate::projects::ProjectStore,
        read_only_history: bool,
    ) -> Result<Self, String> {
        let home = command
            .as_std()
            .get_current_dir()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("brr-engine-{}", std::process::id()))
            });
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Could not start the Codex runtime: {e}"))?;
        let stdin = Arc::new(AsyncMutex::new(Some(
            child.stdin.take().ok_or("Runtime stdin unavailable")?,
        )));
        let stdout = child.stdout.take().ok_or("Runtime stdout unavailable")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let approvals = Arc::new(Mutex::new(HashMap::<String, Value>::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let next_id = Arc::new(AtomicU64::new(1));
        let rpc = RuntimeRpc {
            stdin: stdin.clone(),
            pending: pending.clone(),
            next_id: next_id.clone(),
            alive: alive.clone(),
        };
        let worktrees = WorktreeManager::new(home.clone());
        let agents = crate::agent_host::AgentHost::new(
            home.join("agents"),
            rpc,
            worktrees.clone(),
            events.clone(),
            store,
            projects,
        )
        .await?;
        let agents_reader = agents.clone();
        let (pending_reader, approvals_reader, stdin_reader, alive_reader) = (
            pending.clone(),
            approvals.clone(),
            stdin.clone(),
            alive.clone(),
        );
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("method").is_some() {
                    if read_only_history {
                        if let Some(id) = message.get("id") {
                            let reply = json!({"id":id,"error":{"code":-32601,"message":"Runtime requests are disabled during read-only history export"}});
                            let _ = write_message(&stdin_reader, &reply).await;
                        }
                        continue;
                    }
                    if message["method"] == "thread/deleted" {
                        if let Some(thread_id) = message["params"]["threadId"].as_str() {
                            let _question_guard =
                                agents_reader.questions.thread_guard(thread_id).await;
                            if let Err(error) =
                                agents_reader.cleanup_deleted_thread(thread_id).await
                            {
                                (events)(
                                    json!({"method":"brr/chat/error","params":{"operation":"thread/delete","threadId":thread_id,"message":error}}),
                                );
                            }
                        }
                    }
                    if let Err(error) = agents_reader.capture_question(&message).await {
                        (events)(
                            json!({"method":"brr/questions/error","params":{"message":error}}),
                        );
                        if message["method"] == "item/tool/requestUserInput" {
                            let _ = write_message(&stdin_reader, &json!({"id":message["id"],"error":{"code":-32602,"message":error}})).await;
                            continue;
                        }
                    }
                    if let Err(error) = agents_reader.observe_question_response(&message).await {
                        (events)(
                            json!({"method":"brr/questions/error","params":{"message":error}}),
                        );
                    }
                    if let Some(id) = message.get("id") {
                        let method = message["method"].as_str().unwrap_or("");
                        if method == "item/tool/call" {
                            let host = agents_reader.clone();
                            let writer = stdin_reader.clone();
                            let id = id.clone();
                            let mut params = message["params"].clone();
                            // Capture authority in stdout order, before a later interrupt ACK can
                            // let the host rotate the run's lease. Never trust a model argument for it.
                            let _ = host.capture_tool_fence(&mut params).await;
                            tokio::spawn(async move {
                                let result = match host.handle_tool(params).await {
                                    Ok(value) => {
                                        json!({"success":true,"contentItems":[{"type":"inputText","text":value.to_string()}]})
                                    }
                                    Err(error) => {
                                        json!({"success":false,"contentItems":[{"type":"inputText","text":error}]})
                                    }
                                };
                                let _ =
                                    write_message(&writer, &json!({"id":id,"result":result})).await;
                            });
                            continue;
                        }
                        if matches!(
                            method,
                            "item/commandExecution/requestApproval"
                                | "item/fileChange/requestApproval"
                                | "item/permissions/requestApproval"
                                | "mcpServer/elicitation/request"
                        ) {
                            let _ = agents_reader.before_approval(&message).await;
                            approvals_reader
                                .lock()
                                .unwrap()
                                .insert(id.to_string(), message.clone());
                        } else if method != "item/tool/requestUserInput" {
                            let reply = json!({"id":id,"error":{"code":-32601,"message":"This client does not support this request"}});
                            let _ = write_message(&stdin_reader, &reply).await;
                            continue;
                        }
                    }
                    if message["method"] == "serverRequest/resolved" {
                        approvals_reader
                            .lock()
                            .unwrap()
                            .remove(&message["params"]["requestId"].to_string());
                    }
                    agents_reader.capture_chat_event(&message).await;
                    agents_reader.notify(message.clone());
                    (events)(message);
                } else if let Some(id) = message["id"].as_u64() {
                    let sender = pending_reader.lock().unwrap().remove(&id);
                    if let Some(sender) = sender {
                        let result = if let Some(error) = message.get("error") {
                            Err(error["message"]
                                .as_str()
                                .unwrap_or("Runtime request failed")
                                .to_owned())
                        } else {
                            Ok(message.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                }
            }
            if !read_only_history {
                agents_reader
                    .pause_pending_chats("The runtime disconnected")
                    .await;
            }
            alive_reader.store(false, Ordering::SeqCst);
            for (_, sender) in pending_reader.lock().unwrap().drain() {
                let _ = sender.send(Err(
                    "Codex runtime disconnected. Reconnect to continue.".into()
                ));
            }
            approvals_reader.lock().unwrap().clear();
            if !read_only_history {
                (events)(json!({"method":"brr/disconnected", "params":{}}));
            }
        });
        let engine = Self {
            stdin,
            child: AsyncMutex::new(child),
            pending,
            approvals,
            next_id,
            alive,
            reader: AsyncMutex::new(Some(reader)),
            read_only_history,
            worktrees,
            agents,
            ui_sandboxes: crate::generated_ui_sandbox::SandboxManager::default(),
        };
        if let Err(error) = engine.raw("initialize", json!({"clientInfo":{"name":"brr_desktop","title":"Brr","version":"0.1.0"},"capabilities":{"experimentalApi":true,"requestAttestation":false,"extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":["text/html;profile=mcp-app"]},"openai/form":{},"openai/standard-form-input":{},"openai/elicitation":{"form":{}}}}})).await {
            let _ = engine.shutdown_and_drain().await;
            return Err(error);
        }
        if let Err(error) = write_message(&engine.stdin, &json!({"method":"initialized"})).await {
            let _ = engine.shutdown_and_drain().await;
            return Err(error);
        }
        if !read_only_history {
            engine.agents.start();
        }
        Ok(engine)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub(crate) async fn validate_history_account(&self) -> Result<(), String> {
        let account = self
            .raw("account/read", json!({"refreshToken":false}))
            .await?;
        match account.get("account") {
            Some(Value::Null) => Ok(()),
            Some(_) if policy::is_chatgpt_account(&account) => Ok(()),
            Some(_) => Err("Only ChatGPT accounts are supported by the Brr runtime".into()),
            None => Err("Runtime returned malformed account information".into()),
        }
    }

    async fn raw(&self, method: &str, params: Value) -> Result<Value, String> {
        if !self.is_alive() {
            return Err("Codex runtime disconnected. Reconnect to continue.".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(error) = write_message(
            &self.stdin,
            &json!({"id":id,"method":method,"params":params}),
        )
        .await
        {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        let response = await_raw_response(method, rx, Duration::from_secs(60)).await;
        self.pending.lock().unwrap().remove(&id);
        response
    }

    pub async fn request(&self, method: &str, mut params: Value) -> Result<Value, String> {
        if self.read_only_history
            && !matches!(
                method,
                "thread/list" | "thread/read" | "thread/turns/list" | "account/read"
            )
        {
            return Err(format!(
                "Runtime method `{method}` is disabled during read-only history export"
            ));
        }
        let mut delete_queue_state: Option<Option<(i64, Option<String>)>> = None;
        if method == "turn/interrupt" || method == "thread/archive" {
            if let Some(thread) = params["threadId"].as_str() {
                self.agents
                    .pause_chat(thread, "Stopped or archived by the user")
                    .await;
            }
        }

        policy::validate_request(method, &params)?;
        // Foreground paths share one durable outbox and per-thread question lock.
        // Host-owned task/interview workers call their workflow RPC directly.
        if matches!(method, "turn/start" | "turn/steer") {
            let interview = match params["threadId"].as_str() {
                Some(id) => self
                    .agents
                    .store
                    .session_binding(id)
                    .await
                    .ok()
                    .is_some_and(|binding| {
                        matches!(binding.kind.as_str(), "interview" | "profile")
                    }),
                None => false,
            };
            if !interview {
                return self.agents.legacy_chat_submit(method, params).await;
            }
        }
        let _question_dispatch = if matches!(
            method,
            "turn/start"
                | "turn/steer"
                | "turn/interrupt"
                | "thread/archive"
                | "thread/delete"
                | "thread/resume"
        ) {
            match params["threadId"].as_str() {
                Some(id) => Some(self.agents.questions.thread_guard(id).await),
                None => None,
            }
        } else {
            None
        };
        if matches!(
            method,
            "turn/interrupt" | "thread/archive" | "thread/delete"
        ) {
            if let Some(thread_id) = params["threadId"].as_str() {
                if let Ok(binding) = self.agents.store.session_binding(thread_id).await {
                    if binding.kind == "learning" {
                        return Err(
                            "Learning sessions are owned by the background scheduler".into()
                        );
                    }
                    if binding.kind == "task" {
                        let run = self
                            .agents
                            .store
                            .get_run(
                                binding
                                    .task_run_id
                                    .as_deref()
                                    .ok_or("Task history has no run binding")?,
                            )
                            .await?;
                        self.agents
                            .control_task_with_run(
                                &run.task_id,
                                brr_agent_core::TaskAction::Cancel,
                                Some(&run.id),
                            )
                            .await?;
                        self.agents.changed();
                        if method == "turn/interrupt" {
                            return Ok(json!({}));
                        }
                    }
                }
            }
        }
        if method == "thread/delete" {
            if let Some(thread_id) = params["threadId"].as_str() {
                self.agents.assert_chat_idle_for_deletion(thread_id).await?;
                delete_queue_state = Some(
                    sqlx::query_as::<_, (i64, Option<String>)>(
                        "SELECT paused,reason FROM chat_queue_state WHERE thread_id=?",
                    )
                    .bind(thread_id)
                    .fetch_optional(&self.agents.chat.pool)
                    .await
                    .map_err(|error| error.to_string())?,
                );
                self.agents.chat.dispatch_storage(
                    "queue.pause",
                    json!({"threadId":thread_id,"reason":"Queued work paused while deleting this chat"}),
                ).await?;
                self.agents.chat_changed(Some(thread_id));
                if let Err(error) = self.agents.assert_chat_idle_for_deletion(thread_id).await {
                    self.restore_delete_queue_state(
                        thread_id,
                        delete_queue_state.as_ref().unwrap(),
                    )
                    .await?;
                    self.agents.chat_changed(Some(thread_id));
                    return Err(error);
                }
            }
        }
        if matches!(
            method,
            "thread/start"
                | "thread/resume"
                | "turn/start"
                | "turn/steer"
                | "brr/project/inspect"
                | "brr/worktree/inspect"
                | "brr/worktree/approve"
        ) {
            let account = self
                .raw("account/read", json!({"refreshToken":false}))
                .await?;
            if !policy::is_chatgpt_account(&account) {
                return Err("Sign in with ChatGPT to start a coding session.".into());
            }
        }
        if method == "brr/project/inspect" {
            let project = self
                .agents
                .project_for_start(params["projectId"].as_str(), None)
                .await?;
            let commands = self.worktrees.inspect_session(project.as_ref()).await?;
            return Ok(json!({"commands": commands}));
        }
        if method == "brr/worktree/inspect" {
            let commands = self
                .worktrees
                .inspect(params["cwd"].as_str().unwrap())
                .await?;
            return Ok(json!({"commands": commands}));
        }
        if method == "brr/worktree/approve" {
            let templates = params["templates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|template| template.as_str().unwrap().to_owned())
                .collect();
            self.worktrees
                .approve(params["cwd"].as_str().unwrap(), templates)
                .await?;
            return Ok(json!({"approved": true}));
        }
        if matches!(method, "thread/start" | "thread/resume") {
            params["modelProvider"] = json!("openai");
            params["approvalPolicy"] = json!("on-request");
            if method == "thread/start" && params.get("sandbox").is_none() {
                params["sandbox"] = json!("workspace-write")
            }
        }
        let requested_project_id = params["projectId"].as_str().map(str::to_owned);
        let workspace = if method == "thread/start" {
            let project = self
                .agents
                .project_for_start(requested_project_id.as_deref(), params["cwd"].as_str())
                .await?;
            let mut workspace = self.worktrees.prepare_session(project.as_ref()).await?;
            if requested_project_id.is_none() {
                workspace.scope_project_id = None;
                workspace.project_revision = None;
            }
            params["cwd"] = json!(workspace.cwd);
            params["runtimeWorkspaceRoots"] = json!(workspace.roots);
            Some(workspace)
        } else if matches!(method, "thread/resume" | "turn/start") {
            let id = params["threadId"].as_str().ok_or("threadId is required")?;
            let workspace = self.worktrees.get_session(id)?;
            if let Some(workspace) = &workspace {
                self.worktrees.validate_session(workspace).await?;
                if method == "thread/resume" {
                    params["cwd"] = json!(workspace.cwd);
                    params["runtimeWorkspaceRoots"] = json!(workspace.roots);
                }
            } else if method == "thread/resume" {
                params.as_object_mut().unwrap().remove("cwd");
            }
            workspace
        } else {
            None
        };
        params.as_object_mut().unwrap().remove("projectId");
        if method == "thread/resume" {
            let id = params["threadId"].as_str().ok_or("threadId is required")?;
            self.agents.require_chat_ready(id).await?;
            if let Ok(binding) = self.agents.store.session_binding(id).await {
                if matches!(binding.kind.as_str(), "task" | "learning") {
                    // Inspect a scheduler-owned thread without changing its execution settings.
                    let mut response = self
                        .raw("thread/read", json!({"threadId":id,"includeTurns":true}))
                        .await?;
                    self.annotate_thread(&mut response["thread"]);
                    self.agents.annotate(&mut response["thread"]).await;
                    return Ok(response);
                }
                params["developerInstructions"] =
                    json!(crate::agent_protocol::instructions(&binding.kind));
                params["config"] = workspace
                    .as_ref()
                    .map(|w| crate::agent_host::workspace_config(&binding.kind, w))
                    .unwrap_or_else(|| crate::agent_protocol::thread_config(&binding.kind));
            } else {
                params["developerInstructions"] =
                    json!(crate::agent_protocol::instructions("baseline"));
                params["config"] = workspace
                    .as_ref()
                    .map(|w| crate::agent_host::workspace_config("baseline", w))
                    .unwrap_or_else(|| crate::agent_protocol::thread_config("baseline"));
            }
        }
        if matches!(method, "turn/start" | "turn/steer") {
            let thread_id = params["threadId"].as_str().unwrap_or("").to_owned();
            if let Ok(binding) = self.agents.store.session_binding(&thread_id).await {
                if matches!(binding.kind.as_str(), "task" | "learning") {
                    return Err("Background task turns are owned by the scheduler. Use task controls to pause or cancel work.".into());
                }
            }
            if let Some(items) = params["input"].as_array_mut() {
                for item in items {
                    if item.get("text_elements").is_none() {
                        item["text_elements"] = json!([]);
                    }
                }
            }
            params = self.agents.prepare_turn(&thread_id, params).await?;
        }
        if method == "thread/start" {
            params["developerInstructions"] =
                json!(crate::agent_protocol::instructions("baseline"));
            params["dynamicTools"] = json!(crate::agent_protocol::tools("baseline"));
            params["config"] = workspace
                .as_ref()
                .map(|w| crate::agent_host::workspace_config("baseline", w))
                .unwrap_or_else(|| crate::agent_protocol::thread_config("baseline"));
        }
        let stop_question_source = if matches!(
            method,
            "turn/interrupt" | "thread/archive" | "thread/delete"
        ) {
            params["threadId"].as_str().map(str::to_owned)
        } else {
            None
        };
        let branch_archive_transition = match method {
            "thread/archive" => params["threadId"]
                .as_str()
                .map(|thread_id| (thread_id.to_owned(), true)),
            "thread/unarchive" => params["threadId"]
                .as_str()
                .map(|thread_id| (thread_id.to_owned(), false)),
            _ => None,
        };
        let mut response = match self.raw(method, params).await {
            Ok(response) => response,
            Err(error) => {
                if let ("thread/delete", Some(thread_id), Some(queue_state)) = (
                    method,
                    stop_question_source.as_deref(),
                    delete_queue_state.as_ref(),
                ) {
                    self.restore_delete_queue_state(thread_id, queue_state)
                        .await
                        .map_err(|restore_error| {
                            format!("{error}; could not restore queued-work state: {restore_error}")
                        })?;
                    self.agents.chat_changed(Some(thread_id));
                }
                if method == "thread/start" {
                    if let Some(workspace) = &workspace {
                        return Err(format!(
                            "{error}. Prepared workspace retained at {}",
                            workspace.cwd
                        ));
                    }
                }
                return Err(error);
            }
        };
        if method == "thread/delete" {
            if let Some(thread_id) = stop_question_source.as_deref() {
                if let Err(error) = self.agents.cleanup_deleted_thread(thread_id).await {
                    (self.agents.events)(
                        json!({"method":"brr/chat/error","params":{"operation":"thread/delete","threadId":thread_id,"message":error}}),
                    );
                }
            }
        }
        if let Some((thread_id, archived)) = branch_archive_transition {
            self.agents
                .record_branch_archive(&thread_id, archived)
                .await?;
        }
        if let Some(thread_id) = stop_question_source {
            if method != "thread/delete" {
                self.agents
                    .store
                    .close_questions(&thread_id, None, "The conversation was stopped or archived")
                    .await?;
                self.agents.questions_changed(Some(&thread_id));
            }
        }
        if method == "thread/start" {
            let workspace = workspace
                .as_ref()
                .ok_or("Worktree setup was not completed")?;
            let id = response["thread"]["id"]
                .as_str()
                .ok_or("The runtime did not return a thread ID")?;
            self.worktrees
                .save_session(id, workspace)
                .map_err(|error| {
                    format!(
                        "{error}. Thread {id} and workspace {} were retained.",
                        workspace.cwd
                    )
                })?;
            self.agents
                .bind_baseline(&response["thread"], workspace.scope_project_id.clone())
                .await?;
            self.agents.register_fresh_chat(id);
            self.agents
                .projects
                .set_membership(id, requested_project_id.as_deref())
                .await?;
        }
        if method == "thread/resume" {
            let id = response["thread"]["id"]
                .as_str()
                .ok_or("The runtime did not return a thread ID")?;
            match self.agents.store.session_binding(id).await {
                Ok(binding) if matches!(binding.kind.as_str(), "baseline" | "agent") => {
                    self.raw(
                        "thread/dynamicTools/replace",
                        json!({"threadId":id,"tools":crate::agent_protocol::tools(&binding.kind)}),
                    )
                    .await?;
                }
                Ok(_) => {}
                // Legacy threads stay unbound. Runtime config enables its fixed
                // per-turn UI tool without granting host memory permissions.
                Err(_) => {}
            }
        }
        if matches!(method, "thread/start" | "thread/resume" | "thread/read") {
            self.annotate_thread(&mut response["thread"]);
            self.agents.annotate(&mut response["thread"]).await;
        } else if method == "thread/list" {
            if let Some(threads) = response["data"].as_array_mut() {
                for thread in threads {
                    self.annotate_thread(thread);
                    self.agents.annotate(thread).await;
                }
            }
        }
        Ok(response)
    }

    fn annotate_thread(&self, thread: &mut Value) {
        if let Some(id) = thread["id"].as_str().map(str::to_owned) {
            match self.worktrees.get_session(&id) {
                Ok(Some(workspace)) => {
                    if let Some(worktree) = workspace.worktrees.first() {
                        thread["worktree"] = json!(worktree);
                    }
                    thread["workspace"] = json!(workspace);
                }
                Err(error) => thread["worktreeError"] = json!(error),
                Ok(None) => {}
            }
        }
    }

    async fn restore_delete_queue_state(
        &self,
        thread_id: &str,
        previous: &Option<(i64, Option<String>)>,
    ) -> Result<(), String> {
        if let Some((paused, reason)) = previous {
            sqlx::query("INSERT INTO chat_queue_state(thread_id,paused,reason) VALUES(?,?,?) ON CONFLICT(thread_id) DO UPDATE SET paused=excluded.paused,reason=excluded.reason")
                .bind(thread_id)
                .bind(paused)
                .bind(reason)
                .execute(&self.agents.chat.pool)
                .await
                .map_err(|error| error.to_string())?;
        } else {
            sqlx::query("DELETE FROM chat_queue_state WHERE thread_id=?")
                .bind(thread_id)
                .execute(&self.agents.chat.pool)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        let key = id.to_string();
        let request = self
            .approvals
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or("Approval is no longer pending")?;
        match request["method"].as_str().unwrap_or("") {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                if !matches!(
                    result["decision"].as_str(),
                    Some("accept" | "decline" | "cancel")
                ) {
                    return Err("Unsupported approval decision".into());
                }
            }
            "item/permissions/requestApproval" => {
                if result["permissions"] != json!({})
                    && result["permissions"] != request["params"]["permissions"]
                {
                    return Err("Only the requested permissions can be granted".into());
                }
                if result["scope"] != "turn" {
                    return Err("Permissions can only be granted for this turn".into());
                }
            }
            "item/tool/requestUserInput" => {
                if !result["answers"].is_object() {
                    return Err("Answers are required".into());
                }
            }
            "mcpServer/elicitation/request" => {
                if !matches!(
                    result["action"].as_str(),
                    Some("accept" | "decline" | "cancel")
                ) {
                    return Err("Unsupported MCP elicitation action".into());
                }
                if result
                    .get("content")
                    .is_some_and(|value| !value.is_null() && !value.is_object())
                {
                    return Err("MCP elicitation content must be an object or null".into());
                }
            }
            _ => return Err("Unsupported runtime request".into()),
        }
        self.agents.resume_approval(&request).await?;
        write_message(&self.stdin, &json!({"id":id,"result":result})).await?;
        self.approvals.lock().unwrap().remove(&key);
        Ok(())
    }

    pub async fn shutdown(&self) {
        let _ = self.shutdown_and_drain().await;
    }

    pub(crate) async fn shutdown_and_drain(&self) -> Result<(), String> {
        self.agents.stop().await;
        self.stdin.lock().await.take();
        let mut child = self.child.lock().await;
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(format!("Could not wait for the Codex runtime: {error}")),
            Err(_) => {
                child
                    .kill()
                    .await
                    .map_err(|error| format!("Could not stop the Codex runtime: {error}"))?;
                child
                    .wait()
                    .await
                    .map_err(|error| format!("Could not reap the Codex runtime: {error}"))?;
            }
        }
        drop(child);
        if let Some(reader) = self.reader.lock().await.take() {
            reader
                .await
                .map_err(|error| format!("Runtime response reader failed: {error}"))?;
        }
        Ok(())
    }
}

struct EngineArchiveBackend<'a> {
    engine: &'a Engine,
}

impl crate::project_archive::ArchiveBackend for EngineArchiveBackend<'_> {
    async fn list(&self, archived: bool, cursor: Option<&str>) -> Result<Value, String> {
        self.engine
            .raw(
                "thread/list",
                archive_thread_list_params(archived, cursor, None),
            )
            .await
    }

    async fn list_descendants(
        &self,
        ancestor_thread_id: &str,
        archived: bool,
        cursor: Option<&str>,
    ) -> Result<Value, String> {
        self.engine
            .raw(
                "thread/list",
                archive_thread_list_params(archived, cursor, Some(ancestor_thread_id)),
            )
            .await
    }

    async fn read(&self, thread_id: &str) -> Result<Value, String> {
        self.engine
            .raw("thread/read", json!({"threadId": thread_id}))
            .await
    }

    async fn archive(&self, thread_id: &str) -> Result<(), String> {
        self.engine
            .request("thread/archive", json!({"threadId": thread_id}))
            .await
            .and_then(|response| {
                response
                    .as_object()
                    .map(|_| ())
                    .ok_or_else(|| "thread/archive returned malformed data".into())
            })
    }

    async fn bindings(&self) -> Result<Vec<crate::project_archive::BindingInfo>, String> {
        self.engine
            .agents
            .store
            .list_bindings(None)
            .await
            .map(|bindings| {
                bindings
                    .into_iter()
                    .map(|binding| crate::project_archive::BindingInfo {
                        thread_id: binding.thread_id,
                        project_id: binding.project_id,
                        kind: binding.kind,
                        cwd: binding.cwd,
                    })
                    .collect()
            })
    }

    async fn membership(&self, thread_id: &str) -> Result<Option<String>, String> {
        self.engine.agents.projects.membership(thread_id).await
    }
}

fn archive_thread_list_params(
    archived: bool,
    cursor: Option<&str>,
    ancestor_thread_id: Option<&str>,
) -> Value {
    let mut params = json!({
        "archived": archived,
        "limit": 100,
        "sourceKinds": ["cli", "vscode", "exec", "appServer", "subAgent", "subAgentReview", "subAgentCompact", "subAgentThreadSpawn", "subAgentOther", "unknown"]
    });
    if let Some(cursor) = cursor {
        params["cursor"] = json!(cursor);
    }
    if let Some(ancestor) = ancestor_thread_id {
        params["ancestorThreadId"] = json!(ancestor);
    }
    params
}

#[cfg(test)]
mod project_archive_protocol_tests {
    use super::*;

    #[test]
    fn archive_listing_is_unfiltered_and_explicitly_includes_every_source_kind() {
        let params = archive_thread_list_params(false, Some("next"), None);
        assert_eq!(params["archived"], false);
        assert_eq!(params["cursor"], "next");
        assert_eq!(params["limit"], 100);
        assert!(params["projectId"].is_null());
        assert!(params["cwd"].is_null());
        assert!(params["searchTerm"].is_null());
        assert_eq!(params["sourceKinds"].as_array().unwrap().len(), 10);
        let descendant_params = archive_thread_list_params(false, None, Some("root"));
        assert_eq!(descendant_params["ancestorThreadId"], "root");
    }
}

fn raw_request_timeout(method: &str) -> Option<Duration> {
    if matches!(
        method,
        "mcpServer/tool/call"
            | "mcpServer/resource/read"
            | "mcpServer/prompt/get"
            | "mcpServer/oauth/login"
            | "mcpServer/event/stream/start"
            | "plugin/install"
            | "plugin/uninstall"
            | "plugin/mcpCredential/write"
            | "marketplace/add"
            | "marketplace/remove"
            | "marketplace/upgrade"
    ) {
        None
    } else {
        Some(Duration::from_secs(60))
    }
}

async fn await_raw_response(
    method: &str,
    response: oneshot::Receiver<Result<Value, String>>,
    default_timeout: Duration,
) -> Result<Value, String> {
    match raw_request_timeout(method) {
        Some(_) => tokio::time::timeout(default_timeout, response)
            .await
            .map_err(|_| format!("Runtime request {method} timed out"))?
            .map_err(|_| "Runtime disconnected".to_string())?,
        // Runtime-owned lifecycle operations and user elicitation have their own
        // operation deadlines. Keep their original request IDs pending until the
        // runtime responds or the connection closes.
        None => response
            .await
            .map_err(|_| "Runtime disconnected".to_string())?,
    }
}

async fn write_message(stdin: &SharedStdin, message: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    let mut stdin = stdin.lock().await;
    let writer = stdin
        .as_mut()
        .ok_or_else(|| "Runtime stdin is closed".to_string())?;
    writer.write_all(&bytes).await.map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "bridge_worktrees_tests.rs"]
mod worktree_tests;

#[cfg(test)]
#[path = "bridge_agents_tests.rs"]
mod agent_tests;

#[cfg(test)]
#[path = "bridge_profile_authority_tests.rs"]
mod profile_authority_tests;

#[cfg(test)]
#[path = "bridge_ticket_live_tests.rs"]
mod ticket_live_tests;

#[cfg(test)]
#[path = "bridge_projects_tests.rs"]
mod project_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn long_running_runtime_operations_are_not_cut_off_by_the_bridge_timeout() {
        for method in [
            "mcpServer/tool/call",
            "mcpServer/resource/read",
            "mcpServer/prompt/get",
            "mcpServer/oauth/login",
            "mcpServer/event/stream/start",
            "plugin/install",
            "plugin/uninstall",
            "marketplace/add",
            "marketplace/remove",
            "marketplace/upgrade",
        ] {
            assert_eq!(raw_request_timeout(method), None);
        }
        for method in ["mcpServer/prompts/list", "model/list"] {
            assert_eq!(raw_request_timeout(method), Some(Duration::from_secs(60)));
        }

        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let _ = tx.send(Ok(json!({"ok":true})));
        });
        assert_eq!(
            await_raw_response("mcpServer/tool/call", rx, Duration::from_millis(1)).await,
            Ok(json!({"ok":true})),
            "MCP calls use runtime deadlines even when the bridge's ordinary timeout elapses"
        );

        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let _ = tx.send(Ok(json!({"ok":true})));
        });
        assert_eq!(
            await_raw_response("model/list", rx, Duration::from_millis(1)).await,
            Err("Runtime request model/list timed out".into())
        );
    }

    #[tokio::test]
    async fn non_git_sessions_save_direct_folders_without_inferring_project_membership() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("engine.py");
        std::fs::write(&fixture, r#"
import json,sys
for line in sys.stdin:
    message=json.loads(line)
    if 'id' not in message: continue
    result={'account':{'type':'chatgpt'}} if message['method']=='account/read' else {'thread':{'id':'new'}}
    print(json.dumps({'id':message['id'],'result':result}),flush=True)
"#).unwrap();
        let mut command = Command::new("python3");
        command.arg(&fixture).current_dir(dir.path());
        let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
        let result = engine
            .request("thread/start", json!({"cwd": dir.path()}))
            .await;
        engine.shutdown().await;
        let response = result.unwrap();
        assert!(response["thread"]["projectId"].is_null());
        assert_eq!(
            response["thread"]["workspace"]["cwd"],
            dir.path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(response["thread"]["workspace"]["worktrees"], json!([]));
    }

    #[tokio::test]
    async fn routes_responses_and_notifications_and_rejects_unsigned_inference() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("engine.py");
        std::fs::write(
            &fixture,
            r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    result={'account':None} if m['method']=='account/read' else {'ok':True}
    print(json.dumps({'id':m['id'],'result':result}),flush=True)
    if m['method']=='model/list':
        print(json.dumps({'method':'test/event','params':{'ok':True}}),flush=True)
"#,
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut command = tokio::process::Command::new("python3");
        command.arg(fixture);
        let engine = Engine::connect(
            command,
            std::sync::Arc::new(move |e| {
                let _ = tx.send(e);
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            engine.request("model/list", json!({})).await.unwrap()["ok"],
            true
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event["method"], "test/event");
        let error = engine
            .request(
                "turn/start",
                json!({"threadId":"t", "input":[{"type":"text","text":"hello"}]}),
            )
            .await
            .unwrap_err();
        assert!(error.contains("ChatGPT"));
        for (method, params) in [
            ("brr/worktree/inspect", json!({"cwd":"/not-a-project"})),
            (
                "brr/worktree/approve",
                json!({"cwd":"/not-a-project","templates":["touch must-not-run"]}),
            ),
        ] {
            assert!(engine
                .request(method, params)
                .await
                .unwrap_err()
                .contains("ChatGPT"));
        }
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn steering_requires_chatgpt_and_forwards_the_exact_target_and_text() {
        for account in [
            Value::Null,
            json!({"type":"apiKey"}),
            json!({"type":"chatgpt"}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let fixture = dir.path().join("engine.py");
            let forwarded = dir.path().join("steered.json");
            std::fs::write(
                &fixture,
                r#"
import json,sys
for line in sys.stdin:
    message=json.loads(line)
    if 'id' not in message: continue
    method=message['method']
    result={}
    if method=='account/read': result={'account':json.loads(sys.argv[1])}
    if method=='thread/read': result={'thread':{'id':'thread-live','turns':[{'id':'turn-live','status':'inProgress','items':[]}]}}
    if method=='turn/steer':
        with open(sys.argv[2], 'w') as output: json.dump(message['params'],output)
        result={'turnId':message['params']['expectedTurnId']}
    print(json.dumps({'id':message['id'],'result':result}),flush=True)
"#,
            )
            .unwrap();
            let mut command = tokio::process::Command::new("python3");
            command
                .arg(fixture)
                .arg(account.to_string())
                .arg(&forwarded);
            let engine = Engine::connect(command, std::sync::Arc::new(|_| {}))
                .await
                .unwrap();
            let result = engine.request("turn/steer", json!({"threadId":"thread-live", "expectedTurnId":"turn-live", "input":[{"type":"text", "text":"Check the parser first"}]})).await;
            if account["type"] == "chatgpt" {
                assert_eq!(result.unwrap()["turnId"], "turn-live");
                let mut sent: Value =
                    serde_json::from_str(&std::fs::read_to_string(&forwarded).unwrap()).unwrap();
                assert!(sent["clientUserMessageId"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()));
                sent.as_object_mut().unwrap().remove("clientUserMessageId");
                assert_eq!(
                    sent,
                    json!({"threadId":"thread-live", "expectedTurnId":"turn-live", "input":[{"type":"text", "text":"Check the parser first", "text_elements":[]}]})
                );
            } else {
                assert!(result.unwrap_err().contains("ChatGPT"));
                assert!(!forwarded.exists(), "steering bypassed the account gate");
            }
            engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn shutdown_closes_stdin_and_waits_for_runtime_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("engine.py");
        let sentinel = dir.path().join("cleaned-up");
        std::fs::write(
            &fixture,
            r#"
import json,sys
for line in sys.stdin:
    message=json.loads(line)
    if 'id' in message:
        print(json.dumps({'id':message['id'],'result':{}}),flush=True)
with open(sys.argv[1], 'w') as marker:
    marker.write('stdin closed')
"#,
        )
        .unwrap();

        let mut command = tokio::process::Command::new("python3");
        command.arg(fixture).arg(&sentinel);
        let engine = Engine::connect(command, std::sync::Arc::new(|_| {}))
            .await
            .unwrap();

        engine.shutdown().await;

        assert!(sentinel.exists(), "runtime did not observe stdin EOF");
    }

    #[tokio::test]
    async fn forwards_mcp_elicitation_and_returns_response_with_original_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("engine.py");
        let response_path = dir.path().join("response.json");
        std::fs::write(&fixture, r#"
import json,sys
for line in sys.stdin:
    message=json.loads(line)
    if message.get('method')=='initialize':
        extensions=message.get('params',{}).get('capabilities',{}).get('extensions',{})
        if extensions.get('io.modelcontextprotocol/ui',{}).get('mimeTypes') != ['text/html;profile=mcp-app']:
            print(json.dumps({'id':message['id'],'error':{'code':-32602,'message':'MCP Apps support was not advertised'}}),flush=True)
            continue
        print(json.dumps({'id':message['id'],'result':{}}),flush=True)
        print(json.dumps({'id':700,'method':'mcpServer/elicitation/request','params':{'threadId':'t','turnId':None,'serverName':'review','mode':'url','url':'https://example.test','message':'Continue?'}}),flush=True)
    elif 'id' in message:
        with open(sys.argv[1], 'w') as output: json.dump(message, output)
        print(json.dumps({'id':message['id'],'result':{}}),flush=True)
"#).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut command = Command::new("python3");
        command.arg(fixture).arg(&response_path);
        let engine = Engine::connect(
            command,
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        )
        .await
        .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(event) = rx.recv().await {
                    if event["method"] == "mcpServer/elicitation/request" {
                        break event;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(event["id"], 700);
        assert!(engine
            .respond(
                json!(700),
                json!({"action":"accept","content":{"approved":true},"_meta":null})
            )
            .await
            .is_ok());
        let response: Value = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(bytes) = std::fs::read(&response_path) {
                    if let Ok(response) = serde_json::from_slice::<Value>(&bytes) {
                        break response;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(response["id"], 700);
        assert_eq!(response["result"]["action"], "accept");
        engine.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quiesced_engine_rejects_turn_start_even_when_runtime_would_accept_it() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("runtime.py");
        let calls = dir.path().join("calls.jsonl");
        std::fs::write(
            &fixture,
            format!(
                r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if 'method' not in m: continue
    with open({},'a') as f: f.write(json.dumps(m)+'\n')
    if 'id' not in m: continue
    print(json.dumps({{'id':m['id'],'result':{{'accepted':True}}}}),flush=True)
"#,
                serde_json::to_string(calls.to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
        let root = dir.path().join("runtime-home");
        std::fs::create_dir_all(&root).unwrap();
        let store = brr_agent_core::AgentStore::open(root.join("agents"))
            .await
            .unwrap();
        let projects = crate::projects::ProjectStore::open(root.join("projects.sqlite"))
            .await
            .unwrap();
        let mut command = Command::new("python3");
        command.arg(fixture).current_dir(&root);
        let engine = Engine::connect_quiesced_history_with_stores(
            command,
            Arc::new(|_| {}),
            store,
            projects,
        )
        .await
        .unwrap();
        for (method, params) in [
            ("account/read", json!({})),
            ("thread/list", json!({"limit": 10})),
            ("thread/read", json!({"threadId":"t"})),
            ("thread/turns/list", json!({"threadId":"t","limit":10})),
        ] {
            let response = engine.request(method, params).await;
            assert!(response.is_ok(), "{method}: {response:?}");
        }
        let error = engine
            .request("turn/start", json!({"threadId":"t","input":[]}))
            .await
            .unwrap_err();
        assert!(error.contains("read-only history export"));
        engine.shutdown_and_drain().await.unwrap();
        let calls = std::fs::read_to_string(calls).unwrap();
        assert!(
            !calls.contains("turn/start"),
            "a runtime that would accept inference must never receive it"
        );
        for method in ["thread/list", "thread/read", "thread/turns/list"] {
            assert!(
                calls.contains(method),
                "read-only history method {method} was not forwarded"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_drains_the_runtime_reader_after_a_final_notification() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("runtime.py");
        std::fs::write(
            &fixture,
            r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if m.get('method')=='initialize':
        print(json.dumps({'id':m['id'],'result':{}}),flush=True)
    elif m.get('method')=='initialized':
        print(json.dumps({'method':'test/final','params':{'last':True}}),flush=True)
        sys.exit(0)
"#,
        )
        .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut command = Command::new("python3");
        command.arg(fixture).current_dir(dir.path());
        let engine = Engine::connect(
            command,
            Arc::new(move |event| {
                if event["method"] == "test/final" {
                    std::thread::sleep(Duration::from_millis(120));
                    let _ = tx.send(event);
                }
            }),
        )
        .await
        .unwrap();
        engine.shutdown_and_drain().await.unwrap();
        assert_eq!(rx.try_recv().unwrap()["params"]["last"], true);
    }
}
