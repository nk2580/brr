mod agent_host;
mod agent_protocol;
mod agent_workers;
mod attachment_upload;
mod bridge;
mod chat_branch;
mod chat_fork;
mod chat_service;
mod chat_state;
pub mod export;
mod generated_ui_sandbox;
mod intelligent_ui;
mod policy;
mod project_archive;
mod project_files;
pub use policy::validate_login_url;
pub use project_files::validate_directory;
pub use attachment_upload::{AttachmentUploadMetadata, AttachmentUploadRecord, AttachmentUploadState};
pub mod projects;
mod questions;
mod task_workspaces;
mod tickets;
mod workspace_bridge;
mod worktrees;

#[cfg(test)]
#[path = "chat_project_reference_tests.rs"]
mod chat_project_reference_tests;

use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::sync::Mutex;

pub use workspace_bridge::BrowserToolHandler;
pub type EventSink = Arc<dyn Fn(Value) + Send + Sync>;
const BRR_SERVICE_DATA_DIR_ENV: &str = "BRR_SERVICE_DATA_DIR";

pub struct HostConfig {
    pub data_dir: PathBuf,
    pub runtime_path: PathBuf,
}
pub struct Host {
    data_dir: PathBuf,
    runtime_path: PathBuf,
    events: EventSink,
    stores: (brr_agent_core::AgentStore, projects::ProjectStore),
    chat_store: chat_state::ChatStore,
    engine: Mutex<Option<Arc<bridge::Engine>>>,
    connect_guard: Mutex<()>,
    browser_handler: Mutex<Option<BrowserToolHandler>>,
}
impl Host {
    pub async fn open(config: HostConfig, events: EventSink) -> Result<Self, String> {
        let data_dir = prepare_private_data_dir(&config.data_dir)?;
        let engine_home = data_dir.join("engine");
        std::fs::create_dir_all(&engine_home).map_err(|e| e.to_string())?;
        let engine_metadata = std::fs::symlink_metadata(&engine_home).map_err(|e| e.to_string())?;
        if engine_metadata.file_type().is_symlink() || !engine_metadata.is_dir() {
            return Err("The Brr runtime data directory must be a real directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&engine_home, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let agent_store = brr_agent_core::AgentStore::open(engine_home.join("agents")).await?;
        let project_store = projects::ProjectStore::open(data_dir.join("projects.sqlite")).await?;
        let chat_store = chat_state::ChatStore::open(engine_home.join("agents/chat")).await?;
        Ok(Self {
            data_dir,
            runtime_path: config.runtime_path,
            events,
            stores: (agent_store, project_store),
            chat_store,
            engine: Mutex::new(None),
            connect_guard: Mutex::new(()),
            browser_handler: Mutex::new(None),
        })
    }
    pub async fn connect(&self) -> Result<(), String> {
        let _connection = self.connect_guard.lock().await;
        if self.is_connected().await {
            return Ok(());
        }
        let home = self.data_dir.join("engine");
        std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let mut command = tokio::process::Command::new(&self.runtime_path);
        configure_runtime_environment(&mut command, &home, &self.data_dir);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let started = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            bridge::Engine::connect_with_stores(
                command,
                self.events.clone(),
                self.stores.0.clone(),
                self.stores.1.clone(),
            ),
        )
        .await
        .map_err(|_| {
            "The owned runtime did not finish its startup handshake within 30 seconds".to_owned()
        })??;
        if let Some(handler) = self.browser_handler.lock().await.clone() {
            started.set_browser_handler(handler);
        }
        *self.engine.lock().await = Some(Arc::new(started));
        Ok(())
    }
    async fn engine(&self) -> Result<Arc<bridge::Engine>, String> {
        self.engine
            .lock()
            .await
            .clone()
            .filter(|e| e.is_alive())
            .ok_or_else(|| "Connect the coding runtime first".into())
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.engine().await?.request(method, params).await
    }
    pub async fn chat_request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.engine()
            .await?
            .agents
            .chat_request(method, params)
            .await
    }
    pub async fn attachment_upload_begin(
        &self,
        metadata: AttachmentUploadMetadata,
    ) -> Result<AttachmentUploadRecord, String> {
        self.chat_store.attachment_upload_begin(metadata).await
    }
    pub async fn attachment_upload_status(
        &self,
        owner_id: &str,
        upload_id: &str,
    ) -> Result<Option<AttachmentUploadRecord>, String> {
        self.chat_store
            .attachment_upload_status(owner_id, upload_id)
            .await
    }
    pub async fn attachment_upload_commit(
        &self,
        owner_id: &str,
        upload_id: &str,
        staged_path: &Path,
    ) -> Result<AttachmentUploadRecord, String> {
        self.chat_store
            .attachment_upload_commit(owner_id, upload_id, staged_path)
            .await
    }
    pub async fn attachment_upload_cancel(
        &self,
        owner_id: &str,
        upload_id: &str,
    ) -> Result<AttachmentUploadRecord, String> {
        self.chat_store
            .attachment_upload_cancel(owner_id, upload_id)
            .await
    }
    pub async fn agents_request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.engine().await?.agents_request(method, params).await
    }
    pub async fn agents_request_with_expected_revision(
        &self,
        method: &str,
        params: Value,
        expected_revision: Option<&str>,
    ) -> Result<Value, String> {
        self.engine()
            .await?
            .agents_request_with_expected_revision(method, params, expected_revision)
            .await
    }
    pub async fn tickets_request(&self, method: &str, params: Value) -> Result<Value, String> {
        if method == "approveRun" {
            return self.engine().await?.tickets_request(method, params).await;
        }
        tickets::request_stores(&self.stores.0, &self.stores.1, method, params).await
    }
    pub async fn intelligent_ui_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.engine()
            .await?
            .intelligent_ui_request(method, params)
            .await
    }
    pub async fn project_request(&self, method: &str, params: Value) -> Result<Value, String> {
        let store = &self.stores.1;
        match method {
            "list" => Ok(serde_json::json!({"data":store.list().await?})),
            "create" => Ok(serde_json::to_value(
                store
                    .create(serde_json::from_value(params).map_err(|e| e.to_string())?)
                    .await?,
            )
            .map_err(|e| e.to_string())?),
            "save" => Ok(serde_json::to_value(
                store
                    .save(serde_json::from_value(params).map_err(|e| e.to_string())?)
                    .await?,
            )
            .map_err(|e| e.to_string())?),
            "move" => {
                let thread = params["threadId"].as_str().ok_or("Missing threadId")?;
                store
                    .set_membership(thread, params["projectId"].as_str())
                    .await?;
                Ok(Value::Null)
            }
            "delete" => {
                let id = params["projectId"].as_str().ok_or("Missing projectId")?;
                store.delete(id).await?;
                Ok(Value::Null)
            }
            "archive" => {
                let path = params["path"].as_str().ok_or("Missing path")?;
                self.engine()
                    .await?
                    .archive_project_chats(path)
                    .await
                    .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            }
            _ => Err(format!("Unsupported project operation: {method}")),
        }
    }
    pub async fn project_save_metadata(
        &self,
        id: &str,
        expected_revision: i64,
        name: String,
        model: Option<String>,
        effort: Option<String>,
        permission: Option<projects::ProjectPermission>,
    ) -> Result<projects::ProjectRecord, String> {
        self.stores
            .1
            .save_metadata(id, expected_revision, name, model, effort, permission)
            .await
    }
    pub async fn project_delete_expected_revision(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> Result<(), String> {
        self.stores
            .1
            .delete_expected_revision(id, expected_revision)
            .await
    }
    pub async fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        self.engine().await?.respond(id, result).await
    }
    pub async fn shutdown(&self) {
        let _connection = self.connect_guard.lock().await;
        if let Some(engine) = self.engine.lock().await.take() {
            engine.shutdown().await
        }
    }
    pub async fn set_browser_handler(&self, handler: BrowserToolHandler) {
        *self.browser_handler.lock().await = Some(handler.clone());
        if let Ok(engine) = self.engine().await {
            engine.set_browser_handler(handler)
        }
    }
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    pub fn agent_store(&self) -> brr_agent_core::AgentStore {
        self.stores.0.clone()
    }
    pub fn project_store(&self) -> projects::ProjectStore {
        self.stores.1.clone()
    }
    pub async fn is_connected(&self) -> bool {
        self.engine
            .lock()
            .await
            .as_ref()
            .is_some_and(|e| e.is_alive())
    }

    #[cfg(test)]
    pub(crate) async fn verify_agent_protocol(&self) -> Result<(), String> {
        self.engine().await?.verify_agent_protocol().await
    }

    #[cfg(test)]
    pub(crate) async fn verify_live_agent_task(&self) -> Result<(), String> {
        self.engine().await?.verify_live_agent_task().await
    }
}

