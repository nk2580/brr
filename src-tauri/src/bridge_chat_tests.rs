use crate::bridge::Engine;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::process::Command as TokioCommand;

const THREAD: &str = "chat-thread";

async fn chat_fixture(dir: &std::path::Path, echo_before_ack: bool) -> Engine {
    chat_fixture_with_completion(dir, echo_before_ack, false).await
}

async fn chat_fixture_with_completion(
    dir: &std::path::Path,
    echo_before_ack: bool,
    complete_before_ack: bool,
) -> Engine {
    chat_fixture_options(
        dir,
        echo_before_ack,
        complete_before_ack,
        false,
        false,
        false,
        false,
    )
    .await
}

async fn chat_fixture_with_unsupported_paginated_history(dir: &std::path::Path) -> Engine {
    chat_fixture_options(dir, false, false, true, false, false, false).await
}

async fn chat_fixture_with_empty_paginated_history(dir: &std::path::Path) -> Engine {
    chat_fixture_options(dir, false, false, true, true, false, false).await
}

async fn chat_fixture_options(
    dir: &std::path::Path,
    echo_before_ack: bool,
    complete_before_ack: bool,
    unsupported_paginated_history: bool,
    empty_paginated_snapshot: bool,
    fresh_unmaterialized: bool,
    fresh_read_unmaterialized: bool,
) -> Engine {
    let script = dir.join("chat-runtime.py");
    let state = dir.join("runtime-state.json");
    std::fs::write(
        &script,
        r#"
import json,sys,os,re
state_path=sys.argv[1]
def load():
    try: return json.load(open(state_path))
    except: return {'starts':[],'steers':[],'finished':[],'reads':[],'active':[],'archived':[],'archiveErrors':{}}
def save(s): json.dump(s,open(state_path,'w'))
def emit(v): print(json.dumps(v),flush=True)
active=None
counter=0
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    method=m['method']; p=m.get('params',{}); s=load(); s.setdefault('calls',[]).append(method); result={}
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method=='initialize': result={'capabilities':{}}
    elif method=='model/list': result={'data':[{'model':'test-model','inputModalities':['text','image'],'isDefault':True,'defaultReasoningEffort':'medium'}]}
    elif method=='turn/start':
        invalid=any(entry.get('name') is not None and not re.fullmatch(r'[a-z][a-z0-9_]{0,63}',entry['name']) for entry in p.get('additionalContext',{}).values())
        if invalid:
            emit({'id':m['id'],'error':{'code':-32602,'message':'additional context name must match [a-z][a-z0-9_]{0,63}'}})
            continue
        counter+=1; active='turn-'+str(counter)
        item={'id':'user-'+str(counter),'type':'userMessage','clientId':p.get('clientUserMessageId'),'content':p.get('input',[])}
        turn={'id':active,'status':'inProgress','items':[item]}
        s['starts'].append({'turnId':active,'threadId':p.get('threadId'),'text':'\n'.join(x.get('text','') for x in p.get('input',[]) if x.get('type')=='text'),'clientId':p.get('clientUserMessageId'),'input':p.get('input',[]),'additionalContext':p.get('additionalContext',{})})
        save(s)
        if os.environ.get('CHAT_ECHO_BEFORE_ACK')=='1':
            emit({'method':'item/started','params':{'threadId':p.get('threadId'),'turnId':active,'item':{'id':'echo-'+str(counter),'type':'userMessage','clientId':p.get('clientUserMessageId')}}})
        emit({'method':'turn/started','params':{'threadId':p.get('threadId'),'turn':turn}})
        if os.environ.get('CHAT_COMPLETE_BEFORE_ACK')=='1':
            active=None; s['finished'].append(active or turn['id']); save(s)
            emit({'method':'turn/completed','params':{'threadId':p.get('threadId'),'turn':{'id':turn['id'],'status':'completed','items':[]}}})
        result={'turn':turn}
        emit({'id':m['id'],'result':result})
        if os.environ.get('CHAT_ECHO_BEFORE_ACK')!='1':
            emit({'method':'item/started','params':{'threadId':p.get('threadId'),'turnId':active,'item':{'id':'echo-'+str(counter),'type':'userMessage','clientId':p.get('clientUserMessageId')}}})
        continue
    elif method=='turn/steer':
        s['steers'].append({'threadId':p.get('threadId'),'expectedTurnId':p.get('expectedTurnId'),'input':p.get('input',[]),'clientId':p.get('clientUserMessageId')}); save(s)
        result={'turnId':p.get('expectedTurnId')}
    elif method=='test/finish':
        thread=p.get('threadId'); finished=active
        if finished:
            active=None; s['finished'].append(finished); save(s)
            emit({'method':'turn/completed','params':{'threadId':thread,'turn':{'id':finished,'status':'completed','items':[]}}})
    elif method=='test/existing':
        active='restored-turn'
        emit({'method':'thread/status/changed','params':{'threadId':p.get('threadId'),'status':{'type':'active','activeFlags':[]}}})
    elif method=='test/state': result=s
    elif method=='test/setThreads':
        s['active']=p.get('active',[]); s['archived']=p.get('archived',[]); s['readable']=p.get('readable',[]); s['readErrors']=p.get('readErrors',{}); s['strictReads']=p.get('strictReads',False); save(s); result={'ok':True}
    elif method=='test/setArchiveError':
        s.setdefault('archiveErrors',{})[p.get('threadId')]=p.get('error'); save(s); result={'ok':True}
    elif method=='thread/archive':
        error=s.get('archiveErrors',{}).get(p.get('threadId'))
        if error:
            emit({'id':m['id'],'error':{'code':-32000,'message':error}})
            continue
        result={'threadId':p.get('threadId'),'archived':True}
    elif method=='thread/list':
        result={'data':s.get('archived' if p.get('archived') else 'active',[]),'nextCursor':None}
    elif method=='test/disconnect':
        emit({'id':m['id'],'result':{'ok':True}}); sys.exit(0)
    elif method=='thread/turns/list' and os.environ.get('CHAT_FRESH_UNMATERIALIZED')=='1':
        emit({'id':m['id'],'error':{'code':-32602,'message':'thread '+str(p.get('threadId'))+' is not materialized yet; thread/turns/list is unavailable before first user message'}})
        continue
    elif method=='thread/turns/list' and os.environ.get('CHAT_UNSUPPORTED_PAGINATED_HISTORY')=='1':
        emit({'id':m['id'],'error':{'code':-32601,'message':'list_turns is not supported yet'}})
        continue
    elif method=='thread/read':
        s.setdefault('reads',[]).append(p.get('includeTurns')); save(s)
        known=[x.get('id') for x in s.get('active',[])+s.get('archived',[])]+[x.get('id') for x in s.get('readable',[])]
        if p.get('threadId') in s.get('readErrors',{}):
            emit({'id':m['id'],'error':{'code':-32000,'message':s['readErrors'][p.get('threadId')]}})
            continue
        if s.get('strictReads') and p.get('threadId') not in known:
            emit({'id':m['id'],'error':{'code':-32600,'message':'thread not loaded: '+str(p.get('threadId'))}})
            continue
        if s.get('strictReads'):
            result={'thread':{'id':p.get('threadId'),'cwd':os.environ.get('CHAT_THREAD_CWD','.'),'historyMode':'paginated','turns':[]}}
        elif os.environ.get('CHAT_FRESH_READ_UNMATERIALIZED')=='1' and p.get('includeTurns'):
            emit({'id':m['id'],'error':{'code':-32602,'message':'thread '+str(p.get('threadId'))+' is not materialized yet; includeTurns is unavailable before first user message'}})
            continue
        elif os.environ.get('CHAT_FRESH_READ_UNMATERIALIZED')=='1':
            result={'thread':{'id':p.get('threadId'),'cwd':os.environ.get('CHAT_THREAD_CWD','.'),'historyMode':'paginated'}}
        elif os.environ.get('CHAT_FRESH_UNMATERIALIZED')=='1':
            result={'thread':{'id':p.get('threadId'),'cwd':'.','historyMode':'paginated','turns':[]}}
        elif os.environ.get('CHAT_UNSUPPORTED_PAGINATED_HISTORY')=='1':
            turns=[] if os.environ.get('CHAT_EMPTY_PAGINATED_SNAPSHOT')=='1' else [{'id':'restored-turn','status':'inProgress','items':[]}]
            result={'thread':{'id':p.get('threadId'),'cwd':'.','historyMode':'paginated','turns':turns}}
        else:
            result={'thread':{'id':p.get('threadId'),'cwd':os.environ.get('CHAT_THREAD_CWD','.'),'turns':[{'id':active,'status':'inProgress','items':[]}] if active else []}}
    emit({'id':m['id'],'result':result})
"#,
    )
    .unwrap();
    let mut command = TokioCommand::new("python3");
    command
        .arg(script)
        .arg(state)
        .current_dir(dir)
        .env("CHAT_THREAD_CWD", dir);
    if echo_before_ack {
        command.env("CHAT_ECHO_BEFORE_ACK", "1");
    }
    if complete_before_ack {
        command.env("CHAT_COMPLETE_BEFORE_ACK", "1");
    }
    if unsupported_paginated_history {
        command.env("CHAT_UNSUPPORTED_PAGINATED_HISTORY", "1");
    }
    if empty_paginated_snapshot {
        command.env("CHAT_EMPTY_PAGINATED_SNAPSHOT", "1");
    }
    if fresh_unmaterialized {
        command.env("CHAT_FRESH_UNMATERIALIZED", "1");
    }
    if fresh_read_unmaterialized {
        command.env("CHAT_FRESH_READ_UNMATERIALIZED", "1");
    }
    Engine::connect(command, Arc::new(|_| {})).await.unwrap()
}

