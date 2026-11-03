use brr_agent_core::{
    AgentStore, DelegationNode, NewQuestionRequest, Question, TaskAction, TaskInput, WorkflowInput,
};

async fn open_store(path: &std::path::Path) -> AgentStore {
    let store = AgentStore::open(path).await.unwrap();
    for agent in ["agent-1", "agent-2"] {
        let candidate = serde_json::json!({"name":agent,"description":"test fixture","instructions":"test","soul":"test","model":"gpt-5","effort":"medium"});
        sqlx::query("INSERT OR IGNORE INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES(?,1,?,0,0)")
            .bind(agent).bind(candidate.to_string()).execute(store.pool()).await.unwrap();
    }
    store
}

fn task(title: &str, dependencies: Vec<String>) -> TaskInput {
    TaskInput {
        agent_id: "agent-1".into(),
        title: title.into(),
        instructions: format!("Complete {title}"),
        acceptance_criteria: vec!["result is supported by evidence".into()],
        context: None,
        cwd: None,
        project_id: None,
        model: None,
        effort: None,
        workspace_permission: None,
        dependencies,
    }
}

#[tokio::test]
async fn ready_claims_wait_for_dependencies_and_are_durable() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let parent = store.create_task(task("parent", vec![])).await.unwrap();
    let child = store
        .create_task(task("child", vec![parent.id.clone()]))
        .await
        .unwrap();

    let claim = store.claim_ready("worker-1").await.unwrap().unwrap();
    assert_eq!(claim.task_id, parent.id);
    assert!(store.claim_ready("worker-2").await.unwrap().is_none());

    let reopened = open_store(dir.path()).await;
    assert_eq!(reopened.get_task(&child.id).await.unwrap().status, "queued");
    assert!(reopened.claim_ready("worker-2").await.unwrap().is_none());
}

#[tokio::test]
async fn ready_claims_skip_workspaces_reserved_by_running_or_waiting_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    store
        .update_task_settings(brr_agent_core::TaskSettings {
            max_background: 1,
            max_per_agent: 1,
        })
        .await
        .unwrap();
    let mut first_input = task("first in workspace", vec![]);
    first_input.cwd = Some("/tmp/shared-workspace".into());
    let first = store.create_task(first_input).await.unwrap();
    let mut second_input = task("queued in workspace", vec![]);
    second_input.cwd = Some("/tmp/shared-workspace".into());
    let second = store.create_task(second_input).await.unwrap();
    let owner = store.claim_ready("worker").await.unwrap().unwrap();
    assert!(owner.task_id == first.id || owner.task_id == second.id);
    store
        .waiting_run(&owner.task_id, &owner.id, &owner.lease_token)
        .await
        .unwrap();

    let next = store
        .claim_ready_excluding_workspaces("worker-2", &["/tmp/shared-workspace".into()])
        .await
        .unwrap();
    assert!(
        next.is_none(),
        "queued work must not consume the owner's resume slot"
    );

    let resumed = store
        .resume_run(&owner.task_id, &owner.id, &owner.lease_token, "worker")
        .await
        .unwrap();
    assert_eq!(resumed.id, owner.id);
}

