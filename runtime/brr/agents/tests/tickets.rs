use brr_agent_core::{
    AgentStore, NewAcceptanceCriterion, TicketActor, TicketInput, TicketMutation, TicketPriority,
    TicketStatus,
};

async fn store() -> (tempfile::TempDir, AgentStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    (dir, store)
}

fn mutation(operation_id: &str) -> TicketMutation {
    TicketMutation {
        operation_id: operation_id.into(),
        actor: TicketActor::human(),
    }
}

#[tokio::test]
async fn explicit_null_unassigns_agent_while_an_omitted_field_preserves_assignment() {
    let (_directory, store) = store().await;
    let draft = store
        .propose_profile(
            "user",
            None,
            brr_agent_core::ProfileCandidate {
                name: "Assignee".into(),
                description: "Ticket helper".into(),
                instructions: "Check evidence".into(),
                soul: "Be precise".into(),
                model: "fixture".into(),
                effort: "medium".into(),
            },
        )
        .await
        .unwrap();
    store
        .approve_profile(&draft.id, &draft.hash, "assignment-review")
        .await
        .unwrap();
    let agent = store
        .apply_profile(&draft.id, &draft.hash, "assignment-create")
        .await
        .unwrap();
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project".into(),
                title: "Assigned work".into(),
                description: String::new(),
                acceptance_criteria: vec![],
                agent_id: Some(agent.id.clone()),
                priority: TicketPriority::None,
                status: TicketStatus::Todo,
            },
            mutation("assignment-ticket"),
        )
        .await
        .unwrap();
    let metadata = serde_json::from_value(serde_json::json!({"title":"Reworded work"})).unwrap();
    let unchanged = store
        .update_ticket(
            &ticket.id,
            ticket.revision,
            metadata,
            mutation("assignment-metadata"),
        )
        .await
        .unwrap();
    assert_eq!(unchanged.agent_id.as_deref(), Some(agent.id.as_str()));
    let unassign: brr_agent_core::TicketUpdate =
        serde_json::from_value(serde_json::json!({"agentId":null})).unwrap();
    assert_ne!(
        serde_json::to_value(&unassign).unwrap(),
        serde_json::to_value(brr_agent_core::TicketUpdate::default()).unwrap()
    );
    let cleared = store
        .update_ticket(
            &ticket.id,
            unchanged.revision,
            unassign,
            mutation("assignment-clear"),
        )
        .await
        .unwrap();
    assert_eq!(cleared.agent_id, None);
}

#[tokio::test]
async fn ticket_creation_is_idempotent_and_assigns_public_id_and_criteria_ids() {
    let (dir, store) = store().await;
    let input = TicketInput {
        project_id: "project-1".into(),
        title: "Ship tickets".into(),
        description: "Add local ticket tracking".into(),
        acceptance_criteria: vec![NewAcceptanceCriterion {
            id: None,
            text: "Ticket can be reviewed".into(),
            checked: false,
        }],
        agent_id: None,
        priority: TicketPriority::Medium,
        status: TicketStatus::Todo,
    };
    let first = store
        .create_ticket(input.clone(), mutation("create-1"))
        .await
        .unwrap();
    let retry = store
        .create_ticket(input, mutation("create-1"))
        .await
        .unwrap();
    assert_eq!(first, retry);
    assert!(uuid::Uuid::parse_str(&first.id).is_ok());
    assert!(first.number.starts_with("BRR-"));
    assert_eq!(first.acceptance_criteria[0].text, "Ticket can be reviewed");
    assert!(!first.acceptance_criteria[0].checked);
    let reopened = AgentStore::open(dir.path()).await.unwrap();
    assert_eq!(reopened.get_ticket(&first.id).await.unwrap(), first);
}

