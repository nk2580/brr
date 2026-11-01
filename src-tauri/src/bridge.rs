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
};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type EventSink = Arc<dyn Fn(Value) + Send + Sync>;
type SharedStdin = Arc<AsyncMutex<Option<ChildStdin>>>;

pub struct Engine {
    stdin: SharedStdin,
    child: AsyncMutex<Child>,
    pending: Pending,
    approvals: Arc<Mutex<HashMap<String, Value>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    worktrees: WorktreeManager,
}

impl Engine {
    pub async fn connect(mut command: Command, events: EventSink) -> Result<Self, String> {
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
        let (pending_reader, approvals_reader, stdin_reader, alive_reader) = (
            pending.clone(),
            approvals.clone(),
            stdin.clone(),
            alive.clone(),
        );
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("method").is_some() {
                    if let Some(id) = message.get("id") {
                        let method = message["method"].as_str().unwrap_or("");
                        if matches!(
                            method,
                            "item/commandExecution/requestApproval"
                                | "item/fileChange/requestApproval"
                                | "item/permissions/requestApproval"
                                | "item/tool/requestUserInput"
                        ) {
                            approvals_reader
                                .lock()
                                .unwrap()
                                .insert(id.to_string(), message.clone());
                        } else {
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
            alive_reader.store(false, Ordering::SeqCst);
            for (_, sender) in pending_reader.lock().unwrap().drain() {
                let _ = sender.send(Err(
                    "Codex runtime disconnected. Reconnect to continue.".into()
                ));
            }
            approvals_reader.lock().unwrap().clear();
            (events)(json!({"method":"brr/disconnected", "params":{}}));
        });
        let engine = Self {
            stdin,
            child: AsyncMutex::new(child),
            pending,
            approvals,
            next_id: AtomicU64::new(1),
            alive,
            worktrees: WorktreeManager::new(home),
        };
        engine.raw("initialize", json!({"clientInfo":{"name":"brr_desktop","title":"Brr","version":"0.1.0"},"capabilities":{"experimentalApi":true,"requestAttestation":false}})).await?;
        write_message(&engine.stdin, &json!({"method":"initialized"})).await?;
        Ok(engine)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
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
        let response = tokio::time::timeout(Duration::from_secs(60), rx).await;
        self.pending.lock().unwrap().remove(&id);
        response
            .map_err(|_| format!("Runtime request {method} timed out"))?
            .map_err(|_| "Runtime disconnected".to_string())?
    }

    pub async fn request(&self, method: &str, mut params: Value) -> Result<Value, String> {
        policy::validate_request(method, &params)?;
        if matches!(
            method,
            "thread/start" | "thread/resume" | "turn/start" | "turn/steer"
        ) {
            let account = self
                .raw("account/read", json!({"refreshToken":false}))
                .await?;
            if !policy::is_chatgpt_account(&account) {
                return Err("Sign in with ChatGPT to start a coding session.".into());
            }
        }
        if matches!(method, "thread/start" | "thread/resume") {
            params["modelProvider"] = json!("openai");
            params["approvalPolicy"] = json!("on-request");
            if method == "thread/start" && params.get("sandbox").is_none() {
                params["sandbox"] = json!("workspace-write")
            }
        }
        let workspace = if method == "thread/start" {
            let cwd = params["cwd"]
                .as_str()
                .ok_or("Choose a project folder before starting a session")?;
            let workspace = self.worktrees.prepare(cwd).await?;
            params["cwd"] = json!(workspace.cwd);
            Some(workspace)
        } else if matches!(method, "thread/resume" | "turn/start") {
            let id = params["threadId"].as_str().ok_or("threadId is required")?;
            let workspace = self.worktrees.get(id)?;
            if let Some(workspace) = &workspace {
                self.worktrees.validate(workspace).await?;
                if method == "thread/resume" {
                    params["cwd"] = json!(workspace.cwd);
                }
            } else if method == "thread/resume" {
                params.as_object_mut().unwrap().remove("cwd");
            }
            workspace
        } else {
            None
        };
        if matches!(method, "turn/start" | "turn/steer") {
            if let Some(items) = params["input"].as_array_mut() {
                for item in items {
                    if item.get("text_elements").is_none() {
                        item["text_elements"] = json!([]);
                    }
                }
            }
        }
        let mut response = self.raw(method, params).await.map_err(|error| {
            if method == "thread/start" {
                if let Some(workspace) = &workspace {
                    return format!(
                        "{error}. Worktree retained at {} ({})",
                        workspace.path, workspace.branch
                    );
                }
            }
            error
        })?;
        if method == "thread/start" {
            let workspace = workspace
                .as_ref()
                .ok_or("Worktree setup was not completed")?;
            let id = response["thread"]["id"]
                .as_str()
                .ok_or("The runtime did not return a thread ID")?;
            self.worktrees.save(id, workspace).map_err(|error| {
                format!(
                    "{error}. Thread {id} and worktree {} were retained.",
                    workspace.path
                )
            })?;
        }
        if matches!(method, "thread/start" | "thread/resume") {
            self.annotate_thread(&mut response["thread"]);
        } else if method == "thread/list" {
            if let Some(threads) = response["data"].as_array_mut() {
                for thread in threads {
                    self.annotate_thread(thread);
                }
            }
        }
        Ok(response)
    }

    fn annotate_thread(&self, thread: &mut Value) {
        if let Some(id) = thread["id"].as_str() {
            match self.worktrees.get(id) {
                Ok(Some(workspace)) => thread["worktree"] = json!(workspace),
                Err(error) => thread["worktreeError"] = json!(error),
                Ok(None) => {}
            }
        }
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
            _ => return Err("Unsupported runtime request".into()),
        }
        write_message(&self.stdin, &json!({"id":id,"result":result})).await?;
        self.approvals.lock().unwrap().remove(&key);
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.stdin.lock().await.take();
        let mut child = self.child.lock().await;
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
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
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn new_sessions_require_an_isolated_project_before_starting_the_runtime_thread() {
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
        assert!(
            result.is_err(),
            "a session was started in an unisolated folder"
        );
        assert!(result.unwrap_err().contains("Git"));
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
                assert_eq!(result.unwrap(), json!({"turnId":"turn-live"}));
                let sent: Value =
                    serde_json::from_str(&std::fs::read_to_string(&forwarded).unwrap()).unwrap();
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
}
