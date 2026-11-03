//! Host authority for model-authored, saved interactive documents.
use crate::{agent_host::AgentHost, bridge::Engine};
use brr_agent_core::{AgentStore, SessionBinding, Task, TaskRun};
use serde_json::{json, Map, Value};
use sha2::Digest;

const MAX_DOCUMENT: usize = 5 * 1024 * 1024;
const MAX_STATE: usize = 64 * 1024;
const FORBIDDEN_KEYS: &[&str] = &["__proto__", "prototype", "constructor"];

#[derive(Clone)]
struct Identity {
    thread: String,
    call: String,
    hash: String,
}

pub(crate) async fn init(host: &AgentHost) -> Result<(), String> {
    sqlx::query("CREATE TABLE IF NOT EXISTS intelligent_ui_state(thread_id TEXT NOT NULL,call_id TEXT NOT NULL,document_hash TEXT NOT NULL,state TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(thread_id,call_id,document_hash))")
        .execute(host.store.pool()).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS intelligent_ui_reviews(id TEXT PRIMARY KEY,thread_id TEXT NOT NULL,call_id TEXT NOT NULL,document_hash TEXT NOT NULL,state_revision INTEGER NOT NULL,action_id TEXT NOT NULL,text TEXT NOT NULL,task_id TEXT,agent_id TEXT,project_id TEXT,cwd TEXT,claimed INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL DEFAULT (unixepoch()))")
        .execute(host.store.pool()).await.map_err(|e|e.to_string())?;
    Ok(())
}

