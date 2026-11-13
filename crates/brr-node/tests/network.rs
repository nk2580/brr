#![cfg(feature = "network")]

use brr_node::{
    events::EventHub,
    instance::private_directory,
    network::{self, Dispatch, NetworkApi, SnapshotCallback},
};
use brr_node_auth::{DeviceRole, NodeAuthStore};
use brr_remote_protocol::{
    Channel, EventCursor, NodeSummary, Reply, Request, Snapshot, SnapshotError, PROTOCOL_VERSION,
};
use reqwest::{Certificate, Client, Response};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::watch};

struct RunningNode {
    _dir: TempDir,
    api: Arc<NetworkApi>,
    base_url: String,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

async fn running_node() -> RunningNode {
    let dir = tempfile::tempdir().unwrap();
    let auth_dir = dir.path().join("node-private");
    private_directory(&auth_dir).unwrap();
    let auth = NodeAuthStore::open(auth_dir.join("auth.sqlite"), vec!["localhost".into()])
        .await
        .unwrap();
    let node_id = auth.node_id().to_owned();
    let events = EventHub::new(node_id.clone(), "runtime-1".into(), 8);
    let dispatch: Dispatch = Arc::new(|principal, request| {
        Box::pin(async move {
            Reply::from_result(
                request.id,
                Ok(
                    json!({"caller":{"id":principal.id,"name":principal.name,"role":principal.role,"nodeId":principal.node_id},"method":request.method}),
                ),
            )
        })
    });
    let snapshot: SnapshotCallback = Arc::new(move |principal| {
        let node_id = node_id.clone();
        Box::pin(async move { Ok(make_snapshot(node_id, principal.name)) })
    });
    let api = Arc::new(NetworkApi::new(auth, dispatch, snapshot, events, None));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::spawn(network::serve(listener, api.clone(), receiver));
    RunningNode {
        _dir: dir,
        api,
        base_url: format!("https://localhost:{}", address.port()),
        shutdown,
        task,
    }
}

fn make_snapshot(node_id: String, name: String) -> Snapshot {
    Snapshot {
        protocol_version: PROTOCOL_VERSION,
        node_id: node_id.clone(),
        epoch: "runtime-1".into(),
        cursor: EventCursor {
            epoch: "runtime-1".into(),
            sequence: 0,
        },
        nodes: vec![NodeSummary {
            node_id,
            name,
            platform: "macos".into(),
            online: true,
            primary: true,
            authenticated: true,
            runtime_connected: false,
            epoch: "runtime-1".into(),
        }],
        account: None,
        models: vec![],
        threads: vec![],
        projects: vec![],
        agents: vec![],
        tasks: vec![],
        questions: vec![],
        approvals: vec![],
        sync: json!({}),
        errors: Vec::<SnapshotError>::new(),
    }
}

fn client_with_ca(pem: &str) -> Client {
    Client::builder()
        .add_root_certificate(Certificate::from_pem(pem.as_bytes()).unwrap())
        .build()
        .unwrap()
}

async fn redeem_over_https(node: &RunningNode, client: &Client) -> Value {
    let invite = node
        .api
        .auth
        .create_invite(DeviceRole::Mobile, Duration::from_secs(60))
        .await
        .unwrap();
    let response = client
        .post(format!("{}/v1/pair", node.base_url))
        .json(&json!({"code":invite.code,"requestedRole":"mobile","name":"phone"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

async fn stop(node: RunningNode) {
    let _ = node.shutdown.send(true);
    tokio::time::timeout(Duration::from_secs(3), node.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn tls_pairing_authenticates_callback_and_rejects_replay() {
    let node = running_node().await;
    let paired_client = client_with_ca(node.api.auth.certificate_pem());
    let unauthenticated = paired_client
        .post(format!("{}/v1/request", node.base_url))
        .body("{")
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let invite = node
        .api
        .auth
        .create_invite(DeviceRole::Mobile, Duration::from_secs(60))
        .await
        .unwrap();
    let pair_request = json!({"code":invite.code,"requestedRole":"mobile","name":"phone"});
    let paired = paired_client
        .post(format!("{}/v1/pair", node.base_url))
        .json(&pair_request)
        .send()
        .await
        .unwrap();
    assert_eq!(paired.status(), reqwest::StatusCode::OK);
    let credential: Value = paired.json().await.unwrap();
    assert_eq!(credential["role"], "mobile");
    assert_eq!(
        credential["trustedServerCertPem"],
        node.api.auth.certificate_pem()
    );

    let request = Request {
        id: "request-one".into(),
        channel: Channel::System,
        method: "status".into(),
        params: json!({}),
        operation_id: None,
        expected_epoch: None,
        expected_revision: None,
        target_node_id: None,
    };
    let response: Value = paired_client
        .post(format!("{}/v1/request", node.base_url))
        .bearer_auth(credential["secret"].as_str().unwrap())
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["result"]["caller"]["id"], credential["deviceId"]);
    assert_eq!(response["result"]["caller"]["role"], "mobile");
    assert_eq!(response["result"]["method"], "status");

    let replay = paired_client
        .post(format!("{}/v1/pair", node.base_url))
        .json(&pair_request)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), reqwest::StatusCode::CONFLICT);
    stop(node).await;
}

#[tokio::test]
async fn tls_requires_the_pairing_ca_and_the_configured_hostname() {
    let node = running_node().await;
    let credential =
        redeem_over_https(&node, &client_with_ca(node.api.auth.certificate_pem())).await;
    let paired_client = client_with_ca(node.api.auth.certificate_pem());
    let good = paired_client
        .get(format!("{}/v1/snapshot", node.base_url))
        .bearer_auth(credential["secret"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(good.status(), reqwest::StatusCode::OK);
    let good_snapshot: Value = good.json().await.unwrap();
    assert_eq!(good_snapshot["nodes"][0]["name"], "phone");

    let other_dir = tempfile::tempdir().unwrap();
    let other_private = other_dir.path().join("node-private");
    private_directory(&other_private).unwrap();
    let other = NodeAuthStore::open(other_private.join("auth.sqlite"), vec!["localhost".into()])
        .await
        .unwrap();
    let wrong_ca = client_with_ca(other.certificate_pem());
    assert!(wrong_ca
        .get(format!("{}/v1/snapshot", node.base_url))
        .send()
        .await
        .is_err());
    let wrong_host = format!(
        "https://127.0.0.1:{}",
        node.base_url.rsplit(':').next().unwrap()
    );
    assert!(paired_client.get(wrong_host).send().await.is_err());
    stop(node).await;
}

#[tokio::test]
async fn sse_replays_from_cursor_and_closes_a_revoked_device_stream() {
    let node = running_node().await;
    let credential =
        redeem_over_https(&node, &client_with_ca(node.api.auth.certificate_pem())).await;
    node.api.events.publish(json!({"method":"first"}));
    let client = client_with_ca(node.api.auth.certificate_pem());
    let response = client
        .get(format!(
            "{}/v1/events?epoch=runtime-1&after=0",
            node.base_url
        ))
        .bearer_auth(credential["secret"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut stream = response;
    let first = next_sse(&mut stream).await;
    assert!(
        first.contains("first"),
        "replay data was not delivered: {first}"
    );

    node.api.events.publish(json!({"method":"second"}));
    let second = next_sse(&mut stream).await;
    assert!(
        second.contains("second"),
        "live event was not delivered: {second}"
    );

    node.api
        .auth
        .revoke(credential["deviceId"].as_str().unwrap())
        .await
        .unwrap();
    node.api.events.publish(json!({"method":"device/changed"}));
    let ended = tokio::time::timeout(Duration::from_secs(3), stream.chunk())
        .await
        .unwrap()
        .unwrap();
    assert!(ended.is_none(), "revoked event stream remained open");
    stop(node).await;
}

#[tokio::test]
async fn shutdown_closes_active_sse_before_the_server_returns() {
    let node = running_node().await;
    let credential =
        redeem_over_https(&node, &client_with_ca(node.api.auth.certificate_pem())).await;
    let client = client_with_ca(node.api.auth.certificate_pem());
    let mut stream = client
        .get(format!("{}/v1/events", node.base_url))
        .bearer_auth(credential["secret"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), reqwest::StatusCode::OK);
    let first = next_sse(&mut stream).await;
    assert!(first.contains("snapshot_required"));

    let _ = node.shutdown.send(true);
    tokio::time::timeout(Duration::from_secs(3), node.task)
        .await
        .expect("server shutdown timed out")
        .unwrap()
        .unwrap();
    let final_chunk = tokio::time::timeout(Duration::from_secs(1), stream.chunk())
        .await
        .expect("server returned while SSE connection remained open");
    assert!(matches!(final_chunk, Ok(None) | Err(_)));
}

async fn next_sse(stream: &mut Response) -> String {
    let mut output = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !output.windows(2).any(|window| window == b"\n\n") {
            let Some(chunk) = stream.chunk().await.unwrap() else {
                break;
            };
            output.extend_from_slice(&chunk);
        }
    })
    .await
    .unwrap();
    String::from_utf8_lossy(&output).to_string()
}