async fn bind_recovery_baseline(engine: &Engine, dir: &std::path::Path) {
    engine
        .agents
        .store
        .bind_session(brr_agent_core::SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: None,
            cwd: Some(dir.to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn cold_recovery_lists_without_runtime_actions_restores_once_and_keeps_only_selected_grants()
{
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true}),
        )
        .await
        .unwrap();
    let attachments = engine
        .agents
        .chat
        .dispatch_storage(
            "attachments.importBytes",
            json!({"ownerId":THREAD,"files":[
                {"name":"selected.txt","mime":"text/plain","data":"c2VsZWN0ZWQ="},
                {"name":"unused.txt","mime":"text/plain","data":"dW51c2Vk"}
            ]}),
        )
        .await
        .unwrap();
    let selected_id = attachments[0]["id"].as_str().unwrap();
    let unused_id = attachments[1]["id"].as_str().unwrap();
    let failed = submission("orphan-failed", "Rejected before first turn");
    let failed_dispatching = engine
        .agents
        .chat
        .dispatch_storage("submissions.put", failed.clone())
        .await
        .unwrap();
    let failed_dispatching = engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"orphan-failed","expectedRevision":failed_dispatching["revision"],"status":"dispatching"})).await.unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"orphan-failed","expectedRevision":failed_dispatching["revision"],"status":"failed","error":"invalid context"})).await.unwrap();
    let mut queued = submission("orphan-queued", "Persisted plain text");
    queued["attachmentIds"] = json!([selected_id]);
    queued["document"]["attachmentIds"] = json!([selected_id]);
    engine
        .agents
        .chat
        .dispatch_storage("submissions.put", queued)
        .await
        .unwrap();
    engine.shutdown().await;

    // Reopen the same host stores and runtime fixture to model a cold restart.
    let reopened = chat_fixture(dir.path(), false).await;
    let listed = reopened
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .any(|item| item["sourceId"] == "orphan-failed" && item["restorable"] == true));
    let queued = items
        .iter()
        .find(|item| item["sourceId"] == "orphan-queued")
        .unwrap();
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["text"], "Persisted plain text");
    assert_eq!(queued["revision"], 1);
    let before_restore = reopened
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(before_restore["starts"].as_array().unwrap().is_empty());
    assert!(!before_restore["calls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|call| call == "thread/resume" || call == "turn/start" || call == "thread/start"));

    let restored = reopened.agents.chat_request("recovery.restore", json!({"sourceId":"orphan-queued","sourceKind":"submission","operationId":"op-restore-once"})).await.unwrap();
    let draft_id = restored["draft"]["id"].as_str().unwrap().to_owned();
    assert_eq!(restored["draft"]["threadId"], Value::Null);
    assert_eq!(
        restored["draft"]["document"]["text"],
        "Persisted plain text"
    );
    assert_eq!(
        restored["draft"]["document"]["attachmentIds"],
        json!([selected_id])
    );
    assert_eq!(
        restored["draft"]["settings"],
        json!({"model":"test-model","effort":"medium","mode":"default"})
    );
    assert_eq!(restored["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(restored["attachments"][0]["id"], selected_id);
    assert_ne!(restored["attachments"][0]["id"], unused_id);
    let original = reopened
        .agents
        .chat
        .dispatch_storage("submissions.get", json!({"id":"orphan-queued"}))
        .await
        .unwrap();
    assert_eq!(original["status"], "cancelled");
    let repeated = reopened.agents.chat_request("recovery.restore", json!({"sourceId":"orphan-queued","sourceKind":"submission","operationId":"op-restore-once"})).await.unwrap();
    assert_eq!(repeated["draft"]["id"], draft_id);
    let drafts = reopened
        .agents
        .chat
        .dispatch_storage("drafts.list", json!({}))
        .await
        .unwrap();
    assert_eq!(drafts.as_array().unwrap().len(), 1);
    reopened
        .agents
        .chat
        .dispatch_storage(
            "drafts.promote",
            json!({"id":draft_id,"threadId":"restored-thread"}),
        )
        .await
        .unwrap();
    let repeated_after_promotion = reopened.agents.chat_request("recovery.restore", json!({"sourceId":"orphan-queued","sourceKind":"submission","operationId":"another-operation"})).await.unwrap();
    assert_eq!(repeated_after_promotion["target"], "chat");
    assert_eq!(repeated_after_promotion["threadId"], "restored-thread");
    assert!(repeated_after_promotion.get("draft").is_none());
    let after_restore = reopened
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(after_restore["starts"].as_array().unwrap().is_empty());
    reopened.shutdown().await;
}

#[tokio::test]
async fn healthy_or_archived_runtime_chats_are_not_reported_as_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("still-present", "do not recover"),
        )
        .await
        .unwrap();
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[{"id":THREAD}]}),
        )
        .await
        .unwrap();
    let result = engine
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    assert!(result["items"].as_array().unwrap().is_empty());
    engine.shutdown().await;
}