fn configure_runtime_environment(
    command: &mut tokio::process::Command,
    engine_home: &Path,
    service_data_root: &Path,
) {
    for name in [
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "CODEX_AUTH_JSON",
        "OPENAI_ORG_ID",
        "OPENAI_ORGANIZATION",
        "CODEX_MODEL_PROVIDER",
        "CODEX_HOME",
        BRR_SERVICE_DATA_DIR_ENV,
    ] {
        command.env_remove(name);
    }
    command
        .env("CODEX_HOME", engine_home)
        .env(BRR_SERVICE_DATA_DIR_ENV, service_data_root)
        .current_dir(engine_home);
}

fn prepare_private_data_dir(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    if let Ok(metadata) = std::fs::symlink_metadata(&absolute) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("The Brr data directory must be a real directory".into());
        }
    }
    let requested_parent = absolute
        .parent()
        .ok_or_else(|| "The Brr data directory must have a parent".to_owned())?;
    if let Ok(metadata) = std::fs::symlink_metadata(requested_parent) {
        let system_alias =
            requested_parent == Path::new("/tmp") || requested_parent == Path::new("/var");
        if !system_alias && (metadata.file_type().is_symlink() || !metadata.is_dir()) {
            return Err("The Brr data directory parent must be a real directory".into());
        }
    }

    // Canonicalize an existing ancestor so OS aliases such as macOS /var and
    // /tmp do not count as application-controlled symlinks. The requested
    // data leaf and its direct parent were checked before this canonicalization.
    let leaf = absolute
        .file_name()
        .ok_or_else(|| "The Brr data directory must name a directory".to_owned())?;
    let mut ancestor = requested_parent.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) => {
                let system_alias = ancestor == Path::new("/tmp") || ancestor == Path::new("/var");
                if (!metadata.is_dir() || metadata.file_type().is_symlink()) && !system_alias {
                    return Err("The Brr data directory parent must be a directory".into());
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| "The Brr data directory parent does not exist".to_owned())?;
                missing.push(name.to_os_string());
                if !ancestor.pop() {
                    return Err("The Brr data directory parent does not exist".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let mut canonical_data_dir =
        std::fs::canonicalize(&ancestor).map_err(|error| error.to_string())?;
    for component in missing.iter().rev() {
        canonical_data_dir.push(component);
        match std::fs::symlink_metadata(&canonical_data_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err("The Brr data directory parent must be a real directory".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&canonical_data_dir).map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    canonical_data_dir.push(leaf);
    match std::fs::symlink_metadata(&canonical_data_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("The Brr data directory must be a real directory".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&canonical_data_dir).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let metadata =
        std::fs::symlink_metadata(&canonical_data_dir).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // Parent directories may belong to the OS or user, but the actual app
        // data root must belong to this user before we change its permissions.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("The Brr data directory is not owned by the current user".into());
        }
        std::fs::set_permissions(&canonical_data_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(canonical_data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_runtime_path() -> Result<PathBuf, String> {
        let filename = if cfg!(windows) {
            "brr-runtime.exe"
        } else {
            "brr-runtime"
        };
        let packaged = std::env::current_exe()
            .map_err(|error| error.to_string())?
            .with_file_name(filename);
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../src-tauri/binaries")
            .join(filename);
        resolve_runtime(&packaged, Some(&development))
    }

    fn test_config(data_dir: PathBuf) -> Result<HostConfig, String> {
        Ok(HostConfig {
            data_dir,
            runtime_path: test_runtime_path()?,
        })
    }

    #[test]
    fn runtime_environment_uses_the_host_owned_data_root() {
        let temp = tempfile::tempdir().unwrap();
        let engine_home = temp.path().join("data/engine");
        let service_root = temp.path().join("data");
        let mut command = tokio::process::Command::new("owned-brr-runtime");
        command
            .env(BRR_SERVICE_DATA_DIR_ENV, "/renderer/selected/path")
            .env("CODEX_HOME", "/renderer/selected/home")
            .env("OPENAI_API_KEY", "renderer-secret");
        configure_runtime_environment(&mut command, &engine_home, &service_root);
        let vars = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_os_string()),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            vars[BRR_SERVICE_DATA_DIR_ENV].as_deref(),
            Some(service_root.as_os_str())
        );
        assert_eq!(vars["CODEX_HOME"].as_deref(), Some(engine_home.as_os_str()));
        assert_eq!(vars["OPENAI_API_KEY"], None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stores_and_connection_status_remain_available_during_runtime_handshake() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("hanging-test-runtime");
        std::fs::write(
            &runtime,
            "#!/usr/bin/env python3\nimport time\ntime.sleep(10)\n",
        )
        .unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let host = Arc::new(
            Host::open(
                HostConfig {
                    data_dir: dir.path().join("data"),
                    runtime_path: runtime,
                },
                Arc::new(|_| {}),
            )
            .await
            .unwrap(),
        );
        let connecting = {
            let host = host.clone();
            tokio::spawn(async move { host.connect().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let available =
            tokio::time::timeout(std::time::Duration::from_millis(200), host.is_connected()).await;
        connecting.abort();
        let _ = connecting.await;
        assert_eq!(
            available.ok(),
            Some(false),
            "A slow runtime handshake must not block service status"
        );
        assert!(host
            .project_request("list", serde_json::json!({}))
            .await
            .unwrap()["data"]
            .is_array());
    }

    #[test]
    fn requires_the_owned_runtime_and_prefers_the_packaged_binary() {
        let dir = tempfile::tempdir().unwrap();
        let packaged = dir.path().join("brr-runtime");
        let development = dir.path().join("dev-runtime");
        assert!(resolve_runtime(&packaged, Some(&development))
            .unwrap_err()
            .contains("runtime:build"));
        std::fs::write(&development, b"dev").unwrap();
        assert_eq!(
            resolve_runtime(&packaged, Some(&development)).unwrap(),
            development
        );
        std::fs::write(&packaged, b"packaged").unwrap();
        assert_eq!(
            resolve_runtime(&packaged, Some(&development)).unwrap(),
            packaged
        );
    }

    #[tokio::test]
    #[ignore = "requires the source-built Brr runtime; run pnpm native:smoke"]
    async fn connects_to_owned_runtime_with_isolated_account_and_refuses_inference() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(
            test_config(dir.path().join("data")).unwrap(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        host.connect().await.unwrap();
        let account = host
            .request("account/read", serde_json::json!({"refreshToken":false}))
            .await
            .unwrap();
        assert!(account["account"].is_null());
        let models = host
            .request("model/list", serde_json::json!({"includeHidden":false}))
            .await
            .unwrap();
        assert!(models["data"].is_array());
        let error = host.request("turn/start", serde_json::json!({"threadId":"unauthed", "input":[{"type":"text","text":"Do not execute"}]})).await.unwrap_err();
        assert!(error.contains("Sign in with ChatGPT"));
        host.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires the source-built Brr runtime; validates its real managed-thread protocol"]
    async fn owned_runtime_accepts_agent_tools_and_context_without_relaxing_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(
            test_config(dir.path().join("data")).unwrap(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        host.connect().await.unwrap();
        host.verify_agent_protocol().await.unwrap();
        host.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "explicit live ChatGPT acceptance; BRR_LIVE_AUTH_PATH supplies an existing managed account"]
    async fn live_agent_task_writes_verified_output_and_finishes_its_learning_pass() {
        let Some(auth) = std::env::var_os("BRR_LIVE_AUTH_PATH") else {
            panic!("BRR_LIVE_AUTH_PATH is required")
        };
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let engine_home = data_dir.join("engine");
        std::fs::create_dir_all(&engine_home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&engine_home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::copy(auth, engine_home.join("auth.json")).unwrap();
        let host = Host::open(test_config(data_dir).unwrap(), Arc::new(|_| {}))
            .await
            .unwrap();
        host.connect().await.unwrap();
        let result = host.verify_live_agent_task().await;
        host.shutdown().await;
        result.unwrap();
    }
    #[tokio::test]
    async fn open_initializes_agent_and_project_stores_without_starting_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_path = temp.path().join("runtime-that-does-not-exist");
        let host = Host::open(
            HostConfig {
                data_dir: temp.path().join("data"),
                runtime_path: runtime_path.clone(),
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        assert!(!runtime_path.exists());
        assert!(temp.path().join("data/projects.sqlite").exists());
        assert!(temp.path().join("data/engine/agents").exists());
        assert!(temp
            .path()
            .join("data/engine/agents/chat/chat.sqlite3")
            .exists());
        assert!(host.data_dir().ends_with("data"));
        assert!(!host.is_connected().await);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(temp.path().join("data"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(temp.path().join("data/engine"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
    #[tokio::test]
    async fn attachment_upload_status_works_from_host_store_without_runtime() {
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir().unwrap();
        let host = Host::open(
            HostConfig {
                data_dir: temp.path().join("data"),
                runtime_path: temp.path().join("missing-owned-runtime"),
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        let metadata = AttachmentUploadMetadata {
            owner_id: "thread-owner".into(),
            upload_id: "6ec57d6f-690c-4c88-a45d-d2e66a0c69ec".into(),
            name: "note.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 4,
            sha256: format!("{:x}", Sha256::digest(b"note")),
        };
        let pending = host.attachment_upload_begin(metadata.clone()).await.unwrap();
        assert_eq!(pending.state, AttachmentUploadState::Pending);
        let recovered = host
            .attachment_upload_status(&metadata.owner_id, &metadata.upload_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered, pending);
        assert!(!host.is_connected().await);
        assert!(!temp.path().join("missing-owned-runtime").exists());
    }
    #[tokio::test]
    async fn local_project_requests_work_and_persist_without_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let config = || HostConfig {
            data_dir: data.clone(),
            runtime_path: temp.path().join("missing-owned-runtime"),
        };
        let host = Host::open(config(), Arc::new(|_| {})).await.unwrap();
        let created = host
            .project_request(
                "create",
                serde_json::json!({"name":"Fleet planning","folders":[]}),
            )
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        assert_eq!(
            host.project_request("list", Value::Null).await.unwrap()["data"][0]["id"],
            id
        );
        drop(host);
        let reopened = Host::open(config(), Arc::new(|_| {})).await.unwrap();
        assert_eq!(
            reopened.project_request("list", Value::Null).await.unwrap()["data"][0]["name"],
            "Fleet planning"
        );
        assert!(!reopened.is_connected().await);
    }
    #[tokio::test]
    async fn connect_uses_only_the_configured_owned_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_path = temp.path().join("absent-brr-runtime");
        let host = Host::open(
            HostConfig {
                data_dir: temp.path().join("data"),
                runtime_path: runtime_path.clone(),
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        let error = host.connect().await.unwrap_err();
        assert!(error.contains("Could not start the Codex runtime"));
        assert!(!host.is_connected().await);
        assert!(!runtime_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_a_symlinked_engine_directory_before_store_creation() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let redirected = temp.path().join("redirected-engine-home");
        std::fs::create_dir_all(&redirected).unwrap();
        symlink(&redirected, data_dir.join("engine")).unwrap();

        let error = match Host::open(
            HostConfig {
                data_dir,
                runtime_path: temp.path().join("unused-runtime"),
            },
            Arc::new(|_| {}),
        )
        .await
        {
            Ok(_) => panic!("symlinked engine directory was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("real directory"));
        assert!(!redirected.join("agents").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_a_symlinked_data_root_before_creating_stores() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let redirected = temp.path().join("redirected-data");
        std::fs::create_dir_all(&redirected).unwrap();
        let data_dir = temp.path().join("data");
        symlink(&redirected, &data_dir).unwrap();

        let result = Host::open(
            HostConfig {
                data_dir,
                runtime_path: temp.path().join("unused-runtime"),
            },
            Arc::new(|_| {}),
        )
        .await;

        assert!(result.is_err(), "symlinked data root must be rejected");
        assert!(!redirected.join("projects.sqlite").exists());
        assert!(!redirected.join("engine/agents").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_a_symlinked_data_parent_before_creating_stores() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let redirected = temp.path().join("redirected-parent");
        std::fs::create_dir_all(&redirected).unwrap();
        let linked_parent = temp.path().join("linked-parent");
        symlink(&redirected, &linked_parent).unwrap();
        let data_dir = linked_parent.join("data");

        let result = Host::open(
            HostConfig {
                data_dir,
                runtime_path: temp.path().join("unused-runtime"),
            },
            Arc::new(|_| {}),
        )
        .await;

        assert!(result.is_err(), "symlinked parent must be rejected");
        assert!(!redirected.join("data/projects.sqlite").exists());
        assert!(!redirected.join("data/engine/agents").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_accepts_the_system_tmp_alias_as_a_data_parent() {
        let data_dir = PathBuf::from(format!(
            "/tmp/brr-host-system-tmp-alias-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&data_dir);
        let host = Host::open(
            HostConfig {
                data_dir: data_dir.clone(),
                runtime_path: data_dir.join("missing-runtime"),
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        assert!(host.data_dir().join("projects.sqlite").exists());
        host.shutdown().await;
        std::fs::remove_dir_all(host.data_dir()).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_an_existing_data_root_owned_by_another_user_when_testable() {
        use std::os::unix::fs::MetadataExt;

        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("foreign-data");
        std::fs::create_dir(&data_dir).unwrap();
        let result = unsafe {
            libc::chown(
                std::ffi::CString::new(data_dir.as_os_str().as_encoded_bytes())
                    .unwrap()
                    .as_ptr(),
                65534,
                65534,
            )
        };
        assert_eq!(result, 0);
        assert_ne!(std::fs::metadata(&data_dir).unwrap().uid(), unsafe {
            libc::geteuid()
        });

        let result = Host::open(
            HostConfig {
                data_dir: data_dir.clone(),
                runtime_path: temp.path().join("unused-runtime"),
            },
            Arc::new(|_| {}),
        )
        .await;

        assert!(result.is_err(), "foreign-owned app data must be rejected");
        assert!(!data_dir.join("projects.sqlite").exists());
        assert!(!data_dir.join("engine/agents").exists());
    }
}

#[cfg(test)]
fn resolve_runtime(packaged: &Path, development: Option<&Path>) -> Result<PathBuf, String> {
    if packaged.is_file() {
        return Ok(packaged.to_path_buf());
    }
    if let Some(path) = development.filter(|path| path.is_file()) {
        return Ok(path.to_path_buf());
    }
    Err("Brr's runtime has not been built. Run pnpm runtime:build, then reconnect.".into())
}

#[cfg(test)]
pub(crate) fn runtime_command(home: &Path) -> Result<tokio::process::Command, String> {
    let filename = if cfg!(windows) {
        "brr-runtime.exe"
    } else {
        "brr-runtime"
    };
    let packaged = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name(filename);
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../src-tauri/binaries")
        .join(filename);
    let executable = if packaged.is_file() {
        packaged
    } else if development.is_file() {
        development
    } else {
        return Err(
            "Brr's runtime has not been built. Run pnpm runtime:build, then reconnect.".into(),
        );
    };
    let mut command = tokio::process::Command::new(executable);
    configure_runtime_environment(&mut command, home, home);
    Ok(command)
}
