mod bridge;
mod policy;
mod worktrees;

use bridge::Engine;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex;

#[derive(Default)]
struct Runtime(Mutex<Option<Arc<Engine>>>);

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
async fn engine_connect(app: tauri::AppHandle, runtime: State<'_, Runtime>) -> Result<(), String> {
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
    let events = Arc::new(move |message: Value| {
        let _ = app.emit("engine-event", message);
    });
    *slot = Some(Arc::new(
        Engine::connect(runtime_command(&home)?, events).await?,
    ));
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Runtime::default())
        .invoke_handler(tauri::generate_handler![
            engine_connect,
            engine_request,
            engine_respond,
            chatgpt_login
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Brr")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                let runtime = app.state::<Runtime>();
                tauri::async_runtime::block_on(async {
                    if let Some(engine) = runtime.0.lock().await.take() {
                        engine.shutdown().await;
                    }
                });
            }
        });
}