#[tokio::test]
async fn healthy_runtime_chat_omitted_from_inventory_is_probed_and_not_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("hidden-healthy-chat", "do not recover a live chat"),
        )
        .await
        .unwrap();
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"readable":[{"id":THREAD}],"strictReads":true}),
        )
        .await
        .unwrap();

    let listed = engine
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    assert!(listed["items"].as_array().unwrap().is_empty());
    let error = engine
        .agents
        .chat_request(
            "recovery.restore",
            json!({"sourceId":"hidden-healthy-chat","sourceKind":"submission","operationId":"must-not-restore"}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("still available in the runtime"));
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(state["reads"]
        .as_array()
        .unwrap()
        .iter()
        .all(|include_turns| include_turns == false));
    assert!(!state["calls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|call| { call == "thread/start" || call == "turn/start" || call == "thread/resume" }));
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true,"readErrors":{"chat-thread":"runtime metadata unavailable"}}),
        )
        .await
        .unwrap();
    let error = engine
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap_err();
    assert!(error.contains("Could not verify saved chat"));
    assert!(error.contains("runtime metadata unavailable"));
    engine.shutdown().await;
}

#[tokio::test]
async fn promoted_nonempty_draft_is_restored_once_as_a_new_unpromoted_draft() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true}),
        )
        .await
        .unwrap();
    let saved = engine
        .agents
        .chat
        .dispatch_storage(
            "drafts.save",
            json!({
                "id":"draft-before-first-send","expectedRevision":0,
                "threadId":null,"projectId":null,"agentId":null,
                "document":{"version":1,"text":"draft saved before first turn","references":[],"quotes":[],"attachmentIds":[]},
                "settings":{"model":"test-model","effort":"medium","mode":"plan"}
            }),
        )
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "drafts.promote",
            json!({"id":saved["id"],"threadId":THREAD}),
        )
        .await
        .unwrap();
    engine.shutdown().await;

    let reopened = chat_fixture(dir.path(), false).await;
    let listed = reopened
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    let item = &listed["items"][0];
    assert_eq!(item["sourceId"], THREAD);
    assert_eq!(item["sourceKind"], "draft");
    assert_eq!(item["revision"], 2);
    assert_eq!(item["restorable"], true);
    let restored = reopened
        .agents
        .chat_request(
            "recovery.restore",
            json!({"sourceId":THREAD,"sourceKind":"draft","operationId":"op-draft-restore"}),
        )
        .await
        .unwrap();
    assert_ne!(restored["draft"]["id"], THREAD);
    assert_eq!(restored["draft"]["threadId"], Value::Null);
    assert_eq!(
        restored["draft"]["document"]["text"],
        "draft saved before first turn"
    );
    assert_eq!(restored["draft"]["settings"]["mode"], "plan");
    let repeated = reopened
        .agents
        .chat_request(
            "recovery.restore",
            json!({"sourceId":THREAD,"sourceKind":"draft","operationId":"different-operation"}),
        )
        .await
        .unwrap();
    assert_eq!(repeated["draft"]["id"], restored["draft"]["id"]);
    let state = reopened
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(!state["calls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|call| call == "thread/start" || call == "turn/start" || call == "thread/resume"));
    reopened.shutdown().await;
}

#[tokio::test]
async fn accepted_orphan_receipt_is_visible_but_never_restorable() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    let queued = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("accepted-orphan", "runtime accepted this"),
        )
        .await
        .unwrap();
    let dispatching = engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"accepted-orphan","expectedRevision":queued["revision"],"status":"dispatching"})).await.unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"accepted-orphan","expectedRevision":dispatching["revision"],"status":"accepted","turnId":"accepted-turn"})).await.unwrap();
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true}),
        )
        .await
        .unwrap();
    let listed = engine
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    let item = &listed["items"][0];
    assert_eq!(item["status"], "accepted");
    assert_eq!(item["restorable"], false);
    assert!(item["reason"].as_str().unwrap().contains("accepted"));
    let error = engine.agents.chat_request("recovery.restore", json!({"sourceId":"accepted-orphan","sourceKind":"submission","operationId":"accepted-restore"})).await.unwrap_err();
    assert!(error.contains("queued or definitively failed"));
    engine.shutdown().await;
}

#[tokio::test]
async fn promoted_draft_cannot_be_restored_while_delivery_receipt_is_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    let saved = engine
        .agents
        .chat
        .dispatch_storage(
            "drafts.save",
            json!({
                "id":"promoted-with-receipt","expectedRevision":0,"threadId":null,
                "projectId":null,"agentId":null,
                "document":{"version":1,"text":"possibly duplicated","references":[],"quotes":[]},
                "settings":{"model":"test-model","effort":"medium","mode":"default"}
            }),
        )
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "drafts.promote",
            json!({"id":saved["id"],"threadId":THREAD}),
        )
        .await
        .unwrap();
    let queued = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("ambiguous-first-send", "possibly duplicated"),
        )
        .await
        .unwrap();
    let dispatching = engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"ambiguous-first-send","expectedRevision":queued["revision"],"status":"dispatching"})).await.unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"ambiguous-first-send","expectedRevision":dispatching["revision"],"status":"uncertain"})).await.unwrap();
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true}),
        )
        .await
        .unwrap();
    let list = engine
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    let draft = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["sourceKind"] == "draft")
        .unwrap();
    assert_eq!(draft["restorable"], false);
    assert!(draft["reason"].as_str().unwrap().contains("unconfirmed"));
    let error = engine
        .agents
        .chat_request(
            "recovery.restore",
            json!({"sourceId":THREAD,"sourceKind":"draft","operationId":"ambiguous-draft-restore"}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("unconfirmed"));
    engine.shutdown().await;
}