#[tokio::test]
async fn waiting_for_input_yields_worker_capacity_and_keeps_run_binding() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    store
        .update_task_settings(brr_agent_core::TaskSettings {
            max_background: 1,
            max_per_agent: 1,
        })
        .await
        .unwrap();
    let waiting_task = store
        .create_task(task("needs input", vec![]))
        .await
        .unwrap();
    let run = store.claim_ready("worker-1").await.unwrap().unwrap();
    assert_eq!(run.task_id, waiting_task.id);
    store
        .attach_run(
            &waiting_task.id,
            &run.id,
            "saved-thread",
            "turn-1",
            &run.lease_token,
        )
        .await
        .unwrap();

    let waiting = store
        .wait_for_input(&waiting_task.id, &run.id, &run.lease_token)
        .await
        .unwrap();
    assert_eq!(waiting.status, "waiting_input");
    assert_eq!(store.get_run(&run.id).await.unwrap().state, "waiting_input");
    assert_eq!(
        store.get_run(&run.id).await.unwrap().thread_id.as_deref(),
        Some("saved-thread")
    );
    assert!(
        store
            .unresolved_runs()
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.id == run.id)
    );

    let queued_task = store.create_task(task("next task", vec![])).await.unwrap();
    let next = store.claim_ready("worker-2").await.unwrap().unwrap();
    assert_eq!(next.task_id, queued_task.id);
    store
        .control_task(&waiting_task.id, TaskAction::Cancel)
        .await
        .unwrap();
    assert!(
        store
            .resume_run(&waiting_task.id, &run.id, &run.lease_token, "worker-3")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn waiting_for_input_can_be_paused_and_recovered_without_losing_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let task = store
        .create_task(task("pause while waiting", vec![]))
        .await
        .unwrap();
    let run = store.claim_ready("worker-1").await.unwrap().unwrap();
    store
        .attach_run(
            &task.id,
            &run.id,
            "saved-thread",
            "turn-1",
            &run.lease_token,
        )
        .await
        .unwrap();
    store
        .wait_for_input(&task.id, &run.id, &run.lease_token)
        .await
        .unwrap();
    assert_eq!(
        store
            .recover_run(&run.id, &run.lease_token)
            .await
            .unwrap()
            .state,
        "waiting_input"
    );
    store
        .control_task(&task.id, TaskAction::Pause)
        .await
        .unwrap();
    assert_eq!(store.get_run(&run.id).await.unwrap().state, "paused");
    store
        .control_task(&task.id, TaskAction::Resume)
        .await
        .unwrap();
    let resumed = store.claim_ready("worker-2").await.unwrap().unwrap();
    assert_eq!(resumed.id, run.id);
    assert_eq!(resumed.thread_id.as_deref(), Some("saved-thread"));
}

#[tokio::test]
async fn resuming_paused_question_run_restores_waiting_input_without_reclaiming_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let task = store
        .create_task(task("question task", vec![]))
        .await
        .unwrap();
    let run = store.claim_ready("worker-1").await.unwrap().unwrap();
    let attached = store
        .attach_run(
            &task.id,
            &run.id,
            "saved-thread",
            "source-turn",
            &run.lease_token,
        )
        .await
        .unwrap();
    store
        .wait_for_input(&task.id, &run.id, &attached.lease_token)
        .await
        .unwrap();
    store
        .record_question(NewQuestionRequest {
            thread_id: "saved-thread".into(),
            turn_id: "source-turn".into(),
            item_id: "item-1".into(),
            agent_id: Some(task.agent_id.clone()),
            task_id: Some(task.id.clone()),
            run_id: Some(run.id.clone()),
            kind: "blocking".into(),
            questions: vec![Question {
                id: "choice".into(),
                header: "Choice".into(),
                question: "Choose".into(),
                options: None,
            }],
            runtime_generation: None,
            runtime_request_id: None,
        })
        .await
        .unwrap();

    store
        .control_task(&task.id, TaskAction::Pause)
        .await
        .unwrap();
    let resumed = store
        .control_task(&task.id, TaskAction::Resume)
        .await
        .unwrap();

    assert_eq!(resumed.status, "waiting_input");
    let waiting_run = store.get_run(&run.id).await.unwrap();
    assert_eq!(waiting_run.state, "waiting_input");
    assert_eq!(waiting_run.lease_token, attached.lease_token);
    assert_eq!(waiting_run.thread_id.as_deref(), Some("saved-thread"));
    assert!(store.claim_ready("worker-2").await.unwrap().is_none());
}

#[tokio::test]
async fn dependency_releases_only_after_evidence_and_authoritative_turn_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let first = store.create_task(task("first", vec![])).await.unwrap();
    let second = store
        .create_task(task("second", vec![first.id.clone()]))
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    let run = store
        .attach_run(&first.id, &run.id, "thread", "turn-1", &run.lease_token)
        .await
        .unwrap();

    assert!(
        store
            .settle_run(&first.id, &run.id, "turn-1", &run.lease_token, true)
            .await
            .is_err()
    );
    assert!(store.claim_ready("worker-2").await.unwrap().is_none());
    store
        .propose_completion(
            &first.id,
            &run.id,
            &run.lease_token,
            "finished",
            vec!["file:result.md".into()],
        )
        .await
        .unwrap();
    let settled = store
        .settle_run(&first.id, &run.id, "turn-1", &run.lease_token, true)
        .await
        .unwrap();
    assert_eq!(settled.status, "succeeded");
    let next = store.claim_ready("worker-2").await.unwrap().unwrap();
    assert_eq!(next.task_id, second.id);
}

