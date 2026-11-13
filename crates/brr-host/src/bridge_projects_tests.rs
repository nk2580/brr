use super::*;
use std::collections::HashSet;

async fn fixture() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("runtime.py");
    std::fs::write(
        &script,
        r#"
import json,sys,pathlib
threads={}; count=0; log=pathlib.Path('requests.jsonl')
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    method=m['method']; p=m.get('params',{})
    with log.open('a') as f: f.write(json.dumps(m)+'\n')
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='thread/start':
        count+=1; tid='project-thread-'+str(count)
        threads[tid]={'id':tid,'cwd':p.get('cwd'),'turns':[{'id':'history-1'}]}
        result={'thread':threads[tid]}
    elif method=='thread/read': result={'thread':threads[p['threadId']]}
    elif method=='thread/list': result={'data':list(threads.values()),'nextCursor':None}
    elif method=='thread/resume': result={'thread':threads[p['threadId']]}
    elif method=='turn/start': result={'turn':{'id':'turn-1'}}
    else: result={}
    print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#,
    )
    .unwrap();
    let mut command = Command::new("python3");
    command.arg(script).current_dir(dir.path());
    let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
    (dir, engine)
}

fn input(
    name: &str,
    folders: Vec<String>,
    primary_folder: Option<String>,
) -> crate::projects::ProjectInput {
    crate::projects::ProjectInput {
        name: name.into(),
        folders,
        primary_folder,
        model: None,
        effort: None,
        permission: None,
    }
}

