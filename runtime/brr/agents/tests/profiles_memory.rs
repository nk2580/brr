use brr_agent_core::{
    AgentStore, MemoryScope, ProfileCandidate, SessionBinding, SharedRow, SharedSnapshot,
    SharedTable, TaskInput,
};

#[tokio::test]
async fn shared_skills_retain_their_evidence_and_become_available_to_normal_recall() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = AgentStore::open(source_dir.path()).await.unwrap();
    let access = source.memory_access_for_user(None, None).await.unwrap();
    source
        .record_source(
            &access,
            "procedure-evidence",
            1,
            "Verify the result before publishing.",
        )
        .await
        .unwrap();
    let skill = source
        .publish_skill(
            &access,
            MemoryScope::User,
            "Verify",
            "Check results",
            "Run the acceptance check",
            &["procedure-evidence@1".into()],
        )
        .await
        .unwrap();
    let mut snapshot = source.shared_snapshot().await.unwrap();
    for row in &mut snapshot.rows {
        row.origin_node_id = Some("original-node".into());
    }
    let destination_dir = tempfile::tempdir().unwrap();
    let destination = AgentStore::open(destination_dir.path()).await.unwrap();
    destination
        .materialize_shared_snapshot("execution-node", &snapshot, "git-skills")
        .await
        .unwrap();
    let access = destination
        .memory_access_for_user(None, None)
        .await
        .unwrap();
    let skills = destination.list_skills(&access).await.unwrap();
    assert_eq!(
        skills.len(),
        1,
        "Shared procedures must be available through the execution store"
    );
    assert_eq!(
        skills[0].id,
        brr_agent_core::shared_local_id("original-node", SharedTable::LearnedSkills, &skill.id)
    );
    assert_eq!(
        skills[0].source_ids,
        vec![brr_agent_core::shared_local_id(
            "original-node",
            SharedTable::MemorySources,
            "procedure-evidence"
        )]
    );
}

#[tokio::test]
async fn editing_an_imported_profile_publishes_the_original_identity_and_local_review() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = AgentStore::open(source_dir.path()).await.unwrap();
    let draft = source
        .propose_profile("original-interview", None, candidate())
        .await
        .unwrap();
    source
        .approve_profile(&draft.id, &draft.hash, "original-review")
        .await
        .unwrap();
    let profile = source
        .apply_profile(&draft.id, &draft.hash, "original-apply")
        .await
        .unwrap();
    let mut original = source.shared_snapshot().await.unwrap();
    for row in &mut original.rows {
        row.origin_node_id = Some("original-node".into());
    }
    let destination_dir = tempfile::tempdir().unwrap();
    let destination = AgentStore::open(destination_dir.path()).await.unwrap();
    destination
        .materialize_shared_snapshot("editing-node", &original, "git-original")
        .await
        .unwrap();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<SharedSnapshot>::new()));
    let output = captured.clone();
    destination.set_shared_commit_hook(move |snapshot| {
        output.lock().unwrap().push(snapshot.clone());
        Ok("git-edited".into())
    });
    let imported_id =
        brr_agent_core::shared_local_id("original-node", SharedTable::AgentProfiles, &profile.id);
    let mut edited = candidate();
    edited.instructions = "Cite primary evidence and verify assumptions".into();
    let draft = destination
        .propose_profile("local-review", Some(imported_id), edited)
        .await
        .unwrap();
    destination
        .approve_profile(&draft.id, &draft.hash, "local-review")
        .await
        .unwrap();
    destination
        .apply_profile(&draft.id, &draft.hash, "local-apply")
        .await
        .unwrap();
    let snapshots = captured.lock().unwrap();
    let published = snapshots.last().unwrap();
    let head = published
        .rows
        .iter()
        .find(|row| row.table == SharedTable::AgentProfiles)
        .unwrap();
    assert_eq!(head.origin_node_id.as_deref(), Some("original-node"));
    assert_eq!(head.key, profile.id);
    let review = published
        .rows
        .iter()
        .find(|row| row.table == SharedTable::ProfileDrafts && row.key == draft.id)
        .unwrap();
    assert_eq!(review.origin_node_id.as_deref(), Some("editing-node"));
    assert_eq!(
        head.data["candidate"]["instructions"],
        "Cite primary evidence and verify assumptions"
    );
    assert!(
        published
            .rows
            .iter()
            .any(|row| row.table == SharedTable::ProfileRevisions && row.data["revision"] == 1),
        "Original review provenance must survive foreign-node edits"
    );
    published.validate(true).unwrap();
}

