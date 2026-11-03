use crate::agent_host::AgentHost;
use brr_agent_core::{
    NewQuestionRequest, Question, QuestionAnswers, QuestionOption, QuestionRequest,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::{Mutex as AsyncMutex, OwnedMutexGuard},
    task::JoinSet,
};

pub(crate) struct QuestionState {
    pub generation: String,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    live: Mutex<HashMap<String, String>>,
}

impl QuestionState {
    pub fn new() -> Self {
        Self {
            generation: uuid::Uuid::new_v4().to_string(),
            locks: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
        }
    }

    pub async fn thread_guard(&self, thread_id: &str) -> OwnedMutexGuard<()> {
        let lock = self
            .locks
            .lock()
            .unwrap()
            .entry(thread_id.into())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        lock.lock_owned().await
    }
}

impl AgentHost {
    pub(crate) async fn question_views(
        &self,
        thread_id: Option<&str>,
        pending_only: bool,
    ) -> Result<Vec<Value>, String> {
        let mut views = Vec::new();
        for question in self.store.list_questions(thread_id, pending_only).await? {
            let waiting =
                if let Some(task_id) = question.task_id.as_deref() {
                    self.store.get_task(task_id).await.ok().is_some_and(|task| {
                        matches!(task.status.as_str(), "waiting_input" | "paused")
                    })
                } else {
                    false
                };
            let mut value = json!(question);
            value["waiting"] = json!(waiting || question.kind == "blocking");
            views.push(value);
        }
        Ok(views)
    }
    pub(crate) fn questions_changed(&self, thread_id: Option<&str>) {
        (self.events)(json!({"method":"brr/questions/changed","params":{"threadId":thread_id}}));
    }

    /// Runs in stdout order. Never takes a control lock or calls back into the runtime.
    pub(crate) async fn capture_question(&self, event: &Value) -> Result<(), String> {
        self.capture_question_inner(event, false).await
    }