async fn session_binding_or_none(
    store: &AgentStore,
    thread_id: &str,
) -> Result<Option<SessionBinding>, String> {
    match store.session_binding(thread_id).await {
        Ok(binding) => Ok(Some(binding)),
        Err(error) if error == "session binding not found" => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) async fn render(host: &AgentHost, params: Value) -> Result<Value, String> {
    let thread = required(&params, "threadId")?;
    let binding = session_binding_or_none(&host.store, thread).await?;
    if let Some(binding) = binding.as_ref() {
        if !matches!(binding.kind.as_str(), "baseline" | "agent" | "task") {
            return Err("Intelligent UI is unavailable in this session".into());
        }
    } else {
        // Existing runtime threads predate host bindings. Verify that this is a real
        // source thread, but do not create a memory-capable binding for it.
        host.rpc
            .call(
                "thread/read",
                json!({"threadId":thread,"includeTurns":false}),
            )
            .await?;
    }
    let _control = if let Some(binding) = binding.as_ref().filter(|b| b.kind == "task") {
        validate_task_binding(binding)?;
        let run = host
            .store
            .get_run(
                binding
                    .task_run_id
                    .as_deref()
                    .ok_or("Task session has no run")?,
            )
            .await?;
        let guard = host.workers.task_control_guard(&run.task_id).await;
        let current = host.store.get_run(&run.id).await?;
        if current.state != "running"
            || params["brrHostLease"].as_str() != Some(current.lease_token.as_str())
        {
            return Err("The UI call belongs to a stale or stopped task lease".into());
        }
        Some(guard)
    } else {
        None
    };
    let call = required(&params, "callId")?;
    let doc = params
        .get("arguments")
        .cloned()
        .ok_or("UI document is required")?;
    validate_document(&doc)?;
    let hash = canonical_hash(&doc)?;
    Ok(json!({"documentHash":hash,"document":doc,"threadId":thread,"callId":call}))
}

pub(crate) async fn request(engine: &Engine, method: &str, params: Value) -> Result<Value, String> {
    match method {
        "document.read" => read_document(engine, &params).await,
        "state.read" => read_state(engine, &params).await,
        "state.write" => write_state(engine, &params).await,
        "action.prepare" => prepare_action(engine, &params).await,
        "action.claim" => claim_action(engine, &params).await,
        "sandbox.create" => crate::generated_ui_sandbox::create_for_document(engine, &params).await,
        "sandbox.destroy" => crate::generated_ui_sandbox::destroy_for_token(engine, &params).await,
        _ => Err("Unsupported intelligent UI method".into()),
    }
}

pub(crate) async fn read_document(engine: &Engine, p: &Value) -> Result<Value, String> {
    let thread = required(p, "threadId")?;
    let call = required(p, "callId")?;
    let binding = session_binding_or_none(&engine.agents.store, thread).await?;
    let turns = crate::agent_workers::thread_turns(&engine.agents, thread).await?;
    let (doc, hash) = resolve_document(&turns, call)?;
    let mut out = json!({"document":doc,"documentHash":hash});
    if let Some(binding) = binding.as_ref().filter(|b| b.kind == "task") {
        out["task"] = resolve_task_review_info(engine, binding)
            .await?
            .identity_json();
    }
    Ok(out)
}

fn resolve_document(turns: &[Value], call: &str) -> Result<(Value, String), String> {
    for turn in turns.iter().rev() {
        let Some(items) = turn["items"].as_array() else {
            continue;
        };
        for item in items.iter().rev() {
            if item["type"] != "dynamicToolCall"
                || item["id"].as_str() != Some(call)
                || item["tool"] != "brr_ui_render"
                || item["status"] != "completed"
                || item["success"] != true
            {
                continue;
            }
            let result = tool_result(item);
            let host_result = parse_host_result(&result);
            let doc = host_result
                .get("document")
                .cloned()
                .unwrap_or_else(|| item["arguments"].clone());
            validate_document(&doc)?;
            let hash = canonical_hash(&doc)?;
            if host_result["documentHash"].as_str() != Some(hash.as_str()) {
                continue;
            }
            return Ok((doc, hash));
        }
    }
    Err("No finalized authoritative UI document exists for this call".into())
}

fn tool_result(item: &Value) -> Value {
    if let Some(content) = item.get("contentItems") {
        return json!({"contentItems":content});
    }
    for key in ["result", "output"] {
        if let Some(v) = item.get(key) {
            return v.clone();
        }
    }
    Value::Null
}
fn parse_host_result(result: &Value) -> Value {
    if let Some(v) = result.get("hostResult") {
        return v.clone();
    }
    if let Some(v) = result.get("result") {
        return parse_host_result(v);
    }
    if let Some(a) = result.get("contentItems").and_then(Value::as_array) {
        for part in a {
            if let Some(s) = part["text"].as_str() {
                if let Ok(v) = serde_json::from_str::<Value>(s) {
                    if v.get("documentHash").is_some() {
                        return v;
                    }
                }
            }
        }
    }
    Value::Null
}

async fn identity(
    engine: &Engine,
    p: &Value,
    need_hash: bool,
) -> Result<(Identity, Value), String> {
    let thread = required(p, "threadId")?.to_owned();
    let call = required(p, "callId")?.to_owned();
    let doc = read_document(engine, &json!({"threadId":thread,"callId":call})).await?;
    let hash = required(p, "documentHash")?.to_owned();
    if need_hash && doc["documentHash"].as_str() != Some(hash.as_str()) {
        return Err("Document identity is stale".into());
    }
    Ok((Identity { thread, call, hash }, doc["document"].clone()))
}

pub(crate) async fn read_state(engine: &Engine, p: &Value) -> Result<Value, String> {
    let (id, doc) = identity(engine, p, true).await?;
    let defaults = doc["initialState"].clone();
    check_state(&defaults)?;
    validate_bound_state(&doc, &defaults)?;
    let row:Option<(String,i64)>=sqlx::query_as("SELECT state,revision FROM intelligent_ui_state WHERE thread_id=? AND call_id=? AND document_hash=?")
        .bind(&id.thread).bind(&id.call).bind(&id.hash).fetch_optional(engine.agents.store.pool()).await.map_err(|e|e.to_string())?;
    let (state, revision) = if let Some((state, revision)) = row {
        (
            serde_json::from_str(&state).map_err(|e| e.to_string())?,
            revision,
        )
    } else {
        (defaults, 0)
    };
    Ok(json!({"state":state,"revision":revision}))
}
async fn write_state(engine: &Engine, p: &Value) -> Result<Value, String> {
    let (id, doc) = identity(engine, p, true).await?;
    let state = p.get("state").cloned().ok_or("state is required")?;
    check_state(&state)?;
    validate_bound_state(&doc, &state)?;
    let rev = p["revision"].as_i64().ok_or("revision is required")?;
    if rev < 0 {
        return Err("revision must be nonnegative".into());
    }
    let serialized = canonical(&state)?;
    let next = write_state_row(engine.agents.store.pool(), &id, &serialized, rev).await?;
    Ok(json!({"state":state,"revision":next}))
}

async fn write_state_row(
    pool: &sqlx::SqlitePool,
    id: &Identity,
    state: &str,
    expected: i64,
) -> Result<i64, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let row:Option<i64>=sqlx::query_scalar("SELECT revision FROM intelligent_ui_state WHERE thread_id=? AND call_id=? AND document_hash=?")
        .bind(&id.thread).bind(&id.call).bind(&id.hash).fetch_optional(&mut *tx).await.map_err(|e|e.to_string())?;
    let current = row.unwrap_or(0);
    if current != expected {
        return Err("State revision is stale".into());
    }
    let next = current + 1;
    sqlx::query("INSERT INTO intelligent_ui_state(thread_id,call_id,document_hash,state,revision) VALUES(?,?,?,?,?) ON CONFLICT(thread_id,call_id,document_hash) DO UPDATE SET state=excluded.state,revision=excluded.revision")
        .bind(&id.thread).bind(&id.call).bind(&id.hash).bind(state).bind(next).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(next)
}

async fn prepare_action(engine: &Engine, p: &Value) -> Result<Value, String> {
    let (id, doc) = identity(engine, p, true).await?;
    let action_id = required(p, "actionId")?;
    let action = doc["actions"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["id"] == action_id))
        .ok_or("Action is unavailable")?;
    let saved = read_state(engine, p).await?;
    let inputs = stateful_inputs(&doc, &saved["state"]);
    let binding = session_binding_or_none(&engine.agents.store, &id.thread).await?;
    let task = match binding.as_ref().filter(|b| b.kind == "task") {
        Some(binding) => Some(resolve_task_review_info(engine, binding).await?),
        None => None,
    };
    let mut text = format!(
        "{}\n\nCaptured inputs:\n{}",
        interpolate(
            action["prompt"]
                .as_str()
                .ok_or("Action prompt is invalid")?,
            &inputs
        ),
        canonical(&saved["state"])?
    );
    if let Some(task) = task.as_ref() {
        text.push_str(&format!("\n\nSource task: {} ({}).", task.title, task.id));
    }
    let review_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO intelligent_ui_reviews(id,thread_id,call_id,document_hash,state_revision,action_id,text,task_id,agent_id,project_id,cwd) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&review_id).bind(&id.thread).bind(&id.call).bind(&id.hash).bind(saved["revision"].as_i64()).bind(action_id).bind(&text).bind(task.as_ref().map(|t|t.id.as_str())).bind(task.as_ref().map(|t|t.agent_id.as_str())).bind(task.as_ref().and_then(|t|t.project_id.as_deref())).bind(task.as_ref().and_then(|t|t.cwd.as_deref())).execute(engine.agents.store.pool()).await.map_err(|e|e.to_string())?;
    let mut out = json!({"reviewId":review_id,"text":text,"threadId":id.thread,"callId":id.call,"documentHash":id.hash});
    if let Some(task) = task.as_ref() {
        out["task"] = task.identity_json();
    }
    Ok(out)
}