#[tokio::test]
async fn task_creation_operation_is_idempotent_across_retries_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let first = store
        .create_task_once(task("once", vec![]), "invoke-42")
        .await
        .unwrap();
    let retry = store
        .create_task_once(task("once", vec![]), "invoke-42")
        .await
        .unwrap();
    assert_eq!(first.id, retry.id);
    assert!(
        store
            .create_task_once(task("different payload", vec![]), "invoke-42")
            .await
            .is_err()
    );
    let reopened = open_store(dir.path()).await;
    assert_eq!(reopened.list_tasks(None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_terminalizes_waiting_runs_and_they_cannot_be_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let task = store
        .create_task(task("cancel waiting", vec![]))
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(
            &task.id,
            &run.id,
            "saved-thread",
            "turn-1",
            &run.lease_token,
        )
        .await
        .unwrap();
    store
        .waiting_run(&task.id, &run.id, &run.lease_token)
        .await
        .unwrap();
    store
        .control_task(&task.id, TaskAction::Cancel)
        .await
        .unwrap();
    assert!(
        store
            .unresolved_runs()
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.id != run.id)
    );
    assert!(
        store
            .resume_run(&task.id, &run.id, &run.lease_token, "worker-2")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn pause_and_resume_keep_the_same_run_and_thread_binding() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let task = store
        .create_task(task("pause continuity", vec![]))
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(
            &task.id,
            &run.id,
            "saved-thread",
            "turn-1",
            &run.lease_token,
        )
        .await
        .unwrap();
    store
        .control_task(&task.id, TaskAction::Pause)
        .await
        .unwrap();
    store
        .control_task(&task.id, TaskAction::Resume)
        .await
        .unwrap();
    let resumed = store.claim_ready("worker-2").await.unwrap().unwrap();
    assert_eq!(resumed.id, run.id);
    assert_eq!(resumed.thread_id.as_deref(), Some("saved-thread"));
    assert_ne!(resumed.lease_token, run.lease_token);
}

#[tokio::test]
async fn delegation_keeps_external_dependencies_and_allows_another_agent() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let external = store.create_task(task("external", vec![])).await.unwrap();
    // Keep the external prerequisite unavailable so the claim deterministically owns the parent.
    store
        .control_task(&external.id, TaskAction::Pause)
        .await
        .unwrap();
    let parent = store.create_task(task("parent", vec![])).await.unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(
            &parent.id,
            &run.id,
            "parent-thread",
            "turn",
            &run.lease_token,
        )
        .await
        .unwrap();
    let mut child_input = task("child", vec![external.id.clone()]);
    child_input.agent_id = "agent-2".into();
    let proposal = store
        .propose_delegation(
            &parent.id,
            vec![DelegationNode {
                key: "child".into(),
                task: child_input,
                depends_on: vec![],
            }],
        )
        .await
        .unwrap();
    let child = store
        .approve_delegation(&proposal.id, &proposal.hash)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(child.agent_id, "agent-2");
    assert_eq!(child.dependencies, vec![external.id]);
}

#[tokio::test]
async fn workflow_allows_cross_agent_nodes_and_preserves_external_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let external = store.create_task(task("external", vec![])).await.unwrap();
    let root = task("workflow root", vec![]);
    let mut child = task("workflow child", vec![external.id.clone()]);
    child.agent_id = "agent-2".into();
    let workflow = store
        .create_workflow(WorkflowInput {
            name: "cross agent".into(),
            task: root,
            expression: "* * * * *".into(),
            timezone: "UTC".into(),
            nodes: Some(vec![DelegationNode {
                key: "child".into(),
                task: child,
                depends_on: vec![],
            }]),
        })
        .await
        .unwrap();
    let tasks = store
        .tick_workflows(workflow.next_run_at.unwrap())
        .await
        .unwrap();
    assert_eq!(tasks[0].agent_id, "agent-2");
    assert_eq!(tasks[0].dependencies, vec![external.id]);
}

#[tokio::test]
async fn wildcard_step_one_has_wildcard_calendar_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let regular = store
        .create_workflow(WorkflowInput {
            name: "regular wildcard".into(),
            task: task("regular", vec![]),
            expression: "0 0 * * 1".into(),
            timezone: "UTC".into(),
            nodes: None,
        })
        .await
        .unwrap();
    let stepped = store
        .create_workflow(WorkflowInput {
            name: "step wildcard".into(),
            task: task("stepped", vec![]),
            expression: "0 0 */1 * 1".into(),
            timezone: "UTC".into(),
            nodes: None,
        })
        .await
        .unwrap();
    assert_eq!(stepped.next_run_at, regular.next_run_at);
}

