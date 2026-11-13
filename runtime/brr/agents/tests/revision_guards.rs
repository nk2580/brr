use brr_agent_core::{AgentStore, MemoryScope, ProfileCandidate};

#[tokio::test]
async fn reject_profile_is_hash_checked_and_rolls_back_when_shared_authority_rejects() {
    let directory = tempfile::tempdir().unwrap();
    let store = AgentStore::open(directory.path()).await.unwrap();
    let draft = store
        .propose_profile(
            "interview-thread",
            None,
            ProfileCandidate {
                name: "Planner".into(),
                description: "Plan carefully".into(),
                instructions: "Check assumptions".into(),
                soul: "Be precise".into(),
                model: "model-x".into(),
                effort: "medium".into(),
            },
        )
        .await
        .unwrap();

    store.set_shared_commit_hook(|_| Err("shared review rejected".into()));
    assert!(
        store
            .reject_profile(&draft.id, &draft.hash)
            .await
            .unwrap_err()
            .contains("shared review rejected")
    );
    store.clear_shared_commit_hook();
    let unchanged = store.list_profile_drafts(None).await.unwrap();
    assert_eq!(unchanged[0].status, "proposed");

    assert!(store.reject_profile(&draft.id, "stale-hash").await.is_err());
    let rejected = store.reject_profile(&draft.id, &draft.hash).await.unwrap();
    assert_eq!(rejected.status, "rejected");
}

#[tokio::test]
async fn numeric_forget_and_revoke_revisions_are_checked_before_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let store = AgentStore::open(directory.path()).await.unwrap();
    let access = store
        .memory_access_for_user(Some("agent-one"), None)
        .await
        .unwrap();
    store
        .record_source(&access, "source-one", 1, "verified evidence")
        .await
        .unwrap();

    assert!(
        store
            .forget_memory_with_revision(&access, "source-one", Some(2))
            .await
            .unwrap_err()
            .contains("revision changed")
    );
    assert!(
        store
            .list_sources(&access, 10)
            .await
            .unwrap()
            .iter()
            .any(|source| source.source_id == "source-one")
    );

    let skill = store
        .publish_skill(
            &access,
            MemoryScope::Agent,
            "Build checks",
            "Verify build output",
            "Inspect the build result.",
            &["source-one".into()],
        )
        .await
        .unwrap();
    assert!(
        store
            .revoke_skill_with_revision(&access, &skill.id, Some(2))
            .await
            .unwrap_err()
            .contains("revision changed")
    );
    assert!(
        store
            .list_skills(&access)
            .await
            .unwrap()
            .iter()
            .any(|item| item.id == skill.id)
    );
    store
        .revoke_skill_with_revision(&access, &skill.id, Some(skill.revision))
        .await
        .unwrap();
    assert!(
        !store
            .list_skills(&access)
            .await
            .unwrap()
            .iter()
            .any(|item| item.id == skill.id)
    );
}
