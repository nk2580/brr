use super::*;

#[tokio::test]
#[ignore = "prepares a separate synthetic native QA application; requires explicit QA paths and managed login"]
async fn prepare_isolated_native_ticket_fixture() {
    let root = std::path::PathBuf::from(
        std::env::var_os("BRR_TICKETS_QA_DATA").expect("BRR_TICKETS_QA_DATA is required"),
    );
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .contains("ticketsqa"),
        "Use an isolated QA app data directory"
    );
    let auth = std::env::var_os("BRR_LIVE_AUTH_PATH").expect("BRR_LIVE_AUTH_PATH is required");
    let project = std::path::PathBuf::from(
        std::env::var_os("BRR_TICKETS_QA_PROJECT").expect("BRR_TICKETS_QA_PROJECT is required"),
    );
    assert!(project.to_string_lossy().contains("brr-tickets"));
    let home = root.join("engine");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::copy(auth, home.join("auth.json")).unwrap();
    std::fs::write(project.join("marker.txt"), "BRR_TICKETS_VERIFIED\n").unwrap();
    let store = brr_agent_core::AgentStore::open(home.join("agents"))
        .await
        .unwrap();
    let projects = crate::projects::ProjectStore::open(root.join("projects.sqlite"))
        .await
        .unwrap();
    let engine = Engine::connect_with_stores(
        crate::runtime_command(&home).unwrap(),
        Arc::new(|_| {}),
        store.clone(),
        projects.clone(),
    )
    .await
    .unwrap();
    let outcome = async {
        engine.agents.require_account().await?;
        let models = engine.request("model/list", json!({"includeHidden":false})).await?;
        let candidates = models["data"].as_array().ok_or("No model catalog")?;
        let selected = candidates.iter().find(|item| item["model"].as_str().is_some_and(|name| name.contains("luna")))
            .or_else(|| candidates.iter().find(|item| item["isDefault"] == true)).ok_or("No model")?;
        let registered = projects.create(crate::projects::ProjectInput {
            name: "Ticket QA".into(), folders: vec![project.to_string_lossy().into_owned()],
            primary_folder: Some(project.to_string_lossy().into_owned()), model: None, effort: None,
            permission: Some(crate::projects::ProjectPermission::ReadOnly),
        }).await?;
        let candidate = brr_agent_core::ProfileCandidate {
            name: "Ticket QA helper".into(), description: "Synthetic local ticket verifier".into(),
            instructions: "This is an isolated acceptance fixture. Work only in the Ticket QA project. Do not browse, install packages, invoke external services, or access any other folder. Use the ticket tools only as instructed. For a linked background task, read marker.txt using a shell command and verify its exact contents before calling brr_task_complete with concrete evidence. Do not explicitly change a background task's ticket status. Never write project files.".into(),
            soul: "Be concise and precise.".into(),
            model: selected["model"].as_str().ok_or("No model identifier")?.into(),
            effort: selected["defaultReasoningEffort"].as_str().unwrap_or("medium").into(),
        };
        let draft = store.propose_profile("native-qa-user", None, candidate).await?;
        store.approve_profile(&draft.id, &draft.hash, "native-qa-review").await?;
        let agent = store.apply_profile(&draft.id, &draft.hash, "native-qa-apply").await?;
        let evidence = json!({"projectId":registered.id,"folder":project,"agentId":agent.id,"agentName":agent.name,"model":agent.model,"permission":"read-only","appIdentifier":"dev.brr.desktop.ticketsqa.20261102"});
        let destination = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../projects/tickets/evidence/native-fixture.json");
        std::fs::create_dir_all(destination.parent().unwrap()).map_err(|error|error.to_string())?;
        std::fs::write(destination, serde_json::to_vec_pretty(&evidence).unwrap()).map_err(|error|error.to_string())?;
        eprintln!("TICKET_NATIVE_FIXTURE: isolated project and read-only helper prepared; managed account verified without printing credentials.");
        Ok::<_, String>(())
    }.await;
    engine.shutdown().await;
    outcome.unwrap();
}

