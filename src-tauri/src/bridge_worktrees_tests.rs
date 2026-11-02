use super::*;
use crate::worktrees::WorktreeManager;
use std::path::Path;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn connect(home: &Path, fixture: &Path) -> Engine {
    let mut command = Command::new("python3");
    command.arg(fixture).current_dir(home);
    let mut engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
    engine.worktrees = WorktreeManager::with_config(
        home.to_path_buf(),
        worktrunk::config::UserConfig::default(),
        home.join("approvals.toml"),
    );
    engine
}

#[tokio::test]
async fn project_sessions_persist_their_worktree_and_never_redirect_back_to_the_shared_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project with spaces");
    let home = dir.path().join("engine");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Brr Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "test: fixture",
        ],
    );
    std::fs::write(root.join("other-agent.txt"), "other agent work").unwrap();

    std::fs::create_dir_all(root.join(".config")).unwrap();
    std::fs::write(
        root.join(".config/wt.toml"),
        "[pre-start]\nsetup = \"touch setup-complete.txt\"\n",
    )
    .unwrap();
    git(&root, &["add", ".config/wt.toml"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Brr Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "test: setup hook",
        ],
    );
    let fixture = dir.path().join("engine.py");
    std::fs::write(
        &fixture,
        r#"
import json,sys,pathlib
store=pathlib.Path('threads.json')
threads=json.loads(store.read_text()) if store.exists() else {}
for line in sys.stdin:
    message=json.loads(line)
    if 'id' not in message: continue
    method=message['method']; params=message.get('params',{})
    with open('requests.jsonl','a') as log: log.write(json.dumps(message)+'\n')
    result={}
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='thread/start':
        id='session-'+str(len(threads)+1)
        threads[id]={'id':id,'cwd':params['cwd'],'preview':'','updatedAt':1}
        store.write_text(json.dumps(threads))
        result={'thread':threads[id]}
    elif method=='thread/list': result={'data':list(threads.values()),'nextCursor':None}
    elif method=='thread/resume':
        thread=threads[params['threadId']].copy()
        if 'cwd' in params: thread['cwd']=params['cwd']
        result={'thread':thread}
    elif method=='turn/start': result={'turn':{'id':'turn-1'}}
    print(json.dumps({'id':message['id'],'result':result}),flush=True)
"#,
    )
    .unwrap();

    let engine = connect(&home, &fixture).await;
    let inspection = engine
        .request("brr/worktree/inspect", json!({"cwd":root}))
        .await
        .unwrap();
    assert_eq!(
        inspection["commands"][0]["template"],
        "touch setup-complete.txt"
    );
    assert_eq!(inspection["commands"][0]["approved"], false);
    let rejected = engine
        .request(
            "brr/worktree/approve",
            json!({"cwd":root,"templates":["unreviewed command"]}),
        )
        .await;
    assert!(rejected.is_err());
    let blocked = engine.request("thread/start", json!({"cwd":root})).await;
    assert!(blocked.is_err());
    assert!(!root.join("setup-complete.txt").exists());
    engine
        .request(
            "brr/worktree/approve",
            json!({"cwd":root,"templates":["touch setup-complete.txt"]}),
        )
        .await
        .unwrap();
    let inspection = engine
        .request("brr/worktree/inspect", json!({"cwd":root}))
        .await
        .unwrap();
    assert_eq!(inspection["commands"][0]["approved"], true);
    let started = engine
        .request("thread/start", json!({"cwd":root,"sandbox":"read-only"}))
        .await
        .unwrap();
    let cwd = started["thread"]["cwd"].as_str().unwrap();
    assert_eq!(started["thread"]["brr"]["kind"], "baseline");
    assert_eq!(started["thread"]["brr"]["learningEligible"], true);
    let path = Path::new(cwd);
    assert_ne!(path, root.canonicalize().unwrap());
    assert!(path.join("setup-complete.txt").exists());
    assert!(!root.join("setup-complete.txt").exists());
    assert_eq!(
        started["thread"]["worktree"]["projectPath"],
        root.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    std::fs::write(path.join("session-only.txt"), "isolated edit").unwrap();
    assert!(!root.join("session-only.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("other-agent.txt")).unwrap(),
        "other agent work"
    );
    let second = engine
        .request("thread/start", json!({"cwd":root}))
        .await
        .unwrap();
    assert_ne!(started["thread"]["cwd"], second["thread"]["cwd"]);
    engine.shutdown().await;

    let engine = connect(&home, &fixture).await;
    let listed = engine.request("thread/list", json!({})).await.unwrap();
    assert_eq!(listed["data"][0]["worktree"], started["thread"]["worktree"]);
    let resumed = engine
        .request("thread/resume", json!({"threadId":"session-1","cwd":root}))
        .await
        .unwrap();
    assert_eq!(resumed["thread"]["cwd"], started["thread"]["cwd"]);
    assert_eq!(resumed["thread"]["worktree"], started["thread"]["worktree"]);
    engine
        .request(
            "turn/start",
            json!({"threadId":"session-1","input":[{"type":"text","text":"continue"}]}),
        )
        .await
        .unwrap();

    git(&root, &["worktree", "remove", "--force", cwd]);
    let error = engine
        .request(
            "turn/start",
            json!({"threadId":"session-1","input":[{"type":"text","text":"must not run"}]}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("saved worktree is missing"), "{error}");
    std::fs::write(home.join("worktrees/session-2.json"), "broken metadata").unwrap();
    let listed = engine.request("thread/list", json!({})).await.unwrap();
    assert_eq!(listed["data"].as_array().unwrap().len(), 2);
    assert!(listed["data"][1]["worktreeError"]
        .as_str()
        .unwrap()
        .contains("corrupt"));
    let error = engine
        .request("thread/resume", json!({"threadId":"session-2"}))
        .await
        .unwrap_err();
    assert!(error.contains("corrupt"));
    engine.shutdown().await;
    let requests = std::fs::read_to_string(home.join("requests.jsonl")).unwrap();
    let starts: Vec<Value> = requests
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|m| m["method"] == "thread/start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["params"]["sandbox"], "read-only");
    assert_eq!(starts[0]["params"]["modelProvider"], "openai");
    assert_eq!(starts[0]["params"]["approvalPolicy"], "on-request");
    assert_eq!(starts[0]["params"]["config"]["features.memories"], false);
    assert!(
        starts[0]["params"]["config"]["features.multi_agent"].is_null(),
        "Baseline retains its native temporary agents"
    );
    let forwarded: Vec<Value> = requests
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let resume = forwarded
        .iter()
        .find(|m| m["method"] == "thread/resume")
        .unwrap();
    assert_eq!(
        resume["params"]["config"]["features.memories"], false,
        "New baseline settings survive a cold host resume"
    );
    let turn = forwarded
        .iter()
        .find(|m| m["method"] == "turn/start")
        .unwrap();
    assert_eq!(
        turn["params"]["additionalContext"]["brr_source_index"]["kind"],
        "untrusted"
    );
    assert!(!requests.contains("must not run"));
    assert!(
        !requests.contains("brr/worktree/"),
        "native setup requests must never reach the inference runtime"
    );
}

#[tokio::test]
async fn legacy_sessions_keep_their_saved_directory_when_a_resume_caller_supplies_another() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("engine.py");
    std::fs::write(&fixture, r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    result={'account':{'type':'chatgpt'}} if m['method']=='account/read' else {'thread':{'id':'legacy','cwd':m.get('params',{}).get('cwd','/saved/legacy')}}
    print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
    let mut command = Command::new("python3");
    command.arg(fixture).current_dir(dir.path());
    let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
    let result = engine
        .request(
            "thread/resume",
            json!({"threadId":"legacy","cwd":"/shared/project"}),
        )
        .await
        .unwrap();
    engine.shutdown().await;
    assert_eq!(result["thread"]["cwd"], "/saved/legacy");
}
