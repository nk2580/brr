use brr_agent_core::{
    AgentStore, NewQuestionRequest, Question, QuestionAnswers, QuestionOption, TaskInput,
};
use std::collections::BTreeMap;

async fn store(path: &std::path::Path) -> AgentStore {
    let store = AgentStore::open(path).await.unwrap();
    let candidate = serde_json::json!({"name":"agent-1","description":"test","instructions":"test","soul":"test","model":"gpt-5","effort":"medium"});
    sqlx::query("INSERT OR IGNORE INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-1',1,?,0,0)")
        .bind(candidate.to_string()).execute(store.pool()).await.unwrap();
    store
}

fn request() -> NewQuestionRequest {
    NewQuestionRequest {
        thread_id: "thread-1".into(),
        turn_id: "turn-1".into(),
        item_id: "item-1".into(),
        agent_id: Some("agent-1".into()),
        task_id: Some("task-1".into()),
        run_id: Some("run-1".into()),
        kind: "blocking".into(),
        questions: vec![Question {
            id: "destination".into(),
            header: "Destination".into(),
            question: "Where should I save it?".into(),
            options: Some(vec![QuestionOption {
                label: "Desktop".into(),
                description: "".into(),
            }]),
        }],
        runtime_generation: Some("generation-1".into()),
        runtime_request_id: Some(serde_json::json!({"id":"runtime-1"})),
    }
}

fn answers(value: &str) -> QuestionAnswers {
    BTreeMap::from([("destination".into(), vec![value.into()])])
}

#[tokio::test]
async fn questions_deduplicate_by_source_and_drafts_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let first_store = store(dir.path()).await;
    let first = first_store.record_question(request()).await.unwrap();
    let serialized = serde_json::to_value(&first).unwrap();
    assert_eq!(serialized["threadId"], "thread-1");
    assert!(serialized.get("thread_id").is_none());
    let draft = first_store
        .save_question_draft(&first.id, &answers("Desktop"))
        .await
        .unwrap();
    assert_eq!(draft.resolution, "pending");
    assert!(draft.answers.is_none());

    let mut retry = request();
    retry.runtime_generation = Some("generation-2".into());
    retry.runtime_request_id = Some(serde_json::json!({"id":"runtime-2"}));
    let deduplicated = first_store.record_question(retry).await.unwrap();
    assert_eq!(deduplicated.id, first.id);
    assert_eq!(deduplicated.draft, answers("Desktop"));
    assert_eq!(
        deduplicated.runtime_generation.as_deref(),
        Some("generation-2")
    );

    let reopened = store(dir.path()).await;
    let restored = reopened.get_question(&first.id).await.unwrap();
    assert_eq!(restored.draft, answers("Desktop"));
    assert_eq!(
        reopened
            .list_questions(Some("thread-1"), true)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        reopened
            .record_question({
                let mut changed = request();
                changed.questions[0].question = "Different immutable question".into();
                changed
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn answer_validation_is_exact_and_resolution_and_delivery_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let agent_store = store(dir.path()).await;
    let question = agent_store.record_question(request()).await.unwrap();
    assert!(
        agent_store
            .resolve_question(&question.id, Some(BTreeMap::new()))
            .await
            .is_err()
    );
    assert!(
        agent_store
            .resolve_question(
                &question.id,
                Some(BTreeMap::from([("other".into(), vec!["x".into()])]))
            )
            .await
            .is_err()
    );

    let answered = agent_store
        .resolve_question(&question.id, Some(answers("Desktop")))
        .await
        .unwrap();
    assert_eq!(answered.resolution, "answered");
    assert_eq!(answered.delivery, "queued");
    let marker = answered.client_message_id.clone().unwrap();
    let repeated = agent_store
        .resolve_question(&question.id, Some(answers("Desktop")))
        .await
        .unwrap();
    assert_eq!(repeated.client_message_id.as_deref(), Some(marker.as_str()));
    assert!(
        agent_store
            .resolve_question(&question.id, Some(answers("Other")))
            .await
            .is_err()
    );

    let claim = agent_store
        .claim_question_delivery(&question.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.delivery, "dispatching");
    assert!(
        agent_store
            .claim_question_delivery(&question.id)
            .await
            .unwrap()
            .is_none()
    );
    let reopened = store(dir.path()).await;
    assert_eq!(
        reopened.get_question(&question.id).await.unwrap().delivery,
        "dispatching"
    );
    let delivered = reopened
        .finish_question_delivery(&question.id, "delivered", None)
        .await
        .unwrap();
    assert_eq!(delivered.delivery, "delivered");
}

#[tokio::test]
async fn dismissal_and_question_shape_validation_are_durable() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path()).await;
    let question = store.record_question(request()).await.unwrap();
    let dismissed = store.resolve_question(&question.id, None).await.unwrap();
    assert_eq!(dismissed.resolution, "dismissed");
    assert_eq!(dismissed.delivery, "queued");
    assert!(
        store
            .resolve_question(&question.id, None)
            .await
            .unwrap()
            .client_message_id
            .is_some()
    );

    let mut invalid = request();
    invalid.questions.push(invalid.questions[0].clone());
    invalid.item_id = "item-2".into();
    assert!(store.record_question(invalid).await.is_err());
}