#[tokio::test]
async fn status_writes_advance_status_revision_even_when_the_value_is_unchanged() {
    let (_dir, store) = store().await;
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "Review work".into(),
                description: "".into(),
                acceptance_criteria: vec![],
                agent_id: None,
                priority: TicketPriority::None,
                status: TicketStatus::Todo,
            },
            mutation("create"),
        )
        .await
        .unwrap();

    let metadata = store
        .update_ticket(
            &ticket.id,
            ticket.revision,
            brr_agent_core::TicketUpdate {
                title: Some("Updated title".into()),
                ..Default::default()
            },
            mutation("metadata"),
        )
        .await
        .unwrap();
    assert_eq!(metadata.status_revision, ticket.status_revision);

    let explicit_same_status = store
        .update_ticket(
            &ticket.id,
            metadata.revision,
            brr_agent_core::TicketUpdate {
                status: Some(TicketStatus::Todo),
                ..Default::default()
            },
            mutation("status-write"),
        )
        .await
        .unwrap();
    assert_eq!(
        explicit_same_status.status_revision,
        metadata.status_revision + 1
    );

    let stale = store
        .update_ticket(
            &ticket.id,
            ticket.revision,
            brr_agent_core::TicketUpdate::default(),
            mutation("stale"),
        )
        .await
        .unwrap_err();
    let parsed: serde_json::Value = serde_json::from_str(&stale).unwrap();
    assert_eq!(parsed["code"], "conflict");
    assert_eq!(parsed["current"]["revision"], explicit_same_status.revision);
}

#[tokio::test]
async fn comments_are_append_only_and_operation_retries_are_idempotent() {
    let (_dir, store) = store().await;
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "Discuss".into(),
                description: "".into(),
                acceptance_criteria: vec![],
                agent_id: None,
                priority: TicketPriority::Low,
                status: TicketStatus::Todo,
            },
            mutation("create"),
        )
        .await
        .unwrap();
    let first = store
        .comment_ticket(
            &ticket.id,
            "Keep the runtime transcript private",
            mutation("comment"),
        )
        .await
        .unwrap();
    let retry = store
        .comment_ticket(
            &ticket.id,
            "Keep the runtime transcript private",
            mutation("comment"),
        )
        .await
        .unwrap();
    assert_eq!(first, retry);
    let detail = store.read_ticket(&ticket.id, None, None).await.unwrap();
    assert_eq!(detail.comments.data, vec![first]);
    assert_eq!(detail.ticket.revision, ticket.revision);
}