fn candidate() -> ProfileCandidate {
    ProfileCandidate {
        name: "Researcher".into(),
        description: "Finds evidence carefully".into(),
        instructions: "Cite evidence".into(),
        soul: "Curious and precise".into(),
        model: "gpt-5".into(),
        effort: "medium".into(),
    }
}

#[tokio::test]
async fn recovery_of_git_commit_before_sql_restores_local_review_without_replaying_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    store
        .materialize_shared_snapshot(
            "local-node",
            &SharedSnapshot {
                schema_version: 1,
                rows: vec![],
            },
            "git-empty",
        )
        .await
        .unwrap();
    let draft = store
        .propose_profile("interview", None, candidate())
        .await
        .unwrap();
    store
        .approve_profile(&draft.id, &draft.hash, "review")
        .await
        .unwrap();
    let committed = std::sync::Arc::new(std::sync::Mutex::new(None));
    let output = committed.clone();
    store.set_shared_commit_hook(move |snapshot| {
        *output.lock().unwrap() = Some(snapshot.clone());
        Err("simulated cancellation after Git committed".into())
    });
    assert!(
        store
            .apply_profile(&draft.id, &draft.hash, "publish")
            .await
            .is_err()
    );
    assert!(store.list_profiles().await.unwrap().is_empty());
    let git_head = committed.lock().unwrap().clone().unwrap();
    let events_before = store.events(0, 1000).await.unwrap().len();
    store.clear_shared_commit_hook();
    store
        .materialize_shared_snapshot("local-node", &git_head, "git-committed")
        .await
        .unwrap();
    let recovered = store
        .apply_profile(&draft.id, &draft.hash, "publish")
        .await
        .unwrap();
    assert_eq!(recovered.revision, 1);
    assert_eq!(
        store.events(0, 1000).await.unwrap().len(),
        events_before,
        "Recovery must not replay historical actions"
    );
    assert_eq!(
        store.shared_snapshot().await.unwrap(),
        git_head,
        "Recovery restores the entire reviewed shared transaction"
    );
    store.set_shared_commit_hook(|snapshot| {
        snapshot.validate(true)?;
        Ok("git-next".into())
    });
    let next = store
        .propose_profile("next-interview", Some(recovered.id), candidate())
        .await
        .unwrap();
    store
        .approve_profile(&next.id, &next.hash, "review-next")
        .await
        .unwrap();
    assert_eq!(
        store
            .apply_profile(&next.id, &next.hash, "publish-next")
            .await
            .unwrap()
            .revision,
        2
    );
}