#[tokio::test]
async fn folderless_registered_project_allows_text_recovery_but_rejects_file_references() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    let project = engine
        .agents
        .projects
        .create(crate::projects::ProjectInput {
            name: "No folders yet".into(),
            folders: vec![],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    engine
        .agents
        .store
        .bind_session(brr_agent_core::SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: Some(project.id.clone()),
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    engine
        .agents
        .projects
        .set_membership(THREAD, Some(&project.id))
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage("submissions.put", submission("text-only", "safe text"))
        .await
        .unwrap();
    let mut with_file = submission("file-ref", "needs a folder");
    with_file["document"]["references"] = json!([{"type":"file","id":dir.path().join("note.txt").to_string_lossy().to_string(),"name":"note.txt"}]);
    engine
        .agents
        .chat
        .dispatch_storage("submissions.put", with_file)
        .await
        .unwrap();
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true}),
        )
        .await
        .unwrap();
    let listed = engine
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    let items = listed["items"].as_array().unwrap();
    let text = items
        .iter()
        .find(|item| item["sourceId"] == "text-only")
        .unwrap();
    assert_eq!(text["restorable"], true);
    let file = items
        .iter()
        .find(|item| item["sourceId"] == "file-ref")
        .unwrap();
    assert_eq!(file["restorable"], false);
    assert!(file["reason"]
        .as_str()
        .unwrap()
        .contains("registered folders"));
    engine.shutdown().await;
}

#[tokio::test]
async fn recovery_rebases_old_session_file_references_to_registered_project_roots() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("project");
    let old_session_dir = dir.path().join("old-session");
    std::fs::create_dir_all(source_dir.join("src")).unwrap();
    std::fs::create_dir_all(old_session_dir.join("src")).unwrap();
    let source = std::fs::canonicalize(source_dir).unwrap();
    let old_session = std::fs::canonicalize(old_session_dir).unwrap();
    std::fs::write(source.join("src/file.rs"), "source").unwrap();
    std::fs::write(old_session.join("src/file.rs"), "session").unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    let project = engine
        .agents
        .projects
        .create(crate::projects::ProjectInput {
            name: "Registered project".into(),
            folders: vec![source.to_string_lossy().into_owned()],
            primary_folder: Some(source.to_string_lossy().into_owned()),
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    engine
        .agents
        .store
        .bind_session(brr_agent_core::SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: Some(project.id.clone()),
            cwd: Some(old_session.to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    engine
        .agents
        .projects
        .set_membership(THREAD, Some(&project.id))
        .await
        .unwrap();
    engine
        .agents
        .worktrees
        .save_session(
            THREAD,
            &crate::worktrees::SessionWorkspace {
                cwd: old_session.to_string_lossy().into_owned(),
                roots: vec![old_session.to_string_lossy().into_owned()],
                folders: vec![crate::worktrees::ResolvedFolder {
                    source: source.to_string_lossy().into_owned(),
                    path: old_session.to_string_lossy().into_owned(),
                }],
                worktrees: vec![],
                scope_project_id: Some(project.id),
                project_revision: Some(1),
            },
        )
        .unwrap();
    let mut saved = submission("old-session-file", "use this file");
    let old_file = old_session
        .join("src/file.rs")
        .to_string_lossy()
        .to_string();
    saved["document"]["references"] =
        json!([{"type":"file","id":old_file,"name":"file.rs","path":old_file}]);
    engine
        .agents
        .chat
        .dispatch_storage("submissions.put", saved)
        .await
        .unwrap();
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true}),
        )
        .await
        .unwrap();
    let restored = engine.agents.chat_request("recovery.restore", json!({"sourceId":"old-session-file","sourceKind":"submission","operationId":"rebase-source-file"})).await.unwrap();
    let reference = &restored["draft"]["document"]["references"][0];
    assert_eq!(
        reference["id"],
        source.join("src/file.rs").to_string_lossy().to_string()
    );
    assert_eq!(
        reference["path"],
        source.join("src/file.rs").to_string_lossy().to_string()
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn uncertain_orphan_delivery_is_listed_read_only_and_cannot_be_restored() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    let queued = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("orphan-uncertain", "possibly reached runtime"),
        )
        .await
        .unwrap();
    let dispatching = engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"orphan-uncertain","expectedRevision":queued["revision"],"status":"dispatching"})).await.unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"orphan-uncertain","expectedRevision":dispatching["revision"],"status":"uncertain"})).await.unwrap();
    engine
        .agents
        .rpc
        .call(
            "test/setThreads",
            json!({"active":[],"archived":[],"strictReads":true}),
        )
        .await
        .unwrap();
    let list = engine
        .agents
        .chat_request("recovery.list", json!({}))
        .await
        .unwrap();
    assert_eq!(list["items"][0]["status"], "uncertain");
    assert_eq!(list["items"][0]["restorable"], false);
    assert!(list["items"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("unconfirmed"));
    let error = engine.agents.chat_request("recovery.restore", json!({"sourceId":"orphan-uncertain","sourceKind":"submission","operationId":"op-uncertain"})).await.unwrap_err();
    assert!(error.contains("queued or definitively failed"));
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(state["starts"].as_array().unwrap().is_empty());
    engine.shutdown().await;
}

#[tokio::test]
async fn queued_orphan_can_be_cancelled_without_runtime_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    bind_recovery_baseline(&engine, dir.path()).await;
    let queued = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("cancel-orphan", "cancel this"),
        )
        .await
        .unwrap();
    let cancelled = engine
        .agents
        .chat_request(
            "recovery.cancel",
            json!({"submissionId":"cancel-orphan","expectedRevision":queued["revision"]}),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    let stale = engine
        .agents
        .chat_request(
            "recovery.cancel",
            json!({"submissionId":"cancel-orphan","expectedRevision":queued["revision"]}),
        )
        .await
        .unwrap_err();
    assert!(stale.contains("Only a queued message"));
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(!state["calls"]
        .as_array()
        .unwrap()
        .contains(&json!("thread/list")));
    assert!(!state["calls"]
        .as_array()
        .unwrap()
        .contains(&json!("thread/resume")));
    engine.shutdown().await;
}