#[tokio::test]
async fn folderless_start_keeps_stable_membership_and_history_after_move_and_delete() {
    let (_dir, engine) = fixture().await;
    let first = engine
        .agents
        .projects
        .create(input("Scratch", vec![], None))
        .await
        .unwrap();
    let second = engine
        .agents
        .projects
        .create(input("Other", vec![], None))
        .await
        .unwrap();
    let started = engine
        .request("thread/start", json!({"projectId":first.id}))
        .await
        .unwrap();
    let id = started["thread"]["id"].as_str().unwrap();
    let snapshot = started["thread"]["workspace"].clone();
    assert_eq!(started["thread"]["projectId"], first.id);
    assert_eq!(started["thread"]["brr"]["projectId"], first.id);
    assert!(snapshot["cwd"]
        .as_str()
        .unwrap()
        .contains("session-workspaces"));
    assert_eq!(snapshot["scopeProjectId"], first.id);

    engine
        .agents
        .projects
        .set_membership(id, Some(&second.id))
        .await
        .unwrap();
    let moved = engine
        .request("thread/read", json!({"threadId":id}))
        .await
        .unwrap();
    assert_eq!(moved["thread"]["projectId"], second.id);
    assert_eq!(
        moved["thread"]["brr"]["projectId"], first.id,
        "moving keeps the original task and learning scope"
    );
    assert_eq!(
        moved["thread"]["workspace"], snapshot,
        "moving a chat changes membership without rewriting its workspace snapshot"
    );

    engine.agents.projects.delete(&second.id).await.unwrap();
    let retained = engine
        .request("thread/read", json!({"threadId":id}))
        .await
        .unwrap();
    assert!(retained["thread"]["projectId"].is_null());
    assert_eq!(retained["thread"]["workspace"], snapshot);
    assert_eq!(retained["thread"]["turns"][0]["id"], "history-1");
    let listed = engine.request("thread/list", json!({})).await.unwrap();
    assert_eq!(listed["data"][0]["id"], id);
    let ordinary = engine
        .request("thread/start", json!({"projectId": null}))
        .await
        .unwrap();
    assert!(ordinary["thread"]["projectId"].is_null());
    assert!(ordinary["thread"]["workspace"]["scopeProjectId"].is_null());
    assert!(ordinary["thread"]["workspace"]["folders"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_ne!(
        ordinary["thread"]["workspace"]["cwd"], snapshot["cwd"],
        "ordinary Brr chats receive their own private directory"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn non_git_multi_folder_start_sends_all_roots_and_resume_keeps_the_saved_snapshot() {
    let (dir, engine) = fixture().await;
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    let three = dir.path().join("three");
    for path in [&one, &two, &three] {
        std::fs::create_dir(path).unwrap();
    }
    let original = vec![
        one.canonicalize().unwrap().to_string_lossy().into_owned(),
        two.canonicalize().unwrap().to_string_lossy().into_owned(),
    ];
    let project = engine
        .agents
        .projects
        .create(input(
            "Multi-root",
            original.clone(),
            Some(original[1].clone()),
        ))
        .await
        .unwrap();
    let started = engine
        .request("thread/start", json!({"projectId":project.id}))
        .await
        .unwrap();
    let id = started["thread"]["id"].as_str().unwrap();
    assert_eq!(started["thread"]["cwd"], original[1]);
    assert_eq!(
        started["thread"]["workspace"]["roots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect::<HashSet<_>>(),
        original.iter().cloned().collect()
    );
    let requests = std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap();
    let start = requests
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["method"] == "thread/start")
        .unwrap();
    assert_eq!(
        start["params"]["runtimeWorkspaceRoots"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        start["params"]["config"]["sandbox_workspace_write.writable_roots"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let mut changed = project.clone();
    changed.folders = vec![three.canonicalize().unwrap().to_string_lossy().into_owned()];
    changed.primary_folder = changed.folders.first().cloned();
    engine.agents.projects.save(changed).await.unwrap();
    let resumed = engine
        .request("thread/resume", json!({"threadId":id}))
        .await
        .unwrap();
    assert_eq!(
        resumed["thread"]["workspace"],
        started["thread"]["workspace"]
    );
    let requests = std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap();
    let resume = requests
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["method"] == "thread/resume")
        .unwrap();
    assert_eq!(
        resume["params"]["runtimeWorkspaceRoots"],
        start["params"]["runtimeWorkspaceRoots"]
    );
    assert_eq!(
        resume["params"]["config"]["sandbox_workspace_write.writable_roots"],
        start["params"]["config"]["sandbox_workspace_write.writable_roots"]
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn mention_tasks_capture_the_source_chats_scope_and_folder_configuration() {
    let (dir, engine) = fixture().await;
    let original = dir.path().join("original");
    let secondary = dir.path().join("secondary");
    let replacement = dir.path().join("replacement");
    for path in [&original, &secondary, &replacement] {
        std::fs::create_dir_all(path).unwrap();
    }
    let original = original
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let secondary = secondary
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let project = engine
        .agents
        .projects
        .create(input(
            "Source",
            vec![original.clone(), secondary.clone()],
            Some(original.clone()),
        ))
        .await
        .unwrap();
    let started = engine
        .request("thread/start", json!({"projectId":project.id}))
        .await
        .unwrap();
    let source = started["thread"]["id"].as_str().unwrap();
    let mut changed = project.clone();
    changed.folders = vec![replacement.to_string_lossy().into_owned()];
    changed.primary_folder = changed.folders.first().cloned();
    engine.agents.projects.save(changed).await.unwrap();
    let draft = engine
        .agents
        .store
        .propose_profile(
            "user-test",
            None,
            brr_agent_core::ProfileCandidate {
                name: "Helper".into(),
                description: "Fixture helper".into(),
                instructions: "Read fixtures".into(),
                soul: "Direct".into(),
                model: "fixture".into(),
                effort: "medium".into(),
            },
        )
        .await
        .unwrap();
    engine
        .agents
        .store
        .approve_profile(&draft.id, &draft.hash, "test-review")
        .await
        .unwrap();
    let agent = engine
        .agents
        .store
        .apply_profile(&draft.id, &draft.hash, "test-apply")
        .await
        .unwrap();
    let task=engine.agents_request("tasks.create",json!({"agentId":agent.id,"title":"Inspect old source workspace","instructions":"Inspect both folders","acceptanceCriteria":["Report both folders"],"projectId":project.id,"sourceThreadId":source})).await.unwrap();
    assert_eq!(task["projectId"], project.id);
    assert_eq!(
        task["cwd"], original,
        "the task must not silently adopt edited project roots"
    );
    let captured = engine
        .agents
        .task_workspace_project(task["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(captured.folders, vec![original, secondary]);
    engine.shutdown().await;
}

#[tokio::test]
async fn task_retry_cannot_attach_a_source_workspace_to_an_existing_operation() {
    let (dir, engine) = fixture().await;
    let folder = dir.path().join("source");
    std::fs::create_dir(&folder).unwrap();
    let folder = folder
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let project = engine
        .agents
        .projects
        .create(input("Source", vec![folder.clone()], Some(folder)))
        .await
        .unwrap();
    let started = engine
        .request("thread/start", json!({"projectId": project.id}))
        .await
        .unwrap();
    let source = started["thread"]["id"].as_str().unwrap();
    let draft = engine
        .agents
        .store
        .propose_profile(
            "user-test",
            None,
            brr_agent_core::ProfileCandidate {
                name: "Helper".into(),
                description: "Fixture helper".into(),
                instructions: "Read fixtures".into(),
                soul: "Direct".into(),
                model: "fixture".into(),
                effort: "medium".into(),
            },
        )
        .await
        .unwrap();
    engine
        .agents
        .store
        .approve_profile(&draft.id, &draft.hash, "test-review")
        .await
        .unwrap();
    let agent = engine
        .agents
        .store
        .apply_profile(&draft.id, &draft.hash, "test-apply")
        .await
        .unwrap();
    let mut request = json!({"operationId": "same-operation", "agentId": agent.id, "title": "Inspect source",
        "instructions": "Inspect folders", "acceptanceCriteria": ["Report folders"], "projectId": project.id});
    let task = engine
        .agents_request("tasks.create", request.clone())
        .await
        .unwrap();
    let task_id = task["id"].as_str().unwrap();
    assert!(engine
        .agents
        .task_workspace_project(task_id)
        .await
        .unwrap()
        .is_none());
    request["sourceThreadId"] = json!(source);
    let retry = engine.agents_request("tasks.create", request).await;
    assert!(
        retry.is_err(),
        "an existing operation cannot acquire a new source workspace: {retry:?}"
    );
    assert!(engine
        .agents
        .task_workspace_project(task_id)
        .await
        .unwrap()
        .is_none());
    engine.shutdown().await;
}