#[tokio::test]
async fn removing_a_shared_memory_head_reconciles_sql_and_does_not_reexport_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let access = store.memory_access_for_user(None, None).await.unwrap();
    store
        .record_source(&access, "source", 1, "Keep a short report")
        .await
        .unwrap();
    let memory = store
        .publish_memory(
            &access,
            MemoryScope::User,
            "preference",
            "concise report",
            &["source@1".into()],
        )
        .await
        .unwrap();
    let mut snapshot = store.shared_snapshot().await.unwrap();
    for row in &mut snapshot.rows {
        row.origin_node_id = Some("local-node".into());
    }
    store
        .materialize_shared_snapshot("local-node", &snapshot, "git-present")
        .await
        .unwrap();
    snapshot.rows.retain(|row| {
        !(row.table == SharedTable::MemoryRecords && row.key == memory.id)
            && !(row.table == SharedTable::MemoryRecordSources && row.data["memoryId"] == memory.id)
    });
    let events = store.events(0, 1000).await.unwrap().len();
    store
        .materialize_shared_snapshot("local-node", &snapshot, "git-deleted")
        .await
        .unwrap();
    assert!(
        store
            .search_memory(&access, "concise")
            .await
            .unwrap()
            .is_empty(),
        "Git tombstones must remove the old SQL recall projection"
    );
    assert!(
        !store
            .shared_snapshot()
            .await
            .unwrap()
            .rows
            .iter()
            .any(|row| row.table == SharedTable::MemoryRecords && row.key == memory.id)
    );
    assert_eq!(store.events(0, 1000).await.unwrap().len(), events);
    store.set_shared_commit_hook(|snapshot| {
        snapshot.validate(true)?;
        Ok("git-new".into())
    });
    store
        .publish_memory(
            &access,
            MemoryScope::User,
            "preference",
            "another report",
            &["source@1".into()],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn recovery_reads_git_only_after_acquiring_the_shared_sql_writer_transaction() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let tx = store.pool().begin().await.unwrap();
    let read = Arc::new(AtomicBool::new(false));
    let observed = read.clone();
    let recovery_store = store.clone();
    let task = tokio::spawn(async move {
        recovery_store
            .materialize_shared_from("node", move || {
                observed.store(true, Ordering::SeqCst);
                Ok((
                    "git-current".into(),
                    SharedSnapshot {
                        schema_version: 1,
                        rows: vec![],
                    },
                ))
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let read_while_writer_busy = read.load(Ordering::SeqCst);
    tx.commit().await.unwrap();
    task.await.unwrap().unwrap();
    assert!(
        !read_while_writer_busy,
        "Recovery must not capture old Git HEAD before a concurrent SQL writer commits its newer Git state"
    );
    assert!(read.load(Ordering::SeqCst));
}

#[tokio::test]
async fn standalone_project_memory_can_be_collected_before_host_project_metadata_is_joined() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let access = store
        .memory_access_for_user(None, Some("host-project"))
        .await
        .unwrap();
    store
        .record_source(&access, "project-source", 1, "Project fact")
        .await
        .unwrap();
    store
        .publish_memory(
            &access,
            MemoryScope::Project,
            "fact",
            "Project fact",
            &["project-source@1".into()],
        )
        .await
        .unwrap();
    let snapshot = store.shared_snapshot().await;
    assert!(
        snapshot.is_ok(),
        "The pre-activation AgentStore export must not require access to the separate Host ProjectStore: {snapshot:?}"
    );
    let mut qualified = snapshot.unwrap();
    for row in &mut qualified.rows {
        row.origin_node_id = Some("local-node".into());
    }
    assert!(
        qualified.validate(true).is_err(),
        "Git publication still requires the referenced project metadata"
    );
}

fn task_input(title: &str, agent_id: &str) -> TaskInput {
    TaskInput {
        agent_id: agent_id.into(),
        title: title.into(),
        instructions: format!("Complete {title}"),
        acceptance_criteria: vec!["result is supported by evidence".into()],
        context: None,
        cwd: None,
        project_id: None,
        model: None,
        effort: None,
        workspace_permission: None,
        dependencies: Vec::new(),
    }
}

#[tokio::test]
async fn a_reviewed_shared_profile_is_available_for_execution_on_another_node() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = AgentStore::open(source_dir.path()).await.unwrap();
    let draft = source
        .propose_profile("original-interview", None, candidate())
        .await
        .unwrap();
    source
        .approve_profile(&draft.id, &draft.hash, "review-import")
        .await
        .unwrap();
    let original = source
        .apply_profile(&draft.id, &draft.hash, "apply-import")
        .await
        .unwrap();
    let mut snapshot = source.shared_snapshot().await.unwrap();
    for row in &mut snapshot.rows {
        row.origin_node_id = Some("original-node".into());
    }
    let destination_dir = tempfile::tempdir().unwrap();
    let destination = AgentStore::open(destination_dir.path()).await.unwrap();
    destination
        .materialize_shared_snapshot("execution-node", &snapshot, "git-commit-1")
        .await
        .unwrap();
    let profiles = destination.list_profiles().await.unwrap();
    assert_eq!(
        profiles.len(),
        1,
        "Shared profiles must be usable by ordinary execution APIs"
    );
    let local_id =
        brr_agent_core::shared_local_id("original-node", SharedTable::AgentProfiles, &original.id);
    assert_eq!(profiles[0].id, local_id);
    assert_eq!(profiles[0].name, original.name);
    assert_eq!(
        destination
            .shared_origin(SharedTable::AgentProfiles, &local_id)
            .await
            .unwrap(),
        Some(("original-node".into(), original.id))
    );
    assert!(
        destination
            .session_binding("original-interview")
            .await
            .is_err(),
        "Imported history must not create a live local execution binding"
    );
}

#[tokio::test]
async fn shared_user_memory_keeps_its_source_links_without_replaying_source_sessions() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = AgentStore::open(source_dir.path()).await.unwrap();
    let access = source.memory_access_for_user(None, None).await.unwrap();
    let evidence = source
        .record_source(
            &access,
            "original-evidence",
            1,
            "The owner prefers concise reports.",
        )
        .await
        .unwrap();
    let remembered = source
        .publish_memory(
            &access,
            MemoryScope::User,
            "preference",
            "Use concise reports",
            &["original-evidence@1".into()],
        )
        .await
        .unwrap();
    let mut snapshot = source.shared_snapshot().await.unwrap();
    for row in &mut snapshot.rows {
        row.origin_node_id = Some("original-node".into());
    }
    let destination_dir = tempfile::tempdir().unwrap();
    let destination = AgentStore::open(destination_dir.path()).await.unwrap();
    destination
        .materialize_shared_snapshot("execution-node", &snapshot, "git-memory")
        .await
        .unwrap();
    let access = destination
        .memory_access_for_user(None, None)
        .await
        .unwrap();
    let memories = destination.search_memory(&access, "concise").await.unwrap();
    assert_eq!(
        memories.len(),
        1,
        "Validated shared memory must be available to normal scoped recall"
    );
    assert_eq!(
        memories[0].id,
        brr_agent_core::shared_local_id(
            "original-node",
            SharedTable::MemoryRecords,
            &remembered.id
        )
    );
    assert_eq!(
        memories[0].source_ids,
        vec![brr_agent_core::shared_local_id(
            "original-node",
            SharedTable::MemorySources,
            &evidence.source_id
        )]
    );
    assert_eq!(
        destination.list_sources(&access, 10).await.unwrap().len(),
        1
    );
    assert!(
        destination
            .session_binding(&evidence.thread_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn reviewed_profile_is_applied_once_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let draft = store
        .propose_profile("thread-1", None, candidate())
        .await
        .unwrap();

    assert_eq!(draft.status, "proposed");
    assert!(store.list_profiles().await.unwrap().is_empty());
    let approved = store
        .approve_profile(&draft.id, &draft.hash, "review-1")
        .await
        .unwrap();
    assert_eq!(approved.status, "approved");
    let profile = store
        .apply_profile(&draft.id, &draft.hash, "apply-1")
        .await
        .unwrap();
    assert_eq!(profile.name, "Researcher");
    let repeated = store
        .apply_profile(&draft.id, &draft.hash, "apply-1")
        .await
        .unwrap();
    assert_eq!(repeated, profile);
    let other = store
        .propose_profile("thread-2", None, candidate())
        .await
        .unwrap();
    store
        .approve_profile(&other.id, &other.hash, "review-2")
        .await
        .unwrap();
    assert!(
        store
            .apply_profile(&other.id, &other.hash, "apply-1")
            .await
            .is_err()
    );

    let reopened = AgentStore::open(dir.path()).await.unwrap();
    assert_eq!(reopened.read_profile(&profile.id).await.unwrap(), profile);
    assert!(
        std::path::Path::new(dir.path())
            .join("profiles")
            .join(&profile.id)
            .join("1")
            .join("instructions.md")
            .exists()
    );
}

#[tokio::test]
async fn changed_review_hash_cannot_be_approved_or_applied() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let draft = store
        .propose_profile("thread-1", None, candidate())
        .await
        .unwrap();
    assert!(
        store
            .approve_profile(&draft.id, "wrong", "review-1")
            .await
            .is_err()
    );
    assert!(
        store
            .apply_profile(&draft.id, &draft.hash, "apply-1")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rejected_shared_authority_hook_rolls_back_profile_publication() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let draft = store
        .propose_profile("thread-1", None, candidate())
        .await
        .unwrap();
    store
        .approve_profile(&draft.id, &draft.hash, "review-1")
        .await
        .unwrap();
    let observed_candidate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = observed_candidate.clone();
    let draft_id = draft.id.clone();
    let hash = draft.hash.clone();
    store.set_shared_commit_hook(move |snapshot| {
        let revision = snapshot.rows.iter().any(|row| {
            row.table == SharedTable::ProfileRevisions
                && row.data["draftId"].as_str() == Some(draft_id.as_str())
                && row.data["hash"].as_str() == Some(hash.as_str())
        });
        observed.store(revision, std::sync::atomic::Ordering::SeqCst);
        Err("Git rejected the candidate review hash".into())
    });

    let error = store
        .apply_profile(&draft.id, &draft.hash, "apply-1")
        .await
        .unwrap_err();

    assert!(error.contains("Git rejected"));
    assert!(observed_candidate.load(std::sync::atomic::Ordering::SeqCst));
    assert!(store.list_profiles().await.unwrap().is_empty());
    assert_eq!(
        store.list_profile_drafts(None).await.unwrap()[0].status,
        "approved"
    );
    let applied: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM operation_results WHERE operation_id='apply-1'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(applied, 0);
}

#[tokio::test]
async fn shared_materialization_is_readonly_for_foreign_task_definitions_and_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let binding = SessionBinding {
        thread_id: "local-thread".into(),
        agent_id: None,
        kind: "chat".into(),
        project_id: None,
        cwd: Some("/local/workspace".into()),
        task_run_id: None,
        learning_eligible: false,
    };
    store.bind_session(binding.clone()).await.unwrap();
    let draft = store
        .propose_profile("local-profile-interview", None, candidate())
        .await
        .unwrap();
    store
        .approve_profile(&draft.id, &draft.hash, "local-review")
        .await
        .unwrap();
    let agent = store
        .apply_profile(&draft.id, &draft.hash, "local-publish")
        .await
        .unwrap();
    let task = store
        .create_task(task_input("local execution", &agent.id))
        .await
        .unwrap();
    let mut incoming = store.shared_snapshot().await.unwrap();
    incoming.rows = incoming
        .rows
        .into_iter()
        .filter(|row| row.table == SharedTable::TaskDefinitions)
        .map(|mut row| {
            row.origin_node_id = Some("remote-node".into());
            row
        })
        .collect();
    store
        .materialize_shared_snapshot("local-node", &incoming, "git-revision-1")
        .await
        .unwrap();

    assert_eq!(store.list_tasks(None).await.unwrap(), vec![task]);
    assert_eq!(
        store.session_binding("local-thread").await.unwrap(),
        binding
    );
    let (revision, materialized) = store.materialized_shared_snapshot().await.unwrap().unwrap();
    assert_eq!(revision, "git-revision-1");
    assert_eq!(materialized.rows, incoming.rows);
}

#[tokio::test]
async fn shared_snapshot_rejects_unknown_fields_and_stale_profile_review_hashes() {
    let invalid_unknown = SharedSnapshot {
        schema_version: 1,
        rows: vec![SharedRow {
            origin_node_id: Some("node-a".into()),
            table: SharedTable::TaskDefinitions,
            key: "task-1".into(),
            data: serde_json::json!({
                "id":"task-1", "agentId":"agent-1", "title":"Task",
                "instructions":"Do work", "acceptanceCriteria":"Done",
                "dependencies":[], "createdAt":1, "updatedAt":1, "runId":"foreign-run"
            }),
        }],
    };
    assert!(
        invalid_unknown
            .validate(true)
            .unwrap_err()
            .contains("unknown field")
    );

    let invalid_review = SharedSnapshot {
        schema_version: 1,
        rows: vec![
            SharedRow {
                origin_node_id: Some("node-a".into()),
                table: SharedTable::ProfileDrafts,
                key: "draft-1".into(),
                data: serde_json::json!({
                    "id":"draft-1", "threadId":"thread-1", "agentId":"agent-1",
                    "candidate":{}, "hash":"current-hash", "status":"applied",
                    "baseRevision":1, "createdAt":1
                }),
            },
            SharedRow {
                origin_node_id: Some("node-a".into()),
                table: SharedTable::ProfileRevisions,
                key: "agent-1@2".into(),
                data: serde_json::json!({
                    "agentId":"agent-1", "revision":2, "candidate":{},
                    "createdAt":1, "updatedAt":1, "draftId":"draft-1", "hash":"stale-hash"
                }),
            },
        ],
    };
    assert!(
        invalid_review
            .validate(true)
            .unwrap_err()
            .contains("reviewed draft hash")
    );
}

#[tokio::test]
async fn concurrent_profile_drafts_use_revision_compare_and_swap() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    let first = store
        .propose_profile("thread-1", None, candidate())
        .await
        .unwrap();
    store
        .approve_profile(&first.id, &first.hash, "review-create")
        .await
        .unwrap();
    let created = store
        .apply_profile(&first.id, &first.hash, "apply-create")
        .await
        .unwrap();
    let mut left = candidate();
    left.description = "First edit".into();
    let mut right = candidate();
    right.description = "Second edit".into();
    let a = store
        .propose_profile("thread-1", Some(created.id.clone()), left)
        .await
        .unwrap();
    let b = store
        .propose_profile("thread-1", Some(created.id.clone()), right)
        .await
        .unwrap();
    store
        .approve_profile(&a.id, &a.hash, "review-a")
        .await
        .unwrap();
    store
        .approve_profile(&b.id, &b.hash, "review-b")
        .await
        .unwrap();
    let updated = store
        .apply_profile(&a.id, &a.hash, "apply-a")
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert!(
        store
            .apply_profile(&b.id, &b.hash, "apply-b")
            .await
            .is_err()
    );
    assert_eq!(
        store
            .apply_profile(&a.id, &a.hash, "apply-a-retry")
            .await
            .unwrap(),
        updated
    );
    assert!(
        store
            .apply_profile(&b.id, &b.hash, "apply-a")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn scoped_memory_only_uses_evidence_from_eligible_new_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "agent-a-thread".into(),
            agent_id: Some("agent-a".into()),
            kind: "agent".into(),
            project_id: Some("project-a".into()),
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "agent-b-thread".into(),
            agent_id: Some("agent-b".into()),
            kind: "agent".into(),
            project_id: Some("project-b".into()),
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "legacy-thread".into(),
            agent_id: Some("agent-a".into()),
            kind: "agent".into(),
            project_id: Some("project-a".into()),
            cwd: None,
            task_run_id: None,
            learning_eligible: false,
        })
        .await
        .unwrap();

    let access_a = store.memory_access("agent-a-thread").await.unwrap();
    let access_b = store.memory_access("agent-b-thread").await.unwrap();
    let access_legacy = store.memory_access("legacy-thread").await.unwrap();
    let private = store
        .record_source(&access_a, "source-a", 1, "The project uses SQLite.")
        .await
        .unwrap();
    let record = store
        .publish_memory(
            &access_a,
            MemoryScope::Agent,
            "fact",
            "Uses SQLite",
            &["source-a@1".into()],
        )
        .await
        .unwrap();
    assert_eq!(record.source_ids, vec!["source-a"]);
    let repeated = store
        .publish_memory(
            &access_a,
            MemoryScope::Agent,
            "fact",
            "Uses SQLite",
            &["source-a@1".into()],
        )
        .await
        .unwrap();
    assert_eq!(repeated.id, record.id);
    store
        .publish_memory(
            &access_a,
            MemoryScope::User,
            "fact",
            "User shared SQLite note",
            &["source-a@1".into()],
        )
        .await
        .unwrap();
    store
        .publish_memory(
            &access_a,
            MemoryScope::Project,
            "fact",
            "Project SQLite note",
            &["source-a@1".into()],
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .search_memory(&access_a, "SQLite")
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        store
            .search_memory(&access_b, "shared")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .search_memory(&access_b, "Project")
            .await
            .unwrap()
            .is_empty()
    );
    let skill = store
        .publish_skill(
            &access_a,
            MemoryScope::Agent,
            "sqlite-helper",
            "SQLite procedures",
            "Use SQLite carefully.",
            &["source-a@1".into()],
        )
        .await
        .unwrap();
    let reopened = AgentStore::open(dir.path()).await.unwrap();
    let reopened_access = reopened.memory_access("agent-a-thread").await.unwrap();
    assert_eq!(
        reopened
            .list_skills(&reopened_access)
            .await
            .unwrap()
            .first()
            .unwrap()
            .id,
        skill.id
    );
    assert!(
        store
            .record_source(&access_legacy, "legacy-source", 1, "old history")
            .await
            .is_err()
    );
    assert_eq!(private.source_id, "source-a");
}

