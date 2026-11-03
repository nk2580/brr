use super::*;

async fn fixture() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("runtime.py");
    std::fs::write(&file, r#"
import json,sys
count=0
last_tools={}
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    method=m['method']; p=m.get('params',{})
    if method=='turn/interrupt' and p.get('threadId')=='reject-interrupt':
        print(json.dumps({'id':m['id'],'error':{'code':-32602,'message':'Fixture rejected interruption'}}),flush=True)
        continue
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='thread/start':
        count+=1
        result={'thread':{'id':'interview-'+str(count),'cwd':p.get('cwd'),'turns':[]},'startParams':p}
    elif method=='thread/resume': result={'thread':{'id':p['threadId'],'turns':[]},'resumeParams':p}
    elif method=='thread/read': result={'thread':{'id':p['threadId'],'turns':[]},'readParams':p}
    elif method=='thread/dynamicTools/replace':
        last_tools[p['threadId']]=p['tools']
        result={}
    elif method=='test/last-tools': result=last_tools.get(p['threadId'],[])
    else: result={}
    print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
    let mut command = Command::new("python3");
    command.arg(file).current_dir(dir.path());
    let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
    (dir, engine)
}

#[tokio::test]
async fn ticket_tools_default_to_bound_project_and_derive_the_actor() {
    let (directory, engine) = fixture().await;
    let project_dir = directory.path().join("registered");
    std::fs::create_dir_all(&project_dir).unwrap();
    let project = engine
        .agents
        .projects
        .create(crate::projects::ProjectInput {
            name: "Registered".into(),
            folders: vec![project_dir.to_string_lossy().into_owned()],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    engine
        .agents
        .bind_baseline(
            &json!({"id":"ticket-chat","cwd":"/unrelated/worktree"}),
            Some(project.id.clone()),
        )
        .await
        .unwrap();
    let created = engine
        .agents
        .handle_tool(json!({
            "threadId":"ticket-chat","callId":"create-1","tool":"brr_ticket_create",
            "arguments":{"title":"Bound project default","description":"Ticket actor test"}
        }))
        .await
        .unwrap();
    assert_eq!(created["projectId"], project.id);
    let detail = engine
        .agents
        .store
        .read_ticket(created["id"].as_str().unwrap(), None, None)
        .await
        .unwrap();
    assert_eq!(
        detail.activity.data[0].actor.kind,
        brr_agent_core::TicketActorKind::Agent
    );
    assert_eq!(
        detail.activity.data[0].actor.thread_id.as_deref(),
        Some("ticket-chat")
    );
    assert!(engine
        .agents
        .handle_tool(json!({
            "threadId":"ticket-chat","callId":"create-2","tool":"brr_ticket_create",
            "arguments":{"projectId":"/unregistered","title":"Out of scope"}
        }))
        .await
        .is_err());
    engine.shutdown().await;
}

#[tokio::test]
async fn stale_task_lease_cannot_mutate_a_ticket() {
    use brr_agent_core::{SessionBinding, TaskInput};
    let (directory, engine) = fixture().await;
    let project_dir = directory.path().join("lease-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let project = engine
        .agents
        .projects
        .create(crate::projects::ProjectInput {
            name: "Lease project".into(),
            folders: vec![project_dir.to_string_lossy().into_owned()],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    let profile = json!({"name":"Lease agent","description":"test","instructions":"test","soul":"test","model":"test-model","effort":"medium"});
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('lease-agent',1,?,0,0)")
        .bind(profile.to_string()).execute(engine.agents.store.pool()).await.unwrap();
    let task = engine
        .agents
        .store
        .create_task(TaskInput {
            agent_id: "lease-agent".into(),
            title: "Fence ticket tools".into(),
            instructions: "No ticket write".into(),
            acceptance_criteria: vec!["The stale lease is rejected".into()],
            context: None,
            cwd: Some(project_dir.to_string_lossy().into_owned()),
            project_id: Some(project.id.clone()),
            model: None,
            effort: None,
            workspace_permission: None,
            dependencies: vec![],
        })
        .await
        .unwrap();
    let run = engine
        .agents
        .store
        .claim_ready("ticket-test-worker")
        .await
        .unwrap()
        .unwrap();
    engine
        .agents
        .store
        .bind_session(SessionBinding {
            thread_id: "stale-task-thread".into(),
            agent_id: Some("lease-agent".into()),
            kind: "task".into(),
            project_id: Some(project.id),
            cwd: Some(
                directory
                    .path()
                    .join("saved-worktree")
                    .to_string_lossy()
                    .into_owned(),
            ),
            task_run_id: Some(run.id),
            learning_eligible: false,
        })
        .await
        .unwrap();
    let error = engine
        .agents
        .handle_tool(json!({
            "threadId":"stale-task-thread","callId":"stale-1","brrHostLease":"old-lease",
            "tool":"brr_ticket_create","arguments":{"title":"Must not exist"}
        }))
        .await
        .unwrap_err();
    assert!(error.contains("stale or stopped task lease"));
    assert!(engine
        .agents
        .store
        .list_tickets(&task.project_id.unwrap(), None, 20, None)
        .await
        .unwrap()
        .data
        .is_empty());
    engine.shutdown().await;
}

#[tokio::test]
async fn ticket_task_binding_keeps_stable_project_id_and_executes_in_selected_folder() {
    let (directory, engine) = fixture().await;
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let project = engine
        .agents
        .projects
        .create(crate::projects::ProjectInput {
            name: "Two folders".into(),
            folders: vec![
                first.to_string_lossy().into_owned(),
                second.to_string_lossy().into_owned(),
            ],
            primary_folder: Some(first.to_string_lossy().into_owned()),
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    let session = engine
        .agents
        .open_session(
            None,
            "task",
            "fixture-model",
            Some(second.to_string_lossy().as_ref()),
            Some(&project.id),
            false,
            None,
            Some(brr_agent_core::WorkspacePermission::WorkspaceWrite),
        )
        .await
        .unwrap();
    assert_eq!(
        session["thread"]["cwd"],
        second.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(session["thread"]["brr"]["projectId"], project.id);
    assert_eq!(session["thread"]["workspace"]["scopeProjectId"], project.id);
    let stored = engine
        .agents
        .store
        .session_binding(session["thread"]["id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(stored.project_id.as_deref(), Some(project.id.as_str()));
    engine.shutdown().await;
}

#[tokio::test]
async fn resumed_foreground_chats_refresh_owned_browser_tools_and_dispatch_with_trusted_binding() {
    let (_dir, engine) = fixture().await;
    engine
        .agents
        .bind_baseline(&json!({"id":"old-chat"}), None)
        .await
        .unwrap();
    engine
        .request("thread/resume", json!({"threadId":"old-chat"}))
        .await
        .unwrap();
    let tools = engine
        .raw("test/last-tools", json!({"threadId":"old-chat"}))
        .await
        .unwrap();
    assert!(tools
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "brr_browser"));
    engine.set_browser_handler(Arc::new(|thread, args| {
        Box::pin(async move { Ok(json!({"thread":thread,"args":args})) })
    }));
    let response = engine
        .agents
        .handle_tool(
            json!({"threadId":"old-chat","tool":"brr_browser","arguments":{"action":"list"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        response,
        json!({"thread":"old-chat","args":{"action":"list"}})
    );
    assert!(engine
        .agents
        .handle_tool(
            json!({"threadId":"unbound","tool":"brr_browser","arguments":{"action":"list"}})
        )
        .await
        .is_err());
    let interview = engine
        .agents_request(
            "agents.interview",
            json!({"model":"test-model","effort":"medium"}),
        )
        .await
        .unwrap();
    assert!(engine.agents.handle_tool(json!({"threadId":interview["thread"]["id"],"tool":"brr_browser","arguments":{"action":"list"}})).await.unwrap_err().contains("unavailable"));
    engine.shutdown().await;
}

#[tokio::test]
async fn agent_interview_starts_without_a_git_project_and_registers_reviewed_creation_tools() {
    let (_dir, engine) = fixture().await;
    let response = engine
        .agents_request(
            "agents.interview",
            json!({"model":"test-model","effort":"medium"}),
        )
        .await
        .unwrap();
    assert_eq!(response["thread"]["id"], "interview-1");
    assert_eq!(response["thread"]["brr"]["kind"], "interview");
    let params = &response["startParams"];
    assert_eq!(params["modelProvider"], "openai");
    assert_eq!(params["approvalPolicy"], "on-request");
    assert_eq!(params["config"]["features.memories"], false);
    assert!(params["dynamicTools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "brr_agent_propose"));
    assert!(!params["developerInstructions"]
        .as_str()
        .unwrap()
        .contains("Replace the runtime"));
    engine.shutdown().await;
}

#[tokio::test]
async fn profile_tool_requires_exact_user_review_and_created_agent_gets_fresh_profile_context() {
    let (_dir, engine) = fixture().await;
    engine
        .agents_request(
            "agents.interview",
            json!({"model":"test-model","effort":"medium"}),
        )
        .await
        .unwrap();
    let draft = engine.agents.handle_tool(json!({"threadId":"interview-1","tool":"brr_agent_propose","arguments":{"candidate":{
        "name":"Ada","description":"Project helper","instructions":"Check evidence.","soul":"Be candid.","model":"test-model","effort":"medium"
    }}})).await.unwrap();
    let create = json!({"threadId":"interview-1","tool":"brr_agent_create","arguments":{"draftId":draft["id"],"hash":draft["hash"]}});
    assert!(engine
        .agents
        .handle_tool(create.clone())
        .await
        .unwrap_err()
        .contains("review"));
    assert!(engine
        .agents_request(
            "agents.approve",
            json!({"draftId":draft["id"],"hash":"wrong"})
        )
        .await
        .is_err());
    let created = engine
        .agents_request(
            "agents.approve",
            json!({"draftId":draft["id"],"hash":draft["hash"]}),
        )
        .await
        .unwrap();
    assert_eq!(created["agent"]["name"], "Ada");
    let replay = engine
        .agents_request(
            "agents.approve",
            json!({"draftId":draft["id"],"hash":draft["hash"]}),
        )
        .await
        .unwrap();
    assert_eq!(replay["agent"]["id"], created["agent"]["id"]);
    let opened = engine
        .agents_request("agents.open", json!({"agentId":created["agent"]["id"]}))
        .await
        .unwrap();
    let thread_id = opened["thread"]["id"].as_str().unwrap();
    let turn=engine.agents.prepare_turn(thread_id,json!({"threadId":thread_id,"input":[{"type":"text","text":"Help with this project."}]})).await.unwrap();
    let profile: Value = serde_json::from_str(
        turn["additionalContext"]["brr_agent_profile"]["value"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(profile["profile"]["soul"], "Be candid.");
    assert_eq!(profile["profile"]["revision"], 1);
    assert_eq!(opened["thread"]["brr"]["agentId"], created["agent"]["id"]);
    let resumed = engine
        .request("thread/resume", json!({"threadId":thread_id}))
        .await
        .unwrap();
    assert_eq!(resumed["resumeParams"]["config"]["features.goals"], false);
    assert_eq!(
        resumed["resumeParams"]["config"]["features.memories"],
        false
    );
    let sources: Value = serde_json::from_str(
        turn["additionalContext"]["brr_source_index"]["value"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let source_ref = sources[0]["sourceId"].as_str().unwrap();
    let remembered=engine.agents.handle_tool(json!({"threadId":thread_id,"tool":"brr_memory_remember","arguments":{"scope":"agent","kind":"project","content":"This project needs evidence.","sourceIds":[source_ref]}})).await.unwrap();
    let recalled = engine
        .agents_request(
            "memory.search",
            json!({"agentId":created["agent"]["id"],"query":"evidence"}),
        )
        .await
        .unwrap();
    assert_eq!(recalled["data"][0]["id"], remembered["id"]);
    let skill=engine.agents.handle_tool(json!({"threadId":thread_id,"tool":"brr_skill_publish","arguments":{"scope":"agent","name":"Build","description":"Build it","content":"Run build.","sourceIds":[source_ref]}})).await.unwrap_err();
    assert!(skill.contains("verified successful tool evidence"));
    let forgotten = engine
        .agents_request(
            "memory.forget",
            json!({"agentId":created["agent"]["id"],"id":remembered["id"]}),
        )
        .await
        .unwrap();
    assert!(engine
        .agents_request("memory.list", json!({"agentId":created["agent"]["id"]}))
        .await
        .unwrap()["data"]
        .as_array()
        .unwrap()
        .is_empty());
    engine
        .agents_request("memory.undo", json!({"eventId":forgotten["eventId"]}))
        .await
        .unwrap();
    assert_eq!(
        engine
            .agents_request("memory.list", json!({"agentId":created["agent"]["id"]}))
            .await
            .unwrap()["data"][0]["id"],
        remembered["id"]
    );
    engine.agents.stop().await;
    let task=engine.agents_request("tasks.create",json!({"agentId":created["agent"]["id"],"title":"Controlled task","instructions":"Work on the fixture","acceptanceCriteria":["Verified"]})).await.unwrap();
    let task_id = task["id"].as_str().unwrap();
    let run = engine
        .agents
        .store
        .claim_ready("manual-test")
        .await
        .unwrap()
        .unwrap();
    engine
        .agents
        .store
        .bind_run_thread(task_id, &run.id, "reject-interrupt", &run.lease_token)
        .await
        .unwrap();
    engine
        .agents
        .store
        .attach_continuation(
            task_id,
            &run.id,
            "reject-interrupt",
            "active-turn",
            &run.lease_token,
        )
        .await
        .unwrap();
    engine
        .agents
        .store
        .bind_session(brr_agent_core::SessionBinding {
            thread_id: "reject-interrupt".into(),
            agent_id: Some(created["agent"]["id"].as_str().unwrap().into()),
            kind: "task".into(),
            project_id: None,
            cwd: None,
            task_run_id: Some(run.id.clone()),
            learning_eligible: true,
        })
        .await
        .unwrap();
    let view = engine
        .request(
            "thread/resume",
            json!({"threadId":"reject-interrupt","sandbox":"read-only"}),
        )
        .await
        .unwrap();
    assert_eq!(view["readParams"]["includeTurns"], true);
    assert!(
        view["resumeParams"].is_null(),
        "Viewing history must not resume with new permissions"
    );
    assert!(engine
        .agents_request("tasks.control", json!({"taskId":task_id,"action":"pause"}))
        .await
        .unwrap_err()
        .contains("did not confirm interruption"));
    assert_eq!(
        engine.agents.store.get_task(task_id).await.unwrap().status,
        "running"
    );
    let mut delayed = json!({"threadId":"reject-interrupt","turnId":"active-turn","tool":"brr_memory_search","arguments":{"query":"private"}});
    engine
        .agents
        .capture_tool_fence(&mut delayed)
        .await
        .unwrap();
    engine
        .agents
        .store
        .control_task(task_id, brr_agent_core::TaskAction::Pause)
        .await
        .unwrap();
    engine
        .agents
        .store
        .control_task(task_id, brr_agent_core::TaskAction::Resume)
        .await
        .unwrap();
    let resumed = engine
        .agents
        .store
        .claim_ready("new-owner")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.id, run.id);
    assert_ne!(resumed.lease_token, run.lease_token);
    assert!(engine
        .agents
        .handle_tool(delayed)
        .await
        .unwrap_err()
        .contains("stale or stopped task lease"));
    assert_eq!(
        engine
            .agents
            .store
            .get_run(&resumed.id)
            .await
            .unwrap()
            .lease_token,
        resumed.lease_token
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn unknown_threads_cannot_use_agent_memory_or_profile_tools() {
    let (_dir, engine) = fixture().await;
    let err = engine.agents.handle_tool(json!({"threadId":"old-historical-thread","tool":"brr_memory_search","arguments":{"query":"secret"}})).await.unwrap_err();
    assert!(!err.is_empty());
    engine.shutdown().await;
}

#[tokio::test]
async fn background_task_completes_only_through_the_runtime_tool_and_learns_from_new_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("background.py");
    std::fs::write(&file,r#"
import json,sys
threads={}; turns={}; pending={}; seq=0

def send(m): print(json.dumps(m),flush=True)
def finish(thread,turn):
    turns[thread][-1]['status']='completed'
    send({'method':'turn/completed','params':{'threadId':thread,'turn':turns[thread][-1]}})
def call(thread,turn,tool,args,after):
    global seq
    seq+=1; rid='tool-'+str(seq);pending[rid]=(thread,turn,after)
    send({'id':rid,'method':'item/tool/call','params':{'threadId':thread,'turnId':turn,'callId':rid,'tool':tool,'arguments':args}})
for line in sys.stdin:
    m=json.loads(line)
    if 'method' not in m:
        p=pending.pop(m.get('id'),None)
        if p:
            thread,turn,after=p
            if after=='learn': call(thread,turn,'brr_learning_done',{},'finish')
            else: finish(thread,turn)
        continue
    if 'id' not in m: continue
    method=m['method']; p=m.get('params',{}); result={}
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='account/rateLimits/read': result={'rateLimits':{'primary':{'usedPercent':0}}}
    elif method=='thread/start':
        tid='worker-'+str(len(threads)+1)
        threads[tid]={'id':tid,'cwd':p.get('cwd'),'turns':[],'tools':[s['name'] for s in p.get('dynamicTools',[])]}
        turns[tid]=[];result={'thread':threads[tid]}
    elif method=='thread/resume' or method=='thread/read': result={'thread':threads[p['threadId']]}
    elif method=='thread/turns/list': result={'data':list(reversed(turns[p['threadId']])),'nextCursor':None}
    elif method=='turn/start':
        thread=p['threadId']; turn='turn-'+str(len(turns[thread])+1)
        turns[thread].append({'id':turn,'status':'inProgress','items':[{'id':'input-'+turn,'type':'userMessage','content':p['input'],'clientUserMessageId':p.get('clientUserMessageId')}]})
        send({'method':'turn/started','params':{'threadId':thread,'turn':turns[thread][-1]}})
        send({'id':m['id'],'result':{'turn':{'id':turn}}})
        if 'brr_task_complete' in threads[thread]['tools']:
            item={'id':'proof-'+turn,'type':'commandExecution','status':'completed','exitCode':0,'command':'verify fixture','aggregatedOutput':'The requested fixture output is verified.'}
            turns[thread][-1]['items'].append(item)
            send({'method':'item/completed','params':{'threadId':thread,'turnId':turn,'item':item}})
            call(thread,turn,'brr_task_complete',{'result':'Fixture work is verified.','evidence':['verify fixture: exit 0']},'finish')
        elif 'brr_learning_done' in threads[thread]['tools']:
            sources=json.loads(p.get('additionalContext',{}).get('brr_source_index',{}).get('value','[]'))
            if sources:
                call(thread,turn,'brr_memory_remember',{'scope':'agent','kind':'project','content':'The fixture work was verified.','sourceIds':[sources[0]['sourceId']]},'learn')
            else: call(thread,turn,'brr_learning_done',{},'finish')
        continue
    send({'id':m['id'],'result':result})
"#).unwrap();
    let mut command = Command::new("python3");
    command.arg(file).current_dir(dir.path());
    let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
    let draft = engine
        .agents
        .store
        .propose_profile(
            "user-test",
            None,
            brr_agent_core::ProfileCandidate {
                name: "Runner".into(),
                description: "Tests".into(),
                instructions: "Verify results.".into(),
                soul: "Candid".into(),
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
    let profile = engine
        .agents
        .store
        .apply_profile(&draft.id, &draft.hash, "test-create")
        .await
        .unwrap();
    let task=engine.agents_request("tasks.create",json!({"agentId":profile.id,"title":"Verify fixture","instructions":"Verify the fixture output.","acceptanceCriteria":["Fixture verification succeeds."]})).await.unwrap();
    let task_id = task["id"].as_str().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let task = engine.agents.store.get_task(task_id).await.unwrap();
            if task.status == "succeeded" {
                break task;
            }
            if matches!(task.status.as_str(), "blocked" | "failed") {
                panic!("Task blocked: {:?}", task.reason)
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outcome.result.as_deref(), Some("Fixture work is verified."));
    assert!(outcome.thread_id.is_some());
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let memory = engine
                .agents_request("memory.list", json!({"agentId":profile.id}))
                .await
                .unwrap();
            if !memory["data"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("new completed task evidence must reach the learning worker");
    engine.shutdown().await;
}