    async fn capture_question_inner(&self, event: &Value, recovering: bool) -> Result<(), String> {
        let method = event["method"].as_str().unwrap_or_default();
        let params = &event["params"];
        if method == "turn/started" {
            if let (Some(thread), Some(turn)) =
                (params["threadId"].as_str(), params["turn"]["id"].as_str())
            {
                sqlx::query(
                    "INSERT OR IGNORE INTO question_observed_turns(thread_id,turn_id) VALUES(?,?)",
                )
                .bind(thread)
                .bind(turn)
                .execute(self.store.pool())
                .await
                .map_err(|e| e.to_string())?;
            }
        }
        let blocking = method == "item/tool/requestUserInput";
        let asynchronous = method == "item/completed"
            && params["item"]["delivery"] == "async"
            && params["item"]["questions"].is_array();
        if !blocking && !asynchronous {
            return Ok(());
        }
        let thread_id = required(params, "threadId")?;
        let turn_id = required(params, "turnId")?;
        let item_id = if blocking {
            required(params, "itemId")?
        } else {
            required(&params["item"], "id")?
        };
        if recovering
            && self
                .store
                .list_questions(Some(thread_id), false)
                .await?
                .iter()
                .any(|q| q.turn_id == turn_id && q.item_id == item_id)
        {
            return Ok(());
        }
        let binding = self.store.session_binding(thread_id).await.ok();
        if binding.as_ref().is_some_and(|b| b.kind == "learning") {
            return Err("Learning workers cannot ask the user questions".into());
        }
        let (agent_id, run_id) = binding
            .as_ref()
            .map(|b| (b.agent_id.clone(), b.task_run_id.clone()))
            .unwrap_or_default();
        let task_id = if let Some(run_id) = run_id.as_deref() {
            let run = self.store.get_run(run_id).await?;
            if run.thread_id.as_deref() != Some(thread_id)
                || (run.state != "running" && !(recovering && run.state == "waiting_input"))
                || run.turn_id.as_deref().is_some_and(|id| id != turn_id)
            {
                return Err("The question belongs to a stopped or stale task run".into());
            }
            Some(run.task_id)
        } else {
            None
        };
        let questions = if blocking {
            serde_json::from_value::<Vec<Question>>(params["questions"].clone())
                .map_err(|e| e.to_string())?
        } else {
            params["item"]["questions"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    Ok(Question {
                        id: format!("question_{}", i + 1),
                        header: format!("Question {}", i + 1),
                        question: required(q, "title")?.into(),
                        options: q["options"]
                            .as_array()
                            .map(|options| {
                                options
                                    .iter()
                                    .map(|option| {
                                        option
                                            .as_str()
                                            .map(|label| QuestionOption {
                                                label: label.into(),
                                                description: String::new(),
                                            })
                                            .ok_or_else(|| {
                                                "Question options must be text".to_owned()
                                            })
                                    })
                                    .collect::<Result<Vec<_>, _>>()
                            })
                            .transpose()?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        };
        let recorded = self
            .store
            .record_question(NewQuestionRequest {
                thread_id: thread_id.into(),
                turn_id: turn_id.into(),
                item_id: item_id.into(),
                agent_id,
                task_id,
                run_id,
                kind: if blocking { "blocking" } else { "async" }.into(),
                questions,
                runtime_generation: blocking.then(|| self.questions.generation.clone()),
                runtime_request_id: if blocking {
                    event.get("id").cloned()
                } else {
                    None
                },
            })
            .await?;
        if blocking {
            if let Some(id) = event.get("id") {
                self.questions
                    .live
                    .lock()
                    .unwrap()
                    .insert(id.to_string(), recorded.id);
            }
        }
        self.questions_changed(Some(thread_id));
        Ok(())
    }

    pub(crate) async fn questions_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match method {
            "questions.list" => {
                fields(&params, &["threadId", "pendingOnly"])?;
                if params.get("threadId").is_some() {
                    required(&params, "threadId")?;
                }
                if params.get("pendingOnly").is_some_and(|v| !v.is_boolean()) {
                    return Err("pendingOnly must be a boolean".into());
                }
                Ok(
                    json!({"data":self.question_views(params["threadId"].as_str(), params["pendingOnly"].as_bool().unwrap_or(false)).await?}),
                )
            }
            "questions.saveDraft" => {
                fields(&params, &["requestId", "answers"])?;
                let answers: QuestionAnswers =
                    serde_json::from_value(params["answers"].clone()).map_err(|e| e.to_string())?;
                Ok(json!(
                    self.store
                        .save_question_draft(required(&params, "requestId")?, &answers)
                        .await?
                ))
            }
            "questions.answer" | "questions.dismiss" => {
                fields(
                    &params,
                    if method == "questions.answer" {
                        &["requestId", "answers"]
                    } else {
                        &["requestId"]
                    },
                )?;
                let request = self
                    .store
                    .get_question(required(&params, "requestId")?)
                    .await?;
                let _thread = self.questions.thread_guard(&request.thread_id).await;
                let _task = if let Some(id) = request.task_id.as_deref() {
                    Some(self.workers.task_control_guard(id).await)
                } else {
                    None
                };
                if let Some(task_id) = request.task_id.as_deref() {
                    let task = self.store.get_task(task_id).await?;
                    if task.run_id != request.run_id
                        || matches!(task.status.as_str(), "cancelled" | "succeeded" | "failed")
                    {
                        return Err("This question belongs to a stopped task run".into());
                    }
                }
                let answers = if method == "questions.answer" {
                    Some(
                        serde_json::from_value::<QuestionAnswers>(params["answers"].clone())
                            .map_err(|e| e.to_string())?,
                    )
                } else {
                    None
                };
                let resolved = self.store.resolve_question(&request.id, answers).await?;
                self.questions_changed(Some(&request.thread_id));
                Ok(json!(resolved))
            }
            _ => Err(format!("Unsupported question method: {method}")),
        }
    }

    pub(crate) async fn observe_question_response(&self, event: &Value) -> Result<(), String> {
        if event["method"] == "serverRequest/resolved" {
            // This also means cancellation. It only invalidates the live response route.
            self.questions
                .live
                .lock()
                .unwrap()
                .remove(&event["params"]["requestId"].to_string());
            return Ok(());
        }
        if event["method"] != "item/completed" {
            return Ok(());
        }
        let params = &event["params"];
        let Some(thread_id) = params["threadId"].as_str() else {
            return Ok(());
        };
        let item = &params["item"];
        if item["type"] != "functionCallOutput" || item["name"] != "request_user_input" {
            return Ok(());
        }
        for request in self.store.list_questions(Some(thread_id), true).await? {
            if request.turn_id != params["turnId"].as_str().unwrap_or_default()
                || !matches!(request.delivery.as_str(), "dispatching" | "uncertain")
            {
                continue;
            }
            if accepted_question_output(&request, item) {
                self.store
                    .finish_question_delivery(&request.id, "delivered", None)
                    .await?;
                self.questions_changed(Some(thread_id));
            } else if item["id"].as_str() == Some(request.item_id.as_str())
                && request.delivery == "dispatching"
            {
                self.store.finish_question_delivery(&request.id,"uncertain",Some("The runtime accepted a different response to this question; the saved answer was not resent.")).await?;
                self.questions_changed(Some(thread_id));
            }
        }
        Ok(())
    }

    pub(crate) async fn poll_question_deliveries(
        self: &Arc<Self>,
        tasks: &mut JoinSet<()>,
    ) -> Result<(), String> {
        for request in self.store.list_questions(None, true).await? {
            if request.delivery == "uncertain" {
                self.reconcile_answer(&request, false).await?;
                continue;
            }
            if request.delivery != "queued" {
                continue;
            }
            if let Some(claimed) = self.store.claim_question_delivery(&request.id).await? {
                let host = Arc::downgrade(self);
                tasks.spawn(async move {
                    if let Some(host) = host.upgrade() {
                        if let Err(error) = host.deliver_question(&claimed).await {
                            let _ = host
                                .store
                                .finish_question_delivery(&claimed.id, "uncertain", Some(&error))
                                .await;
                        }
                        host.questions_changed(Some(&claimed.thread_id));
                    }
                });
            }
        }
        Ok(())
    }

    async fn defer_question(&self, request: &QuestionRequest, reason: &str) -> Result<(), String> {
        self.store
            .finish_question_delivery(&request.id, "queued", Some(reason))
            .await?;
        Ok(())
    }

    async fn deliver_question(&self, request: &QuestionRequest) -> Result<(), String> {
        let _thread = self.questions.thread_guard(&request.thread_id).await;
        let _task = if let Some(id) = request.task_id.as_deref() {
            Some(self.workers.task_control_guard(id).await)
        } else {
            None
        };
        let current = self.store.get_question(&request.id).await?;
        if current.delivery != "dispatching" || current.resolution == "closed" {
            return Ok(());
        }
        if let Err(error) = self.require_account().await {
            return self.defer_question(&current, &error).await;
        }
        let mut task_run = if current.run_id.is_some() {
            match self.activate_question_run(&current).await? {
                Some(run) => Some(run),
                None => {
                    return self
                        .defer_question(&current, "Waiting for task resume or worker capacity")
                        .await
                }
            }
        } else {
            None
        };
        let live_id = current.runtime_request_id.as_ref().filter(|id| {
            current.runtime_generation.as_deref() == Some(self.questions.generation.as_str())
                && self.questions.live.lock().unwrap().get(&id.to_string()) == Some(&current.id)
        });
        if let Some(id) = live_id {
            let answers = current
                .answers
                .as_ref()
                .map(|answers| {
                    answers
                        .iter()
                        .map(|(id, values)| (id.clone(), json!({"answers":values})))
                        .collect::<serde_json::Map<_, _>>()
                })
                .unwrap_or_default();
            // Only the handler's persisted output proves acceptance; request resolution also means cancellation.
            self.rpc
                .respond(id.clone(), json!({"answers":answers}))
                .await?;
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let request = self.store.get_question(&current.id).await?;
                    if request.delivery == "delivered" || request.resolution == "closed" {
                        return Ok::<_, String>(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(|_| {
                "The runtime did not acknowledge the saved answer; delivery needs attention"
                    .to_owned()
            })??;
            return Ok(());
        }

        let binding = self.store.session_binding(&current.thread_id).await.ok();
        let kind = binding
            .as_ref()
            .map(|b| b.kind.as_str())
            .unwrap_or("baseline");
        self.rpc
            .call(
                "thread/resume",
                json!({
                    "threadId":current.thread_id,"excludeTurns":true,
                    "developerInstructions":crate::agent_protocol::instructions(kind),
                    "config":crate::agent_protocol::thread_config(kind),
                }),
            )
            .await
            .or_else(|error| {
                Err(format!(
                    "Could not resume the question's conversation: {error}"
                ))
            })?;
        let mut turns = crate::agent_workers::thread_turns(self, &current.thread_id).await?;
        let active = turns
            .iter()
            .find(|t| matches!(t["status"].as_str(), Some("inProgress" | "in_progress")));
        let marker = current
            .client_message_id
            .as_deref()
            .ok_or("The answer has no delivery marker")?;
        if turns
            .iter()
            .any(|turn| crate::agent_workers::turn_has_client_message(turn, marker))
        {
            self.store
                .finish_question_delivery(&current.id, "delivered", None)
                .await?;
            return Ok(());
        }
        let prompt = answer_prompt(&current);
        let input = json!([{"type":"text","text":prompt,"text_elements":[]}]);
        if let Some(active) = active {
            let params = self.prepare_turn(&current.thread_id,json!({"threadId":current.thread_id,"clientUserMessageId":marker,"expectedTurnId":active["id"],"input":input})).await?;
            match self.rpc.call("turn/steer", params).await {
                Ok(_) => {
                    self.store
                        .finish_question_delivery(&current.id, "delivered", None)
                        .await?;
                    return Ok(());
                }
                Err(error) if definitive_inactive(&error) => {
                    let refreshed =
                        crate::agent_workers::thread_turns(self, &current.thread_id).await?;
                    if refreshed
                        .iter()
                        .any(|t| matches!(t["status"].as_str(), Some("inProgress" | "in_progress")))
                    {
                        return self
                            .defer_question(
                                &current,
                                "The active turn changed; the answer remains queued",
                            )
                            .await;
                    }
                    turns = refreshed;
                }
                Err(error) => return Err(format!("Could not confirm answer delivery: {error}")),
            }
        }
        if !turns
            .iter()
            .any(|t| t["id"].as_str() == Some(current.turn_id.as_str()))
        {
            return Err("The original question turn is absent from runtime history; automatic continuation is unsafe".into());
        }
        let latest = if let Some(turn_id) = task_run.as_ref().and_then(|run| run.turn_id.as_deref())
        {
            turns.iter().find(|turn|turn["id"].as_str()==Some(turn_id)).ok_or("The current task turn is absent from runtime history; automatic continuation is unsafe")?
        } else {
            turns
                .first()
                .ok_or("The conversation has no authoritative turn history")?
        };
        if !matches!(
            latest["status"].as_str(),
            Some("completed" | "interrupted" | "failed")
        ) || crate::agent_workers::has_unknown_effects(latest)
        {
            return Err("The current turn has an unresolved external tool outcome or unknown status; review it before continuing".into());
        }
        if let Some(run) = task_run.as_mut() {
            let source_turn = run.turn_id.as_deref().unwrap_or(&current.turn_id);
            *run = self
                .store
                .prepare_question_continuation(&run.task_id, &run.id, source_turn, &run.lease_token)
                .await?;
            sqlx::query("INSERT OR IGNORE INTO agent_task_submissions(run_id,client_user_message_id,continuation,dispatched,created_at) VALUES(?,?,1,1,?)")
                .bind(&run.id).bind(marker).bind(current.updated_at).execute(self.store.pool()).await.map_err(|e|e.to_string())?;
        }
        let params = self
            .prepare_turn(
                &current.thread_id,
                json!({"threadId":current.thread_id,"clientUserMessageId":marker,"input":input}),
            )
            .await?;
        let response = self.rpc.call("turn/start", params).await?;
        if let Some(run) = task_run.as_ref() {
            let turn_id = response["turn"]["id"]
                .as_str()
                .or_else(|| response["turnId"].as_str())
                .ok_or("The answer turn has no runtime ID")?;
            self.attach_question_turn(&current, run, turn_id).await?;
        }
        self.store
            .finish_question_delivery(&current.id, "delivered", None)
            .await?;
        Ok(())
    }

    /// Only tracked requests/turns are examined. Historical questions are never backfilled.
    pub(crate) async fn reconcile_questions(&self) -> Result<(), String> {
        let sources: Vec<(String, String)> = sqlx::query_as(
            "SELECT thread_id,turn_id FROM question_observed_turns WHERE reconciled=0",
        )
        .fetch_all(self.store.pool())
        .await
        .map_err(|e| e.to_string())?;
        let mut histories = HashMap::new();
        for (thread, turn_id) in sources {
            if !histories.contains_key(&thread) {
                if let Ok(turns) = crate::agent_workers::thread_turns(self, &thread).await {
                    histories.insert(thread.clone(), turns);
                }
            }
            let Some(turn) = histories.get(&thread).and_then(|turns| {
                turns
                    .iter()
                    .find(|t| t["id"].as_str() == Some(turn_id.as_str()))
            }) else {
                continue;
            };
            for item in turn["items"].as_array().into_iter().flatten() {
                if item["delivery"] == "async" && item["questions"].is_array() {
                    let event = json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn_id,"item":item}});
                    if let Err(error) = self.capture_question_inner(&event, true).await {
                        (self.events)(
                            json!({"method":"brr/questions/error","params":{"message":error}}),
                        );
                    }
                }
            }
            if !matches!(turn["status"].as_str(), Some("inProgress" | "in_progress")) {
                sqlx::query("UPDATE question_observed_turns SET reconciled=1 WHERE thread_id=? AND turn_id=?")
                    .bind(thread).bind(turn_id).execute(self.store.pool()).await.map_err(|e|e.to_string())?;
            }
        }
        for request in self.store.list_questions(None, true).await? {
            if !matches!(request.delivery.as_str(), "dispatching" | "uncertain") {
                continue;
            }
            self.reconcile_answer(&request, true).await?;
        }
        self.questions_changed(None);
        Ok(())
    }

    async fn reconcile_answer(
        &self,
        request: &QuestionRequest,
        restarting: bool,
    ) -> Result<(), String> {
        let confirmed = if let Some(marker) = request.client_message_id.as_deref() {
            crate::agent_workers::thread_turns(self, &request.thread_id)
                .await
                .ok()
                .is_some_and(|turns| {
                    turns.iter().any(|turn| {
                        crate::agent_workers::turn_has_client_message(turn, marker)
                            || (turn["id"].as_str() == Some(request.turn_id.as_str())
                                && turn["items"].as_array().is_some_and(|items| {
                                    items
                                        .iter()
                                        .any(|item| accepted_question_output(request, item))
                                }))
                    })
                })
        } else {
            false
        };
        if confirmed {
            self.store
                .finish_question_delivery(&request.id, "delivered", None)
                .await?;
            self.questions_changed(Some(&request.thread_id));
        } else if restarting && request.delivery == "dispatching" {
            self.store.finish_question_delivery(&request.id,"uncertain",Some("The runtime stopped before answer delivery was confirmed. The answer was saved and will not be resent automatically.")).await?;
        }
        Ok(())
    }
}

fn accepted_question_output(request: &QuestionRequest, item: &Value) -> bool {
    if request.kind != "blocking"
        || item["type"] != "functionCallOutput"
        || item["name"] != "request_user_input"
        || item["id"].as_str() != Some(request.item_id.as_str())
    {
        return false;
    }
    let Some(content) = item["output"].as_str() else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return false;
    };
    let answers = request
        .answers
        .as_ref()
        .map(|answers| {
            answers
                .iter()
                .map(|(id, values)| (id.clone(), json!({"answers":values})))
                .collect::<serde_json::Map<_, _>>()
        })
        .unwrap_or_default();
    value == json!({"answers":answers})
}

fn answer_prompt(request: &QuestionRequest) -> String {
    let mut text = format!(
        "User response to question request {} (from turn {}):\n",
        request.id, request.turn_id
    );
    for question in &request.questions {
        text.push_str(&format!("\n{}\n", question.question));
        if request.resolution == "dismissed" {
            text.push_str("The user dismissed this question without an answer. Do not infer a choice or authorization.\n");
        } else if let Some(answers) = request.answers.as_ref().and_then(|a| a.get(&question.id)) {
            text.push_str(&answers.join("\n"));
            text.push('\n');
        }
    }
    text.push_str("\nContinue from the existing conversation using this response. Preserve completed work and do not repeat operations with uncertain outcomes.");
    text
}

fn definitive_inactive(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("no active turn")
        || error.contains("not currently active")
        || error.contains("expected turn id")
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn fields(params: &Value, allowed: &[&str]) -> Result<(), String> {
    let fields = params
        .as_object()
        .ok_or("Question parameters must be an object")?;
    if let Some(key) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("Unsupported question field: {key}"));
    }
    Ok(())
}
