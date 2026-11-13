use brr_agent_core::{AgentStore, MemoryScope, SessionBinding, SharedSnapshot};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn identical_git_materialization_preserves_local_undo_event_handles() {
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
    store
        .bind_session(SessionBinding {
            thread_id: "thread-local".into(),
            agent_id: None,
            kind: "user".into(),
            project_id: None,
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let access = store.memory_access("thread-local").await.unwrap();
    store
        .record_source(&access, "source-local", 1, "Local evidence")
        .await
        .unwrap();
    let source_ids = ["source-local@1".to_string()];
    let memory = store
        .publish_memory(
            &access,
            MemoryScope::User,
            "fact",
            "Local memory",
            &source_ids,
        )
        .await
        .unwrap();
    store
        .publish_skill(
            &access,
            MemoryScope::User,
            "procedure",
            "desc",
            "Local skill",
            &source_ids,
        )
        .await
        .unwrap();

    let captured = Arc::new(Mutex::new(Vec::<SharedSnapshot>::new()));
    let output = captured.clone();
    store.set_shared_commit_hook(move |snapshot| {
        output.lock().unwrap().push(snapshot.clone());
        Ok("git-commit".into())
    });
    let forget_event = store.forget_memory(&access, "source-local").await.unwrap();
    let committed = captured.lock().unwrap().last().unwrap().clone();

    // An unrelated fleet sync can materialize the exact Git projection the
    // local forget just committed; local event handles must remain undoable.
    store
        .materialize_shared_snapshot("local-node", &committed, "git-sync-same-head")
        .await
        .unwrap();
    store
        .undo_memory_event(&access, forget_event)
        .await
        .unwrap();
    assert_eq!(store.list_memory(&access).await.unwrap().len(), 1);
    assert_eq!(store.list_skills(&access).await.unwrap().len(), 1);
    assert_eq!(store.list_sources(&access, 10).await.unwrap().len(), 1);
    assert_eq!(memory.content, "Local memory");
}

#[tokio::test]
async fn foreign_event_counters_never_become_local_undo_handles() {
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
    let foreign_dir = tempfile::tempdir().unwrap();
    let foreign = AgentStore::open(foreign_dir.path()).await.unwrap();
    foreign
        .bind_session(SessionBinding {
            thread_id: "thread-foreign".into(),
            agent_id: None,
            kind: "user".into(),
            project_id: None,
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        })
        .await
        .unwrap();
    let access = foreign.memory_access("thread-foreign").await.unwrap();
    foreign
        .record_source(&access, "foreign-source", 1, "Foreign evidence")
        .await
        .unwrap();
    foreign
        .publish_memory(
            &access,
            MemoryScope::User,
            "fact",
            "Foreign memory",
            &["foreign-source@1".into()],
        )
        .await
        .unwrap();
    let foreign_forget = foreign
        .forget_memory(&access, "foreign-source")
        .await
        .unwrap();
    let mut snapshot = foreign.shared_snapshot().await.unwrap();
    for row in &mut snapshot.rows {
        row.origin_node_id = Some("foreign-node".into());
    }

    store
        .materialize_shared_snapshot("local-node", &snapshot, "git-foreign")
        .await
        .unwrap();
    let imported_id = brr_agent_core::shared_local_id(
        "foreign-node",
        brr_agent_core::SharedTable::MemorySources,
        "foreign-source",
    );
    let imported_event: Option<i64> =
        sqlx::query_scalar("SELECT event_id FROM memory_sources WHERE source_id=?")
            .bind(imported_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(imported_event, None);
    let _ = foreign_forget;
}