struct TaskReviewInfo {
    id: String,
    agent_id: String,
    project_id: Option<String>,
    cwd: Option<String>,
    title: String,
}

impl TaskReviewInfo {
    fn identity_json(&self) -> Value {
        let mut task = json!({"id":self.id,"agentId":self.agent_id});
        if let Some(project_id) = &self.project_id {
            task["projectId"] = json!(project_id);
        }
        if let Some(cwd) = &self.cwd {
            task["cwd"] = json!(cwd);
        }
        task
    }
}

fn validate_task_binding(binding: &SessionBinding) -> Result<(&str, &str), String> {
    if binding.kind != "task" {
        return Err("Session is not a task session".into());
    }
    let run_id = binding
        .task_run_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or("Task session has no run")?;
    let agent_id = binding
        .agent_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or("Task session has no owner")?;
    Ok((run_id, agent_id))
}

async fn resolve_task_review_info(
    engine: &Engine,
    binding: &SessionBinding,
) -> Result<TaskReviewInfo, String> {
    let (run_id, owner_id) = validate_task_binding(binding)?;
    let run = engine.agents.store.get_run(run_id).await?;
    let task = engine.agents.store.get_task(&run.task_id).await?;
    validate_task_run_association(binding, owner_id, &run, &task)?;
    Ok(TaskReviewInfo {
        id: task.id,
        agent_id: task.agent_id,
        project_id: task.project_id,
        cwd: task.cwd,
        title: task.title,
    })
}

fn validate_task_run_association(
    binding: &SessionBinding,
    owner_id: &str,
    run: &TaskRun,
    task: &Task,
) -> Result<(), String> {
    if run.agent_id != owner_id || task.agent_id != owner_id || task.id != run.task_id {
        return Err("Task metadata does not match its source run".into());
    }
    if run
        .thread_id
        .as_deref()
        .is_some_and(|thread_id| thread_id != binding.thread_id)
    {
        return Err("Task source run does not belong to this thread".into());
    }
    // task.run_id is mutable and points at the latest retry. Historical source
    // threads remain valid through the immutable run.task_id/agent_id association.
    Ok(())
}

async fn claim_action(engine: &Engine, p: &Value) -> Result<Value, String> {
    let review = required(p, "reviewId")?;
    let row:Option<(String,String,String,i64,String,Option<String>,Option<String>,Option<String>,Option<String>)>=sqlx::query_as("SELECT thread_id,call_id,document_hash,state_revision,text,task_id,agent_id,project_id,cwd FROM intelligent_ui_reviews WHERE id=? AND claimed=0")
        .bind(review).fetch_optional(engine.agents.store.pool()).await.map_err(|e|e.to_string())?;
    let (thread, call, hash, revision, text, task_id, agent_id, project_id, cwd) =
        row.ok_or("Review is unavailable or already claimed")?;
    let authoritative = read_document(engine, &json!({"threadId":thread,"callId":call})).await?;
    if authoritative["documentHash"].as_str() != Some(hash.as_str()) {
        return Err("Review document is stale".into());
    }
    let current = read_state(
        engine,
        &json!({"threadId":thread,"callId":call,"documentHash":hash}),
    )
    .await?;
    if current["revision"].as_i64() != Some(revision) {
        return Err("Review state is stale".into());
    }
    claim_review_row(
        engine.agents.store.pool(),
        review,
        &thread,
        &call,
        &hash,
        revision,
    )
    .await?;
    claimed_review_payload(
        review, &thread, &call, &hash, &text, task_id, agent_id, project_id, cwd,
    )
}

fn claimed_review_payload(
    review_id: &str,
    thread: &str,
    call: &str,
    hash: &str,
    text: &str,
    task_id: Option<String>,
    agent_id: Option<String>,
    project_id: Option<String>,
    cwd: Option<String>,
) -> Result<Value, String> {
    let mut out = json!({"reviewId":review_id,"documentHash":hash,"text":text,"threadId":thread,"callId":call});
    match (task_id, agent_id) {
        (None, None) => {
            if project_id.is_some() || cwd.is_some() {
                return Err("Review task metadata is incomplete".into());
            }
        }
        (Some(id), Some(agent_id)) if !id.trim().is_empty() && !agent_id.trim().is_empty() => {
            let mut task = json!({"id":id,"agentId":agent_id});
            if let Some(project_id) = project_id {
                task["projectId"] = json!(project_id);
            }
            if let Some(cwd) = cwd {
                task["cwd"] = json!(cwd);
            }
            out["task"] = task;
        }
        _ => return Err("Review task metadata is incomplete".into()),
    }
    Ok(out)
}

