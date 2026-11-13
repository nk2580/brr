#![cfg(all(feature = "host", feature = "network"))]
use brr_node::{
    network,
    service::{Service, ServiceConfig},
};
use brr_remote_protocol::{Channel, ClientRole, Request};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::{net::TcpListener, sync::watch};

async fn running(
    data: &std::path::Path,
    name: &str,
) -> (
    Arc<Service>,
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), String>>,
) {
    use std::os::unix::fs::PermissionsExt;
    let runtime = data.join("fixture-runtime");
    std::fs::write(&runtime, include_str!("fixtures/runtime.py")).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let service = Service::open(ServiceConfig {
        data_dir: data.to_owned(),
        runtime_path: runtime,
        name: name.into(),
        endpoint_hosts: vec!["localhost".into()],
    })
    .await
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    service.set_endpoint(endpoint).await.unwrap();
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(network::serve(listener, service.network_api(), shutdown));
    (service, stop, task)
}
fn command(channel: Channel, method: &str, params: Value, op: &str) -> Request {
    Request {
        id: op.into(),
        channel,
        method: method.into(),
        params,
        operation_id: Some(op.into()),
        expected_epoch: None,
        expected_revision: None,
        target_node_id: None,
    }
}

fn seed_history(data: &std::path::Path, id: &str, text: &str) {
    let mut threads = serde_json::Map::new();
    threads.insert(id.into(),json!({"id":id,"preview":text,"createdAt":1,"updatedAt":2,"status":{"type":"idle"},"turns":[{"id":"historical-turn","status":"completed","items":[{"id":"historical-user","type":"userMessage","content":[{"type":"text","text":text}]}]}]}));
    std::fs::write(
        data.join("node/fixture-runtime.json"),
        serde_json::to_vec(&json!({"threads":threads,"calls":[],"approvals":[]})).unwrap(),
    )
    .unwrap();
    let sessions = data.join("engine/sessions/fixture");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.jsonl")),
        format!("{}\n", json!({"type":"session_meta","payload":{"id":id}})),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_pairs_once_and_routes_retries_to_the_original_execution_owner() {
    let primary_dir = tempfile::tempdir().unwrap();
    let node_dir = tempfile::tempdir().unwrap();
    let (primary, stop_primary, server_primary) = running(primary_dir.path(), "Primary Mac").await;
    let (node, stop_node, server_node) = running(node_dir.path(), "Worker").await;
    seed_history(
        primary_dir.path(),
        "older-primary-thread",
        "Prior primary conversation",
    );
    seed_history(
        node_dir.path(),
        "older-worker-thread",
        "Prior worker conversation",
    );
    let store = primary.host().agent_store();
    let draft = store
        .propose_profile(
            "legacy-interview",
            None,
            brr_agent_core::ProfileCandidate {
                name: "Existing researcher".into(),
                description: "Existing agent".into(),
                instructions: "Verify primary sources".into(),
                soul: "Precise".into(),
                model: "fixture-model".into(),
                effort: "medium".into(),
            },
        )
        .await
        .unwrap();
    store
        .approve_profile(&draft.id, &draft.hash, "legacy-approve")
        .await
        .unwrap();
    let profile = store
        .apply_profile(&draft.id, &draft.hash, "legacy-apply")
        .await
        .unwrap();
    let access = store.memory_access_for_user(None, None).await.unwrap();
    store
        .record_source(
            &access,
            "legacy-evidence",
            1,
            "Verify the source before reporting",
        )
        .await
        .unwrap();
    store
        .publish_memory(
            &access,
            brr_agent_core::MemoryScope::User,
            "fact",
            "Verified portable evidence",
            &["legacy-evidence@1".into()],
        )
        .await
        .unwrap();
    let init = primary
        .handle(
            command(Channel::Fleet, "init", json!({}), "init"),
            ClientRole::Desktop,
            "owner",
        )
        .await;
    assert!(init.error.is_none(), "{:?}", init.error);
    let initialized = init.result.unwrap();
    assert_eq!(initialized["sharedDataInitialized"], true);
    assert_eq!(initialized["historyCount"], 1);
    let invitation = primary
        .handle(
            command(
                Channel::Fleet,
                "invitation.create",
                json!({"role":"node"}),
                "invite-worker",
            ),
            ClientRole::Desktop,
            "owner",
        )
        .await
        .result
        .unwrap();
    let joined = node
        .handle(
            command(
                Channel::Fleet,
                "join",
                json!({"invitation":invitation}),
                "join-worker",
            ),
            ClientRole::Desktop,
            "owner",
        )
        .await;
    assert!(joined.error.is_none(), "{:?}", joined.error);
    let joined = joined.result.unwrap();
    assert_eq!(joined["sharedDataInitialized"], true);
    assert_eq!(joined["historyCount"], 1);
    let imported = node.host().agent_store();
    let imported_id = brr_agent_core::shared_entity_id(
        primary.node_id(),
        brr_agent_core::SharedTable::AgentProfiles,
        &profile.id,
    );
    assert_eq!(
        imported
            .read_profile(&imported_id)
            .await
            .unwrap()
            .instructions,
        profile.instructions
    );
    let imported_access = imported.memory_access_for_user(None, None).await.unwrap();
    assert_eq!(
        imported
            .search_memory(&imported_access, "portable")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(primary.snapshot().await.unwrap().nodes[0].primary);
    assert!(
        primary
            .snapshot()
            .await
            .unwrap()
            .nodes
            .iter()
            .any(|owner| owner.node_id == node.node_id()),
        "A phone's primary must expose every registered execution owner"
    );
    let invitation = primary
        .auth()
        .create_invite(
            brr_node_auth::DeviceRole::Mobile,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let phone = primary
        .auth()
        .redeem_invite(&invitation.code, brr_node_auth::DeviceRole::Mobile, "Phone")
        .await
        .unwrap();
    let client = reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .add_root_certificate(
            reqwest::Certificate::from_pem(primary.auth().certificate_pem().as_bytes()).unwrap(),
        )
        .build()
        .unwrap();
    let status = primary
        .handle(
            command(Channel::System, "status", json!({}), "status"),
            ClientRole::Desktop,
            "owner",
        )
        .await
        .result
        .unwrap();
    let endpoint = status["endpoint"].as_str().unwrap();
    let roster = brr_node::peers::Peers::open(node_dir.path().join("node/peers.sqlite"))
        .await
        .unwrap();
    let worker_credential = roster.git_transport().await.unwrap();
    for forbidden in [
        command(
            Channel::Fleet,
            "device.revoke",
            json!({"deviceId": phone.device_id}),
            "worker-cannot-revoke-phone",
        ),
        command(
            Channel::Sync,
            "resolve",
            json!({"conflictId": "any"}),
            "worker-cannot-resolve",
        ),
    ] {
        let response = client
            .post(format!("{endpoint}/v1/request"))
            .bearer_auth(worker_credential.credential.expose_secret())
            .json(&forbidden)
            .send()
            .await
            .unwrap();
        let reply: Value = response.json().await.unwrap();
        assert_eq!(reply["error"]["code"], "forbidden", "{reply}");
        assert!(reply.get("result").is_none());
    }
    assert_eq!(
        client
            .get(format!("{endpoint}/v1/snapshot"))
            .bearer_auth(worker_credential.credential.expose_secret())
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let mut agent = command(
        Channel::Agents,
        "agents.read",
        json!({"agentId":imported_id}),
        "canonical-profile",
    );
    agent.operation_id = None;
    agent.target_node_id = Some(node.node_id().into());
    let reply: Value = client
        .post(format!("{endpoint}/v1/request"))
        .bearer_auth(&phone.secret)
        .json(&agent)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(reply["result"]["id"], imported_id);
    assert_eq!(reply["result"]["originNodeId"], primary.node_id());

    let mut create = command(
        Channel::Projects,
        "create",
        json!({"name":"Linux research"}),
        "create-research",
    );
    create.target_node_id = Some(node.node_id().to_owned());
    let mut replies = Vec::new();
    for _ in 0..2 {
        let reply: Value = client
            .post(format!("{endpoint}/v1/request"))
            .bearer_auth(&phone.secret)
            .json(&create)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(reply.get("error").is_none(), "{reply}");
        replies.push(reply["result"].clone());
    }
    assert_eq!(replies[0], replies[1]);
    let overview = primary.snapshot().await.unwrap();
    assert!(overview
        .nodes
        .iter()
        .any(|owner| owner.node_id == node.node_id() && owner.online));
    assert!(overview
        .projects
        .iter()
        .any(|project| project["name"] == "Linux research"
            && project["ownerNodeId"] == node.node_id()));
    let (stop_monitor, monitor_shutdown) = watch::channel(false);
    let monitor = tokio::spawn(primary.clone().run_peer_monitor(monitor_shutdown));
    let mut updates = primary.events().subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(4),async {
        loop {
            node.events().publish(json!({"method":"item/agentMessage/delta","params":{"threadId":"owned-thread","delta":"remote token"}}));
            if let Ok(Ok(event))=tokio::time::timeout(std::time::Duration::from_millis(100),updates.recv()).await {
                if event.node_id==node.node_id() && event.message["method"]=="item/agentMessage/delta" {
                    assert_eq!(event.epoch,primary.events().cursor().epoch);
                    assert_eq!(event.owner_epoch.as_deref(),Some(node.events().cursor().epoch.as_str()));
                    break;
                }
            }
        }
    }).await.expect("The primary must relay live owner events through the phone's single stream");
    stop_monitor.send(true).unwrap();
    monitor.await.unwrap();
    assert_eq!(
        node.host()
            .project_request("list", json!({}))
            .await
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        primary
            .host()
            .project_request("list", json!({}))
            .await
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let (stop_history, history_shutdown) = watch::channel(false);
    let history_task = tokio::spawn(node.clone().run_history_projection(history_shutdown));
    let mut start = command(
        Channel::Sessions,
        "start",
        json!({"model":"fixture-model","text":"Created after joining the fleet","effort":"medium","mode":"default"}),
        "start-after-join",
    );
    start.target_node_id = Some(node.node_id().into());
    let started = primary.handle(start, ClientRole::Mobile, "phone").await;
    assert!(started.error.is_none(), "{:?}", started.error);
    let thread = started.result.unwrap()["thread"].clone();
    let thread_id = thread["id"].as_str().unwrap();
    let turn_id = thread["turns"][0]["id"].as_str().unwrap();
    let mut stop = command(
        Channel::Sessions,
        "stop",
        json!({"threadId":thread_id,"turnId":turn_id}),
        "stop-after-join",
    );
    stop.target_node_id = Some(node.node_id().into());
    stop.expected_epoch = Some(node.events().cursor().epoch);
    let stopped = primary.handle(stop, ClientRole::Mobile, "phone").await;
    assert!(stopped.error.is_none(), "{:?}", stopped.error);
    use sha2::{Digest, Sha256};
    let id = format!("transcript-{:x}", Sha256::digest(thread_id.as_bytes()));
    let key = format!("{}\0{id}", node.node_id());
    let git_path = format!("HEAD:records/transcript/{:x}.json", Sha256::digest(key));
    let writer = node_dir.path().join("fleet-catalog/writer/.brr-fleet/repo");
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let published = tokio::process::Command::new("git")
                .current_dir(&writer)
                .args(["show", &git_path])
                .output()
                .await
                .unwrap();
            if published.status.success() {
                let record: Value = serde_json::from_slice(&published.stdout).unwrap();
                if record["content"]["turns"][0]["status"] == "interrupted" {
                    assert_eq!(record["content"]["thread"]["id"], thread_id);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("A completed turn must project through the background Git writer");
    stop_history.send(true).unwrap();
    history_task.await.unwrap();
    let published = node
        .handle(
            command(Channel::Sync, "synchronize", json!({}), "publish-worker"),
            ClientRole::Desktop,
            "owner",
        )
        .await;
    assert!(published.error.is_none(), "{:?}", published.error);
    let merged = primary
        .handle(
            command(Channel::Sync, "synchronize", json!({}), "merge-primary"),
            ClientRole::Desktop,
            "owner",
        )
        .await;
    assert!(merged.error.is_none(), "{:?}", merged.error);
    let projects = primary
        .host()
        .project_store()
        .shared_snapshot(primary.node_id())
        .await
        .unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].origin_node_id.as_deref(), Some(node.node_id()));
    assert_eq!(projects[0].data["name"], "Linux research");
    let _ = stop_node.send(true);
    server_node.await.unwrap().unwrap();
    let mut historical = command(
        Channel::Sessions,
        "read",
        json!({"threadId":"older-worker-thread"}),
        "offline-history",
    );
    historical.target_node_id = Some(node.node_id().into());
    historical.operation_id = None;
    let historical = primary
        .handle(historical, ClientRole::Mobile, "phone")
        .await;
    assert!(historical.error.is_none(), "{:?}", historical.error);
    let cached = historical.result.unwrap();
    assert_eq!(cached["readOnly"], true);
    assert_eq!(cached["ownerNodeId"], node.node_id());
    assert_eq!(
        cached["turns"][0]["items"][0]["content"][0]["text"],
        "Prior worker conversation"
    );
    let mut historical = command(
        Channel::Sessions,
        "read",
        json!({"threadId":thread_id}),
        "new-offline-history",
    );
    historical.target_node_id = Some(node.node_id().into());
    historical.operation_id = None;
    let cached = primary
        .handle(historical, ClientRole::Mobile, "phone")
        .await
        .result
        .unwrap();
    assert_eq!(cached["historical"], true);
    assert_eq!(cached["turns"][0]["status"], "interrupted");
    assert_eq!(
        cached["turns"][0]["items"][0]["content"][0]["text"],
        "Created after joining the fleet"
    );
    let mut send = command(
        Channel::Sessions,
        "send",
        json!({"threadId":"older-worker-thread","text":"Must wait for owner"}),
        "offline-send",
    );
    send.target_node_id = Some(node.node_id().into());
    assert_eq!(
        primary
            .handle(send, ClientRole::Mobile, "phone")
            .await
            .error
            .unwrap()
            .code,
        "owner_offline"
    );
    let _ = stop_primary.send(true);
    server_primary.await.unwrap().unwrap();
    primary.shutdown().await;
    node.shutdown().await;
}
