use brr_agent_core::{AgentStore, DelegationNode, TaskInput};

async fn store(path: &std::path::Path) -> AgentStore {
    let store = AgentStore::open(path).await.unwrap();
    for agent in ["agent-a", "agent-b"] {
        let candidate = serde_json::json!({
            "name": agent,
            "description": "fixture",
            "instructions": "test",
            "soul": "test",
            "model": "gpt-5",
            "effort": "medium"
        });
        sqlx::query("INSERT OR IGNORE INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES(?,1,?,0,0)")
            .bind(agent)
            .bind(candidate.to_string())
            .execute(store.pool())
            .await
            .unwrap();
    }
    store
}

fn task(agent_id: &str, title: &str, dependencies: Vec<String>) -> TaskInput {
    TaskInput {
        agent_id: agent_id.into(),
        title: title.into(),
        instructions: format!("Complete {title}"),
        acceptance_criteria: vec!["provide evidence".into()],
        context: Some("reviewed graph template".into()),
        cwd: None,
        project_id: Some("project-x".into()),
        model: Some("gpt-5".into()),
        effort: Some("medium".into()),
        dependencies,
    }
}

#[tokio::test]
async fn reviewed_multi_agent_graph_becomes_one_idempotent_recurring_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path()).await;
    let parent = store
        .create_task(task("agent-a", "reviewed graph", vec![]))
        .await
        .unwrap();
    let blocked_dependency = store
        .create_task(task(
            "agent-a",
            "blocked prerequisite",
            vec![parent.id.clone()],
        ))
        .await
        .unwrap();
    let external = store
        .create_task(task(
            "agent-a",
            "external prerequisite",
            vec![blocked_dependency.id],
        ))
        .await
        .unwrap();
    let run = store.claim_ready("reviewer").await.unwrap().unwrap();
    store
        .attach_run(
            &parent.id,
            &run.id,
            "review-thread",
            "turn-1",
            &run.lease_token,
        )
        .await
        .unwrap();
    let nodes = vec![
        DelegationNode {
            key: "research".into(),
            task: task("agent-b", "research", vec![external.id.clone()]),
            depends_on: vec![],
        },
        DelegationNode {
            key: "write".into(),
            task: task("agent-a", "write", vec![]),
            depends_on: vec!["research".into()],
        },
    ];
    let proposal = store.propose_delegation(&parent.id, nodes).await.unwrap();

    assert!(
        store
            .schedule_delegation(
                &proposal.id,
                &proposal.hash,
                "Daily review",
                "not cron",
                "UTC"
            )
            .await
            .is_err()
    );
    assert!(store.list_workflows(None).await.unwrap().is_empty());
    assert_eq!(
        store.list_proposals(None).await.unwrap()[0].status,
        "proposed"
    );

    let workflow = store
        .schedule_delegation(
            &proposal.id,
            &proposal.hash,
            "Daily review",
            "0 9 * * 1-5",
            "UTC",
        )
        .await
        .unwrap();
    let replay = store
        .schedule_delegation(
            &proposal.id,
            &proposal.hash,
            "Daily review",
            "0 9 * * 1-5",
            "UTC",
        )
        .await
        .unwrap();
    assert_eq!(replay.id, workflow.id);
    assert!(
        store
            .schedule_delegation(
                &proposal.id,
                &proposal.hash,
                "Changed name",
                "0 9 * * 1-5",
                "UTC"
            )
            .await
            .is_err()
    );
    assert_eq!(store.list_workflows(None).await.unwrap().len(), 1);
    assert_eq!(
        store.list_proposals(None).await.unwrap()[0].status,
        "scheduled"
    );

    let tasks = store
        .tick_workflows(workflow.next_run_at.unwrap())
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    let research = tasks.iter().find(|task| task.title == "research").unwrap();
    let write = tasks.iter().find(|task| task.title == "write").unwrap();
    assert_eq!(research.agent_id, "agent-b");
    assert_eq!(research.dependencies, vec![external.id]);
    assert_eq!(write.dependencies, vec![research.id.clone()]);
    let stored_parent = store.get_task(&parent.id).await.unwrap();
    assert_eq!(stored_parent.status, "running");
    assert_eq!(stored_parent.thread_id.as_deref(), Some("review-thread"));
}
