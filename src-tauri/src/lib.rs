mod agent_host;
mod agent_protocol;
mod agent_workers;
mod bridge;
mod chat_branch;
mod chat_fork;
mod chat_service;
mod chat_state;
mod external_url;
mod generated_ui_sandbox;
mod intelligent_ui;
mod mcp_sandbox;
mod policy;
mod project_archive;
mod project_files;
mod projects;
mod questions;
mod task_workspaces;
mod tickets;
mod workspace_bridge;
mod workspace_browser;
mod workspace_tools;
mod worktrees;

#[cfg(test)]
#[path = "chat_project_reference_tests.rs"]
mod chat_project_reference_tests;

use bridge::Engine;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex;

#[derive(Default)]
struct Runtime(Mutex<Option<Arc<Engine>>>);

#[derive(Default)]
struct TicketStores(
    tokio::sync::Mutex<Option<(brr_agent_core::AgentStore, projects::ProjectStore)>>,
);

impl TicketStores {
    async fn ensure(
        &self,
        app: &tauri::AppHandle,
    ) -> Result<(brr_agent_core::AgentStore, projects::ProjectStore), String> {
        let mut stores = self.0.lock().await;
        if stores.is_none() {
            let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let agent = brr_agent_core::AgentStore::open(app_data.join("engine/agents")).await?;
            let projects = projects::ProjectStore::open(app_data.join("projects.sqlite")).await?;
            *stores = Some((agent, projects));
        }
        Ok(stores.as_ref().unwrap().clone())
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
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
        let runtime = Engine::connect(runtime_command(dir.path()).unwrap(), Arc::new(|_| {}))
            .await
            .unwrap();
        let account = runtime
            .request("account/read", json!({"refreshToken":false}))
            .await
            .unwrap();
        assert!(account["account"].is_null());
        let models = runtime
            .request("model/list", json!({"includeHidden":false}))
            .await
            .unwrap();
        assert!(models["data"].is_array());
        let error = runtime
            .request(
                "turn/start",
                json!({"threadId":"unauthed", "input":[{"type":"text","text":"Do not execute"}]}),
            )
            .await
            .unwrap_err();
        assert!(error.contains("Sign in with ChatGPT"));
        runtime.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires the source-built Brr runtime; validates its real managed-thread protocol"]
    async fn owned_runtime_accepts_agent_tools_and_context_without_relaxing_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Engine::connect(runtime_command(dir.path()).unwrap(), Arc::new(|_| {}))
            .await
            .unwrap();
        runtime.verify_agent_protocol().await.unwrap();
        runtime.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "explicit live ChatGPT acceptance; BRR_LIVE_AUTH_PATH supplies an existing managed account"]
    async fn live_agent_task_writes_verified_output_and_finishes_its_learning_pass() {
        let Some(auth) = std::env::var_os("BRR_LIVE_AUTH_PATH") else {
            panic!("BRR_LIVE_AUTH_PATH is required")
        };
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::copy(auth, dir.path().join("auth.json")).unwrap();
        let runtime = Engine::connect(runtime_command(dir.path()).unwrap(), Arc::new(|_| {}))
            .await
            .unwrap();
        let result = runtime.verify_live_agent_task().await;
        runtime.shutdown().await;
        result.unwrap();
    }
}

fn resolve_runtime(
    packaged: &std::path::Path,
    development: Option<&std::path::Path>,
) -> Result<PathBuf, String> {
    if packaged.is_file() {
        return Ok(packaged.to_path_buf());
    }
    if let Some(path) = development.filter(|path| path.is_file()) {
        return Ok(path.to_path_buf());
    }
    Err("Brr's runtime has not been built. Run pnpm runtime:build, then reconnect.".into())
}

fn runtime_command(home: &std::path::Path) -> Result<tokio::process::Command, String> {
    let filename = if cfg!(windows) {
        "brr-runtime.exe"
    } else {
        "brr-runtime"
    };
    let packaged = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name(filename);
    let development = cfg!(debug_assertions).then(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(filename)
    });
    let executable = resolve_runtime(&packaged, development.as_deref())?;
    let mut command = tokio::process::Command::new(executable);
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
    ] {
        command.env_remove(name);
    }
    command.env("CODEX_HOME", home).current_dir(home);
    Ok(command)
}

#[tauri::command]
async fn engine_connect(
    app: tauri::AppHandle,
    runtime: State<'_, Runtime>,
    stores: State<'_, TicketStores>,
) -> Result<(), String> {
    let mut slot = runtime.0.lock().await;
    if slot.as_ref().is_some_and(|engine| engine.is_alive()) {
        return Ok(());
    }
    let home = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("engine");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let browser_handler = workspace_bridge::browser_handler(app.clone());
    let (agent_store, project_store) = stores.ensure(&app).await?;
    let events = Arc::new(move |message: Value| {
        let _ = app.emit("engine-event", message);
    });
    *slot = Some(Arc::new(
        Engine::connect_with_stores(runtime_command(&home)?, events, agent_store, project_store)
            .await?,
    ));
    if let Some(engine) = slot.as_ref() {
        engine.set_browser_handler(browser_handler);
    }
    Ok(())
}

async fn engine(runtime: &Runtime) -> Result<Arc<Engine>, String> {
    runtime
        .0
        .lock()
        .await
        .clone()
        .ok_or("Connect the coding runtime first".into())
}

