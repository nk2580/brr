use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};
use tauri::{Manager, State, Webview};

pub(crate) type BrowserToolHandler = Arc<
    dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

fn require_main_view(view: &Webview) -> Result<(), String> {
    let url = view.url().map_err(|e| e.to_string())?;
    if !is_main_view(view.label(), &url) {
        return Err("Workspace tools are available only in the Brr application view".into());
    }
    Ok(())
}

fn is_main_view(label: &str, url: &url::Url) -> bool {
    let local = (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && matches!(url.scheme(), "http" | "https")
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
            && url.port() == Some(1420));
    label == "main" && local
}

#[tauri::command]
pub async fn workspace_request(
    view: Webview,
    tools: State<'_, crate::workspace_tools::WorkspaceTools>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    require_main_view(&view)?;
    tools.dispatch(&method, params).await
}

#[tauri::command]
pub async fn workspace_browser_request(
    view: Webview,
    app: tauri::AppHandle,
    browser: State<'_, crate::workspace_browser::BrowserManager>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    require_main_view(&view)?;
    browser.dispatch(&app, &method, params).await
}

pub(crate) fn browser_handler(app: tauri::AppHandle) -> BrowserToolHandler {
    Arc::new(move |thread, args| {
        let app = app.clone();
        Box::pin(async move {
            app.state::<crate::workspace_browser::BrowserManager>()
                .agent_request(&app, &thread, args)
                .await
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_and_child_views_cannot_access_workspace_commands() {
        assert!(is_main_view("main", &"tauri://localhost/".parse().unwrap()));
        assert!(!is_main_view(
            "main",
            &"https://example.com".parse().unwrap()
        ));
        assert!(!is_main_view(
            "workspace-browser-one",
            &"http://tauri.localhost/".parse().unwrap()
        ));
        assert!(!is_main_view(
            "workspace-browser-one",
            &"https://example.com".parse().unwrap()
        ));
    }
}