#[tokio::test]
async fn approved_run_is_linked_once_and_lifecycle_auto_status_respects_explicit_writes() {
    let (_dir, store) = store().await;
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-a',1,'{\"name\":\"Agent A\",\"description\":\"fixture\",\"instructions\":\"test\",\"soul\":\"test\",\"model\":\"fixture\",\"effort\":\"medium\"}',0,0)")
        .execute(store.pool()).await.unwrap();
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "Implement feature".into(),
                description: "Details".into(),
                acceptance_criteria: vec![NewAcceptanceCriterion {
                    id: None,
                    text: "Works".into(),
                    checked: false,
                }],
                agent_id: Some("agent-a".into()),
                priority: TicketPriority::High,
                status: TicketStatus::Todo,
            },
            mutation("create"),
        )
        .await
        .unwrap();
    let snapshot = brr_agent_core::TaskInput {
        agent_id: "agent-a".into(),
        title: ticket.title.clone(),
        instructions: ticket.description.clone(),
        acceptance_criteria: vec!["Works".into()],
        context: None,
        cwd: Some("/registered/project".into()),
        project_id: Some(ticket.project_id.clone()),
        model: Some("fixture".into()),
        effort: Some("medium".into()),
        workspace_permission: Some(brr_agent_core::WorkspacePermission::WorkspaceWrite),
        dependencies: vec![],
    };
    let proposal = store
        .propose_ticket_run(&ticket.id, ticket.revision, snapshot, mutation("propose"))
        .await
        .unwrap();
    let task = store
        .approve_ticket_run(&ticket.id, &proposal.hash, mutation("approve"))
        .await
        .unwrap();
    let retry = store
        .approve_ticket_run(&ticket.id, &proposal.hash, mutation("approve"))
        .await
        .unwrap();
    assert_eq!(task, retry);
    let repeated_approval = store
        .approve_ticket_run(&ticket.id, &proposal.hash, mutation("approve-again"))
        .await
        .unwrap();
    assert_eq!(repeated_approval, task);

    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_continuation(&task.id, &run.id, "thread", "turn-1", &run.lease_token)
        .await
        .unwrap();
    let in_progress = store.get_ticket(&ticket.id).await.unwrap();
    assert_eq!(in_progress.status, TicketStatus::InProgress);
    store
        .propose_completion(
            &task.id,
            &run.id,
            &run.lease_token,
            "Implemented",
            vec!["Rust behavioral test passed".into()],
        )
        .await
        .unwrap();
    let explicit = store
        .update_ticket(
            &ticket.id,
            in_progress.revision,
            brr_agent_core::TicketUpdate {
                status: Some(TicketStatus::Complete),
                ..Default::default()
            },
            mutation("explicit-complete"),
        )
        .await
        .unwrap();
    store
        .settle_run(&task.id, &run.id, "turn-1", &run.lease_token, true)
        .await
        .unwrap();
    assert_eq!(
        store.get_ticket(&ticket.id).await.unwrap().status,
        explicit.status
    );
    let detail = store.read_ticket(&ticket.id, None, None).await.unwrap();
    assert_eq!(detail.runs.len(), 1);
    assert_eq!(detail.runs[0].status, "succeeded");
    assert_eq!(detail.runs[0].result.as_deref(), Some("Implemented"));
}