#[tokio::test]
async fn fresh_chat_send_skips_unsupported_history_listing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_with_unsupported_paginated_history(dir.path()).await;
    engine.agents.register_fresh_chat(THREAD);

    let result = engine
        .agents
        .chat_request(
            "foreground.submit",
            submission("fresh-history", "first message"),
        )
        .await
        .unwrap();
    assert_eq!(result["submission"]["status"], "accepted");
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(state["starts"].as_array().unwrap().len(), 1);

    engine.shutdown().await;
}

#[tokio::test]
async fn explicit_resume_recovers_a_definitively_rejected_first_turn_without_replaying_it() {
    use brr_agent_core::SessionBinding;

    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_options(dir.path(), false, false, false, false, true, false).await;
    engine
        .agents
        .store
        .bind_session(SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();

    engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("rejected-first", "bad context"),
        )
        .await
        .unwrap();
    let dispatching = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.update",
            json!({"id":"rejected-first","expectedRevision":1,"status":"dispatching"}),
        )
        .await
        .unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"rejected-first","expectedRevision":dispatching["revision"],"status":"failed","error":"additional context name must match [a-z][a-z0-9_]{0,63}"})).await.unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("after-rejection", "send once after resume"),
        )
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "queue.pause",
            json!({"threadId":THREAD,"reason":"additionalcontext.name rejected"}),
        )
        .await
        .unwrap();

    let resumed = engine
        .agents
        .chat_request("queue.resume", json!({"threadId":THREAD}))
        .await
        .unwrap();
    assert_eq!(resumed, json!({}));
    let failed_after = engine
        .agents
        .chat
        .dispatch_storage("submissions.get", json!({"id":"rejected-first"}))
        .await
        .unwrap();
    assert_eq!(
        failed_after["status"], "failed",
        "definitively rejected submission must never be retried"
    );
    let delivered = wait_for_submission(&engine, "after-rejection", "accepted").await;
    assert_eq!(delivered["status"], "accepted");
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(
        state["starts"].as_array().unwrap().len(),
        1,
        "only the explicit-resume queued message may start"
    );
    assert_eq!(state["starts"][0]["clientId"], "after-rejection");
    assert_eq!(state["starts"][0]["text"], "send once after resume");
    let queue = engine
        .agents
        .chat_request("queue.list", json!({"threadId":THREAD}))
        .await
        .unwrap();
    assert_eq!(queue["state"]["paused"], false);
    engine.shutdown().await;
}

#[tokio::test]
async fn include_turns_unmaterialized_error_recovers_only_after_metadata_read_and_durable_proof() {
    use brr_agent_core::SessionBinding;

    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_options(dir.path(), false, false, false, false, false, true).await;
    engine
        .agents
        .store
        .bind_session(SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("read-rejected-first", "rejected before rollout"),
        )
        .await
        .unwrap();
    let dispatching = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.update",
            json!({"id":"read-rejected-first","expectedRevision":1,"status":"dispatching"}),
        )
        .await
        .unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"read-rejected-first","expectedRevision":dispatching["revision"],"status":"failed","error":"invalid additional context"})).await.unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("read-recovery-queued", "send once after explicit resume"),
        )
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "queue.pause",
            json!({"threadId":THREAD,"reason":"first turn rejected"}),
        )
        .await
        .unwrap();

    engine
        .agents
        .chat_request("queue.resume", json!({"threadId":THREAD}))
        .await
        .unwrap();
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(state["starts"].as_array().unwrap().len(), 1);
    assert_eq!(state["starts"][0]["clientId"], "read-recovery-queued");
    assert_eq!(
        state["reads"],
        json!([true, false]),
        "metadata-only read follows the durable proof"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn include_turns_unmaterialized_error_refuses_uncertain_delivery_before_metadata_read() {
    use brr_agent_core::SessionBinding;

    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_options(dir.path(), false, false, false, false, false, true).await;
    engine
        .agents
        .store
        .bind_session(SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let queued = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("read-uncertain", "possibly delivered"),
        )
        .await
        .unwrap();
    let dispatching = engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"read-uncertain","expectedRevision":queued["revision"],"status":"dispatching"})).await.unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"read-uncertain","expectedRevision":dispatching["revision"],"status":"uncertain","error":"transport response missing"})).await.unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "queue.pause",
            json!({"threadId":THREAD,"reason":"delivery unconfirmed"}),
        )
        .await
        .unwrap();

    let error = engine
        .agents
        .chat_request("queue.resume", json!({"threadId":THREAD}))
        .await
        .unwrap_err();
    assert!(
        error.contains("may have reached the runtime"),
        "unexpected error: {error}"
    );
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(state["starts"].as_array().unwrap().is_empty());
    assert_eq!(
        state["reads"],
        json!([true]),
        "uncertain rows must prevent the metadata-only recovery read"
    );
    let queue = engine
        .agents
        .chat_request("queue.list", json!({"threadId":THREAD}))
        .await
        .unwrap();
    assert_eq!(queue["state"]["paused"], true);
    engine.shutdown().await;
}

#[tokio::test]
async fn fresh_unmaterialized_recovery_refuses_uncertain_delivery_and_keeps_queue_paused() {
    use brr_agent_core::SessionBinding;

    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_options(dir.path(), false, false, false, false, true, false).await;
    engine
        .agents
        .store
        .bind_session(SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let queued = engine
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            submission("uncertain-delivery", "may have reached runtime"),
        )
        .await
        .unwrap();
    let dispatching = engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"uncertain-delivery","expectedRevision":queued["revision"],"status":"dispatching"})).await.unwrap();
    engine.agents.chat.dispatch_storage("submissions.update", json!({"id":"uncertain-delivery","expectedRevision":dispatching["revision"],"status":"uncertain","error":"transport response missing"})).await.unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "queue.pause",
            json!({"threadId":THREAD,"reason":"delivery unconfirmed"}),
        )
        .await
        .unwrap();

    let error = engine
        .agents
        .chat_request("queue.resume", json!({"threadId":THREAD}))
        .await
        .unwrap_err();
    assert!(
        error.contains("unmaterialized")
            || error.contains("unconfirmed")
            || error.contains("may have reached"),
        "unexpected error: {error}"
    );
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(
        state["starts"].as_array().unwrap().is_empty(),
        "uncertain delivery must never be replayed"
    );
    let current = engine
        .agents
        .chat
        .dispatch_storage("submissions.get", json!({"id":"uncertain-delivery"}))
        .await
        .unwrap();
    assert_eq!(current["status"], "uncertain");
    let queue = engine
        .agents
        .chat_request("queue.list", json!({"threadId":THREAD}))
        .await
        .unwrap();
    assert_eq!(queue["state"]["paused"], true);
    engine.shutdown().await;
}