#[tokio::test]
async fn tasks_require_a_persisted_agent_profile() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    assert!(
        store
            .create_task(task("unknown owner", vec![]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn only_current_lease_owner_can_record_or_settle_a_completion() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let task = store.create_task(task("fenced", vec![])).await.unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(&task.id, &run.id, "thread", "turn", &run.lease_token)
        .await
        .unwrap();
    assert!(
        store
            .propose_completion(
                &task.id,
                &run.id,
                "stale-token",
                "done",
                vec!["evidence".into()]
            )
            .await
            .is_err()
    );
    assert!(
        store
            .settle_run(&task.id, &run.id, "turn", "stale-token", true)
            .await
            .is_err()
    );
    assert_eq!(store.get_task(&task.id).await.unwrap().status, "running");
}

#[tokio::test]
async fn reviewed_delegation_dispatches_once_and_parent_waits_for_all_children() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let parent = store.create_task(task("parent", vec![])).await.unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(
            &parent.id,
            &run.id,
            "parent-thread",
            "parent-turn",
            &run.lease_token,
        )
        .await
        .unwrap();
    let graph = vec![
        DelegationNode {
            key: "research".into(),
            task: task("research", vec![]),
            depends_on: vec![],
        },
        DelegationNode {
            key: "write".into(),
            task: task("write", vec![]),
            depends_on: vec!["research".into()],
        },
    ];
    let proposal = store.propose_delegation(&parent.id, graph).await.unwrap();
    assert!(
        store
            .approve_delegation(&proposal.id, "wrong-hash")
            .await
            .is_err()
    );
    let children = store
        .approve_delegation(&proposal.id, &proposal.hash)
        .await
        .unwrap();
    let retry = store
        .approve_delegation(&proposal.id, &proposal.hash)
        .await
        .unwrap();
    assert_eq!(children, retry);
    assert_eq!(
        store.get_task(&parent.id).await.unwrap().status,
        "waiting_children"
    );

    let first = store.claim_ready("worker").await.unwrap().unwrap();
    assert_eq!(first.task_id, children[0].id);
    store
        .attach_run(
            &first.task_id,
            &first.id,
            "child-thread",
            "child-turn",
            &first.lease_token,
        )
        .await
        .unwrap();
    store
        .propose_completion(
            &first.task_id,
            &first.id,
            &first.lease_token,
            "done",
            vec!["evidence".into()],
        )
        .await
        .unwrap();
    store
        .settle_run(
            &first.task_id,
            &first.id,
            "child-turn",
            &first.lease_token,
            true,
        )
        .await
        .unwrap();
    let second = store.claim_ready("worker").await.unwrap().unwrap();
    assert_eq!(second.task_id, children[1].id);
    assert_eq!(
        store.get_task(&parent.id).await.unwrap().status,
        "waiting_children"
    );
    store
        .attach_run(
            &second.task_id,
            &second.id,
            "child-2-thread",
            "child-2-turn",
            &second.lease_token,
        )
        .await
        .unwrap();
    store
        .propose_completion(
            &second.task_id,
            &second.id,
            &second.lease_token,
            "done",
            vec!["evidence".into()],
        )
        .await
        .unwrap();
    store
        .settle_run(
            &second.task_id,
            &second.id,
            "child-2-turn",
            &second.lease_token,
            true,
        )
        .await
        .unwrap();

    let resumed = store.claim_ready("worker").await.unwrap().unwrap();
    assert_eq!(resumed.task_id, parent.id);
    assert_eq!(resumed.id, run.id);
    assert_eq!(resumed.thread_id.as_deref(), Some("parent-thread"));
    assert_ne!(resumed.lease_token, run.lease_token);
    let continued = store
        .attach_continuation(
            &parent.id,
            &resumed.id,
            "parent-thread",
            "parent-turn-2",
            &resumed.lease_token,
        )
        .await
        .unwrap();
    assert_eq!(continued.turn_id.as_deref(), Some("parent-turn-2"));
}

#[tokio::test]
async fn delegation_cycles_are_rejected_before_review() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let parent = store.create_task(task("parent", vec![])).await.unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(&parent.id, &run.id, "thread", "turn", &run.lease_token)
        .await
        .unwrap();
    let cyclic = vec![
        DelegationNode {
            key: "a".into(),
            task: task("a", vec![]),
            depends_on: vec!["b".into()],
        },
        DelegationNode {
            key: "b".into(),
            task: task("b", vec![]),
            depends_on: vec!["a".into()],
        },
    ];
    assert!(store.propose_delegation(&parent.id, cyclic).await.is_err());
}