#[tokio::test]
async fn run_proposal_cannot_change_ticket_title_instructions_or_acceptance_criteria() {
    let (_dir, store) = store().await;
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-a',1,'{\"name\":\"Agent A\",\"description\":\"fixture\",\"instructions\":\"test\",\"soul\":\"test\",\"model\":\"fixture\",\"effort\":\"medium\"}',0,0)")
        .execute(store.pool()).await.unwrap();
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "Canonical title".into(),
                description: "Canonical description".into(),
                acceptance_criteria: vec![NewAcceptanceCriterion {
                    id: None,
                    text: "Criterion".into(),
                    checked: false,
                }],
                agent_id: Some("agent-a".into()),
                priority: TicketPriority::Medium,
                status: TicketStatus::Todo,
            },
            mutation("create"),
        )
        .await
        .unwrap();
    let snapshot = brr_agent_core::TaskInput {
        agent_id: "agent-a".into(),
        title: "Different title".into(),
        instructions: "Canonical description".into(),
        acceptance_criteria: vec!["Criterion".into()],
        context: None,
        cwd: Some("/project".into()),
        project_id: Some(ticket.project_id.clone()),
        model: Some("fixture".into()),
        effort: Some("medium".into()),
        workspace_permission: None,
        dependencies: vec![],
    };
    assert!(
        store
            .propose_ticket_run(&ticket.id, ticket.revision, snapshot, mutation("propose"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn actual_continuation_moves_any_unchanged_status_once_then_success_moves_to_review() {
    let (_dir, store) = store().await;
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-a',1,'{\"name\":\"Agent A\",\"description\":\"fixture\",\"instructions\":\"test\",\"soul\":\"test\",\"model\":\"fixture\",\"effort\":\"medium\"}',0,0)")
        .execute(store.pool()).await.unwrap();
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "Continue work".into(),
                description: "Keep going".into(),
                acceptance_criteria: vec![],
                agent_id: Some("agent-a".into()),
                priority: TicketPriority::None,
                status: TicketStatus::Review,
            },
            mutation("create"),
        )
        .await
        .unwrap();
    let snapshot = brr_agent_core::TaskInput {
        agent_id: "agent-a".into(),
        title: ticket.title.clone(),
        instructions: ticket.description.clone(),
        acceptance_criteria: vec![],
        context: None,
        cwd: Some("/project".into()),
        project_id: Some(ticket.project_id.clone()),
        model: Some("fixture".into()),
        effort: Some("medium".into()),
        workspace_permission: None,
        dependencies: vec![],
    };
    let proposal = store
        .propose_ticket_run(&ticket.id, ticket.revision, snapshot, mutation("propose"))
        .await
        .unwrap();
    let task = store
        .approve_ticket_run(&ticket.id, &proposal.hash, mutation("approve"))
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    assert_eq!(
        store.get_ticket(&ticket.id).await.unwrap().status,
        TicketStatus::Review,
        "claiming a task must not change ticket status"
    );
    assert!(
        store
            .attach_continuation(&task.id, &run.id, "thread", "turn-1", "stale-lease")
            .await
            .is_err()
    );
    assert_eq!(
        store.get_ticket(&ticket.id).await.unwrap().status,
        TicketStatus::Review
    );
    store
        .attach_continuation(&task.id, &run.id, "thread", "turn-1", &run.lease_token)
        .await
        .unwrap();
    let in_progress = store.get_ticket(&ticket.id).await.unwrap();
    assert_eq!(in_progress.status, TicketStatus::InProgress);
    // A second attachment of the same continuation is idempotent for ticket lifecycle state.
    store
        .attach_continuation(&task.id, &run.id, "thread", "turn-1", &run.lease_token)
        .await
        .unwrap();
    assert_eq!(
        store.get_ticket(&ticket.id).await.unwrap().status_revision,
        in_progress.status_revision
    );
    let metadata = store
        .update_ticket(
            &ticket.id,
            in_progress.revision,
            brr_agent_core::TicketUpdate {
                description: Some("Clarified while work is running".into()),
                ..Default::default()
            },
            mutation("description"),
        )
        .await
        .unwrap();
    store
        .comment_ticket(&ticket.id, "Progress note", mutation("comment"))
        .await
        .unwrap();
    store
        .propose_completion(
            &task.id,
            &run.id,
            &run.lease_token,
            "Completed",
            vec!["verified".into()],
        )
        .await
        .unwrap();
    store
        .settle_run(&task.id, &run.id, "turn-1", &run.lease_token, true)
        .await
        .unwrap();
    let completed = store.get_ticket(&ticket.id).await.unwrap();
    assert_eq!(
        completed.status,
        TicketStatus::Review,
        "metadata edits and comments must not disable automatic transitions"
    );
    assert!(completed.revision > metadata.revision);
}

#[tokio::test]
async fn explicit_same_status_write_prevents_later_automatic_status_changes() {
    let (_dir, store) = store().await;
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-a',1,'{\"name\":\"Agent A\",\"description\":\"fixture\",\"instructions\":\"test\",\"soul\":\"test\",\"model\":\"fixture\",\"effort\":\"medium\"}',0,0)")
        .execute(store.pool()).await.unwrap();
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "Explicit status".into(),
                description: "Do work".into(),
                acceptance_criteria: vec![],
                agent_id: Some("agent-a".into()),
                priority: TicketPriority::None,
                status: TicketStatus::Todo,
            },
            mutation("create"),
        )
        .await
        .unwrap();
    let snapshot = brr_agent_core::TaskInput {
        agent_id: "agent-a".into(),
        title: ticket.title.clone(),
        instructions: ticket.description.clone(),
        acceptance_criteria: vec![],
        context: None,
        cwd: Some("/project".into()),
        project_id: Some(ticket.project_id.clone()),
        model: Some("fixture".into()),
        effort: Some("medium".into()),
        workspace_permission: None,
        dependencies: vec![],
    };
    let proposal = store
        .propose_ticket_run(&ticket.id, ticket.revision, snapshot, mutation("propose"))
        .await
        .unwrap();
    let task = store
        .approve_ticket_run(&ticket.id, &proposal.hash, mutation("approve"))
        .await
        .unwrap();
    let explicit = store
        .update_ticket(
            &ticket.id,
            ticket.revision,
            brr_agent_core::TicketUpdate {
                status: Some(TicketStatus::Todo),
                ..Default::default()
            },
            mutation("explicit-same"),
        )
        .await
        .unwrap();
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_continuation(&task.id, &run.id, "thread", "turn-1", &run.lease_token)
        .await
        .unwrap();
    assert_eq!(
        store.get_ticket(&ticket.id).await.unwrap().status,
        TicketStatus::Todo
    );
    store
        .propose_completion(
            &task.id,
            &run.id,
            &run.lease_token,
            "Done",
            vec!["verified".into()],
        )
        .await
        .unwrap();
    store
        .settle_run(&task.id, &run.id, "turn-1", &run.lease_token, true)
        .await
        .unwrap();
    let current = store.get_ticket(&ticket.id).await.unwrap();
    assert_eq!(current.status, TicketStatus::Todo);
    assert_eq!(current.status_revision, explicit.status_revision);
}