#[tokio::test]
async fn source_correction_stales_derived_records_and_skills_and_forget_can_be_undone() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "thread-a".into(),
            agent_id: Some("agent-a".into()),
            kind: "agent".into(),
            project_id: Some("project-a".into()),
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let access = store.memory_access("thread-a").await.unwrap();
    store
        .record_source(&access, "source-a", 1, "Uses sqlite")
        .await
        .unwrap();
    let source_ids = vec!["source-a@1".to_string()];
    let record = store
        .publish_memory(
            &access,
            MemoryScope::Agent,
            "fact",
            "Uses sqlite",
            &source_ids,
        )
        .await
        .unwrap();
    let skill = store
        .publish_skill(
            &access,
            MemoryScope::Agent,
            "sqlite-helper",
            "Store local data",
            "Use SQLite carefully.",
            &source_ids,
        )
        .await
        .unwrap();
    let package = dir
        .path()
        .join("skills/6167656e74/6167656e742d61")
        .join(&skill.id)
        .join("1/SKILL.md");
    assert!(package.exists());

    let forget_event = store.forget_memory(&access, "source-a").await.unwrap();
    assert!(!package.exists());
    assert!(store.list_memory(&access).await.unwrap().is_empty());
    assert!(store.list_skills(&access).await.unwrap().is_empty());
    assert!(
        store
            .record_source(&access, "source-a", 2, "Uses sqlite updated")
            .await
            .is_err()
    );
    store
        .undo_memory_event(&access, forget_event)
        .await
        .unwrap();
    assert!(package.exists());
    assert_eq!(store.list_memory(&access).await.unwrap().len(), 1);
    assert_eq!(store.list_skills(&access).await.unwrap().len(), 1);

    store
        .revise_source(&access, "source-a", 1, "Uses SQLite with WAL")
        .await
        .unwrap();
    assert!(store.list_memory(&access).await.unwrap().is_empty());
    assert!(store.list_skills(&access).await.unwrap().is_empty());
    assert!(
        store
            .revise_memory(&access, &record.id, 99, "incorrect stale edit", &source_ids)
            .await
            .is_err()
    );
    let corrected = store
        .revise_memory(
            &access,
            &record.id,
            1,
            "Uses SQLite with WAL",
            &["source-a@2".into()],
        )
        .await
        .unwrap();
    assert_eq!(corrected.revision, 2);
    assert_eq!(store.search_memory(&access, "WAL").await.unwrap().len(), 1);

    let reopened = AgentStore::open(dir.path()).await.unwrap();
    let reopened_access = reopened.memory_access("thread-a").await.unwrap();
    assert_eq!(
        reopened.list_skills(&reopened_access).await.unwrap().len(),
        0
    );
}