#[tokio::test]
#[ignore = "explicit live ChatGPT ticket acceptance; BRR_LIVE_AUTH_PATH supplies an existing managed login"]
async fn owned_runtime_creates_ticket_and_runs_only_after_native_review() {
    let auth = std::env::var_os("BRR_LIVE_AUTH_PATH").expect("BRR_LIVE_AUTH_PATH is required");
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::copy(auth, directory.path().join("auth.json")).unwrap();
    let project = directory.path().join("synthetic-project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("marker.txt"), "BRR_TICKETS_VERIFIED\n").unwrap();
    let project = std::fs::canonicalize(project).unwrap();
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let observed = events.clone();
    let engine = Engine::connect(
        crate::runtime_command(directory.path()).unwrap(),
        Arc::new(move |event| observed.lock().unwrap().push(event)),
    )
    .await
    .unwrap();
    let outcome = async {
        engine.agents.require_account().await?;
        let models = engine.request("model/list", json!({"includeHidden":false})).await?;
        let candidates = models["data"].as_array().ok_or("No available model catalog")?;
        let selected = candidates.iter().find(|item| item["model"].as_str().is_some_and(|name| name.contains("luna")))
            .or_else(|| candidates.iter().find(|item| item["isDefault"] == true))
            .ok_or("No available ChatGPT model")?;
        let model = selected["model"].as_str().ok_or("Model is missing its identifier")?;
        let effort = selected["defaultReasoningEffort"].as_str().unwrap_or("medium");
        let registered = engine.agents.projects.create(crate::projects::ProjectInput {
            name: "Synthetic ticket project".into(), folders: vec![project.to_string_lossy().into_owned()],
            primary_folder: Some(project.to_string_lossy().into_owned()), model: None, effort: None,
            permission: Some(crate::projects::ProjectPermission::ReadOnly),
        }).await?;
        let candidate = brr_agent_core::ProfileCandidate {
            name: "Synthetic ticket verifier".into(),
            description: "Isolated ticket acceptance".into(),
            instructions: "Follow the synthetic task exactly. Work only in the selected temporary project. Do not browse, install packages, contact people, or invoke external service tools. For background ticket work, inspect marker.txt with a shell command, verify the exact marker, then call brr_task_complete with concrete evidence. Do not change ticket status yourself. Never write project files.".into(),
            soul: "Be concise and evidence-based.".into(),
            model: model.into(), effort: effort.into(),
        };
        let draft = engine.agents.store.propose_profile("synthetic-user", None, candidate).await?;
        engine.agents.store.approve_profile(&draft.id, &draft.hash, "synthetic-review").await?;
        let agent = engine.agents.store.apply_profile(&draft.id, &draft.hash, "synthetic-apply").await?;
        let session = engine.agents_request("agents.open", json!({"agentId":agent.id,"projectId":registered.id,"readOnly":true})).await?;
        let thread = session["thread"]["id"].as_str().ok_or("Agent session has no ID")?;
        let description = "Read marker.txt in the current project and verify it contains exactly BRR_TICKETS_VERIFIED followed by a newline. Do not write any file. Finish with brr_task_complete only after checking the exact contents, with command output as evidence.";
        let prompt = format!("This is an isolated ticket tool acceptance check. Call brr_ticket_create exactly once, using the current project, title 'Synthetic ticket lifecycle', description {description:?}, acceptanceCriteria [{{\"text\":\"marker.txt contains exactly BRR_TICKETS_VERIFIED followed by a newline.\"}}], and agentId {:?}. Do not start or propose any task or change ticket status. Return only its ticket reference.", agent.id);
        let started = engine.request("turn/start", json!({"threadId":thread,"input":[{"type":"text","text":prompt}]})).await?;
        let turn = started["turn"]["id"].as_str().ok_or("Ticket creation turn has no ID")?.to_owned();
        tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                let settled = events.lock().unwrap().iter().any(|event| event["method"] == "turn/completed" && event["params"]["threadId"] == thread && event["params"]["turn"]["id"] == turn);
                if settled { break; }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }).await.map_err(|_| "Ticket creation turn timed out")?;
        let page = engine.agents.store.list_tickets(&registered.id, None, 100, None).await?;
        let ticket = page.data.iter().find(|ticket| ticket.title == "Synthetic ticket lifecycle").ok_or("The real model did not create the requested ticket")?;
        if ticket.status != brr_agent_core::TicketStatus::Todo { return Err("Ticket creation changed its initial status".into()); }
        if !engine.agents.store.read_ticket(&ticket.id, None, None).await?.runs.is_empty() { return Err("Ticket creation dispatched work without review".into()); }
        let proposal = engine.tickets_request("proposeRun", json!({"ticketId":ticket.id,"revision":ticket.revision,"agentId":agent.id,"operationId":"live-proposal"})).await?;
        if proposal["snapshot"]["workspacePermission"] != "read-only" { return Err("Project permission did not reach the review snapshot".into()); }
        let task = engine.tickets_request("approveRun", json!({"ticketId":ticket.id,"hash":proposal["hash"],"operationId":"live-approval"})).await?;
        let task_id = task["id"].as_str().ok_or("Native approval did not return a Task")?;
        let settled = tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                let current = engine.agents.store.get_task(task_id).await?;
                if matches!(current.status.as_str(), "succeeded" | "failed" | "blocked" | "cancelled") { return Ok::<_, String>(current); }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }).await.map_err(|_| "Reviewed ticket execution timed out")??;
        if settled.status != "succeeded" { return Err(format!("Reviewed task ended {}: {:?}", settled.status, settled.reason)); }
        let result = engine.agents.store.read_ticket(&ticket.id, None, None).await?;
        if result.ticket.status != brr_agent_core::TicketStatus::Review { return Err("Successful execution did not move the ticket to Review".into()); }
        if result.runs.len() != 1 || result.runs[0].evidence.as_ref().is_none_or(|items| items.is_empty()) { return Err("Published linked task evidence is missing".into()); }
        if std::fs::read(project.join("marker.txt")).map_err(|error| error.to_string())? != b"BRR_TICKETS_VERIFIED\n" { return Err("Read-only fixture contents changed".into()); }
        let replay = engine.tickets_request("approveRun", json!({"ticketId":ticket.id,"hash":proposal["hash"],"operationId":"live-approval-retry"})).await?;
        if replay["id"] != task_id { return Err("Repeated approval created a different Task".into()); }
        eprintln!("TICKET_LIVE_ACCEPTANCE: real model created a Todo ticket; native review queued one read-only Task; execution published evidence and moved to Review; repeat approval returned the original Task.");
        Ok::<_, String>(())
    }.await;
    engine.shutdown().await;
    outcome.unwrap();
}