#[tokio::test]
async fn paused_chat_delegation_parent_dispatches_only_after_review() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let parent = store
        .create_task_paused(task("chat delegation", vec![]))
        .await
        .unwrap();
    let proposal = store
        .propose_delegation(
            &parent.id,
            vec![DelegationNode {
                key: "child".into(),
                task: task("child", vec![]),
                depends_on: vec![],
            }],
        )
        .await
        .unwrap();
    assert!(store.claim_ready("worker").await.unwrap().is_none());
    let child = store
        .approve_delegation(&proposal.id, &proposal.hash)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        store.get_task(&parent.id).await.unwrap().status,
        "waiting_children"
    );
    assert_eq!(
        store.claim_ready("worker").await.unwrap().unwrap().task_id,
        child.id
    );
}

#[tokio::test]
async fn recurring_workflow_coalesces_missed_occurrences_and_materializes_fresh_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let workflow = store
        .create_workflow(WorkflowInput {
            name: "minute job".into(),
            task: task("scheduled", vec![]),
            expression: "* * * * *".into(),
            timezone: "UTC".into(),
            nodes: None,
        })
        .await
        .unwrap();
    let first_at = workflow.next_run_at.unwrap();
    let first = store.tick_workflows(first_at).await.unwrap();
    assert_eq!(first.len(), 1);
    assert!(store.tick_workflows(first_at).await.unwrap().is_empty());

    let latest_missed = first_at + 5 * 60;
    assert!(
        store
            .tick_workflows(latest_missed)
            .await
            .unwrap()
            .is_empty()
    );
    let first_run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(
            &first[0].id,
            &first_run.id,
            "thread",
            "turn",
            &first_run.lease_token,
        )
        .await
        .unwrap();
    store
        .propose_completion(
            &first[0].id,
            &first_run.id,
            &first_run.lease_token,
            "done",
            vec!["evidence".into()],
        )
        .await
        .unwrap();
    store
        .settle_run(
            &first[0].id,
            &first_run.id,
            "turn",
            &first_run.lease_token,
            true,
        )
        .await
        .unwrap();

    let second = store.tick_workflows(latest_missed).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].id, second[0].id);
    let workflows = store.list_workflows(None).await.unwrap();
    assert_eq!(workflows[0].last_status.as_deref(), Some("running"));
}

#[tokio::test]
async fn recurring_cron_uses_earlier_fold_and_skips_nonexistent_local_minutes() {
    use jiff::{Timestamp, tz::TimeZone};
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let gap_workflow = store
        .create_workflow(WorkflowInput {
            name: "spring gap".into(),
            task: task("gap", vec![]),
            expression: "30 2 14 3 *".into(),
            timezone: "America/New_York".into(),
            nodes: None,
        })
        .await
        .unwrap();
    let gap_next = Timestamp::from_second(gap_workflow.next_run_at.unwrap())
        .unwrap()
        .to_zoned(TimeZone::get("America/New_York").unwrap())
        .datetime();
    assert_eq!(
        (
            gap_next.year(),
            gap_next.month(),
            gap_next.day(),
            gap_next.hour(),
            gap_next.minute()
        ),
        (2028, 3, 14, 2, 30)
    );

    let fold_workflow = store
        .create_workflow(WorkflowInput {
            name: "fall fold".into(),
            task: task("fold", vec![]),
            expression: "30 1 31 10 *".into(),
            timezone: "America/New_York".into(),
            nodes: None,
        })
        .await
        .unwrap();
    let zone = TimeZone::get("America/New_York").unwrap();
    let fold_local = jiff::civil::date(2027, 10, 31).at(1, 30, 0, 0);
    let earlier = zone
        .to_ambiguous_timestamp(fold_local)
        .earlier()
        .unwrap()
        .as_second();
    let later = zone
        .to_ambiguous_timestamp(fold_local)
        .later()
        .unwrap()
        .as_second();
    assert_eq!(fold_workflow.next_run_at, Some(earlier));
    store.tick_workflows(later + 60).await.unwrap();
    let current = store
        .list_workflows(None)
        .await
        .unwrap()
        .into_iter()
        .find(|workflow| workflow.id == fold_workflow.id)
        .unwrap();
    assert_eq!(current.last_run_at, Some(earlier));
}