#[tokio::test]
async fn learning_requires_an_exact_current_source_revision_and_keeps_raw_sources_private() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    for (thread, agent, project) in [("a", "agent-a", "project-x"), ("b", "agent-b", "project-x")] {
        store
            .bind_session(SessionBinding {
                thread_id: thread.into(),
                agent_id: Some(agent.into()),
                kind: "agent".into(),
                project_id: Some(project.into()),
                cwd: None,
                task_run_id: None,
                learning_eligible: true,
            })
            .await
            .unwrap();
    }
    let a = store.memory_access("a").await.unwrap();
    let b = store.memory_access("b").await.unwrap();
    store
        .record_source(&a, "race-source", 1, "Initial evidence")
        .await
        .unwrap();
    assert!(store.list_sources(&b, 20).await.unwrap().is_empty());
    let published = store
        .publish_memory(
            &a,
            MemoryScope::Agent,
            "fact",
            "Initial evidence",
            &["race-source@1".into()],
        )
        .await
        .unwrap();
    assert_eq!(published.source_ids, vec!["race-source"]);
    store
        .revise_source(&a, "race-source", 1, "Corrected evidence")
        .await
        .unwrap();
    assert!(
        store
            .publish_memory(
                &a,
                MemoryScope::Agent,
                "fact",
                "Stale",
                &["race-source@1".into()]
            )
            .await
            .is_err()
    );
    let corrected = store
        .publish_memory(
            &a,
            MemoryScope::Agent,
            "fact",
            "Corrected evidence",
            &["race-source@2".into()],
        )
        .await
        .unwrap();
    assert_eq!(corrected.source_ids, vec!["race-source"]);
}

