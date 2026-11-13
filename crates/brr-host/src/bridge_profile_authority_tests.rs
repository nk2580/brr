use super::*;

#[tokio::test]
async fn profile_rejection_uses_shared_commit_hook_and_preserves_proposed_draft_on_rejection() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join("runtime.py");
    std::fs::write(&runtime, "import json,sys\nfor line in sys.stdin:\n m=json.loads(line)\n if 'id' in m: print(json.dumps({'id':m['id'],'result':{'account':{'type':'chatgpt'}} if m['method']=='account/read' else {}}),flush=True)\n").unwrap();
    let mut command = Command::new("python3");
    command.arg(runtime).current_dir(directory.path());
    let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
    let draft = engine
        .agents
        .store
        .propose_profile(
            "interview-thread",
            None,
            brr_agent_core::ProfileCandidate {
                name: "First agent".into(),
                description: "A fresh profile".into(),
                instructions: "Plan carefully".into(),
                soul: "Be candid".into(),
                model: "fixture-model".into(),
                effort: "medium".into(),
            },
        )
        .await
        .unwrap();

    engine
        .agents
        .store
        .set_shared_commit_hook(|_| Err("shared authority denied rejection".into()));
    let error = engine
        .agents_request(
            "agents.reject",
            json!({"draftId":draft.id,"hash":draft.hash}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("shared authority denied rejection"));
    engine.agents.store.clear_shared_commit_hook();
    let unchanged = engine.agents.store.list_profile_drafts(None).await.unwrap();
    assert_eq!(unchanged[0].status, "proposed");
    engine.shutdown().await;
}