#[tokio::test]
async fn active_paginated_snapshot_is_respected_when_turn_listing_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_with_unsupported_paginated_history(dir.path()).await;

    let result = engine
        .agents
        .chat_request(
            "foreground.submit",
            submission("active-history", "next message"),
        )
        .await
        .unwrap();
    assert_eq!(result["queued"], true);
    assert_eq!(result["submission"]["status"], "queued");
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(state["starts"].as_array().unwrap().is_empty());

    engine.shutdown().await;
}

#[tokio::test]
async fn empty_paginated_placeholder_fails_closed_when_turn_listing_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_with_empty_paginated_history(dir.path()).await;

    let result = engine
        .agents
        .chat_request("foreground.submit", submission("empty-history", "message"))
        .await;
    assert_eq!(result.unwrap_err(), "list_turns is not supported yet");
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(state["starts"].as_array().unwrap().is_empty());

    engine.shutdown().await;
}

fn submission(id: &str, text: &str) -> Value {
    json!({
        "id":id,"threadId":THREAD,"document":{"version":1,"text":text,"references":[],"quotes":[]},
        "attachmentIds":[],"settings":{"model":"test-model","effort":"medium","mode":"default"},"delivery":"send"
    })
}

async fn wait_for_submission(engine: &Engine, id: &str, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let listed = engine
                .agents
                .chat_request("queue.list", json!({"threadId":THREAD}))
                .await
                .unwrap();
            if let Some(entry) = listed["entries"]
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry["id"] == id))
            {
                if entry["status"] == status {
                    return entry.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn foreground_sends_queue_until_the_turn_finishes_then_dispatch_fifo() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    let first = engine
        .agents
        .chat_request("foreground.submit", submission("first-message", "first"))
        .await
        .unwrap();
    assert_eq!(first["submission"]["status"], "accepted");
    assert_eq!(first["submission"]["turnId"], "turn-1");

    let queued = engine
        .agents
        .chat_request("foreground.submit", submission("second-message", "second"))
        .await
        .unwrap();
    assert_eq!(queued["queued"], true);
    assert_eq!(queued["submission"]["status"], "queued");
    let before_finish = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(before_finish["starts"].as_array().unwrap().len(), 1);
    assert!(before_finish["steers"].as_array().unwrap().is_empty());

    engine
        .agents
        .rpc
        .call("test/finish", json!({"threadId":THREAD}))
        .await
        .unwrap();
    let delivered = wait_for_submission(&engine, "second-message", "accepted").await;
    assert_eq!(delivered["turnId"], "turn-2");
    let after_finish = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    let starts = after_finish["starts"].as_array().unwrap();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["text"], "first");
    assert_eq!(starts[1]["text"], "second");
    assert_eq!(starts[1]["clientId"], "second-message");
    engine.shutdown().await;
}

#[tokio::test]
async fn early_echo_is_idempotent_and_completion_before_ack_does_not_restore_a_running_turn() {
    use brr_agent_core::SessionBinding;

    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture_with_completion(dir.path(), true, true).await;
    engine
        .agents
        .store
        .bind_session(SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();

    let first = engine.agents.chat_request(
        "foreground.submit",
        submission("echo-before-ack", "accepted once"),
    );
    let first = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("early echo must not block the runtime response")
        .unwrap();
    assert_eq!(first["submission"]["status"], "accepted");
    assert_eq!(first["submission"]["turnId"], "turn-1");
    let evidence: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM host_learning_evidence WHERE source_id LIKE ?")
            .bind(format!("{THREAD}:submission:echo-before-ack:%"))
            .fetch_one(engine.agents.store.pool())
            .await
            .unwrap();
    assert_eq!(
        evidence, 1,
        "early event plus ACK must record evidence once"
    );

    let second = engine
        .agents
        .chat_request(
            "foreground.submit",
            submission("after-early-completion", "next"),
        )
        .await
        .unwrap();
    assert_eq!(second["submission"]["status"], "accepted");
    assert_eq!(second["submission"]["turnId"], "turn-2");
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(state["starts"].as_array().unwrap().len(), 2);
    assert_eq!(state["finished"].as_array().unwrap().len(), 2);
    engine.shutdown().await;
}

#[tokio::test]
async fn unrelated_status_events_cannot_hide_a_resumed_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    engine
        .agents
        .rpc
        .call("test/existing", json!({"threadId":THREAD}))
        .await
        .unwrap();
    let result = engine
        .agents
        .chat_request("foreground.submit", submission("after-resume", "must wait"))
        .await
        .unwrap();
    assert_eq!(result["queued"], true);
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert!(state["starts"].as_array().unwrap().is_empty());
    assert!(state["steers"].as_array().unwrap().is_empty());
    engine.shutdown().await;
}

#[tokio::test]
async fn accepted_submission_receipts_survive_composer_grant_removal() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    let source = dir.path().join("notes.txt");
    std::fs::write(&source, "selected original").unwrap();
    let files = engine
        .agents
        .chat
        .dispatch_storage(
            "attachments.import",
            json!({"ownerId":THREAD,"paths":[source]}),
        )
        .await
        .unwrap();
    let mut intent = submission("receipt-id", "Read this file");
    intent["attachmentIds"] = json!([files[0]["id"]]);
    let first = engine
        .agents
        .chat_request("foreground.submit", intent.clone())
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "attachments.remove",
            json!({"ownerId":THREAD,"id":files[0]["id"]}),
        )
        .await
        .unwrap();
    let retry = engine
        .agents
        .chat_request("foreground.submit", intent)
        .await
        .unwrap();
    assert_eq!(retry["submission"]["id"], first["submission"]["id"]);
    assert_eq!(retry["submission"]["status"], "accepted");
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(state["starts"].as_array().unwrap().len(), 1);
    engine.shutdown().await;
}

#[tokio::test]
async fn selected_workspace_context_uses_runtime_compliant_names() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "selected workspace bytes").unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    engine.agents.register_fresh_chat(THREAD);
    let mut intent = submission("selected-file-context", "Use notes.txt");
    intent["document"]["references"] =
        json!([{"type":"file","id":"notes.txt","name":"notes.txt","start":4,"end":13}]);

    let result = engine
        .agents
        .chat_request("foreground.submit", intent)
        .await
        .unwrap();
    assert_eq!(result["submission"]["status"], "accepted");
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(state["starts"][0]["threadId"], THREAD);
    assert_eq!(
        state["starts"][0]["additionalContext"]["brr_selected_context"]["name"],
        "brr_selected_context"
    );
    assert!(
        state["starts"][0]["additionalContext"]["brr_selected_context"]["value"]
            .as_str()
            .unwrap()
            .contains("notes.txt")
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn a_stale_queued_row_never_authorizes_a_second_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    let stale = engine
        .agents
        .chat
        .dispatch_storage("submissions.put", submission("stale-row", "deliver once"))
        .await
        .unwrap();
    let first = engine.agents.dispatch_entry(stale.clone()).await.unwrap();
    assert_eq!(first["submission"]["status"], "accepted");
    assert!(engine.agents.dispatch_entry(stale).await.is_err());
    let state = engine
        .agents
        .rpc
        .call("test/state", json!({}))
        .await
        .unwrap();
    assert_eq!(state["starts"].as_array().unwrap().len(), 1);
    engine.shutdown().await;
}

