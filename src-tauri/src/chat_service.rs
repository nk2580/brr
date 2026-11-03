//! Foreground chat delivery. Durable intent belongs to the host, never the webview.
#[path = "chat_assignment.rs"]
mod assignment;
#[path = "chat_input.rs"]
mod input;
#[path = "chat_recovery_messages.rs"]
mod recovery_messages;
#[path = "chat_saved_drafts.rs"]
mod saved_drafts;
use crate::agent_host::AgentHost;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub(crate) struct ForegroundState {
    live: Mutex<HashMap<String, Live>>,
    pub(crate) task_creation: tokio::sync::Mutex<()>,
}
#[derive(Clone, Default)]
struct Live {
    turn: Option<String>,
    hydrated: bool,
    approvals: HashSet<String>,
    finished: VecDeque<String>,
}

fn string<'a>(p: &'a Value, k: &str) -> Result<&'a str, String> {
    p[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{k} is required"))
}
fn is_unmaterialized_first_turn_error(thread: &str, error: &str) -> bool {
    error
        == format!(
            "thread {thread} is not materialized yet; thread/turns/list is unavailable before first user message"
        )
}
fn is_unmaterialized_include_turns_error(thread: &str, error: &str) -> bool {
    error
        == format!(
            "thread {thread} is not materialized yet; includeTurns is unavailable before first user message"
        )
}
fn ids(p: &Value) -> Result<Vec<String>, String> {
    match p.get("attachmentIds") {
        None => Ok(vec![]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("Invalid attachment ID".into())
            })
            .collect(),
        _ => Err("attachmentIds must be an array".into()),
    }
}

