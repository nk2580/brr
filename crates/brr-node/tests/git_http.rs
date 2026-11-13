#![cfg(feature = "network")]

use brr_node::{
    events::EventHub,
    git_http::{GitHttp, GitHttpClient},
    network::{self, Dispatch, NetworkApi, SnapshotCallback},
    peers::{JoinConfig, Peers},
};
use brr_node_auth::{DeviceRole, NodeAuthStore};
use brr_remote_protocol::{
    ApiError, Channel, EventCursor, NodeSummary, Reply, Snapshot, SnapshotError, PROTOCOL_VERSION,
};
use reqwest::{Certificate, Client};
use serde_json::{json, Value};
use std::{net::SocketAddr, process::Command, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::watch};

fn git(args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

async fn private_store(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::create_dir(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

fn receive_update(old: &str, new: &str, reference: &str) -> Vec<u8> {
    let command = format!("{old} {new} {reference}\0report-status side-band-64k\n");
    let mut body = format!("{:04x}", command.len() + 4).into_bytes();
    body.extend_from_slice(command.as_bytes());
    body.extend_from_slice(b"0000PACK\0invalid-pack");
    body
}

struct Server {
    url: String,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

async fn start_server(api: Arc<NetworkApi>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let (shutdown, rx) = watch::channel(false);
    let task = tokio::spawn(network::serve(listener, api, rx));
    Server {
        url: format!("https://localhost:{}", address.port()),
        shutdown,
        task,
    }
}

fn snapshot(node_id: String, name: String) -> Snapshot {
    Snapshot {
        protocol_version: PROTOCOL_VERSION,
        node_id: node_id.clone(),
        epoch: "runtime-one".into(),
        cursor: EventCursor {
            epoch: "runtime-one".into(),
            sequence: 0,
        },
        nodes: vec![NodeSummary {
            node_id,
            name,
            platform: "test".into(),
            online: true,
            primary: true,
            authenticated: true,
            runtime_connected: false,
            epoch: "runtime-one".into(),
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

fn api(
    auth: NodeAuthStore,
    peers: Peers,
    git: Option<brr_node::network::GitCallback>,
) -> Arc<NetworkApi> {
    let local_id = auth.node_id().to_owned();
    let dispatch: Dispatch = Arc::new(move |principal, request| {
        let peers = peers.clone();
        Box::pin(async move {
            if request.channel == Channel::Fleet && request.method == "node.register" {
                Reply::from_result(
                    request.id,
                    peers.register_reverse(&principal, request.params).await,
                )
            } else {
                Reply::from_result(
                    request.id,
                    Err(ApiError::new("unsupported_method", "unsupported")),
                )
            }
        })
    });
    let snapshot_node_id = local_id.clone();
    let snapshot: SnapshotCallback = Arc::new(move |principal| {
        let node_id = snapshot_node_id.clone();
        Box::pin(async move { Ok(snapshot(node_id, principal.name)) })
    });
    Arc::new(NetworkApi::new(
        auth,
        dispatch,
        snapshot,
        EventHub::new(local_id, "runtime-one".into(), 16),
        git,
    ))
}

#[tokio::test]
async fn fixed_primary_repository_is_opened_as_a_bare_repository() {
    let dir = TempDir::new().unwrap();
    let bare = dir.path().join("origin.git");
    git(&["init", "--bare", bare.to_str().unwrap()]);
    let peers_dir = dir.path().join("private");
    std::fs::create_dir(&peers_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&peers_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let peers = Peers::open(peers_dir.join("peers.sqlite")).await.unwrap();
    peers.init_primary("primary-node").await.unwrap();
    let transport = GitHttp::open(&bare, peers)
        .await
        .expect("fixed bare repository should be accepted");
    let _callback = Arc::new(transport).callback();
}

#[tokio::test]
async fn paired_node_can_fetch_and_push_only_its_registered_branch_over_pinned_tls() {
    let dir = TempDir::new().unwrap();
    let primary_private = private_store(dir.path(), "primary-private").await;
    let primary_auth = NodeAuthStore::open(
        primary_private.join("auth.sqlite"),
        vec!["localhost".into()],
    )
    .await
    .unwrap();
    let primary_id = primary_auth.node_id().to_owned();
    let primary_peers = Peers::open(primary_private.join("peers.sqlite"))
        .await
        .unwrap();
    primary_peers.init_primary(&primary_id).await.unwrap();

    let bare = dir.path().join("origin.git");
    git(&["init", "--bare", bare.to_str().unwrap()]);
    let seed = dir.path().join("seed");
    git(&["init", "--initial-branch=main", seed.to_str().unwrap()]);
    git_in(&seed, &["config", "user.name", "Fleet Test"]);
    git_in(&seed, &["config", "user.email", "fleet@example.invalid"]);
    std::fs::write(seed.join("README"), "fleet\n").unwrap();
    git_in(&seed, &["add", "README"]);
    git_in(&seed, &["commit", "-m", "seed"]);
    git_in(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git_in(&seed, &["push", "origin", "main"]);

    let git_service = Arc::new(GitHttp::open(&bare, primary_peers.clone()).await.unwrap());
    let primary = start_server(api(
        primary_auth.clone(),
        primary_peers.clone(),
        Some(git_service.callback()),
    ))
    .await;
    let child_private = private_store(dir.path(), "child-private").await;
    let child_auth =
        NodeAuthStore::open(child_private.join("auth.sqlite"), vec!["localhost".into()])
            .await
            .unwrap();
    let child_peers = Peers::open(child_private.join("peers.sqlite"))
        .await
        .unwrap();
    let child = start_server(api(child_auth.clone(), child_peers.clone(), None)).await;

    let invite = primary_auth
        .create_invite(DeviceRole::Node, Duration::from_secs(60))
        .await
        .unwrap();
    child_peers
        .join(
            &child_auth,
            JoinConfig {
                primary_endpoint: primary.url.clone(),
                primary_node_id: primary_id,
                primary_ca_pem: primary_auth.certificate_pem().to_owned(),
                primary_fingerprint: primary_auth.fingerprint().to_owned(),
                invite_code: invite.code,
                name: "Linux Node".into(),
                node_endpoint: child.url.clone(),
                reverse_invite_ttl: Duration::from_secs(60),
            },
        )
        .await
        .unwrap();

    let transport = child_peers.git_transport().await.unwrap();
    let saved_secret = transport.credential.expose_secret().to_owned();
    assert_eq!(
        GitHttpClient::new(transport).unwrap().origin_url(),
        format!("{}/v1/git/origin.git", primary.url)
    );
    let transport = child_peers.git_transport().await.unwrap();
    let client = Arc::new(GitHttpClient::new(transport).unwrap());
    let checkout = dir.path().join("node-checkout");
    std::fs::create_dir(&checkout).unwrap();
    let clone_client = client.clone();
    let clone_path = checkout.clone();
    tokio::task::spawn_blocking(move || clone_client.clone_repository(&clone_path))
        .await
        .unwrap()
        .unwrap();
    git_in(
        &checkout,
        &[
            "config",
            "url.https://attacker.invalid/.insteadOf",
            client.origin_url(),
        ],
    );
    let unsafe_client = client.clone();
    let unsafe_path = checkout.clone();
    assert!(
        tokio::task::spawn_blocking(move || unsafe_client.fetch(&unsafe_path))
            .await
            .unwrap()
            .is_err(),
        "a local URL rewrite must not receive the pinned bearer"
    );
    git_in(
        &checkout,
        &[
            "config",
            "--unset-all",
            "url.https://attacker.invalid/.insteadOf",
        ],
    );
    git_in(&checkout, &["config", "user.name", "Fleet Test"]);
    git_in(
        &checkout,
        &["config", "user.email", "fleet@example.invalid"],
    );
    let node_id = child_auth.node_id().to_owned();
    let branch = format!("nodes/{node_id}");
    git_in(&checkout, &["checkout", "-b", &branch]);
    std::fs::write(checkout.join("node.txt"), "owned by node\n").unwrap();
    git_in(&checkout, &["add", "node.txt"]);
    git_in(&checkout, &["commit", "-m", "node update"]);
    let push_client = client.clone();
    let push_path = checkout.clone();
    let push_node_id = node_id.clone();
    tokio::task::spawn_blocking(move || push_client.push_node_branch(&push_path, &push_node_id))
        .await
        .unwrap()
        .unwrap();
    let pushed = git(&[
        "--git-dir",
        bare.to_str().unwrap(),
        "rev-parse",
        &format!("refs/heads/nodes/{node_id}"),
    ]);
    assert_eq!(pushed.len(), 40);
    let fetch_client = client.clone();
    let fetch_path = checkout.clone();
    tokio::task::spawn_blocking(move || fetch_client.fetch(&fetch_path))
        .await
        .unwrap()
        .unwrap();
    assert!(git_in(&checkout, &["branch", "-r"]).contains(&format!("origin/{branch}")));

    // Replacing a node ref with a commit based on main is non-fast-forward;
    // the fixed bare-repository receive policy must retain the accepted head.
    git_in(&checkout, &["reset", "--hard", "origin/main"]);
    std::fs::write(checkout.join("divergent.txt"), "not a descendant\n").unwrap();
    git_in(&checkout, &["add", "divergent.txt"]);
    git_in(&checkout, &["commit", "-m", "divergent node history"]);
    let rejected = client.clone();
    let rejected_path = checkout.clone();
    let rejected_id = node_id.clone();
    assert!(tokio::task::spawn_blocking(
        move || rejected.push_node_branch(&rejected_path, &rejected_id)
    )
    .await
    .unwrap()
    .is_err());
    assert_eq!(
        git(&[
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            &format!("refs/heads/nodes/{node_id}")
        ]),
        pushed
    );

    let attacker = Client::builder()
        .add_root_certificate(
            Certificate::from_pem(primary_auth.certificate_pem().as_bytes()).unwrap(),
        )
        .build()
        .unwrap();
    let receive_url = format!("{}/v1/git/origin.git/git-receive-pack", primary.url);
    let zero = "0".repeat(40);
    let one = "1".repeat(40);
    for reference in [
        "refs/heads/main",
        "refs/heads/nodes/other-node",
        "refs/tags/release",
    ] {
        let response = attacker
            .post(&receive_url)
            .bearer_auth(&saved_secret)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-git-receive-pack-request",
            )
            .body(receive_update(&zero, &one, reference))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::FORBIDDEN,
            "accepted {reference}"
        );
    }
    let deletion = attacker
        .post(&receive_url)
        .bearer_auth(&saved_secret)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-git-receive-pack-request",
        )
        .body(receive_update(
            &one,
            &zero,
            &format!("refs/heads/nodes/{node_id}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(deletion.status(), reqwest::StatusCode::FORBIDDEN);
    let malformed = attacker
        .post(&receive_url)
        .bearer_auth(&saved_secret)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-git-receive-pack-request",
        )
        .body("0009bad0000PACK")
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), reqwest::StatusCode::FORBIDDEN);

    // A valid Node credential without a roster binding cannot read the repository.
    let stranger = NodeAuthStore::open(
        private_store(dir.path(), "stranger-private")
            .await
            .join("auth.sqlite"),
        vec!["localhost".into()],
    )
    .await
    .unwrap();
    let stranger_invite = primary_auth
        .create_invite(DeviceRole::Node, Duration::from_secs(60))
        .await
        .unwrap();
    let ca = Certificate::from_pem(primary_auth.certificate_pem().as_bytes()).unwrap();
    let credential: Value = Client::builder()
        .add_root_certificate(ca)
        .build()
        .unwrap()
        .post(format!("{}/v1/pair", primary.url))
        .json(&json!({"code": stranger_invite.code, "requestedRole":"node", "name":"unregistered"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let response = Client::builder()
        .add_root_certificate(
            Certificate::from_pem(primary_auth.certificate_pem().as_bytes()).unwrap(),
        )
        .build()
        .unwrap()
        .get(format!(
            "{}/v1/git/origin.git/info/refs?service=git-upload-pack",
            primary.url
        ))
        .bearer_auth(credential["secret"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    drop(stranger);

    let _ = primary.shutdown.send(true);
    let _ = child.shutdown.send(true);
    primary.task.await.unwrap().unwrap();
    child.task.await.unwrap().unwrap();
}