#[tokio::test]
async fn profile_editing_retains_its_existing_workflow_sender() {
    let dir = tempfile::tempdir().unwrap();
    let engine = chat_fixture(dir.path(), false).await;
    engine
        .agents
        .store
        .bind_session(brr_agent_core::SessionBinding {
            thread_id: THREAD.into(),
            agent_id: None,
            kind: "profile".into(),
            project_id: None,
            cwd: Some(dir.path().to_string_lossy().into()),
            task_run_id: None,
            learning_eligible: false,
        })
        .await
        .unwrap();
    let response = engine
        .request(
            "turn/start",
            json!({"threadId":THREAD,"input":[{"type":"text","text":"Update the profile"}]}),
        )
        .await
        .unwrap();
    assert_eq!(response["turn"]["id"], "turn-1");
    engine.shutdown().await;
}

#[test]
fn snapshot_materialization_precedes_destination_hooks_without_source_switch_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let source = root.join("source");
    let staging = root.join("staging");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&staging).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&source)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-b", "main"]);
    git(&[
        "-c",
        "user.name=Brr Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "test: snapshot fixture",
    ]);
    std::fs::create_dir(source.join(".config")).unwrap();
    let before = "echo touched > source-hook.txt";
    let after = "test -f selected.txt && echo ready > setup-ready.txt";
    std::fs::write(
        source.join(".config/wt.toml"),
        format!("pre-switch = {before:?}\npost-create = {after:?}\n"),
    )
    .unwrap();
    std::fs::write(source.join("selected.txt"), "current dirty bytes").unwrap();
    let snapshot = crate::chat_branch::Snapshot::capture(&source, &staging).unwrap();
    let head = snapshot.head().to_owned();
    let manager = worktrunk::embedded::SessionManager::new(
        worktrunk::config::UserConfig::default(),
        root.join("approvals.toml"),
    );
    manager
        .approve(&source, &[before.into(), after.into()])
        .unwrap();
    let destination = manager
        .create_snapshot(&source, "codex/chat-hooks", &head, move |target| {
            snapshot
                .materialize(target)
                .map_err(std::io::Error::other)
                .map_err(Into::into)
        })
        .unwrap();
    assert!(!source.join("source-hook.txt").exists());
    assert_eq!(
        std::fs::read_to_string(destination.join("selected.txt")).unwrap(),
        "current dirty bytes"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("setup-ready.txt")).unwrap(),
        "ready\n"
    );
}