#[tokio::test]
async fn concurrent_approvals_admit_one_unfinished_run_and_block_old_task_retry() {
    let (_dir, store) = store().await;
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-a',1,'{\"name\":\"Agent A\",\"description\":\"fixture\",\"instructions\":\"test\",\"soul\":\"test\",\"model\":\"fixture\",\"effort\":\"medium\"}',0,0)")
        .execute(store.pool()).await.unwrap();
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "One run at a time".into(),
                description: "Run this task".into(),
                acceptance_criteria: vec![],
                agent_id: Some("agent-a".into()),
                priority: TicketPriority::None,
                status: TicketStatus::Todo,
            },
            mutation("create"),
        )
        .await
        .unwrap();
    let mut first_snapshot = brr_agent_core::TaskInput {
        agent_id: "agent-a".into(),
        title: ticket.title.clone(),
        instructions: ticket.description.clone(),
        acceptance_criteria: vec![],
        context: None,
        cwd: Some("/project".into()),
        project_id: Some(ticket.project_id.clone()),
        model: Some("fixture".into()),
        effort: Some("medium".into()),
        workspace_permission: Some(brr_agent_core::WorkspacePermission::ReadOnly),
        dependencies: vec![],
    };
    let first_proposal = store
        .propose_ticket_run(
            &ticket.id,
            ticket.revision,
            first_snapshot.clone(),
            mutation("propose-a"),
        )
        .await
        .unwrap();
    first_snapshot.workspace_permission = Some(brr_agent_core::WorkspacePermission::WorkspaceWrite);
    let second_proposal = store
        .propose_ticket_run(
            &ticket.id,
            ticket.revision,
            first_snapshot.clone(),
            mutation("propose-b"),
        )
        .await
        .unwrap();
    assert_ne!(first_proposal.hash, second_proposal.hash);

    let store_a = store.clone();
    let store_b = store.clone();
    let ticket_a = ticket.id.clone();
    let ticket_b = ticket.id.clone();
    let hash_a = first_proposal.hash.clone();
    let hash_b = second_proposal.hash.clone();
    let (approved_a, approved_b) = tokio::join!(
        store_a.approve_ticket_run(&ticket_a, &hash_a, mutation("approve-a")),
        store_b.approve_ticket_run(&ticket_b, &hash_b, mutation("approve-b")),
    );
    assert_ne!(
        approved_a.is_ok(),
        approved_b.is_ok(),
        "only one approval may claim the ticket slot"
    );
    let admitted = approved_a.or(approved_b).unwrap();
    let admitted_proposal = if admitted.workspace_permission
        == Some(brr_agent_core::WorkspacePermission::WorkspaceWrite)
    {
        second_proposal
    } else {
        first_proposal
    };
    let run = store.claim_ready("worker").await.unwrap().unwrap();
    store
        .attach_run(&admitted.id, &run.id, "thread", "turn", &run.lease_token)
        .await
        .unwrap();
    assert_eq!(
        store.get_ticket(&ticket.id).await.unwrap().status,
        TicketStatus::InProgress
    );
    store
        .settle_run(&admitted.id, &run.id, "turn", &run.lease_token, false)
        .await
        .unwrap();

    let mut retry_snapshot = admitted_proposal.snapshot;
    retry_snapshot.context = Some("Retry after newer proposal".into());
    let refreshed_ticket = store.get_ticket(&ticket.id).await.unwrap();
    let newer_proposal = store
        .propose_ticket_run(
            &ticket.id,
            refreshed_ticket.revision,
            retry_snapshot,
            mutation("propose-newer"),
        )
        .await
        .unwrap();
    let newer_task = store
        .approve_ticket_run(&ticket.id, &newer_proposal.hash, mutation("approve-newer"))
        .await
        .unwrap();
    assert_ne!(newer_task.id, admitted.id);
    assert!(
        store
            .control_task(&admitted.id, brr_agent_core::TaskAction::Retry)
            .await
            .is_err(),
        "retrying an old linked task must not create a second unfinished execution"
    );
    let detail = store.read_ticket(&ticket.id, None, None).await.unwrap();
    assert_eq!(detail.runs.len(), 2);
    assert_eq!(
        detail
            .runs
            .iter()
            .filter(|run| !matches!(run.status.as_str(), "succeeded" | "failed" | "cancelled"))
            .count(),
        1
    );
}