#[tokio::test]
async fn an_old_forget_undo_cannot_revive_a_later_forget_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "undo-thread".into(),
            agent_id: Some("agent-a".into()),
            kind: "agent".into(),
            project_id: None,
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let access = store.memory_access("undo-thread").await.unwrap();
    store
        .record_source(&access, "undo-source", 1, "Fact")
        .await
        .unwrap();
    store
        .publish_memory(
            &access,
            MemoryScope::Agent,
            "fact",
            "Fact",
            &["undo-source@1".into()],
        )
        .await
        .unwrap();
    let first = store.forget_memory(&access, "undo-source").await.unwrap();
    store.undo_memory_event(&access, first).await.unwrap();
    store.forget_memory(&access, "undo-source").await.unwrap();
    store.undo_memory_event(&access, first).await.unwrap();
    assert!(store.list_memory(&access).await.unwrap().is_empty());
}

#[tokio::test]
async fn search_limits_only_after_filtering_to_the_callers_memory_scope() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "owner".into(),
            agent_id: Some("agent-a".into()),
            kind: "agent".into(),
            project_id: Some("same-project".into()),
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "reader".into(),
            agent_id: Some("agent-b".into()),
            kind: "agent".into(),
            project_id: Some("same-project".into()),
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let owner = store.memory_access("owner").await.unwrap();
    let reader = store.memory_access("reader").await.unwrap();
    for i in 0..105 {
        let source = format!("private-source-{i}");
        store
            .record_source(&owner, &source, 1, "Private corpus evidence")
            .await
            .unwrap();
        store
            .publish_memory(
                &owner,
                MemoryScope::Agent,
                "fact",
                &format!("target private result {i}"),
                &[format!("{source}@1")],
            )
            .await
            .unwrap();
    }
    let source = "shared-result-source";
    store
        .record_source(&owner, source, 1, "Shared target evidence")
        .await
        .unwrap();
    store
        .publish_memory(
            &owner,
            MemoryScope::User,
            "fact",
            &format!(
                "{} target visible result",
                "long surrounding context ".repeat(12)
            ),
            &[format!("{source}@1")],
        )
        .await
        .unwrap();
    let results = store.search_memory(&reader, "target").await.unwrap();
    assert!(
        results
            .iter()
            .any(|record| record.content.contains("visible result"))
    );
    assert!(
        results
            .iter()
            .all(|record| record.scope == MemoryScope::User)
    );
}

#[tokio::test]
async fn revision_without_replacing_evidence_uses_its_exact_current_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = AgentStore::open(dir.path()).await.unwrap();
    store
        .bind_session(SessionBinding {
            thread_id: "revision-thread".into(),
            agent_id: Some("agent-a".into()),
            kind: "agent".into(),
            project_id: None,
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let access = store.memory_access("revision-thread").await.unwrap();
    store
        .record_source(&access, "revision-source", 1, "Evidence")
        .await
        .unwrap();
    let record = store
        .publish_memory(
            &access,
            MemoryScope::Agent,
            "fact",
            "Original",
            &["revision-source@1".into()],
        )
        .await
        .unwrap();
    let revised = store
        .revise_memory_record(&access, &record.id, 1, "Corrected wording")
        .await
        .unwrap();
    assert_eq!(revised.revision, 2);
    assert_eq!(revised.content, "Corrected wording");
}
