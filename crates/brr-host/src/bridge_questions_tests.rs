use super::*;

async fn question_fixture(dir: &std::path::Path) -> Engine {
    let file = dir.join("questions-runtime.py");
    std::fs::write(&file, r#"
import json,sys,os
history='question-history.json'
turns=json.load(open(history)) if os.path.exists(history) else []
for old in turns:
    if old['status']=='inProgress': old['status']='interrupted'
live_answers=[]
crash_mode=0
def emit(value): print(json.dumps(value),flush=True)
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    if 'method' not in m:
        if m.get('id')=='live-question':
            live_answers.append(m.get('result'))
            emit({'method':'serverRequest/resolved','params':{'threadId':'q-thread','requestId':m['id']}})
            accepted={'id':'wait-item','type':'functionCallOutput','name':'request_user_input','output':json.dumps(m.get('result'))}
            turns[-1]['items'].append(accepted)
            emit({'method':'item/completed','params':{'threadId':'q-thread','turnId':turns[-1]['id'],'item':accepted}})
            json.dump(turns,open(history,'w'))
        continue
    method=m['method']; p=m.get('params',{}); result={}
    if method=='account/read': result={'account':{'type':'chatgpt'}}
    elif method in ['thread/start','thread/resume','thread/read']:
        result={'thread':{'id':'q-thread','cwd':'.','turns':turns},'resumeParams':p,'liveAnswers':live_answers}
    elif method=='thread/turns/list': result={'data':list(reversed(turns)),'nextCursor':None}
    elif method=='turn/start':
        turn={'id':'turn-'+str(len(turns)+1),'status':'inProgress','items':[{'id':'user-'+str(len(turns)+1),'type':'userMessage','clientId':p.get('clientUserMessageId'),'content':p.get('input',[])}]}
        turns.append(turn); result={'turn':turn}
    elif method=='turn/steer':
        if crash_mode==2: sys.exit(0)
        turns[-1]['items'].append({'id':'reply','type':'userMessage','clientId':p.get('clientUserMessageId'),'content':p.get('input',[])})
        if crash_mode==1:
            json.dump(turns,open(history,'w'))
            sys.exit(0)
        result={'turnId':turns[-1]['id']}
    emit({'id':m['id'],'result':result})
    if method=='turn/start':
        emit({'method':'turn/started','params':{'threadId':'q-thread','turn':turn}})
        text=p.get('input',[{}])[0].get('text','')
        if text=='ask-wait':
            emit({'method':'item/tool/requestUserInput','id':'live-question','params':{'threadId':'q-thread','turnId':turn['id'],'itemId':'wait-item','isBlocking':True,'questions':[{'id':'scope','header':'Scope','question':'Which scope?','options':None}]}})
        if text.startswith('ask-async'):
            if text=='ask-async-admit-disconnect': crash_mode=1
            if text=='ask-async-drop-disconnect': crash_mode=2
            item={'id':'async-item','type':'agentMessage','text':'Which scope?','delivery':'async','questions':[{'title':'Which scope?','options':['Project','Workspace']}]}
            turn['items'].append(item)
            emit({'method':'item/completed','params':{'threadId':'q-thread','turnId':turn['id'],'item':item}})
        if text=='ask-async-idle':
            turn['status']='completed'
            emit({'method':'turn/completed','params':{'threadId':'q-thread','turn':turn}})
        if text=='unsafe-work':
            turn['items'].append({'id':'uncertain-command','type':'commandExecution','status':'inProgress','command':'synthetic external effect'})
            turn['status']='failed'
            emit({'method':'turn/completed','params':{'threadId':'q-thread','turn':turn}})
    json.dump(turns,open(history,'w'))
"#).unwrap();
    let mut command = Command::new("python3");
    command.arg(file).current_dir(dir);
    Engine::connect(command, Arc::new(|_| {})).await.unwrap()
}

async fn open_question_thread(engine: &Engine) {
    engine
        .agents_request(
            "agents.interview",
            json!({"model":"test","effort":"medium"}),
        )
        .await
        .unwrap();
}

async fn pending_question(engine: &Engine) -> Value {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let listed = engine
                .agents_request("questions.list", json!({"threadId":"q-thread"}))
                .await
                .unwrap();
            if let Some(question) = listed["data"].as_array().and_then(|v| v.first()) {
                return question.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn blocking_questions_are_durable_and_separate_from_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let engine = question_fixture(dir.path()).await;
    open_question_thread(&engine).await;
    engine
        .request(
            "turn/start",
            json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-wait"}]}),
        )
        .await
        .unwrap();
    let question = pending_question(&engine).await;
    assert_eq!(question["kind"], "blocking");
    assert_eq!(question["questions"][0]["question"], "Which scope?");
    let decisions = engine
        .agents_request("agents.decisions", json!({}))
        .await
        .unwrap();
    assert!(decisions["runtimeRequests"].as_array().unwrap().is_empty());
    let answers = json!({"scope":["Project and tests"]});
    engine
        .agents_request(
            "questions.saveDraft",
            json!({"requestId":question["id"],"answers":answers}),
        )
        .await
        .unwrap();
    engine.shutdown().await;
    let reopened = question_fixture(dir.path()).await;
    let saved = pending_question(&reopened).await;
    assert_eq!(saved["id"], question["id"]);
    assert_eq!(saved["draft"], answers);
    reopened.shutdown().await;
}

#[tokio::test]
async fn async_answers_steer_the_originating_chat_and_duplicate_submits_do_not_repeat_input() {
    let dir = tempfile::tempdir().unwrap();
    let engine = question_fixture(dir.path()).await;
    open_question_thread(&engine).await;
    engine
        .request(
            "turn/start",
            json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-async"}]}),
        )
        .await
        .unwrap();
    let question = pending_question(&engine).await;
    assert_eq!(question["kind"], "async");
    let params = json!({"requestId":question["id"],"answers":{"question_1":["Project and tests"]}});
    engine
        .agents_request("questions.answer", params.clone())
        .await
        .unwrap();
    engine
        .agents_request("questions.answer", params)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let request = pending_question(&engine).await;
            if request["delivery"] == "delivered" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let history = engine
        .request(
            "thread/read",
            json!({"threadId":"q-thread","includeTurns":true}),
        )
        .await
        .unwrap();
    let replies = history["thread"]["turns"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["id"] == "reply")
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 1);
    let text = replies[0]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Which scope?"));
    assert!(text.contains("Project and tests"));
    engine.shutdown().await;
}

#[tokio::test]
async fn waiting_answer_uses_original_request_and_dismissal_does_not_invent_an_answer() {
    for dismiss in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let engine = question_fixture(dir.path()).await;
        open_question_thread(&engine).await;
        engine
            .request(
                "turn/start",
                json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-wait"}]}),
            )
            .await
            .unwrap();
        let question = pending_question(&engine).await;
        if dismiss {
            engine
                .agents_request("questions.dismiss", json!({"requestId":question["id"]}))
                .await
                .unwrap();
        } else {
            assert!(engine
                .agents_request(
                    "questions.answer",
                    json!({"requestId":question["id"],"answers":{"scope":[" "]}})
                )
                .await
                .is_err());
            engine
                .agents_request(
                    "questions.answer",
                    json!({"requestId":question["id"],"answers":{"scope":["Project"]}}),
                )
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if pending_question(&engine).await["delivery"] == "delivered" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let result = engine
            .request(
                "thread/read",
                json!({"threadId":"q-thread","includeTurns":true}),
            )
            .await
            .unwrap();
        assert_eq!(result["thread"]["turns"].as_array().unwrap().len(), 1);
        assert_eq!(result["liveAnswers"].as_array().unwrap().len(), 1);
        if dismiss {
            assert_eq!(result["liveAnswers"][0], json!({"answers":{}}));
        } else {
            assert_eq!(
                result["liveAnswers"][0],
                json!({"answers":{"scope":{"answers":["Project"]}}})
            );
        }
        engine.shutdown().await;
    }
}

#[tokio::test]
async fn late_async_answer_starts_one_followup_in_the_same_chat() {
    let dir = tempfile::tempdir().unwrap();
    let engine = question_fixture(dir.path()).await;
    open_question_thread(&engine).await;
    engine
        .request(
            "turn/start",
            json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-async-idle"}]}),
        )
        .await
        .unwrap();
    let question = pending_question(&engine).await;
    engine
        .agents_request(
            "questions.answer",
            json!({"requestId":question["id"],"answers":{"question_1":["Workspace"]}}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if pending_question(&engine).await["delivery"] == "delivered" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let result = engine
        .request(
            "thread/read",
            json!({"threadId":"q-thread","includeTurns":true}),
        )
        .await
        .unwrap();
    assert_eq!(result["thread"]["turns"].as_array().unwrap().len(), 2);
    assert!(
        result["thread"]["turns"][1]["items"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Workspace")
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn restarted_waiting_question_uses_a_followup_and_never_reuses_the_old_rpc_id() {
    let dir = tempfile::tempdir().unwrap();
    let engine = question_fixture(dir.path()).await;
    open_question_thread(&engine).await;
    engine
        .request(
            "turn/start",
            json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-wait"}]}),
        )
        .await
        .unwrap();
    let question = pending_question(&engine).await;
    engine.shutdown().await;
    let reopened = question_fixture(dir.path()).await;
    reopened
        .agents_request(
            "questions.answer",
            json!({"requestId":question["id"],"answers":{"scope":["Recovered answer"]}}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if pending_question(&reopened).await["delivery"] == "delivered" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let result = reopened
        .request(
            "thread/read",
            json!({"threadId":"q-thread","includeTurns":true}),
        )
        .await
        .unwrap();
    assert_eq!(result["thread"]["turns"].as_array().unwrap().len(), 2);
    assert!(result["liveAnswers"].as_array().unwrap().is_empty());
    assert!(
        result["thread"]["turns"][1]["items"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Recovered answer")
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn cancelled_runtime_request_is_not_proof_that_an_answer_was_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let engine = question_fixture(dir.path()).await;
    open_question_thread(&engine).await;
    engine.agents.stop().await;
    engine
        .request(
            "turn/start",
            json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-wait"}]}),
        )
        .await
        .unwrap();
    let question = pending_question(&engine).await;
    let id = question["id"].as_str().unwrap();
    let mut answers = brr_agent_core::QuestionAnswers::new();
    answers.insert("scope".into(), vec!["Project".into()]);
    engine
        .agents
        .store
        .resolve_question(id, Some(answers))
        .await
        .unwrap();
    engine
        .agents
        .store
        .claim_question_delivery(id)
        .await
        .unwrap()
        .unwrap();
    engine.agents.observe_question_response(&json!({"method":"serverRequest/resolved","params":{"threadId":"q-thread","requestId":"live-question"}})).await.unwrap();
    assert_ne!(
        engine.agents.store.get_question(id).await.unwrap().delivery,
        "delivered"
    );
    let accepted = json!({"method":"item/completed","params":{"threadId":"q-thread","turnId":"turn-1","item":{"id":"wait-item","type":"functionCallOutput","name":"request_user_input","output":json!({"answers":{"scope":{"answers":["Project"]}}}).to_string()}}});
    engine
        .agents
        .observe_question_response(&accepted)
        .await
        .unwrap();
    assert_eq!(
        engine.agents.store.get_question(id).await.unwrap().delivery,
        "delivered"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn an_earlier_question_cannot_continue_past_a_newer_unresolved_tool_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let engine = question_fixture(dir.path()).await;
    open_question_thread(&engine).await;
    engine
        .request(
            "turn/start",
            json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-async-idle"}]}),
        )
        .await
        .unwrap();
    let question = pending_question(&engine).await;
    engine
        .request(
            "turn/start",
            json!({"threadId":"q-thread","input":[{"type":"text","text":"unsafe-work"}]}),
        )
        .await
        .unwrap();
    engine
        .agents_request(
            "questions.answer",
            json!({"requestId":question["id"],"answers":{"question_1":["Project"]}}),
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let request = pending_question(&engine).await;
            if !matches!(request["delivery"].as_str(), Some("queued" | "dispatching")) {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(result["delivery"], "uncertain");
    let history = engine
        .request(
            "thread/read",
            json!({"threadId":"q-thread","includeTurns":true}),
        )
        .await
        .unwrap();
    assert_eq!(history["thread"]["turns"].as_array().unwrap().len(), 2);
    engine.shutdown().await;
}

#[tokio::test]
async fn uncertain_admission_is_reconciled_from_history_without_resending() {
    for admitted in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let engine = question_fixture(dir.path()).await;
        open_question_thread(&engine).await;
        let prompt = if admitted {
            "ask-async-admit-disconnect"
        } else {
            "ask-async-drop-disconnect"
        };
        engine
            .request(
                "turn/start",
                json!({"threadId":"q-thread","input":[{"type":"text","text":prompt}]}),
            )
            .await
            .unwrap();
        let question = pending_question(&engine).await;
        engine
            .agents_request(
                "questions.answer",
                json!({"requestId":question["id"],"answers":{"question_1":["Project"]}}),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if pending_question(&engine).await["delivery"] == "uncertain" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        engine.shutdown().await;
        let reopened = question_fixture(dir.path()).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        let request = pending_question(&reopened).await;
        assert_eq!(
            request["delivery"],
            if admitted { "delivered" } else { "uncertain" }
        );
        let history = reopened
            .request(
                "thread/read",
                json!({"threadId":"q-thread","includeTurns":true}),
            )
            .await
            .unwrap();
        assert_eq!(history["thread"]["turns"].as_array().unwrap().len(), 1);
        let replies = history["thread"]["turns"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["id"] == "reply")
            .count();
        assert_eq!(replies, usize::from(admitted));
        reopened.shutdown().await;
    }
}

#[tokio::test]
async fn explicit_stop_or_archive_closes_a_queued_answer_before_it_can_dispatch() {
    for method in ["turn/interrupt", "thread/archive"] {
        let dir = tempfile::tempdir().unwrap();
        let engine = question_fixture(dir.path()).await;
        open_question_thread(&engine).await;
        engine.agents.stop().await;
        engine
            .request(
                "turn/start",
                json!({"threadId":"q-thread","input":[{"type":"text","text":"ask-wait"}]}),
            )
            .await
            .unwrap();
        let question = pending_question(&engine).await;
        let params = json!({"requestId":question["id"],"answers":{"scope":["Project"]}});
        engine
            .agents_request("questions.answer", params.clone())
            .await
            .unwrap();
        let request_params = if method == "turn/interrupt" {
            json!({"threadId":"q-thread","turnId":"turn-1"})
        } else {
            json!({"threadId":"q-thread"})
        };
        engine.request(method, request_params).await.unwrap();
        let closed = pending_question(&engine).await;
        assert_eq!(closed["resolution"], "closed");
        assert_eq!(closed["delivery"], "none");
        assert!(engine
            .agents_request("questions.answer", params)
            .await
            .is_err());
        engine.shutdown().await;
    }
}

#[tokio::test]
#[ignore = "live owned-runtime question roundtrips; BRR_LIVE_AUTH_PATH supplies a managed ChatGPT login"]
async fn owned_runtime_questions_roundtrip_in_both_collaboration_modes() {
    let auth = std::env::var_os("BRR_LIVE_AUTH_PATH").expect("BRR_LIVE_AUTH_PATH is required");
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::copy(auth, dir.path().join("auth.json")).unwrap();
    let engine = Engine::connect(
        crate::runtime_command(dir.path()).unwrap(),
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let models = engine
        .request("model/list", json!({"includeHidden":false}))
        .await
        .unwrap();
    let available = models["data"].as_array().unwrap();
    let selected = available
        .iter()
        .find(|m| m["model"].as_str().is_some_and(|s| s.contains("luna")))
        .or_else(|| available.iter().find(|m| m["isDefault"] == true))
        .unwrap();
    let model = selected["model"].as_str().unwrap();
    let effort = selected["defaultReasoningEffort"]
        .as_str()
        .unwrap_or("medium");
    let result=async {
        for mode in ["default","plan"] {
            for kind in ["blocking","async"] {
                let opened=engine.agents_request("agents.interview",json!({"model":model,"effort":effort})).await?;
                let thread=opened["thread"]["id"].as_str().ok_or("No smoke thread")?;
                let instructions=if kind=="blocking" {
                    "This is a synthetic Brr question smoke test. Your only action is to call request_user_input now with exactly one text-only question, id verification, header Verify, question 'What is the verification marker?', and omit options. Do not inspect files or call any other tools. Once the user answers, reply with exactly their marker, then finish."
                } else {
                    "This is a synthetic Brr question smoke test. Call request_user_input_async now with exactly one text-only question whose title is 'What is the verification marker?' and omit options. Do not inspect files or call any other tools. Then finish your turn with BRR_ASYNC_ASKED. When the user answer arrives, reply with exactly their marker, then finish."
                };
                engine.request("turn/start",json!({"threadId":thread,"input":[{"type":"text","text":instructions,"text_elements":[]}],"model":model,"effort":effort,"collaborationMode":{"mode":mode,"settings":{"model":model,"reasoning_effort":effort,"developer_instructions":null}}})).await?;
                let question=tokio::time::timeout(Duration::from_secs(120),async {
                    loop {
                        let questions=engine.agents.store.list_questions(Some(thread),false).await?;
                        if let Some(question)=questions.into_iter().find(|q|q.kind==kind) { return Ok::<_,String>(question); }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }).await.map_err(|_|format!("{mode}/{kind} did not ask a question"))??;
                if question.questions[0].options.is_some() { return Err(format!("{mode}/{kind} did not use a text-only question")); }
                let marker=format!("BRR_{}_{}_OK",mode.to_uppercase(),kind.to_uppercase());
                let mut answers=brr_agent_core::QuestionAnswers::new();
                answers.insert(question.questions[0].id.clone(),vec![marker.clone()]);
                engine.agents_request("questions.answer",json!({"requestId":question.id,"answers":answers})).await?;
                tokio::time::timeout(Duration::from_secs(120),async {
                    loop {
                        let request=engine.agents.store.get_question(&question.id).await?;
                        if request.delivery=="uncertain" { return Err(request.reason.unwrap_or_default()); }
                        let turns=crate::agent_workers::thread_turns(&engine.agents,thread).await?;
                        let completed=turns.iter().any(|t|t["status"]=="completed" && t["items"].as_array().is_some_and(|items|items.iter().any(|i|i["type"]=="agentMessage" && i["text"].as_str().is_some_and(|text|text.contains(&marker)))));
                        if completed && request.delivery=="delivered" { return Ok::<_,String>(()); }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }).await.map_err(|_|format!("{mode}/{kind} did not finish the question roundtrip"))??;
                println!("Owned runtime {mode}/{kind} question and answer passed");
            }
        }
        Ok::<_,String>(())
    }.await;
    engine.shutdown().await;
    result.unwrap();
}