async fn claim_review_row(
    pool: &sqlx::SqlitePool,
    review: &str,
    thread: &str,
    call: &str,
    hash: &str,
    revision: i64,
) -> Result<(), String> {
    let result=sqlx::query("UPDATE intelligent_ui_reviews SET claimed=1 WHERE id=? AND claimed=0 AND state_revision=? AND document_hash=? AND state_revision=COALESCE((SELECT revision FROM intelligent_ui_state WHERE thread_id=? AND call_id=? AND document_hash=?),0)")
        .bind(review).bind(revision).bind(hash).bind(thread).bind(call).bind(hash).execute(pool).await.map_err(|e|e.to_string())?;
    if result.rows_affected() != 1 {
        return Err("Review is stale or already claimed".into());
    }
    Ok(())
}

fn validate_document(d: &Value) -> Result<(), String> {
    if !d.is_object() || d["version"] != 1 || !matches!(d["kind"].as_str(), Some("native" | "app"))
    {
        return Err("Unsupported Intelligent UI document".into());
    }
    let allowed: &[&str] = if d["kind"] == "native" {
        &[
            "version",
            "kind",
            "title",
            "text",
            "initialState",
            "actions",
            "components",
        ]
    } else {
        &[
            "version",
            "kind",
            "title",
            "text",
            "initialState",
            "actions",
            "html",
        ]
    };
    if d.as_object()
        .unwrap()
        .keys()
        .any(|k| !allowed.contains(&k.as_str()))
    {
        return Err("Document contains unsupported host or RPC fields".into());
    }
    if required(d, "title").is_err() || required(d, "text").is_err() {
        return Err("Document title and accessible text are required".into());
    }
    check_state(&d["initialState"])?;
    let actions = d["actions"].as_array().ok_or("actions must be an array")?;
    if actions.len() > 64 {
        return Err("A UI document may contain at most 64 actions".into());
    }
    let mut action_ids = std::collections::HashSet::new();
    for a in actions {
        if a.as_object().is_none_or(|o| {
            o.keys()
                .any(|k| !matches!(k.as_str(), "id" | "label" | "prompt"))
        }) {
            return Err("Unsupported action field".into());
        }
        let action_id = required(a, "id")?;
        if !safe_key(action_id) || !action_ids.insert(action_id.to_owned()) {
            return Err("Action IDs must be unique".into());
        }
        required(a, "label")?;
        if !a["prompt"].is_string() {
            return Err("Action prompt must be text".into());
        }
    }
    if d["kind"] == "app" {
        let html = required(d, "html")?;
        if html.trim().is_empty() || html.len() > MAX_DOCUMENT {
            return Err("App HTML must be nonempty and no larger than 5 MB".into());
        }
    } else {
        let components = d["components"]
            .as_array()
            .ok_or("Native UI components are required")?;
        let mut component_ids = std::collections::HashSet::new();
        let mut count = 0;
        for c in components {
            validate_component(c, 0, &mut component_ids, &mut count)?;
        }
        validate_action_references(components, &action_ids)?;
        validate_bound_state(d, &d["initialState"])?;
    }
    let bytes = serde_json::to_vec(d).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_DOCUMENT {
        return Err("UI document exceeds 5 MB".into());
    }
    Ok(())
}
fn validate_component(
    c: &Value,
    depth: usize,
    ids: &mut std::collections::HashSet<String>,
    count: &mut usize,
) -> Result<(), String> {
    if depth >= 16 {
        return Err("UI component nesting exceeds 16".into());
    }
    *count += 1;
    if *count > 256 {
        return Err("A UI document may contain at most 256 components".into());
    }
    let id = required(c, "id")?.to_owned();
    if FORBIDDEN_KEYS.contains(&id.as_str()) || !ids.insert(id) {
        return Err("Component IDs must be unique and safe".into());
    }
    let ty = required(c, "type")?;
    let fields: &[&str] = match ty {
        "text" => &["id", "type", "text", "visibleWhen"],
        "group" => &["id", "type", "children", "visibleWhen"],
        "comparison" => &["id", "type", "columns", "visibleWhen"],
        "table" => &["id", "type", "columns", "rows", "visibleWhen"],
        "timeline" => &["id", "type", "entries", "visibleWhen"],
        "chart" => &["id", "type", "chartType", "points", "visibleWhen"],
        "diagram" => &["id", "type", "nodes", "edges", "visibleWhen"],
        "tabs" => &["id", "type", "tabs", "visibleWhen"],
        "select" => &["id", "type", "label", "stateKey", "options", "visibleWhen"],
        "slider" => &[
            "id",
            "type",
            "label",
            "stateKey",
            "min",
            "max",
            "step",
            "visibleWhen",
        ],
        "input" => &[
            "id",
            "type",
            "label",
            "stateKey",
            "inputType",
            "visibleWhen",
        ],
        "form" => &["id", "type", "label", "children", "actionId", "visibleWhen"],
        "button" => &["id", "type", "label", "actionId", "visibleWhen"],
        _ => return Err("Unsupported native component type".into()),
    };
    if c.as_object()
        .is_none_or(|o| o.keys().any(|k| !fields.contains(&k.as_str())))
    {
        return Err("Unsupported native component field".into());
    }
    if let Some(when) = c.get("visibleWhen") {
        if when
            .as_object()
            .is_none_or(|o| o.keys().any(|k| !matches!(k.as_str(), "key" | "equals")))
        {
            return Err("visibleWhen has unsupported fields".into());
        }
        let key = required(when, "key")?;
        if !safe_key(key)
            || when
                .get("equals")
                .is_none_or(|v| !(v.is_null() || v.is_string() || v.is_boolean() || v.is_number()))
        {
            return Err("visibleWhen must compare a safe key to a scalar".into());
        }
    }
    match ty {
        "text" => {
            if !c["text"].is_string() {
                return Err("Text component needs text".into());
            }
        }
        "group" | "form" => {
            if ty == "form" {
                required(c, "label")?;
                if c.get("actionId")
                    .is_some_and(|v| v.as_str().is_none_or(|s| !safe_key(s)))
                {
                    return Err("Invalid form action".into());
                }
            }
            let ch = c["children"]
                .as_array()
                .ok_or("Component children are required")?;
            for child in ch {
                validate_component(child, depth + 1, ids, count)?
            }
        }
        "tabs" => {
            let tabs = c["tabs"].as_array().ok_or("tabs are required")?;
            if tabs.is_empty() {
                return Err("Tabs cannot be empty".into());
            }
            let mut tab_ids = std::collections::HashSet::new();
            if tabs.is_empty() {
                return Err("Tabs cannot be empty".into());
            }
            for tab in tabs {
                let tid = required(tab, "id")?;
                if !safe_key(tid) || !tab_ids.insert(tid) {
                    return Err("Tab IDs must be unique and safe".into());
                }
                required(tab, "label")?;
                if tab.as_object().is_none_or(|o| {
                    o.keys()
                        .any(|k| !matches!(k.as_str(), "id" | "label" | "children"))
                }) {
                    return Err("Unsupported tab field".into());
                }
                for child in tab["children"]
                    .as_array()
                    .ok_or("Tab children are required")?
                {
                    validate_component(child, depth + 1, ids, count)?
                }
            }
        }
        "select" => {
            required(c, "label")?;
            safe_state_key(c)?;
            let opts = c["options"]
                .as_array()
                .ok_or("Select options are required")?;
            if opts.is_empty() {
                return Err("Select options cannot be empty".into());
            }
            for o in opts {
                required(o, "label")?;
                if o.as_object()
                    .is_none_or(|m| m.keys().any(|k| !matches!(k.as_str(), "label" | "value")))
                    || !o["value"].is_string()
                {
                    return Err("Invalid select option".into());
                }
            }
        }
        "slider" => {
            required(c, "label")?;
            safe_state_key(c)?;
            let min = c["min"].as_f64().ok_or("Slider min is required")?;
            let max = c["max"].as_f64().ok_or("Slider max is required")?;
            if !min.is_finite()
                || !max.is_finite()
                || max <= min
                || c.get("step")
                    .is_some_and(|s| s.as_f64().is_none_or(|n| !n.is_finite() || n <= 0.0))
            {
                return Err("Slider range or step is invalid".into());
            }
        }
        "input" => {
            required(c, "label")?;
            safe_state_key(c)?;
            if !matches!(c["inputType"].as_str().unwrap_or("text"), "text" | "number") {
                return Err("Unsupported input type".into());
            }
        }
        "button" => {
            required(c, "label")?;
            required(c, "actionId")?;
        }
        "comparison" | "table" | "timeline" | "chart" | "diagram" => {
            validate_data_component(ty, c)?
        }
        _ => {}
    }
    Ok(())
}
fn safe_state_key(c: &Value) -> Result<&str, String> {
    let k = required(c, "stateKey")?;
    if !safe_key(k) {
        return Err("State key is unsafe".into());
    }
    Ok(k)
}
fn safe_key(k: &str) -> bool {
    !k.trim().is_empty() && !FORBIDDEN_KEYS.contains(&k)
}
fn validate_action_references(
    components: &[Value],
    actions: &std::collections::HashSet<String>,
) -> Result<(), String> {
    fn visit(c: &Value, actions: &std::collections::HashSet<String>) -> Result<(), String> {
        if let Some(id) = c.get("actionId") {
            let id = id.as_str().ok_or("actionId must be text")?;
            if !actions.contains(id) {
                return Err("Component refers to an unknown action".into());
            }
        }
        if let Some(children) = c["children"].as_array() {
            for child in children {
                visit(child, actions)?
            }
        }
        if let Some(tabs) = c["tabs"].as_array() {
            for tab in tabs {
                for child in tab["children"].as_array().into_iter().flatten() {
                    visit(child, actions)?
                }
            }
        }
        Ok(())
    }
    for c in components {
        visit(c, actions)?
    }
    Ok(())
}
fn validate_bound_state(doc: &Value, state: &Value) -> Result<(), String> {
    fn walk(c: &Value, state: &Value) -> Result<(), String> {
        let ty = c["type"].as_str().unwrap_or("");
        if matches!(ty, "select" | "slider" | "input") {
            let k = safe_state_key(c)?;
            let v = state
                .get(k)
                .ok_or("Initial state is missing a bound control value")?;
            let valid = match ty {
                "select" => v.as_str().is_some_and(|s| {
                    c["options"]
                        .as_array()
                        .is_some_and(|o| o.iter().any(|x| x["value"] == s))
                }),
                "slider" => v.as_f64().is_some_and(|n| {
                    n.is_finite()
                        && n >= c["min"].as_f64().unwrap_or(f64::INFINITY)
                        && n <= c["max"].as_f64().unwrap_or(f64::NEG_INFINITY)
                }),
                "input" => match c["inputType"].as_str().unwrap_or("text") {
                    "number" => v.as_f64().is_some_and(f64::is_finite),
                    _ => v.is_string(),
                },
                _ => false,
            };
            if !valid {
                return Err(format!(
                    "State value for control {k} has the wrong type or range"
                ));
            }
        }
        if ty == "tabs" {
            let key = required(c, "id")?;
            if let Some(value) = state.get(key) {
                let valid = value.as_str().is_some_and(|v| {
                    c["tabs"]
                        .as_array()
                        .is_some_and(|tabs| tabs.iter().any(|t| t["id"] == v))
                });
                if !valid {
                    return Err(format!("State value for tabs {key} is invalid"));
                }
            }
        }
        if let Some(a) = c["children"].as_array() {
            for child in a {
                walk(child, state)?
            }
        }
        if let Some(tabs) = c["tabs"].as_array() {
            for tab in tabs {
                for child in tab["children"]
                    .as_array()
                    .ok_or("Tab children are required")?
                {
                    walk(child, state)?
                }
            }
        }
        Ok(())
    }
    if doc["kind"] == "native" {
        for c in doc["components"].as_array().into_iter().flatten() {
            walk(c, state)?
        }
    }
    Ok(())
}
fn validate_data_component(ty: &str, c: &Value) -> Result<(), String> {
    match ty {
        "comparison" => {
            let a = c["columns"]
                .as_array()
                .ok_or("Comparison columns are required")?;
            if a.is_empty() {
                return Err("Comparison columns cannot be empty".into());
            }
            for x in a {
                if x.as_object()
                    .is_none_or(|o| o.keys().any(|k| !matches!(k.as_str(), "label" | "text")))
                    || required(x, "label").is_err()
                    || !x["text"].is_string()
                {
                    return Err("Invalid comparison column".into());
                }
            }
        }
        "table" => {
            let cols = c["columns"]
                .as_array()
                .ok_or("Table columns are required")?;
            if cols.is_empty() || cols.iter().any(|v| !v.is_string()) {
                return Err("Table columns must be strings".into());
            }
            let rows = c["rows"].as_array().ok_or("Table rows are required")?;
            if rows.len() > 2000 {
                return Err("Table row limit exceeded".into());
            }
            for row in rows {
                let cells = row.as_array().ok_or("Table rows must be arrays")?;
                if cells.len() != cols.len()
                    || cells
                        .iter()
                        .any(|v| !(v.is_string() || v.is_number() || v.is_boolean() || v.is_null()))
                {
                    return Err("Invalid table row".into());
                }
            }
        }
        "timeline" => {
            let a = c["entries"]
                .as_array()
                .ok_or("Timeline entries are required")?;
            for x in a {
                if x.as_object()
                    .is_none_or(|o| o.keys().any(|k| !matches!(k.as_str(), "label" | "text")))
                    || required(x, "label").is_err()
                    || !x["text"].is_string()
                {
                    return Err("Invalid timeline entry".into());
                }
            }
        }
        "chart" => {
            if !matches!(c["chartType"].as_str(), Some("bar" | "line")) {
                return Err("Unsupported chart type".into());
            }
            let a = c["points"].as_array().ok_or("Chart points are required")?;
            if a.len() > 2000 {
                return Err("Chart point limit exceeded".into());
            }
            for x in a {
                if x.as_object()
                    .is_none_or(|o| o.keys().any(|k| !matches!(k.as_str(), "label" | "value")))
                    || required(x, "label").is_err()
                {
                    return Err("Invalid chart point".into());
                }
                if x["value"].as_f64().is_none_or(|n| !n.is_finite()) {
                    return Err("Chart values must be finite numbers".into());
                }
            }
        }
        "diagram" => {
            let nodes = c["nodes"].as_array().ok_or("Diagram nodes are required")?;
            let mut ids = std::collections::HashSet::new();
            for n in nodes {
                if n.as_object().is_none_or(|o| {
                    o.keys()
                        .any(|k| !matches!(k.as_str(), "id" | "label" | "x" | "y"))
                }) {
                    return Err("Invalid diagram node fields".into());
                }
                let id = required(n, "id")?;
                if !safe_key(id)
                    || required(n, "label").is_err()
                    || !ids.insert(id)
                    || n["x"].as_f64().is_none_or(|x| !x.is_finite())
                    || n["y"].as_f64().is_none_or(|y| !y.is_finite())
                {
                    return Err("Invalid diagram node".into());
                }
            }
            let edges = c["edges"].as_array().ok_or("Diagram edges are required")?;
            for e in edges {
                if e.as_object()
                    .is_none_or(|o| o.keys().any(|k| !matches!(k.as_str(), "from" | "to")))
                {
                    return Err("Invalid diagram edge fields".into());
                }
                let from = required(e, "from")?;
                let to = required(e, "to")?;
                if !ids.contains(from) || !ids.contains(to) {
                    return Err("Diagram edge points to an unknown node".into());
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn check_state(v: &Value) -> Result<(), String> {
    if !v.is_object() {
        return Err("State must be a JSON object".into());
    }
    if serde_json::to_vec(v).map_err(|e| e.to_string())?.len() > MAX_STATE {
        return Err("State exceeds 64 KiB".into());
    }
    fn walk(v: &Value, depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        match v {
            Value::Object(m) => m
                .iter()
                .all(|(k, v)| !FORBIDDEN_KEYS.contains(&k.as_str()) && walk(v, depth + 1)),
            Value::Array(a) => a.iter().all(|v| walk(v, depth + 1)),
            Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
            _ => true,
        }
    }
    if !walk(v, 0) {
        return Err("State contains an unsafe key, excessive nesting or non-finite number".into());
    }
    Ok(())
}
fn canonical(v: &Value) -> Result<String, String> {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                let mut o = Map::new();
                for k in keys {
                    o.insert(k.clone(), sort(&m[k]));
                }
                Value::Object(o)
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            _ => v.clone(),
        }
    }
    serde_json::to_string(&sort(v)).map_err(|e| e.to_string())
}
fn canonical_hash(v: &Value) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        sha2::Sha256::digest(canonical(v)?.as_bytes())
    ))
}
fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{k} is required"))
}
fn stateful_inputs(doc: &Value, state: &Value) -> Map<String, Value> {
    let mut m = doc["initialState"].as_object().cloned().unwrap_or_default();
    if let Some(s) = state.as_object() {
        m.extend(s.clone())
    }
    m
}
fn interpolate(s: &str, state: &Map<String, Value>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        rest = &rest[i + 2..];
        if let Some(end) = rest.find("}}") {
            let key = &rest[..end];
            if let Some(v) = state.get(key) {
                out.push_str(
                    match v {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        _ => v.to_string(),
                    }
                    .as_str(),
                )
            }
            rest = &rest[end + 2..]
        } else {
            out.push_str("{{");
            break;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_document_fixtures_match_the_host_validator() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../fixtures/intelligent-ui-documents.json"))
                .unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let result = validate_document(&case["document"]);
            assert_eq!(
                result.is_ok(),
                case["valid"].as_bool().unwrap(),
                "fixture {}: {:?}",
                case["name"],
                result.err()
            );
        }
    }
    #[test]
    fn malformed_task_binding_metadata_is_rejected() {
        let mut binding = SessionBinding {
            thread_id: "thread-1".into(),
            agent_id: Some("agent-1".into()),
            kind: "task".into(),
            project_id: None,
            cwd: None,
            task_run_id: None,
            learning_eligible: false,
        };
        assert!(validate_task_binding(&binding)
            .unwrap_err()
            .contains("no run"));
        binding.task_run_id = Some("run-1".into());
        binding.agent_id = None;
        assert!(validate_task_binding(&binding)
            .unwrap_err()
            .contains("no owner"));
        binding.agent_id = Some(" ".into());
        assert!(validate_task_binding(&binding)
            .unwrap_err()
            .contains("no owner"));
    }
    #[test]
    fn historical_task_ui_source_survives_a_task_retry_but_rejects_wrong_ownership() {
        let binding = SessionBinding {
            thread_id: "source-thread".into(),
            agent_id: Some("agent-1".into()),
            kind: "task".into(),
            project_id: None,
            cwd: None,
            task_run_id: Some("old-run".into()),
            learning_eligible: false,
        };
        let run = TaskRun {
            id: "old-run".into(),
            task_id: "task-1".into(),
            agent_id: "agent-1".into(),
            worker_id: "worker-1".into(),
            lease_token: "old-lease".into(),
            thread_id: Some("source-thread".into()),
            turn_id: None,
            state: "completed".into(),
            created_at: 1,
            settled_at: Some(2),
        };
        let task = Task {
            id: "task-1".into(),
            agent_id: "agent-1".into(),
            title: "Task".into(),
            instructions: "Instructions".into(),
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
            run_id: Some("new-retry-run".into()),
            thread_id: None,
            result: None,
            evidence: None,
            created_at: 1,
            updated_at: 3,
        };

        assert!(validate_task_run_association(&binding, "agent-1", &run, &task).is_ok());
        assert!(validate_task_run_association(&binding, "agent-2", &run, &task).is_err());

        let mut wrong_source = run.clone();
        wrong_source.thread_id = Some("different-thread".into());
        assert!(validate_task_run_association(&binding, "agent-1", &wrong_source, &task).is_err());
    }
    #[test]
    fn claimed_action_returns_the_full_immutable_review_identity() {
        let result = claimed_review_payload(
            "review-1",
            "thread-1",
            "call-1",
            "hash-1",
            "Reviewed prompt",
            Some("task-1".into()),
            Some("agent-1".into()),
            Some("project-1".into()),
            Some("/workspace/project".into()),
        )
        .unwrap();
        assert_eq!(result["reviewId"], "review-1");
        assert_eq!(result["documentHash"], "hash-1");
        assert_eq!(result["threadId"], "thread-1");
        assert_eq!(result["callId"], "call-1");
        assert_eq!(result["text"], "Reviewed prompt");
        assert_eq!(result["task"]["id"], "task-1");
        assert_eq!(result["task"]["agentId"], "agent-1");
        assert_eq!(result["task"]["projectId"], "project-1");
        assert_eq!(result["task"]["cwd"], "/workspace/project");
        assert!(claimed_review_payload(
            "review-2",
            "thread-1",
            "call-1",
            "hash-1",
            "text",
            Some("task-1".into()),
            None,
            None,
            None
        )
        .is_err());
    }
    #[test]
    fn canonical_identity_ignores_object_insertion_order() {
        assert_eq!(
            canonical_hash(&json!({"b":2,"a":1})).unwrap(),
            canonical_hash(&json!({"a":1,"b":2})).unwrap()
        );
    }
    #[test]
    fn document_limits_and_schema_are_enforced() {
        let doc = json!({"version":1,"kind":"app","title":"x","text":"x","initialState":{},"actions":[],"html":"<p>x</p>"});
        validate_document(&doc).unwrap();
        let mut bad = doc.clone();
        bad["initialState"] = json!([]);
        assert!(validate_document(&bad).is_err());
        let mut huge = doc;
        huge["html"] = json!("x".repeat(MAX_DOCUMENT));
        assert!(validate_document(&huge).is_err());
    }
    #[test]
    fn state_rejects_prototype_keys_and_oversize_values() {
        assert!(check_state(&json!({"__proto__":1})).is_err());
        assert!(check_state(&json!({"ok":"x".repeat(MAX_STATE)})).is_err());
    }
    #[test]
    fn bound_controls_require_matching_initial_and_saved_values() {
        let d = json!({"version":1,"kind":"native","title":"x","text":"x","initialState":{"count":2},"actions":[],"components":[{"id":"s","type":"slider","label":"Count","stateKey":"count","min":0,"max":4}]});
        validate_document(&d).unwrap();
        assert!(validate_bound_state(&d, &json!({"count":5})).is_err());
        assert!(validate_bound_state(&d, &json!({"count":"2"})).is_err());
    }
    #[test]
    fn only_successful_matching_renderer_calls_are_authoritative() {
        let d = json!({"version":1,"kind":"app","title":"x","text":"x","initialState":{},"actions":[],"html":"<p>x</p>"});
        let hash = canonical_hash(&d).unwrap();
        let turn = json!({"items":[{"type":"dynamicToolCall","id":"c","tool":"brr_ui_render","status":"inProgress","success":null,"arguments":d},{"type":"dynamicToolCall","id":"c","tool":"brr_ui_render","status":"completed","success":false,"arguments":d},{"type":"dynamicToolCall","id":"other","tool":"brr_ui_render","status":"completed","success":true,"arguments":d,"contentItems":[{"text":format!("{{\"documentHash\":\"{hash}\"}}")}]},{"type":"dynamicToolCall","id":"c","tool":"brr_ui_render","status":"completed","success":true,"arguments":d,"contentItems":[{"text":format!("{{\"documentHash\":\"{hash}\",\"document\":{}}}",d)}]}]});
        let (found, found_hash) = resolve_document(&[turn], "c").unwrap();
        assert_eq!(found, d);
        assert_eq!(found_hash, hash);
        assert!(resolve_document(&[], "c").is_err());
    }
    #[tokio::test]
    async fn state_writes_are_ordered_cas_and_isolated_by_document_hash() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        init_test_schema(&pool).await;
        let id = Identity {
            thread: "t".into(),
            call: "c".into(),
            hash: "h1".into(),
        };
        assert_eq!(write_state_row(&pool, &id, "{}", 0).await.unwrap(), 1);
        assert!(write_state_row(&pool, &id, "{}", 0)
            .await
            .unwrap_err()
            .contains("stale"));
        assert_eq!(
            write_state_row(&pool, &id, "{\"x\":1}", 1).await.unwrap(),
            2
        );
        let other = Identity {
            hash: "h2".into(),
            ..id.clone()
        };
        assert_eq!(write_state_row(&pool, &other, "{}", 0).await.unwrap(), 1);
    }
    #[tokio::test]
    async fn review_claim_rejects_stale_state_and_consumes_once() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        init_test_schema(&pool).await;
        let id = Identity {
            thread: "t".into(),
            call: "c".into(),
            hash: "h".into(),
        };
        write_state_row(&pool, &id, "{}", 0).await.unwrap();
        sqlx::query("INSERT INTO intelligent_ui_reviews(id,thread_id,call_id,document_hash,state_revision,action_id,text) VALUES('stale','t','c','h',0,'a','x'),('once','t','c','h',1,'a','x')").execute(&pool).await.unwrap();
        assert!(claim_review_row(&pool, "stale", "t", "c", "h", 0)
            .await
            .is_err());
        claim_review_row(&pool, "once", "t", "c", "h", 1)
            .await
            .unwrap();
        assert!(claim_review_row(&pool, "once", "t", "c", "h", 1)
            .await
            .is_err());
        write_state_row(&pool, &id, "{}", 1).await.unwrap();
        sqlx::query("INSERT INTO intelligent_ui_reviews(id,thread_id,call_id,document_hash,state_revision,action_id,text) VALUES('stale2','t','c','h',1,'a','x')").execute(&pool).await.unwrap();
        assert!(claim_review_row(&pool, "stale2", "t", "c", "h", 1)
            .await
            .is_err());
    }
    async fn init_test_schema(pool: &sqlx::SqlitePool) {
        sqlx::query("CREATE TABLE intelligent_ui_state(thread_id TEXT,call_id TEXT,document_hash TEXT,state TEXT,revision INTEGER,PRIMARY KEY(thread_id,call_id,document_hash))").execute(pool).await.unwrap();
        sqlx::query("CREATE TABLE intelligent_ui_reviews(id TEXT PRIMARY KEY,thread_id TEXT,call_id TEXT,document_hash TEXT,state_revision INTEGER,action_id TEXT,text TEXT,claimed INTEGER DEFAULT 0)").execute(pool).await.unwrap();
    }
}