#[tokio::test]
async fn old_tasks_table_migrates_permission_and_new_tasks_round_trip_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("agents.sqlite3");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true);
    let legacy = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE tasks (id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,title TEXT NOT NULL,instructions TEXT NOT NULL,acceptance_criteria TEXT NOT NULL,context TEXT,cwd TEXT,project_id TEXT,model TEXT,effort TEXT,dependencies TEXT NOT NULL,status TEXT NOT NULL,reason TEXT,run_id TEXT,thread_id TEXT,result TEXT,evidence TEXT,parent_task_id TEXT,delegation_id TEXT,delegation_index INTEGER,workflow_id TEXT,occurrence_id TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL)")
        .execute(&legacy).await.unwrap();
    legacy.close().await;

    let store = AgentStore::open(dir.path()).await.unwrap();
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-a',1,'{}',0,0)")
        .execute(store.pool()).await.unwrap();
    let task = store
        .create_task(brr_agent_core::TaskInput {
            agent_id: "agent-a".into(),
            title: "Permission migration".into(),
            instructions: "Keep the selected permission".into(),
            acceptance_criteria: vec![],
            context: None,
            cwd: Some("/project".into()),
            project_id: Some("project-1".into()),
            model: None,
            effort: None,
            workspace_permission: Some(brr_agent_core::WorkspacePermission::ReadOnly),
            dependencies: vec![],
        })
        .await
        .unwrap();
    assert_eq!(
        task.workspace_permission,
        Some(brr_agent_core::WorkspacePermission::ReadOnly)
    );
    assert_eq!(
        store.get_task(&task.id).await.unwrap().workspace_permission,
        task.workspace_permission
    );
    let old_tasks_default = store
        .create_task(brr_agent_core::TaskInput {
            agent_id: "agent-a".into(),
            title: "Old task behavior".into(),
            instructions: "Keep None for old behavior".into(),
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
    assert_eq!(old_tasks_default.workspace_permission, None);
}

#[tokio::test]
async fn ticket_list_filters_search_text_and_uses_project_scoped_cursors() {
    let (_dir, store) = store().await;
    let create = |title: &str, description: &str| TicketInput {
        project_id: "project-1".into(),
        title: title.into(),
        description: description.into(),
        acceptance_criteria: vec![],
        agent_id: None,
        priority: TicketPriority::None,
        status: TicketStatus::Todo,
    };
    let first = store
        .create_ticket(create("Amber", "Use 50% less"), mutation("a"))
        .await
        .unwrap();
    store
        .create_ticket(create("Blue", "Misc"), mutation("b"))
        .await
        .unwrap();
    let third = store
        .create_ticket(create("Green", "Also amber"), mutation("c"))
        .await
        .unwrap();
    let page = store
        .list_tickets(
            "project-1",
            None,
            1,
            Some(brr_agent_core::TicketFilter {
                query: Some("AMBER".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].id, first.id);
    assert_eq!(page.next_cursor.as_deref(), Some(first.id.as_str()));
    assert_eq!(
        store
            .list_tickets(
                "project-1",
                page.next_cursor.as_deref(),
                10,
                Some(brr_agent_core::TicketFilter {
                    query: Some("amber".into()),
                    ..Default::default()
                })
            )
            .await
            .unwrap()
            .data[0]
            .id,
        third.id
    );
    assert!(
        store
            .list_tickets("another-project", Some(&first.id), 10, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn approval_rejects_a_stale_agent_model_snapshot() {
    let (_dir, store) = store().await;
    let original_profile = r#"{"name":"Agent A","description":"fixture","instructions":"test","soul":"test","model":"fixture","effort":"medium"}"#;
    sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES('agent-a',1,?,0,0)")
        .bind(original_profile).execute(store.pool()).await.unwrap();
    let ticket = store
        .create_ticket(
            TicketInput {
                project_id: "project-1".into(),
                title: "Current profile settings".into(),
                description: "Run with current defaults".into(),
                acceptance_criteria: vec![],
                agent_id: Some("agent-a".into()),
                priority: TicketPriority::None,
                status: TicketStatus::Todo,
            },
            mutation("create"),
        )
        .await
        .unwrap();
    let snapshot = brr_agent_core::TaskInput {
        agent_id: "agent-a".into(),
        title: ticket.title.clone(),
        instructions: ticket.description.clone(),
        acceptance_criteria: vec![],
        context: None,
        cwd: Some("/project".into()),
        project_id: Some(ticket.project_id.clone()),
        model: Some("fixture".into()),
        effort: Some("medium".into()),
        workspace_permission: Some(brr_agent_core::WorkspacePermission::ReadOnly),
        dependencies: vec![],
    };
    let proposal = store
        .propose_ticket_run(
            &ticket.id,
            ticket.revision,
            snapshot.clone(),
            mutation("propose-stale"),
        )
        .await
        .unwrap();
    let changed_profile = r#"{"name":"Agent A","description":"fixture","instructions":"test","soul":"test","model":"new-model","effort":"medium"}"#;
    sqlx::query("UPDATE agent_profiles SET revision=revision+1,candidate=? WHERE id='agent-a'")
        .bind(changed_profile)
        .execute(store.pool())
        .await
        .unwrap();
    let error = store
        .approve_ticket_run(&ticket.id, &proposal.hash, mutation("approve-stale"))
        .await
        .unwrap_err();
    assert!(error.contains("model or effort changed"));
    assert_eq!(store.list_tasks(None).await.unwrap().len(), 0);
    assert_eq!(
        store.list_ticket_run_proposals(&ticket.id).await.unwrap()[0].status,
        "pending"
    );

    let mut refreshed_snapshot = snapshot;
    refreshed_snapshot.model = Some("new-model".into());
    let refreshed = store
        .propose_ticket_run(
            &ticket.id,
            ticket.revision,
            refreshed_snapshot,
            mutation("propose-fresh"),
        )
        .await
        .unwrap();
    let task = store
        .approve_ticket_run(&ticket.id, &refreshed.hash, mutation("approve-fresh"))
        .await
        .unwrap();
    assert_eq!(task.model.as_deref(), Some("new-model"));
}