impl AgentHost {
    pub(crate) fn chat_changed(&self, thread: Option<&str>) {
        (self.events)(json!({"method":"brr/chat/changed","params":{"threadId":thread}}));
    }
    pub(crate) fn register_fresh_chat(&self, thread: &str) {
        self.foreground
            .live
            .lock()
            .unwrap()
            .entry(thread.to_owned())
            .or_default()
            .hydrated = true;
    }
    pub(crate) async fn chat_request(
        self: &Arc<Self>,
        method: &str,
        p: Value,
    ) -> Result<Value, String> {
        match method {
            "foreground.submit" => self.chat_submit(p).await,
            "tasks.assign" => self.assign_chat_task(p).await,
            "queue.list" => {
                let thread = string(&p, "threadId")?;
                let entries = self
                    .chat
                    .dispatch_storage("submissions.list", json!({"threadId":thread}))
                    .await?;
                let state = self
                    .chat
                    .dispatch_storage("queue.state", json!({"threadId":thread}))
                    .await?;
                Ok(json!({"entries":entries,"state":state}))
            }
            "queue.pause" => {
                let r = self.chat.dispatch_storage(method, p.clone()).await?;
                self.chat_changed(p["threadId"].as_str());
                Ok(r)
            }
            "queue.resume" => {
                let thread = string(&p, "threadId")?.to_owned();
                self.require_foreground(&thread).await?;
                let guard = self.questions.thread_guard(&thread).await;
                self.ensure_chat_live(&thread).await?;
                self.reconcile_chat(&thread).await?;
                let all = self
                    .chat
                    .dispatch_storage("submissions.list", json!({"threadId":thread}))
                    .await?;
                if all
                    .as_array()
                    .is_some_and(|a| a.iter().any(|e| e["status"] == "uncertain"))
                {
                    return Err("Delivery is unconfirmed. Inspect the conversation before retrying or removing that message.".into());
                }
                self.chat.dispatch_storage(method, p).await?;
                drop(guard);
                self.dispatch_next(&thread).await?;
                self.chat_changed(Some(&thread));
                Ok(json!({}))
            }
            "queue.remove" | "queue.edit" | "queue.steer" => self.mutate_queue(method, p).await,
            m if m.starts_with("recovery.") => self.recovery_messages_request(m, p).await,
            "references.search" => self.chat_references(p).await,
            "references.resolve" => self.resolve_chat_reference(p).await,
            "branches.create" => self.create_chat_branch(p).await,
            "branches.pending" => self.pending_chat_branches().await,
            "branches.recover" => self.recover_chat_branch(p).await,
            "drafts.list" => self.list_saved_drafts().await,
            // The webview can manage its drafts and selected originals, but never write
            // arbitrary outbox states or operation phases.
            m if m.starts_with("drafts.") || m.starts_with("attachments.") => {
                let result = self.chat.dispatch_storage(m, p).await?;
                self.chat_changed(None);
                Ok(result)
            }
            _ => Err("Unknown chat operation".into()),
        }
    }
    pub(crate) async fn require_foreground(&self, thread: &str) -> Result<(), String> {
        self.require_account().await?;
        if let Ok(binding) = self.store.session_binding(thread).await {
            if !matches!(binding.kind.as_str(), "baseline" | "agent") {
                return Err(
                    "Task and interview conversations use their owning workflow controls".into(),
                );
            }
        }
        self.require_chat_ready(thread).await?;
        if let Some(w) = self.worktrees.get(thread)? {
            self.worktrees.validate(&w).await?;
        }
        Ok(())
    }
    async fn ensure_chat_live(&self, thread: &str) -> Result<(), String> {
        if self
            .foreground
            .live
            .lock()
            .unwrap()
            .get(thread)
            .is_some_and(|live| live.hydrated)
        {
            return Ok(());
        }
        let (loaded, known_empty_fresh) = match self
            .rpc
            .call(
                "thread/read",
                json!({"threadId":thread,"includeTurns":true}),
            )
            .await
        {
            Ok(loaded) => (loaded, false),
            Err(error) if is_unmaterialized_include_turns_error(thread, &error) => {
                // This is the same pre-first-turn state reported by turns/list. Verify
                // durable intent first, then fetch only thread metadata; never infer a
                // fresh rollout from this error alone.
                self.require_safe_unmaterialized_recovery(thread).await?;
                let metadata = self
                    .rpc
                    .call(
                        "thread/read",
                        json!({"threadId":thread,"includeTurns":false}),
                    )
                    .await?;
                if metadata["thread"]["id"] != thread {
                    return Err(
                        "Runtime returned inconsistent metadata for the fresh thread".into(),
                    );
                }
                (metadata, true)
            }
            Err(error) => return Err(error),
        };
        let turns = if known_empty_fresh {
            Vec::new()
        } else if loaded["thread"]["historyMode"] == "paginated" {
            match self
                .rpc
                .call(
                    "thread/turns/list",
                    json!({"threadId":thread,"sortDirection":"desc","limit":1,"itemsView":"full"}),
                )
                .await
            {
                Ok(page) => page["data"].as_array().cloned().unwrap_or_default(),
                // Some runtimes advertise paginated history but do not implement
                // the store operation. `thread/read` already returned its available
                // history, so use that snapshot while keeping all other RPC failures
                // fail-closed.
                Err(error) if error.contains("list_turns is not supported yet") => {
                    match loaded["thread"]["turns"]
                        .as_array()
                        .filter(|turns| !turns.is_empty())
                    {
                        Some(turns) => turns.clone(),
                        // An empty paginated read can be a placeholder rather than
                        // proof that no active turn exists. Keep the operation
                        // fail-closed unless a fresh session was registered locally.
                        None => return Err(error),
                    }
                }
                Err(error) if is_unmaterialized_first_turn_error(thread, &error) => {
                    self.require_safe_unmaterialized_recovery(thread).await?;
                    if loaded["thread"]["id"] != thread
                        || !loaded["thread"]["turns"]
                            .as_array()
                            .is_some_and(Vec::is_empty)
                    {
                        return Err("Runtime returned an inconsistent empty-thread snapshot".into());
                    }
                    Vec::new()
                }
                Err(error) => return Err(error),
            }
        } else {
            loaded["thread"]["turns"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        let mut restored = Live::default();
        for turn in turns.iter().rev().take(64) {
            if let Some(id) = turn["id"].as_str() {
                if turn["status"] == "inProgress" {
                    restored.turn = Some(id.into());
                } else {
                    restored.finished.push_back(id.into());
                }
            }
        }
        let mut map = self.foreground.live.lock().unwrap();
        let live = map.entry(thread.into()).or_default();
        if !live.hydrated {
            live.turn = restored.turn.filter(|id| !live.finished.contains(id));
            for id in restored.finished {
                if !live.finished.contains(&id) {
                    live.finished.push_back(id);
                }
            }
            live.hydrated = true;
        }
        Ok(())
    }
    /// The runtime creates a thread identity before its first turn, but it does not
    /// materialize a rollout until that turn is accepted. Treat its exact empty-history
    /// response as fresh only when the host has a durable baseline/agent binding and
    /// every prior submission is known not to have reached the runtime.
    async fn require_safe_unmaterialized_recovery(&self, thread: &str) -> Result<(), String> {
        let binding =
            self.store.session_binding(thread).await.map_err(|_| {
                "Fresh-thread recovery requires a host-owned chat binding".to_string()
            })?;
        if !matches!(binding.kind.as_str(), "baseline" | "agent") {
            return Err(
                "Fresh-thread recovery is only available for baseline or agent chats".into(),
            );
        }
        let submissions = self
            .chat
            .dispatch_storage("submissions.list", json!({"threadId":thread}))
            .await?;
        let entries = submissions
            .as_array()
            .ok_or("Could not verify durable chat delivery state")?;
        let has_definitive_failure = entries.iter().any(|entry| entry["status"] == "failed");
        if !has_definitive_failure
            || entries.iter().any(|entry| {
                !matches!(
                    entry["status"].as_str(),
                    Some("queued" | "failed" | "cancelled")
                )
            })
        {
            return Err(
                "Cannot recover this thread as fresh: a delivery may have reached the runtime"
                    .into(),
            );
        }
        Ok(())
    }
    pub(crate) async fn legacy_chat_submit(
        self: &Arc<Self>,
        method: &str,
        p: Value,
    ) -> Result<Value, String> {
        let text = p["input"]
            .as_array()
            .ok_or("Message input is required")?
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let mut intent = json!({"threadId":p["threadId"],"document":{"version":1,"text":text,"references":[],"quotes":[]},"attachmentIds":[],"settings":{"model":p["model"].as_str().unwrap_or("_runtime_default"),"effort":p["effort"],"mode":p["collaborationMode"]["mode"].as_str().unwrap_or("default")},"delivery":if method=="turn/steer"{"steer"}else{"send"}});
        if let Some(id) = p["clientUserMessageId"].as_str() {
            intent["id"] = json!(id);
        }
        if method == "turn/steer" {
            intent["expectedTurnId"] = p["expectedTurnId"].clone();
        }
        self.chat_submit(intent).await
    }
    pub(crate) async fn chat_submit(self: &Arc<Self>, mut p: Value) -> Result<Value, String> {
        let thread = string(&p, "threadId")?.to_owned();
        self.require_foreground(&thread).await?;
        let _guard = self.questions.thread_guard(&thread).await;
        self.ensure_chat_live(&thread).await?;
        let document = p
            .get("document")
            .cloned()
            .unwrap_or_else(|| json!({"version":1,"text":p["text"],"references":[],"quotes":[]}));
        if document["text"].as_str().unwrap_or("").trim().is_empty() && ids(&p)?.is_empty() {
            return Err("Write a message or add an attachment".into());
        }
        input::validate_document(&document)?;
        let settings = p["settings"].clone();
        string(&settings, "model")?;
        if !matches!(settings["mode"].as_str(), Some("default" | "plan")) {
            return Err("Choose a valid chat mode".into());
        }
        let id = p["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Ok(existing) = self
            .chat
            .dispatch_storage("submissions.get", json!({"id":id}))
            .await
        {
            if existing["threadId"] != thread
                || existing["document"] != document
                || existing["attachmentIds"] != json!(ids(&p)?)
                || existing["settings"] != settings
                || existing["delivery"] != p["delivery"]
                || existing.get("expectedTurnId").filter(|v| !v.is_null())
                    != p.get("expectedTurnId").filter(|v| !v.is_null())
            {
                return Err("Submission ID already belongs to another message".into());
            }
            return Ok(json!({"submission":existing,"queued":existing["status"]=="queued"}));
        }
        self.selected_chat_inputs(&thread, &document).await?;
        self.chat.attachment_paths(&thread, &ids(&p)?).await?;
        p["id"] = json!(id);
        p["document"] = document;
        p["status"] = json!("queued");
        let entry = self.chat.dispatch_storage("submissions.put", p).await?;
        let live = self
            .foreground
            .live
            .lock()
            .unwrap()
            .get(&thread)
            .cloned()
            .unwrap_or_default();
        let delivery = entry["delivery"].as_str().unwrap_or("send");
        if delivery == "steer" {
            if live.turn.is_none() || entry["expectedTurnId"].as_str() != live.turn.as_deref() {
                self.finish_submission(
                    &entry,
                    "failed",
                    None,
                    Some("The running turn changed. Your message was retained."),
                )
                .await?;
                return Err("The running turn changed. Your message was retained.".into());
            }
        } else if live.turn.is_some()
            || !live.approvals.is_empty()
            || self
                .chat
                .dispatch_storage("queue.state", json!({"threadId":thread}))
                .await?["paused"]
                == true
            || self.has_pending_questions(&thread).await?
        {
            self.chat_changed(Some(&thread));
            return Ok(json!({"submission":entry,"queued":true}));
        }
        let all = self
            .chat
            .dispatch_storage("submissions.list", json!({"threadId":thread}))
            .await?;
        if delivery != "steer"
            && all.as_array().is_some_and(|a| {
                a.iter()
                    .any(|e| matches!(e["status"].as_str(), Some("uncertain" | "dispatching")))
            })
        {
            self.chat_changed(Some(&thread));
            return Ok(json!({"submission":entry,"queued":true}));
        }
        let candidate = if delivery == "steer" {
            entry.clone()
        } else {
            all.as_array()
                .and_then(|a| a.iter().find(|e| e["status"] == "queued"))
                .cloned()
                .unwrap_or(entry.clone())
        };
        let same = candidate["id"] == entry["id"];
        let result = self.dispatch_entry(candidate).await?;
        if !same {
            self.chat_changed(Some(&thread));
            return Ok(json!({"submission":entry,"queued":true}));
        }

        self.chat_changed(Some(&thread));
        Ok(result)
    }
    async fn has_pending_questions(&self, thread: &str) -> Result<bool, String> {
        Ok(!self
            .store
            .list_questions(Some(thread), true)
            .await?
            .is_empty())
    }
    async fn finish_submission(
        &self,
        entry: &Value,
        status: &str,
        turn: Option<&str>,
        error: Option<&str>,
    ) -> Result<Value, String> {
        let result = self.chat.dispatch_storage("submissions.update",json!({"id":entry["id"],"expectedRevision":entry["revision"],"status":status,"turnId":turn,"error":error})).await;
        if result.is_err() {
            if let Ok(current) = self
                .chat
                .dispatch_storage("submissions.get", json!({"id":entry["id"]}))
                .await
            {
                if current["status"] == "accepted"
                    && matches!(status, "accepted" | "uncertain" | "failed")
                {
                    return Ok(current);
                }
            }
        }
        result
    }

    async fn dispatch_entry(self: &Arc<Self>, entry: Value) -> Result<Value, String> {
        let thread = string(&entry, "threadId")?;
        self.require_foreground(thread).await?;
        let inflight = self
            .finish_submission(&entry, "dispatching", None, None)
            .await?;
        let delivery = entry["delivery"].as_str().unwrap_or("send");
        let method = if delivery == "steer" {
            "turn/steer"
        } else {
            "turn/start"
        };
        let outcome:Result<Value,String>=async {
            let (mut input, selected_context)=self.selected_chat_inputs(thread,&entry["document"]).await?;let mut text=entry["document"]["text"].as_str().unwrap_or("").to_owned();
            let attachment_ids=ids(&entry)?;
            if !attachment_ids.is_empty(){
                let owner=string(&entry,"id")?;
                let staged=self.chat.stage_for_thread(thread,owner,&attachment_ids).await?;
                let listed=self.chat.dispatch_storage("attachments.list",json!({"ownerId":owner})).await?;
                let files=listed.as_array().ok_or("Invalid attachment registry")?;
                let mut selected_images=vec![];
                for attachment in files.iter().filter(|a|attachment_ids.iter().any(|id|a["id"].as_str()==Some(id))){
                    let path=staged.iter().find(|(id,_)|attachment["id"].as_str()==Some(id)).map(|(_,path)|path.clone()).ok_or("Selected attachment was not staged")?;
                    if attachment["kind"]=="image"{selected_images.push(path)}else{text.push_str(&format!("\n\nAttached local file: {}\nRead-only original: {}\nUse existing tools to inspect it. If its format cannot be read, explain that limitation.",attachment["name"].as_str().unwrap_or("File"),path.display()));}
                }
                if !selected_images.is_empty(){
                    let catalog=self.rpc.call("model/list",json!({"includeHidden":false})).await?;
                    let selected=catalog["data"].as_array().and_then(|a|a.iter().find(|m|m["model"]==entry["settings"]["model"] || m["id"]==entry["settings"]["model"])).ok_or("Selected model is unavailable")?;
                    if !selected["inputModalities"].as_array().is_some_and(|a|a.iter().any(|v|v=="image")){return Err("This model does not accept images. Choose an image-capable model or remove the images.".into())}
                    for path in selected_images{input.push(json!({"type":"localImage","path":path}));}
                }
            }
            if !text.trim().is_empty(){input.insert(0,json!({"type":"text","text":text,"text_elements":[]}));}
            let mut params=json!({"threadId":thread,"input":input,"clientUserMessageId":entry["id"]});
            if method=="turn/steer"{params["expectedTurnId"]=entry["expectedTurnId"].clone()}else{
                let settings=&entry["settings"];if settings["model"] != "_runtime_default" { params["model"]=settings["model"].clone();params["effort"]=settings["effort"].clone();params["collaborationMode"]=json!({"mode":settings["mode"],"settings":{"model":settings["model"],"reasoning_effort":settings["effort"],"developer_instructions":null}}); }
            }
            params=self.prepare_turn(thread,params).await?;
            if !selected_context.as_array().is_none_or(Vec::is_empty) {
                if !params["additionalContext"].is_object() { params["additionalContext"] = json!({}); }
                params["additionalContext"]["brr_selected_context"] = json!({"kind":"untrusted","name":"brr_selected_context","value":selected_context.to_string()});
            }
            if let Some(manifest) = self.chat.staged_context(thread)? {
                if !params["additionalContext"].is_object() { params["additionalContext"] = json!({}); }
                params["additionalContext"]["brr_attachments"] = json!({"kind":"untrusted","name":"brr_attachments","value":manifest});
            }
            self.rpc.call(method,params).await
        }.await;
        match outcome {
            Ok(response) => {
                let turn = response["turn"]["id"]
                    .as_str()
                    .or_else(|| response["turnId"].as_str());
                let accepted = self
                    .finish_submission(&inflight, "accepted", turn, None)
                    .await?;
                // Learning failure must not turn accepted delivery into a resend opportunity.
                if let Err(error) = self.record_accepted_chat_evidence(&accepted).await {
                    (self.events)(
                        json!({"method":"brr/chat/evidenceError","params":{"threadId":thread,"message":error}}),
                    );
                }
                if let Some(turn) = turn {
                    let mut map = self.foreground.live.lock().unwrap();
                    let live = map.entry(thread.into()).or_default();
                    if !live.finished.iter().any(|id| id == turn) {
                        live.turn = Some(turn.into());
                    }
                }
                let mut out = response;
                out["submission"] = accepted;
                out["queued"] = json!(false);
                Ok(out)
            }
            Err(error) => {
                // Authoritative acceptance wins over a missing or failed transport ACK.
                if let Ok(current) = self
                    .chat
                    .dispatch_storage("submissions.get", json!({"id":inflight["id"]}))
                    .await
                {
                    if current["status"] == "accepted" {
                        return Ok(
                            json!({"turnId":current["turnId"],"submission":current,"queued":false}),
                        );
                    }
                }
                let uncertain = error.contains("Runtime transport error")
                    || error.contains("disconnect")
                    || error.contains("timed out")
                    || error.contains("Timeout")
                    || error.contains("response");
                let status = if uncertain { "uncertain" } else { "failed" };
                let settled = self
                    .finish_submission(&inflight, status, None, Some(&error))
                    .await?;
                if settled["status"] == "accepted" {
                    return Ok(
                        json!({"turnId":settled["turnId"],"submission":settled,"queued":false}),
                    );
                }
                self.chat
                    .dispatch_storage("queue.pause", json!({"threadId":thread,"reason":error}))
                    .await?;
                self.chat_changed(Some(thread));
                Err(error)
            }
        }
    }
    async fn dispatch_next(self: &Arc<Self>, thread: &str) -> Result<(), String> {
        let _guard = self.questions.thread_guard(thread).await;
        let live = self
            .foreground
            .live
            .lock()
            .unwrap()
            .get(thread)
            .cloned()
            .unwrap_or_default();
        if live.turn.is_some()
            || !live.approvals.is_empty()
            || self.has_pending_questions(thread).await?
            || self
                .chat
                .dispatch_storage("queue.state", json!({"threadId":thread}))
                .await?["paused"]
                == true
        {
            return Ok(());
        }
        let entries = self
            .chat
            .dispatch_storage("submissions.list", json!({"threadId":thread}))
            .await?;
        if entries.as_array().is_some_and(|a| {
            a.iter()
                .any(|e| e["status"] == "uncertain" || e["status"] == "dispatching")
        }) {
            return Ok(());
        }
        if let Some(entry) = entries
            .as_array()
            .and_then(|a| a.iter().find(|v| v["status"] == "queued"))
        {
            self.dispatch_entry(entry.clone()).await?;
        }
        self.chat_changed(Some(thread));
        Ok(())
    }
    pub(crate) async fn capture_chat_event(self: &Arc<Self>, event: &Value) {
        let method = event["method"].as_str().unwrap_or("");
        let p = &event["params"];
        let Some(thread) = p["threadId"].as_str() else {
            return;
        };
        let mut dispatch = false;
        let mut stopped = false;
        {
            let mut map = self.foreground.live.lock().unwrap();
            let live = map.entry(thread.into()).or_default();
            match method {
                "turn/started" => {
                    live.hydrated = true;
                    if let Some(id) = p["turn"]["id"].as_str() {
                        if !live.finished.iter().any(|done| done == id) {
                            live.turn = Some(id.into());
                        }
                    }
                }
                "turn/completed" => {
                    live.hydrated = true;
                    if let Some(id) = p["turn"]["id"].as_str() {
                        let current = live.turn.as_deref() == Some(id);
                        // A completion can precede its start ACK. Remember it so a late
                        // ACK cannot resurrect this turn. Ignore stale failure events.
                        if !live.finished.iter().any(|done| done == id) {
                            live.finished.push_back(id.into());
                            if live.finished.len() > 64 {
                                live.finished.pop_front();
                            }
                        }
                        if current {
                            live.turn = None;
                        }
                        dispatch =
                            (current || live.turn.is_none()) && p["turn"]["status"] == "completed";
                        stopped = (current || live.turn.is_none()) && !dispatch;
                    }
                }
                "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/permissions/requestApproval" => {
                    live.approvals.insert(event["id"].to_string());
                }
                "serverRequest/resolved" => {
                    live.approvals.remove(&p["requestId"].to_string());
                    dispatch = live.turn.is_none() && live.approvals.is_empty();
                }
                _ => {}
            }
        }
        if stopped {
            let _ = self
                .chat
                .dispatch_storage(
                    "queue.pause",
                    json!({"threadId":thread,"reason":"The previous turn stopped or failed"}),
                )
                .await;
        }
        if method == "item/started" || method == "item/completed" {
            if let Some(id) = p["item"]["clientId"].as_str() {
                if let Ok(entry) = self
                    .chat
                    .dispatch_storage("submissions.get", json!({"id":id}))
                    .await
                {
                    if entry["threadId"] == thread
                        && matches!(entry["status"].as_str(), Some("dispatching" | "uncertain"))
                    {
                        if let Ok(accepted) = self
                            .finish_submission(&entry, "accepted", p["turnId"].as_str(), None)
                            .await
                        {
                            // The runtime stdout reader must keep consuming responses.
                            // Evidence resolution may issue another runtime request.
                            let host = self.clone();
                            tokio::spawn(async move {
                                let _ = host.record_accepted_chat_evidence(&accepted).await;
                            });
                        }
                    }
                }
            }
        }
        if dispatch {
            let host = self.clone();
            let thread = thread.to_owned();
            tokio::spawn(async move {
                let _ = host.dispatch_next(&thread).await;
            });
        }
        self.chat_changed(Some(thread));
    }
    pub(crate) async fn pause_pending_chats(&self, reason: &str) {
        let threads: Result<Vec<String>, _> = sqlx::query_scalar("SELECT DISTINCT thread_id FROM chat_submissions WHERE status IN ('queued','dispatching','uncertain')").fetch_all(&self.chat.pool).await;
        if let Ok(threads) = threads {
            for thread in threads {
                self.pause_chat(&thread, reason).await;
            }
        }
    }
    pub(crate) async fn pause_chat(&self, thread: &str, reason: &str) {
        let _ = self
            .chat
            .dispatch_storage("queue.pause", json!({"threadId":thread,"reason":reason}))
            .await;
        self.chat_changed(Some(thread));
    }
    async fn reconcile_chat(&self, thread: &str) -> Result<(), String> {
        let mut cursor: Option<String> = None;
        loop {
            let mut p =
                json!({"threadId":thread,"limit":100,"sortDirection":"asc","itemsView":"full"});
            if let Some(c) = cursor {
                p["cursor"] = json!(c)
            }
            let page = match self.rpc.call("thread/turns/list", p).await {
                Ok(page) => page,
                Err(error) if is_unmaterialized_first_turn_error(thread, &error) => {
                    self.require_safe_unmaterialized_recovery(thread).await?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if let Some(turns) = page["data"].as_array() {
                for turn in turns {
                    for item in turn["items"].as_array().into_iter().flatten() {
                        if let Some(id) = item["clientId"].as_str() {
                            if let Ok(entry) = self
                                .chat
                                .dispatch_storage("submissions.get", json!({"id":id}))
                                .await
                            {
                                if entry["threadId"] == thread && entry["status"] == "uncertain" {
                                    let accepted = self
                                        .finish_submission(
                                            &entry,
                                            "accepted",
                                            turn["id"].as_str(),
                                            None,
                                        )
                                        .await?;
                                    self.record_accepted_chat_evidence(&accepted).await?;
                                }
                            }
                        }
                    }
                }
            }
            cursor = page["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(())
    }
    async fn mutate_queue(self: &Arc<Self>, method: &str, p: Value) -> Result<Value, String> {
        let entry = self
            .chat
            .dispatch_storage("submissions.get", json!({"id":string(&p,"id")?}))
            .await?;
        let thread = string(&entry, "threadId")?.to_owned();
        self.require_foreground(&thread).await?;
        let _guard = self.questions.thread_guard(&thread).await;
        // An automatic dispatcher may have delivered this row while we waited.
        let entry = self
            .chat
            .dispatch_storage("submissions.get", json!({"id":entry["id"]}))
            .await?;
        if entry["status"] != "queued" {
            return Err("Only a message waiting in the queue can be changed".into());
        }
        if method == "queue.remove" {
            let r = self
                .finish_submission(&entry, "cancelled", None, None)
                .await?;
            self.chat_changed(Some(&thread));
            return Ok(r);
        }
        if method == "queue.edit" {
            let text = string(&p, "text")?;
            let mut doc = entry["document"].clone();
            doc["text"] = json!(text);
            doc["references"] = json!([]);
            doc["quotes"] = json!([]);
            let count=sqlx::query("UPDATE chat_submissions SET payload=json_set(payload,'$.document',json(?),'$.revision',revision+1),revision=revision+1 WHERE id=? AND status='queued' AND revision=?").bind(doc.to_string()).bind(string(&entry,"id")?).bind(entry["revision"].as_i64()).execute(&self.chat.pool).await.map_err(|e|e.to_string())?.rows_affected();
            if count != 1 {
                return Err("The queued message changed. Refresh and try again.".into());
            }
            self.chat_changed(Some(&thread));
            return self
                .chat
                .dispatch_storage("submissions.get", json!({"id":entry["id"]}))
                .await;
        }
        let turn = self
            .foreground
            .live
            .lock()
            .unwrap()
            .get(&thread)
            .and_then(|s| s.turn.clone())
            .ok_or("No running turn to steer")?;
        let mut steered = entry.clone();
        steered["delivery"] = json!("steer");
        steered["expectedTurnId"] = json!(turn);
        let result = self.dispatch_entry(steered).await?;
        self.chat_changed(Some(&thread));
        Ok(result)
    }
    pub(crate) async fn record_accepted_chat_evidence(&self, entry: &Value) -> Result<(), String> {
        let thread = string(entry, "threadId")?;
        let Ok(binding) = self.store.session_binding(thread).await else {
            return Ok(());
        };
        if !binding.learning_eligible {
            return Ok(());
        }
        let access = self.tool_access(&binding).await?;
        for (index, chunk) in entry["document"]["text"]
            .as_str()
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(100)
            .enumerate()
        {
            let source_id = format!("{}:submission:{}:{index}", thread, string(entry, "id")?);
            self.store
                .record_source(
                    &access,
                    &source_id,
                    1,
                    &crate::agent_host::redact_evidence(&format!("User: {chunk}")),
                )
                .await?;
            sqlx::query(
                "INSERT OR IGNORE INTO host_learning_evidence(source_id,thread_id) VALUES(?,?)",
            )
            .bind(&source_id)
            .bind(thread)
            .execute(self.store.pool())
            .await
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    async fn reference_workspace(
        &self,
        thread: &str,
    ) -> Result<(std::path::PathBuf, Vec<String>), String> {
        if let Some(workspace) = self.worktrees.get_session(thread)? {
            self.worktrees.validate_session(&workspace).await?;
            return Ok((workspace.cwd.into(), workspace.roots));
        }
        let loaded = self
            .rpc
            .call(
                "thread/read",
                json!({"threadId":thread,"includeTurns":false}),
            )
            .await?;
        let cwd =
            std::fs::canonicalize(string(&loaded["thread"], "cwd")?).map_err(|e| e.to_string())?;
        Ok((cwd.clone(), vec![cwd.to_string_lossy().into_owned()]))
    }
    async fn project_reference_workspace(
        &self,
        project_id: &str,
    ) -> Result<(std::path::PathBuf, Vec<String>), String> {
        let project = self
            .projects
            .read(project_id)
            .await?
            .filter(|project| !project.hidden)
            .ok_or("Project does not exist")?;
        let mut roots = Vec::new();
        for folder in &project.folders {
            if let Ok(root) = crate::project_files::validate_directory(folder) {
                let root = root.to_string_lossy().into_owned();
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
        if roots.is_empty() {
            return Err("This project has no registered folders available for references".into());
        }
        let cwd = project
            .primary_folder
            .as_deref()
            .and_then(|primary| crate::project_files::validate_directory(primary).ok())
            .filter(|primary| {
                roots
                    .iter()
                    .any(|root| std::path::Path::new(root) == primary)
            })
            .unwrap_or_else(|| std::path::PathBuf::from(&roots[0]));
        Ok((cwd, roots))
    }
    async fn reference_scope_workspace(
        &self,
        p: &Value,
    ) -> Result<(std::path::PathBuf, Vec<String>), String> {
        let thread = p["threadId"].as_str();
        let project = p["projectId"].as_str();
        if p.get("cwd").is_some()
            || p.get("path").is_some()
            || thread.is_some() == project.is_some()
            || (p.get("threadId").is_some() && thread.is_none())
            || (p.get("projectId").is_some() && project.is_none())
        {
            return Err("Choose exactly one conversation or project reference scope".into());
        }
        if let Some(thread) = thread {
            self.reference_workspace(thread).await
        } else {
            self.project_reference_workspace(project.unwrap()).await
        }
    }
    async fn resolve_chat_reference(&self, p: Value) -> Result<Value, String> {
        let (cwd, roots) = self.reference_scope_workspace(&p).await?;
        let id = string(&p, "id")?;
        let folder = p["type"] == "folder";
        let path = if let Some(thread) = p["threadId"].as_str() {
            if let Some(workspace) = self.worktrees.get_session(thread)? {
                input::session_workspace_reference(&workspace, id, folder)?
            } else {
                input::workspace_reference(&cwd, &roots, id, folder)?
            }
        } else {
            input::workspace_reference(&cwd, &roots, id, folder)?
        };
        let root = roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.len())
            .ok_or("No matching saved folder")?;
        Ok(
            json!({"root":root,"path":path.strip_prefix(root).map_err(|e| e.to_string())?,"absolutePath":path}),
        )
    }
    async fn selected_chat_inputs(
        &self,
        thread: &str,
        document: &Value,
    ) -> Result<(Vec<Value>, Value), String> {
        let mut inputs = vec![];
        let mut context = vec![];
        let references = document["references"]
            .as_array()
            .ok_or("Invalid message references")?;
        if references.is_empty() {
            return Ok((inputs, json!(context)));
        }
        let saved_workspace = self.worktrees.get_session(thread)?;
        let (cwd, roots) = if let Some(workspace) = &saved_workspace {
            self.worktrees.validate_session(workspace).await?;
            (
                std::path::PathBuf::from(&workspace.cwd),
                workspace.roots.clone(),
            )
        } else {
            self.reference_workspace(thread).await?
        };
        let mut catalog: Option<Value> = None;
        for reference in references {
            let kind = string(reference, "type")?;
            let id = string(reference, "id")?;
            match kind {
                "file" | "folder" => {
                    let path = if let Some(workspace) = &saved_workspace {
                        input::session_workspace_reference(workspace, id, kind == "folder")?
                    } else {
                        input::workspace_reference(&cwd, &roots, id, kind == "folder")?
                    };
                    context.push(json!({"kind":kind,"path":path}));
                }
                "skill" if id == "plan" => {}
                "skill" => {
                    if catalog.is_none() {
                        catalog = Some(
                            self.rpc
                                .call("skills/list", json!({"cwds":[cwd],"forceReload":false}))
                                .await?,
                        );
                    }
                    let skill = catalog.as_ref().unwrap()["data"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|group| group["skills"].as_array().into_iter().flatten())
                        .find(|skill| skill["path"] == id && skill["enabled"] != false)
                        .ok_or("That selected skill is unavailable or disabled")?;
                    inputs.push(json!({"type":"skill","name":skill["name"],"path":skill["path"]}));
                }
                "agent" => return Err("Assign selected agent mentions with the task action".into()),
                _ => return Err("Unsupported context reference".into()),
            }
        }
        Ok((inputs, json!(context)))
    }
    async fn chat_references(&self, p: Value) -> Result<Value, String> {
        let (cwd, roots) = self.reference_scope_workspace(&p).await?;
        let query = p["query"].as_str().unwrap_or("").to_lowercase();
        let mut data = vec![];
        let primary = cwd.to_string_lossy().into_owned();
        for root in roots {
            let root = std::path::PathBuf::from(root);
            let output = tokio::process::Command::new("git")
                .args([
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                ])
                .current_dir(&root)
                .output()
                .await
                .map_err(|e| e.to_string())?;
            if output.status.success() {
                for path in output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                    let Ok(path) = std::str::from_utf8(path) else {
                        continue;
                    };
                    if path.to_lowercase().contains(&query) {
                        let absolute = root.join(path);
                        let id = if root.to_string_lossy() == primary {
                            path.to_owned()
                        } else {
                            absolute.to_string_lossy().into_owned()
                        };
                        data.push(json!({"id":id,"name":path,"path":absolute,"type":"file"}));
                        if data.len() >= 80 {
                            break;
                        }
                    }
                }
            } else {
                for entry in std::fs::read_dir(&root)
                    .map_err(|e| e.to_string())?
                    .filter_map(Result::ok)
                {
                    let path = entry.path();
                    if path
                        .file_name()
                        .is_some_and(|s| s.to_string_lossy().to_lowercase().contains(&query))
                    {
                        let id = if root.to_string_lossy() == primary {
                            path.strip_prefix(&root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .into_owned()
                        } else {
                            path.to_string_lossy().into_owned()
                        };
                        data.push(json!({"id":id,"name":entry.file_name().to_string_lossy(),"path":path,"type":if path.is_dir(){"folder"}else{"file"}}));
                        if data.len() >= 80 {
                            break;
                        }
                    }
                }
            }
            if data.len() >= 80 {
                break;
            }
        }
        Ok(json!(data))
    }
}

#[cfg(test)]
#[path = "bridge_chat_tests.rs"]
mod tests;