#[tauri::command]
async fn engine_request(
    runtime: State<'_, Runtime>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    engine(&runtime).await?.request(&method, params).await
}

#[tauri::command]
async fn chat_request(
    view: tauri::Webview,
    runtime: State<'_, Runtime>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let url = view.url().map_err(|e| e.to_string())?;
    let local = (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
            && url.port() == Some(1420));
    if view.label() != "main" || !local {
        return Err("Chat controls are available only in Brr's main view".into());
    }
    engine(&runtime)
        .await?
        .agents
        .chat_request(&method, params)
        .await
}

#[tauri::command]
async fn intelligent_ui_request(
    view: tauri::Webview,
    runtime: State<'_, Runtime>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let url = view.url().map_err(|e| e.to_string())?;
    let local = (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && matches!(url.scheme(), "http" | "https")
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
            && url.port() == Some(1420));
    if view.label() != "main" || !local {
        return Err("Intelligent UI is available only in the Brr application view".into());
    }
    engine(&runtime)
        .await?
        .intelligent_ui_request(&method, params)
        .await
}

#[tauri::command]
async fn agents_request(
    runtime: State<'_, Runtime>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    engine(&runtime)
        .await?
        .agents_request(&method, params)
        .await
}

#[tauri::command]
async fn tickets_request(
    view: tauri::Webview,
    runtime: State<'_, Runtime>,
    app: tauri::AppHandle,
    stores: State<'_, TicketStores>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let url = view.url().map_err(|e| e.to_string())?;
    let local = (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && matches!(url.scheme(), "http" | "https")
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
            && url.port() == Some(1420));
    if view.label() != "main" || !local {
        return Err("Tickets are available only in the Brr application view".into());
    }
    let (agent_store, project_store) = stores.ensure(&app).await?;
    let connected = runtime.0.lock().await.clone();
    if matches!(method.as_str(), "approveRun") {
        return connected
            .ok_or("Connect the coding runtime first")?
            .tickets_request(&method, params)
            .await;
    }
    let response =
        crate::tickets::request_stores(&agent_store, &project_store, &method, params).await?;
    if matches!(
        method.as_str(),
        "create" | "update" | "move" | "comment" | "proposeRun" | "approveRun" | "rejectRun"
    ) {
        let _ = app.emit(
            "engine-event",
            json!({"method":"brr/tickets/changed","params":{}}),
        );
    }
    Ok(response)
}

#[tauri::command]
async fn archive_project_chats(
    runtime: State<'_, Runtime>,
    project_id: String,
) -> Result<project_archive::ArchiveReport, String> {
    engine(&runtime)
        .await?
        .archive_project_chats(&project_id)
        .await
}

#[tauri::command]
async fn engine_respond(
    runtime: State<'_, Runtime>,
    id: Value,
    result: Value,
) -> Result<(), String> {
    engine(&runtime).await?.respond(id, result).await
}

#[tauri::command]
async fn chatgpt_login(runtime: State<'_, Runtime>) -> Result<Value, String> {
    let engine = engine(&runtime).await?;
    let login = engine
        .request("account/login/start", json!({"type":"chatgpt"}))
        .await?;
    let url = login["authUrl"]
        .as_str()
        .ok_or("The runtime did not return a login URL")?;
    if !policy::validate_login_url(url) {
        return Err("The runtime returned an untrusted login URL".into());
    }
    #[cfg(target_os = "macos")]
    let mut browser = tokio::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut browser = tokio::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut browser = {
        let mut command = tokio::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    let status = browser
        .arg(url)
        .status()
        .await
        .map_err(|e| format!("Could not open your browser: {e}"))?;
    if !status.success() {
        return Err("Could not open your browser. Try signing in again.".into());
    }
    Ok(json!({"loginId": login["loginId"]}))
}

#[tauri::command]
async fn open_external_url(url: String) -> Result<(), String> {
    external_url::validate(&url)?;
    #[cfg(target_os = "macos")]
    let mut browser = tokio::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut browser = tokio::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut browser = {
        let mut command = tokio::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    let status = browser
        .arg(&url)
        .status()
        .await
        .map_err(|error| format!("Could not open your browser: {error}"))?;
    if !status.success() {
        return Err("Could not open your browser".into());
    }
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Runtime::default())
        .manage(TicketStores::default())
        .manage(mcp_sandbox::SandboxManager::default())
        .manage(workspace_tools::WorkspaceTools::default())
        .manage(workspace_browser::BrowserManager::default())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            engine_connect,
            engine_request,
            chat_request,
            intelligent_ui_request,
            workspace_bridge::workspace_request,
            workspace_bridge::workspace_browser_request,
            agents_request,
            tickets_request,
            engine_respond,
            chatgpt_login,
            open_external_url,
            project_files::reveal_project_folder,
            projects::list_projects,
            projects::save_project,
            projects::create_project,
            projects::move_chat_to_project,
            projects::delete_project,
            archive_project_chats,
            mcp_sandbox::create_mcp_sandbox,
            mcp_sandbox::destroy_mcp_sandbox
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Brr")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::RunEvent::Reopen { .. }) {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<workspace_tools::WorkspaceTools>().shutdown();
                app.state::<workspace_browser::BrowserManager>()
                    .shutdown(app);
                let runtime = app.state::<Runtime>();
                tauri::async_runtime::block_on(async {
                    if let Some(engine) = runtime.0.lock().await.take() {
                        engine.shutdown().await;
                    }
                });
            }
        });
}
