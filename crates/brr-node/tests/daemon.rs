#![cfg(all(unix, feature = "host", feature = "network"))]
use serde_json::Value;
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon(data: &Path) -> Daemon {
    Daemon(
        Command::new(env!("CARGO_BIN_EXE_brrd"))
            .arg("--data-dir")
            .arg(data)
            .arg("--runtime-path")
            .arg(data.join("missing-owned-runtime"))
            .arg("--listen")
            .arg("127.0.0.1:0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn wait_status(data: &Path, daemon: &mut Daemon) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "Daemon stopped before serving clients"
        );
        let status = Command::new(env!("CARGO_BIN_EXE_brrctl"))
            .arg("--data-dir")
            .arg(data)
            .arg("status")
            .output()
            .unwrap();
        if status.status.success() {
            return serde_json::from_slice(&status.stdout).unwrap();
        }
        assert!(Instant::now() < deadline, "Daemon did not become available");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn daemon_survives_client_exit_and_reopens_the_same_node_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let mut first = daemon(directory.path());
    let original = wait_status(directory.path(), &mut first);
    let again = wait_status(directory.path(), &mut first);
    assert_eq!(original["nodeId"], again["nodeId"]);
    assert_eq!(original["pid"], again["pid"]);
    assert_eq!(original["runtimeConnected"], false);
    drop(first);
    let mut restarted = daemon(directory.path());
    let current = wait_status(directory.path(), &mut restarted);
    assert_eq!(original["nodeId"], current["nodeId"]);
    assert_ne!(original["pid"], current["pid"]);
}

#[tokio::test]
async fn a_local_invitation_pairs_a_phone_to_the_running_service_over_pinned_tls() {
    let directory = tempfile::tempdir().unwrap();
    let mut running = daemon(directory.path());
    let status = wait_status(directory.path(), &mut running);
    let output = Command::new(env!("CARGO_BIN_EXE_brrctl"))
        .arg("--data-dir")
        .arg(directory.path())
        .args(["pair", "--name", "Test phone"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let invitation: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(invitation["nodeId"], status["nodeId"]);
    let client = reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .add_root_certificate(
            reqwest::Certificate::from_pem(
                invitation["certificatePem"].as_str().unwrap().as_bytes(),
            )
            .unwrap(),
        )
        .build()
        .unwrap();
    let endpoint = invitation["endpoint"].as_str().unwrap();
    let response = client.post(format!("{endpoint}/v1/pair"))
        .json(&serde_json::json!({"code":invitation["code"],"requestedRole":"mobile","name":"Test phone"}))
        .send().await.unwrap();
    assert!(response.status().is_success());
    let credential: Value = response.json().await.unwrap();
    let snapshot = client
        .get(format!("{endpoint}/v1/snapshot"))
        .bearer_auth(credential["secret"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert!(snapshot.status().is_success());
    let snapshot: Value = snapshot.json().await.unwrap();
    assert_eq!(snapshot["nodeId"], status["nodeId"]);
    assert_eq!(snapshot["nodes"][0]["runtimeConnected"], false);
    assert!(snapshot["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error["scope"] == "runtime"));
    let forbidden = client.post(format!("{endpoint}/v1/request"))
        .bearer_auth(credential["secret"].as_str().unwrap())
        .json(&serde_json::json!({"id":"remote-invite","channel":"fleet","method":"invitation.create","operationId":"remote-invite","params":{}}))
        .send().await.unwrap().json::<Value>().await.unwrap();
    assert_eq!(forbidden["error"]["code"], "forbidden");
}

#[tokio::test]
async fn desktop_attachment_detaches_and_reuses_the_existing_service() {
    let directory = tempfile::tempdir().unwrap();
    let mut settings = brr_node::settings::Settings::default();
    settings.listen = "127.0.0.1:0".parse().unwrap();
    settings.save(&directory.path().join("node")).unwrap();
    let client = brr_node::attachment::attach(
        directory.path(),
        Path::new(env!("CARGO_BIN_EXE_brrd")),
        Some(&directory.path().join("missing-runtime")),
    )
    .await
    .unwrap();
    let first = brr_node::attachment::status(&client).await.unwrap();
    let pid = first["pid"].as_u64().unwrap() as i32;
    struct Stop(i32);
    impl Drop for Stop {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGTERM);
            }
        }
    }
    let _stop = Stop(pid);
    drop(client);
    let attached = brr_node::attachment::attach(
        directory.path(),
        Path::new("/missing-service-should-not-start"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        first["pid"],
        brr_node::attachment::status(&attached).await.unwrap()["pid"]
    );
}