#[tokio::test]
async fn a_nonterminal_turn_continues_with_the_same_task_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let task = store.create_task(task("multi-turn", vec![])).await.unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(&task.id, &run.id, "thread", "turn-1", &run.lease_token)
        .await
        .unwrap();
    store
        .settle_turn(&task.id, &run.id, "turn-1", &run.lease_token)
        .await
        .unwrap();
    let continued = store
        .attach_continuation(&task.id, &run.id, "thread", "turn-2", &run.lease_token)
        .await
        .unwrap();
    assert_eq!(continued.id, run.id);
    assert_eq!(continued.thread_id.as_deref(), Some("thread"));
    assert_eq!(continued.turn_id.as_deref(), Some("turn-2"));
    assert_eq!(
        store
            .attach_continuation(&task.id, &run.id, "thread", "turn-2", &run.lease_token)
            .await
            .unwrap(),
        continued
    );
    assert_eq!(store.get_task(&task.id).await.unwrap().status, "running");
}

#[tokio::test]
async fn thread_binding_survives_a_crash_before_turn_start() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let task = store
        .create_task(task("thread-first", vec![]))
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    let bound = store
        .bind_run_thread(&task.id, &run.id, "persisted-thread", &run.lease_token)
        .await
        .unwrap();
    assert_eq!(bound.turn_id, None);
    let reopened = open_store(dir.path()).await;
    let recovered = reopened.get_run(&run.id).await.unwrap();
    assert_eq!(recovered.thread_id.as_deref(), Some("persisted-thread"));
    assert_eq!(recovered.turn_id, None);
    let attached = reopened
        .attach_continuation(
            &task.id,
            &run.id,
            "persisted-thread",
            "started-turn",
            &run.lease_token,
        )
        .await
        .unwrap();
    assert_eq!(attached.turn_id.as_deref(), Some("started-turn"));
}

#[tokio::test]
async fn unknown_runs_block_automatic_replay_and_resume_with_a_new_fence() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let first = store.create_task(task("uncertain", vec![])).await.unwrap();
    let second = store.create_task(task("next", vec![])).await.unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    let uncertain_id = run.task_id.clone();
    let next_id = if uncertain_id == first.id {
        second.id.clone()
    } else {
        first.id.clone()
    };
    store
        .attach_run(
            &uncertain_id,
            &run.id,
            "thread",
            "possibly-active",
            &run.lease_token,
        )
        .await
        .unwrap();
    store
        .block_run(
            &uncertain_id,
            &run.id,
            &run.lease_token,
            "runtime state is uncertain",
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .unresolved_runs()
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == run.id)
            .unwrap()
            .state,
        "unknown"
    );
    let next = store.claim_ready("worker").await.unwrap().unwrap();
    assert_eq!(next.task_id, next_id);
    let resumed = store
        .resume_run(&uncertain_id, &run.id, &run.lease_token, "worker")
        .await
        .unwrap();
    assert_eq!(resumed.id, run.id);
    assert_ne!(resumed.lease_token, run.lease_token);
    assert_eq!(resumed.thread_id.as_deref(), Some("thread"));
    assert_eq!(resumed.turn_id.as_deref(), Some("possibly-active"));
    assert!(
        store
            .propose_completion(
                &uncertain_id,
                &run.id,
                &run.lease_token,
                "stale",
                vec!["evidence".into()]
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn retry_is_explicit_and_failed_dependency_blocks_downstream_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let failed = store.create_task(task("fails", vec![])).await.unwrap();
    let dependent = store
        .create_task(task("waits", vec![failed.id.clone()]))
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(&failed.id, &run.id, "thread", "turn", &run.lease_token)
        .await
        .unwrap();
    assert_eq!(
        store
            .settle_run(&failed.id, &run.id, "turn", &run.lease_token, false)
            .await
            .unwrap()
            .status,
        "failed"
    );
    assert_eq!(
        store.get_task(&dependent.id).await.unwrap().status,
        "blocked"
    );
    assert_eq!(
        store
            .control_task(&failed.id, TaskAction::Retry)
            .await
            .unwrap()
            .status,
        "queued"
    );
    let retry = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(
            &failed.id,
            &retry.id,
            "thread-2",
            "turn-2",
            &retry.lease_token,
        )
        .await
        .unwrap();
    store
        .propose_completion(
            &failed.id,
            &retry.id,
            &retry.lease_token,
            "fixed",
            vec!["evidence".into()],
        )
        .await
        .unwrap();
    store
        .settle_run(&failed.id, &retry.id, "turn-2", &retry.lease_token, true)
        .await
        .unwrap();
    assert_eq!(
        store.get_task(&dependent.id).await.unwrap().status,
        "queued"
    );
}