#[tokio::test]
async fn closing_thread_invalidates_pending_and_undelivered_question_intents() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path()).await;
    let mut dispatching = request();
    dispatching.item_id = "dispatching".into();
    let dispatching = store.record_question(dispatching).await.unwrap();
    store
        .resolve_question(&dispatching.id, Some(answers("Desktop")))
        .await
        .unwrap();
    store
        .claim_question_delivery(&dispatching.id)
        .await
        .unwrap()
        .unwrap();

    let mut uncertain = request();
    uncertain.item_id = "uncertain".into();
    let uncertain = store.record_question(uncertain).await.unwrap();
    store
        .resolve_question(&uncertain.id, Some(answers("Desktop")))
        .await
        .unwrap();
    store
        .claim_question_delivery(&uncertain.id)
        .await
        .unwrap()
        .unwrap();
    store
        .finish_question_delivery(&uncertain.id, "uncertain", Some("connection lost"))
        .await
        .unwrap();

    let mut queued = request();
    queued.item_id = "queued".into();
    let queued = store.record_question(queued).await.unwrap();
    store
        .resolve_question(&queued.id, Some(answers("Desktop")))
        .await
        .unwrap();
    let mut pending = request();
    pending.item_id = "pending".into();
    let pending = store.record_question(pending).await.unwrap();

    let closed = store
        .close_questions("thread-1", Some("turn-1"), "thread stopped")
        .await
        .unwrap();
    assert_eq!(closed.len(), 4);
    assert!(
        closed
            .iter()
            .all(|question| question.resolution == "closed" && question.delivery == "none")
    );
    assert!(!store.pending_run_questions("run-1").await.unwrap());
    for question in [&dispatching, &uncertain, &queued, &pending] {
        assert!(
            store
                .claim_question_delivery(&question.id)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        store
            .close_questions("thread-1", Some("turn-1"), "thread stopped")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn uncertain_delivery_can_be_reconciled_as_delivered_without_requeueing() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path()).await;
    let question = store.record_question(request()).await.unwrap();
    let queued = store
        .resolve_question(&question.id, Some(answers("Desktop")))
        .await
        .unwrap();
    store
        .claim_question_delivery(&question.id)
        .await
        .unwrap()
        .unwrap();
    store
        .finish_question_delivery(&question.id, "uncertain", Some("response lost"))
        .await
        .unwrap();
    let delivered = store
        .finish_question_delivery(&question.id, "delivered", None)
        .await
        .unwrap();
    assert_eq!(delivered.delivery, "delivered");
    assert_eq!(delivered.client_message_id, queued.client_message_id);
    assert!(!store.pending_run_questions("run-1").await.unwrap());
    assert!(
        store
            .claim_question_delivery(&question.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .finish_question_delivery(&question.id, "queued", None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn task_run_cannot_complete_until_question_is_answered_and_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path()).await;
    let task = store
        .create_task(TaskInput {
            agent_id: "agent-1".into(),
            title: "ask".into(),
            instructions: "ask a question".into(),
            acceptance_criteria: vec![],
            context: None,
            cwd: None,
            project_id: None,
            model: None,
            effort: None,
            workspace_permission: None,
            dependencies: vec![],
        })
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(&task.id, &run.id, "thread-1", "turn-1", &run.lease_token)
        .await
        .unwrap();
    let mut question_request = request();
    question_request.task_id = Some(task.id.clone());
    question_request.run_id = Some(run.id.clone());
    store.record_question(question_request).await.unwrap();
    assert!(
        store
            .propose_completion(
                &task.id,
                &run.id,
                &run.lease_token,
                "done",
                vec!["evidence".into()]
            )
            .await
            .is_err()
    );

    let question = store
        .list_questions(Some("thread-1"), true)
        .await
        .unwrap()
        .remove(0);
    store
        .resolve_question(&question.id, Some(answers("Desktop")))
        .await
        .unwrap();
    assert!(store.pending_run_questions(&run.id).await.unwrap());
    let claimed = store
        .claim_question_delivery(&question.id)
        .await
        .unwrap()
        .unwrap();
    store
        .finish_question_delivery(&question.id, "delivered", None)
        .await
        .unwrap();
    assert_eq!(claimed.delivery, "dispatching");
    assert!(!store.pending_run_questions(&run.id).await.unwrap());
    store
        .propose_completion(
            &task.id,
            &run.id,
            &run.lease_token,
            "done",
            vec!["evidence".into()],
        )
        .await
        .unwrap();
    store
        .settle_run(&task.id, &run.id, "turn-1", &run.lease_token, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn question_continuation_clears_source_turn_and_stale_completion_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path()).await;
    let task = store
        .create_task(TaskInput {
            agent_id: "agent-1".into(),
            title: "follow up".into(),
            instructions: "continue after input".into(),
            acceptance_criteria: vec![],
            context: None,
            cwd: None,
            project_id: None,
            model: None,
            effort: None,
            workspace_permission: None,
            dependencies: vec![],
        })
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(
            &task.id,
            &run.id,
            "thread-1",
            "source-turn",
            &run.lease_token,
        )
        .await
        .unwrap();
    store
        .propose_completion(
            &task.id,
            &run.id,
            &run.lease_token,
            "stale proposal",
            vec!["old evidence".into()],
        )
        .await
        .unwrap();
    let mut question_request = request();
    question_request.task_id = Some(task.id.clone());
    question_request.run_id = Some(run.id.clone());
    store.record_question(question_request).await.unwrap();
    store
        .wait_for_input(&task.id, &run.id, &run.lease_token)
        .await
        .unwrap();
    let resumed = store
        .resume_run(&task.id, &run.id, &run.lease_token, "worker")
        .await
        .unwrap();
    assert!(
        store
            .prepare_question_continuation(&task.id, &run.id, "wrong-turn", &resumed.lease_token,)
            .await
            .is_err()
    );
    let prepared = store
        .prepare_question_continuation(&task.id, &run.id, "source-turn", &resumed.lease_token)
        .await
        .unwrap();
    assert_eq!(prepared.state, "running");
    assert!(prepared.turn_id.is_none());
    assert!(
        store
            .attach_continuation(
                &task.id,
                &run.id,
                "thread-1",
                "answer-turn",
                &prepared.lease_token
            )
            .await
            .is_ok()
    );
}
