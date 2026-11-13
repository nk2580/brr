//! Desktop attachment starts an owned service when needed; dropping a client never stops it.
use crate::{instance::private_directory, local::LocalClient};
use brr_remote_protocol::{ApiError, Channel, Request};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

pub async fn attach(
    data_dir: &Path,
    daemon: &Path,
    runtime: Option<&Path>,
) -> Result<LocalClient, ApiError> {
    let client = LocalClient::new(data_dir.join("node/brrd.sock"));
    if status(&client).await.is_ok() {
        return Ok(client);
    }
    if !daemon.is_file() {
        return Err(ApiError::new(
            "missing_service",
            "The packaged Brr service is missing. Build or reinstall Brr's owned service.",
        ));
    }
    private_directory(data_dir)?;
    let directory = data_dir.join("node");
    private_directory(&directory)?;
    let log_path = directory.join("service.log");
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let log = options
        .open(log_path)
        .map_err(|e| ApiError::new("service_log", e.to_string()))?;
    let mut command = tokio::process::Command::new(daemon);
    command
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .kill_on_drop(false);
    #[cfg(debug_assertions)]
    if let Some(runtime) = runtime {
        command.arg("--runtime-path").arg(runtime);
    }
    #[cfg(not(debug_assertions))]
    let _ = runtime;
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|e| ApiError::new("service_start", e.to_string()))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if status(&client).await.is_ok() {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(client);
        }
        if let Some(code) = child
            .try_wait()
            .map_err(|e| ApiError::new("service_start", e.to_string()))?
        {
            // A simultaneous desktop can win the single-owner lock.
            if status(&client).await.is_ok() {
                return Ok(client);
            }
            return Err(ApiError::new(
                "service_start",
                format!("The Brr service exited ({code}). Inspect node/service.log."),
            ));
        }
        if tokio::time::Instant::now() > deadline {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Err(ApiError::new(
                "service_start",
                "The service has not become ready. Inspect node/service.log.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
pub async fn status(client: &LocalClient) -> Result<Value, ApiError> {
    let reply = client
        .request(Request {
            id: uuid::Uuid::new_v4().to_string(),
            channel: Channel::System,
            method: "status".into(),
            params: json!({}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        })
        .await?;
    if let Some(error) = reply.error {
        return Err(error);
    }
    reply
        .result
        .ok_or_else(|| ApiError::new("invalid_reply", "Service returned no status"))
}
pub fn packaged_daemon_path() -> PathBuf {
    std::env::current_exe()
        .unwrap_or_default()
        .with_file_name("brrd")
}
