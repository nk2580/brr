use brr_agent_core::{AgentStore, MemoryScope, ProfileCandidate, SessionBinding};

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
