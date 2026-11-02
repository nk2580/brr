use super::*;

async fn fixture() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("runtime.py");
    std::fs::write(&file, r#"
import json,sys
count=0
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
    else: result={}
    print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
    let mut command = Command::new("python3");
    command.arg(file).current_dir(dir.path());
    let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
    (dir, engine)
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