#[tokio::test]
async fn drafts_list_discovers_only_journaled_empty_prefix_edit_branches() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_root = std::fs::canonicalize(dir.path()).unwrap();
    let workspace_path = workspace_root.to_string_lossy().into_owned();
    let engine = chat_fixture(dir.path(), false).await;
    let project = engine
        .agents
        .projects
        .create(crate::projects::ProjectInput {
            name: "Edit branch project".into(),
            folders: vec![workspace_path.clone()],
            primary_folder: Some(workspace_path.clone()),
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    let branch_id = "empty-edit-branch";
    let source_id = "source-chat";
    let turn_id = "turn-to-edit";
    let source_submission = "accepted-source-submission";
    let document = json!({"version":1,"text":"restore my edit","references":[],"quotes":[],"attachmentIds":[]});
    let settings = json!({"model":"test-model","effort":"medium","mode":"default"});
    let workspace = json!({
        "cwd":workspace_path,
        "roots":[workspace_path],
        "folders":[],
        "worktrees":[],
        "scopeProjectId":project.id,
        "projectRevision":1
    });
    let lineage =
        json!({"threadId":source_id,"turnId":turn_id,"usesCurrentFiles":true,"sharedFolder":false});

    engine
        .agents
        .store
        .bind_session(brr_agent_core::SessionBinding {
            thread_id: branch_id.into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: Some(project.id.clone()),
            cwd: Some(workspace_path.clone()),
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    engine
        .agents
        .projects
        .set_membership(branch_id, Some(&project.id))
        .await
        .unwrap();
    engine
        .agents
        .worktrees
        .save_session(
            branch_id,
            &serde_json::from_value(workspace.clone()).unwrap(),
        )
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "drafts.save",
            json!({
                "id":branch_id,"threadId":branch_id,"projectId":project.id,"agentId":null,
                "document":document,"settings":settings,"expectedRevision":0
            }),
        )
        .await
        .unwrap();
    engine.agents.chat.dispatch_storage("operations.put", json!({
        "id":"edit-branch-op","kind":"branch","phase":"ready","before":true,
        "workspaceComplete":true,"workspace":workspace,"threadId":branch_id,
        "sourceThreadId":source_id,"turnId":turn_id,"sidebarProjectId":project.id,
        "agentKind":"baseline","agentId":null,"learningEligible":true,
        "editingDraft":{"sourceSubmissionId":source_submission,"document":document,"settings":settings,"attachmentIds":[]},
        "result":{"thread":{"id":branch_id,"projectId":project.id,"workspace":workspace,"branchOf":lineage,
            "brr":{"threadId":branch_id,"agentId":null,"kind":"baseline","projectId":project.id,"cwd":workspace_path,"learningEligible":true}}}
    })).await.unwrap();
    sqlx::query("INSERT INTO chat_branch_lineage(thread_id,lineage) VALUES(?,?)")
        .bind(branch_id)
        .bind(lineage.to_string())
        .execute(&engine.agents.chat.pool)
        .await
        .unwrap();

    let listed = engine
        .agents
        .chat_request("drafts.list", json!({}))
        .await
        .unwrap();
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == branch_id)
        .unwrap();
    assert_eq!(row["threadId"], branch_id);
    assert_eq!(row["document"]["text"], "restore my edit");
    assert_eq!(row["branchEdit"], true);
    assert_eq!(row["branchOperationId"], "edit-branch-op");
    assert_eq!(row["branchThread"]["id"], branch_id);
    let branch_revision = row["revision"].as_i64().unwrap();

    let attachment = engine
        .agents
        .chat
        .dispatch_storage(
            "attachments.importBytes",
            json!({"ownerId":branch_id,"files":[{"name":"added.txt","mime":"text/plain","data":"YWRkZWQ="}]}),
        )
        .await
        .unwrap();
    let attachment_id = attachment[0]["id"].as_str().unwrap().to_owned();
    let mut edited_document = document.clone();
    edited_document["text"] = json!("edited after branch creation");
    edited_document["attachmentIds"] = json!([attachment_id]);
    let edited_settings = json!({"model":"test-model","effort":"high","mode":"plan"});
    let moved_project = engine
        .agents
        .projects
        .create(crate::projects::ProjectInput {
            name: "Moved sidebar membership".into(),
            folders: vec![],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        })
        .await
        .unwrap();
    engine
        .agents
        .projects
        .set_membership(branch_id, Some(&moved_project.id))
        .await
        .unwrap();
    engine
        .agents
        .chat
        .dispatch_storage(
            "drafts.save",
            json!({
                "id":branch_id,"threadId":branch_id,"projectId":project.id,"agentId":null,
                "document":edited_document,"settings":edited_settings,"expectedRevision":branch_revision
            }),
        )
        .await
        .unwrap();
    engine.shutdown().await;

    let reopened = chat_fixture(dir.path(), false).await;
    let listed = reopened
        .agents
        .chat_request("drafts.list", json!({}))
        .await
        .unwrap();
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == branch_id)
        .unwrap();
    assert_eq!(row["document"]["text"], "edited after branch creation");
    assert_eq!(row["document"]["attachmentIds"], json!([attachment_id]));
    assert_eq!(row["settings"]["effort"], "high");
    assert_eq!(row["settings"]["mode"], "plan");
    assert_eq!(row["branchThread"]["id"], branch_id);
    assert_eq!(row["projectId"], moved_project.id);
    assert_eq!(row["branchThread"]["projectId"], moved_project.id);
    let edited_revision = row["revision"].as_i64().unwrap();
    let mut cleared_document = edited_document.clone();
    cleared_document["text"] = json!("");
    cleared_document["references"] = json!([]);
    cleared_document["quotes"] = json!([]);
    cleared_document["attachmentIds"] = json!([]);
    reopened
        .agents
        .chat
        .dispatch_storage(
            "attachments.remove",
            json!({"ownerId":branch_id,"id":attachment_id}),
        )
        .await
        .unwrap();
    reopened
        .agents
        .chat
        .dispatch_storage(
            "drafts.save",
            json!({
                "id":branch_id,"threadId":branch_id,"projectId":moved_project.id,"agentId":null,
                "document":cleared_document,"settings":edited_settings,"expectedRevision":edited_revision
            }),
        )
        .await
        .unwrap();
    reopened.shutdown().await;

    let reopened = chat_fixture(dir.path(), false).await;
    let listed = reopened
        .agents
        .chat_request("drafts.list", json!({}))
        .await
        .unwrap();
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == branch_id)
        .unwrap();
    assert_eq!(row["branchEdit"], true);
    assert_eq!(row["document"]["text"], "");
    assert_eq!(row["document"]["attachmentIds"], json!([]));
    assert_eq!(row["settings"]["effort"], "high");
    assert_eq!(row["settings"]["mode"], "plan");

    reopened
        .agents
        .rpc
        .call(
            "test/setArchiveError",
            json!({"threadId":branch_id,"error":"synthetic archive failure"}),
        )
        .await
        .unwrap();
    let error = reopened
        .request("thread/archive", json!({"threadId":branch_id}))
        .await
        .unwrap_err();
    assert!(error.contains("synthetic archive failure"));
    let operation = reopened
        .agents
        .chat
        .dispatch_storage("operations.get", json!({"id":"edit-branch-op"}))
        .await
        .unwrap();
    assert_eq!(operation.get("archived"), None);

    let failed = reopened
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            json!({
                "id":"branch-failed-first-send","threadId":branch_id,"document":document,
                "settings":settings,"attachmentIds":[],"delivery":"start","expectedRevision":0
            }),
        )
        .await
        .unwrap();
    let dispatching = reopened.agents.chat.dispatch_storage("submissions.update", json!({"id":"branch-failed-first-send","expectedRevision":failed["revision"],"status":"dispatching"})).await.unwrap();
    reopened.agents.chat.dispatch_storage("submissions.update", json!({"id":"branch-failed-first-send","expectedRevision":dispatching["revision"],"status":"failed","error":"synthetic rejection"})).await.unwrap();
    let listed = reopened
        .agents
        .chat_request("drafts.list", json!({}))
        .await
        .unwrap();
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == branch_id));

    let queued = reopened
        .agents
        .chat
        .dispatch_storage(
            "submissions.put",
            json!({
                "id":"branch-confirmed-first-send","threadId":branch_id,"document":document,
                "settings":settings,"attachmentIds":[],"delivery":"start","expectedRevision":0
            }),
        )
        .await
        .unwrap();
    let dispatching = reopened.agents.chat.dispatch_storage("submissions.update", json!({"id":"branch-confirmed-first-send","expectedRevision":queued["revision"],"status":"dispatching"})).await.unwrap();
    let uncertain = reopened.agents.chat.dispatch_storage("submissions.update", json!({"id":"branch-confirmed-first-send","expectedRevision":dispatching["revision"],"status":"uncertain"})).await.unwrap();
    reopened.agents.chat.dispatch_storage("submissions.update", json!({"id":"branch-confirmed-first-send","expectedRevision":uncertain["revision"],"status":"accepted","turnId":"turn-accepted"})).await.unwrap();
    let listed = reopened
        .agents
        .chat_request("drafts.list", json!({}))
        .await
        .unwrap();
    assert!(!listed
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == branch_id));

    // The test removes these synthetic receipts so archive discovery can be
    // observed independently from accepted-delivery suppression.
    sqlx::query("DELETE FROM chat_submissions WHERE thread_id=?")
        .bind(branch_id)
        .execute(&reopened.agents.chat.pool)
        .await
        .unwrap();
    reopened
        .agents
        .rpc
        .call(
            "test/setArchiveError",
            json!({"threadId":branch_id,"error":null}),
        )
        .await
        .unwrap();
    reopened
        .request("thread/archive", json!({"threadId":branch_id}))
        .await
        .unwrap();
    let operation = reopened
        .agents
        .chat
        .dispatch_storage("operations.get", json!({"id":"edit-branch-op"}))
        .await
        .unwrap();
    assert_eq!(operation["archived"], true);
    reopened.shutdown().await;

    let reopened = chat_fixture(dir.path(), false).await;
    let listed = reopened
        .agents
        .chat_request("drafts.list", json!({}))
        .await
        .unwrap();
    assert!(!listed
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == branch_id));
    reopened.shutdown().await;
}
