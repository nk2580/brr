use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{
    webview::{Webview, WebviewBuilder},
    AppHandle, Emitter, EventTarget, Manager, WebviewUrl,
};
use tokio::sync::oneshot;
use url::Url;

const PAGE_SCRIPT: &str = include_str!("workspace-browser/page.js");
const MAX_TEXT: usize = 24_000;
const MAX_REFS: usize = 200;
const MAX_BROWSER_TABS: usize = 12;

#[derive(Clone)]
struct Tab {
    webview: Webview<tauri::Wry>,
    owner: Option<String>,
    url: String,
}
#[derive(Default)]
struct BrowserState {
    tabs: HashMap<String, Tab>,
    active_thread: Option<String>,
    active_tab: Option<String>,
}

#[derive(Clone, Default)]
pub struct BrowserManager(Arc<Mutex<BrowserState>>);

pub fn validate_url(raw: &str) -> Result<Url, String> {
    if raw == "about:blank" {
        return Url::parse(raw).map_err(|e| e.to_string());
    }
    let parsed =
        Url::parse(raw).map_err(|_| "URL must be absolute and use http or https".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Only http, https, and about:blank URLs are allowed".into());
    }
    Ok(parsed)
}

fn bounded_snapshot(mut value: Value) -> Value {
    if let Some(text) = value.get("text").and_then(Value::as_str).map(str::to_owned) {
        value["text"] = Value::String(text.chars().take(MAX_TEXT).collect());
    }
    if let Some(refs) = value.get_mut("refs").and_then(Value::as_array_mut) {
        refs.truncate(MAX_REFS);
    }
    value
}

impl BrowserManager {
    async fn evaluate(
        &self,
        webview: &Webview<tauri::Wry>,
        expression: String,
    ) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        let tx = Mutex::new(Some(tx));
        webview
            .eval_with_callback(expression, move |result| {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        let raw = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "Browser evaluation timed out".to_string())?
            .map_err(|_| "Browser evaluation was canceled".to_string())?;
        if raw.len() > 2 * 1024 * 1024 {
            return Err("Browser response exceeds the size limit".into());
        }
        let decoded: Value = serde_json::from_str(&raw).map_err(|_| raw.clone())?;
        if let Some(error) = decoded.get("error").and_then(Value::as_str) {
            return Err(error.to_string());
        }
        let value = if let Some(s) = decoded.as_str() {
            serde_json::from_str(s).map_err(|e| e.to_string())?
        } else {
            decoded
        };
        Ok(bounded_snapshot(value))
    }
    fn tab(&self, id: &str) -> Result<Tab, String> {
        self.0
            .lock()
            .unwrap()
            .tabs
            .get(id)
            .cloned()
            .ok_or_else(|| "Browser tab not found".into())
    }
    fn create(
        &self,
        app: &AppHandle,
        url: &str,
        owner: Option<String>,
    ) -> Result<(String, Webview<tauri::Wry>), String> {
        let url = validate_url(url)?;
        if self.0.lock().unwrap().tabs.len() >= MAX_BROWSER_TABS {
            return Err("Close a browser tab before opening another (limit 12)".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let label = format!("workspace-browser-{id}");
        let main = app.get_window("main").ok_or("Main window is unavailable")?;
        let page_state = self.0.clone();
        let page_events = app.clone();
        let page_id = id.clone();
        let app_events = app.clone();
        let popup_state = self.0.clone();
        let source_id = id.clone();
        let builder = WebviewBuilder::new(label, WebviewUrl::External(url.clone()))
            .initialization_script(PAGE_SCRIPT)
            .on_page_load(move |_view, payload| {
                if let Some(tab) = page_state.lock().unwrap().tabs.get_mut(&page_id) {
                    tab.url = payload.url().to_string();
                }
                let _ = page_events.emit_to(EventTarget::webview("main"), "workspace-browser-location", json!({"id":page_id,"url":payload.url().as_str()}));
            })
            .on_navigation(|next| {
                matches!(next.scheme(), "http" | "https") || next.as_str() == "about:blank"
            })
            .on_new_window(move |next, _| {
                let source_owner = {
                    let state = popup_state.lock().unwrap();
                    state.tabs.get(&source_id).filter(|tab| popup_allowed(tab.owner.as_deref(), state.active_thread.as_deref(), state.active_tab.as_deref() == Some(source_id.as_str()))).map(|tab| tab.owner.clone())
                };
                if matches!(next.scheme(), "http" | "https") && source_owner.is_some() {
                    let _ = app_events.emit_to(
                        EventTarget::webview("main"),
                        "workspace-browser-new-tab",
                        json!({"url":next.as_str(),"sourceId":source_id,"threadId":source_owner.flatten()}),
                    );
                }
                tauri::webview::NewWindowResponse::Deny
            });
        let view = main
            .add_child(
                builder,
                tauri::LogicalPosition::new(0., 0.),
                tauri::LogicalSize::new(1., 1.),
            )
            .map_err(|e| e.to_string())?;
        let _ = view.hide();
        self.0.lock().unwrap().tabs.insert(
            id.clone(),
            Tab {
                webview: view.clone(),
                owner,
                url: url.to_string(),
            },
        );
        Ok((id, view))
    }
    pub async fn dispatch(
        &self,
        app: &AppHandle,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match method {
            "browser.create" => {
                let url = params
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("about:blank");
                if let Some(existing_id) = params.get("id").and_then(Value::as_str) {
                    if let Ok(existing) = self.tab(existing_id) {
                        let target = validate_url(url)?;
                        if existing.url != target.as_str() {
                            existing
                                .webview
                                .navigate(target.clone())
                                .map_err(|e| e.to_string())?;
                            if let Some(tab) = self.0.lock().unwrap().tabs.get_mut(existing_id) {
                                tab.url = target.to_string();
                            }
                        }
                        return Ok(json!({"id":existing_id,"url":target,"title":""}));
                    }
                }
                let owner = params
                    .get("ownerThreadId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let (id, _) = self.create(app, url, owner)?;
                Ok(json!({"id":id,"url":url,"title":""}))
            }
            "browser.bounds" => {
                let id = param_str(&params, "id")?;
                let tab = self.tab(id)?;
                let x = num(&params, "x", 0.);
                let y = num(&params, "y", 0.);
                let w = num(&params, "width", 1.).max(1.);
                let h = num(&params, "height", 1.).max(1.);
                tab.webview
                    .set_position(tauri::LogicalPosition::new(x, y))
                    .map_err(|e| e.to_string())?;
                tab.webview
                    .set_size(tauri::LogicalSize::new(w, h))
                    .map_err(|e| e.to_string())?;
                if params
                    .get("visible")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    tab.webview.show().map_err(|e| e.to_string())?;
                } else {
                    tab.webview.hide().map_err(|e| e.to_string())?;
                }
                Ok(
                    json!({"id":id,"x":x,"y":y,"width":w,"height":h,"visible":params["visible"].as_bool().unwrap_or(true)}),
                )
            }
            "browser.navigate" => {
                let id = param_str(&params, "id")?;
                let tab = self.tab(id)?;
                let url = validate_url(param_str(&params, "url")?)?;
                tab.webview
                    .navigate(url.clone())
                    .map_err(|e| e.to_string())?;
                if let Some(tab) = self.0.lock().unwrap().tabs.get_mut(id) {
                    tab.url = url.to_string();
                }
                Ok(json!({"id":id,"url":url}))
            }
            "browser.action" => {
                let id = param_str(&params, "id")?;
                let tab = self.tab(id)?;
                let action = param_str(&params, "action")?;
                match action {
                    "back" => tab.webview.eval("history.back()"),
                    "forward" => tab.webview.eval("history.forward()"),
                    "reload" => tab.webview.eval("location.reload()"),
                    "annotate" => {
                        let enabled = params
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        return self
                            .evaluate(
                                &tab.webview,
                                format!(
                                    "window.__brrBrowser.annotate({enabled}, {})",
                                    params.get("color").unwrap_or(&Value::Null)
                                ),
                            )
                            .await;
                    }
                    _ => return Err("Unsupported browser action".into()),
                }
                .map_err(|e| e.to_string())?;
                Ok(json!({"id":id,"action":action}))
            }
            "browser.location" => {
                let id = param_str(&params, "id")?;
                let tab = self.tab(id)?;
                Ok(json!({"id":id,"url":tab.url}))
            }
            "browser.snapshot" => {
                let id = param_str(&params, "id")?;
                let tab = self.tab(id)?;
                self.snapshot(id, &tab, true).await
            }
            "browser.close" => {
                let id = param_str(&params, "id")?;
                let tab = self
                    .0
                    .lock()
                    .unwrap()
                    .tabs
                    .remove(id)
                    .ok_or("Browser tab not found")?;
                tab.webview.close().map_err(|e| e.to_string())?;
                Ok(json!({"id":id,"closed":true}))
            }
            "browser.context" => {
                let thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let tab_id = params
                    .get("tabId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let mut state = self.0.lock().unwrap();
                state.active_thread = thread.clone();
                state.active_tab = tab_id.clone();
                if let Some(id) = tab_id.as_deref() {
                    if let Some(tab) = state.tabs.get_mut(id) {
                        claim_unowned_tab(
                            &mut tab.owner,
                            params.get("ownerThreadId").and_then(Value::as_str),
                        );
                    }
                }
                // Visibility and bounds are controlled exclusively by browser.bounds.
                Ok(json!({"threadId":thread,"tabId":tab_id}))
            }
            _ => Err("Unsupported browser method".into()),
        }
    }
    pub async fn agent_request(
        &self,
        app: &AppHandle,
        thread: &str,
        args: Value,
    ) -> Result<Value, String> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or("Missing browser action")?;
        let current = self.0.lock().unwrap().active_thread.clone();
        if current.as_deref() != Some(thread) {
            return Err("Browser access is limited to the active chat".into());
        }
        if action == "open" {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("about:blank");
            let (id, _) = self.create(app, url, Some(thread.into()))?;
            let _ = app.emit_to(
                EventTarget::webview("main"),
                "workspace-browser-open",
                json!({"id":id,"url":url,"title":"","threadId":thread}),
            );
            return Ok(json!({"id":id,"url":url}));
        }
        if action == "list" {
            let tabs = self
                .0
                .lock()
                .unwrap()
                .tabs
                .iter()
                .filter(|(_, tab)| tab.owner.as_deref() == Some(thread))
                .map(|(id, tab)| (id.clone(), tab.clone()))
                .collect::<Vec<_>>();
            return Ok(Value::Array(
                tabs.into_iter()
                    .map(|(id, tab)| json!({"id":id,"url":tab.url}))
                    .collect(),
            ));
        }
        let active = self.0.lock().unwrap().active_tab.clone();
        let id = args
            .get("tabId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(active)
            .ok_or("No active browser tab")?;
        let tab = self.tab(&id)?;
        if tab.owner.as_deref() != Some(thread) {
            return Err("Browser tab is not owned by the active chat".into());
        }
        match action {
            "navigate" => {
                let u = validate_url(args["url"].as_str().ok_or("Missing URL")?)?;
                tab.webview.navigate(u.clone()).map_err(|e| e.to_string())?;
                if let Some(tab) = self.0.lock().unwrap().tabs.get_mut(&id) {
                    tab.url = u.to_string();
                }
                Ok(json!({"id":id,"url":u}))
            }
            "snapshot" => self.snapshot(&id, &tab, false).await,
            "click" | "fill" | "press" | "scroll" => {
                let payload = json!({"action":action,"ref":args.get("ref"),"text":args.get("text"),"key":args.get("key"),"deltaY":args.get("deltaY")});
                self.evaluate(
                    &tab.webview,
                    format!("window.__brrBrowser.action({})", payload),
                )
                .await
            }
            _ => Err("Unsupported agent browser action".into()),
        }
    }
    async fn snapshot(&self, id: &str, tab: &Tab, drain: bool) -> Result<Value, String> {
        let js = format!("window.__brrBrowser ? window.__brrBrowser.snapshot({drain}) : ({{url:location.href,title:document.title,text:'',refs:[],ready:false}})");
        let mut value = self.evaluate(&tab.webview, js).await?;
        if !value.is_object() {
            return Err("Browser returned an invalid snapshot".into());
        }
        if let Some(object) = value.as_object_mut() {
            object.insert("id".into(), json!(id));
            object.entry("url").or_insert_with(|| json!(tab.url));
            object.entry("title").or_insert_with(|| json!(""));
            object.entry("ready").or_insert_with(|| json!(false));
        }
        Ok(value)
    }
    pub fn shutdown(&self, _app: &AppHandle) {
        let tabs = std::mem::take(&mut self.0.lock().unwrap().tabs);
        for (_, tab) in tabs {
            let _ = tab.webview.close();
        }
    }
}
fn popup_allowed(owner: Option<&str>, foreground: Option<&str>, source_active: bool) -> bool {
    source_active && owner == foreground
}

fn claim_unowned_tab(owner: &mut Option<String>, thread: Option<&str>) {
    if owner.is_none() {
        *owner = thread.map(str::to_owned);
    }
}

fn param_str<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing {k}"))
}
fn num(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_foreground_owned_page_can_open_a_popup() {
        assert!(popup_allowed(Some("a"), Some("a"), true));
        assert!(popup_allowed(None, None, true));
        assert!(!popup_allowed(Some("a"), Some("b"), true));
        assert!(!popup_allowed(Some("a"), Some("a"), false));
        assert!(!popup_allowed(None, Some("b"), true));
    }
    #[test]
    fn changing_foreground_chat_never_transfers_an_owned_tab() {
        let mut owner = None;
        claim_unowned_tab(&mut owner, None);
        assert_eq!(owner, None);
        claim_unowned_tab(&mut owner, Some("chat-a"));
        claim_unowned_tab(&mut owner, Some("chat-b"));
        claim_unowned_tab(&mut owner, None);
        assert_eq!(owner.as_deref(), Some("chat-a"));
    }
    #[test]
    fn url_policy_only_allows_web_and_blank() {
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("about:blank").is_ok());
        for u in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "tauri://localhost",
        ] {
            assert!(validate_url(u).is_err(), "{u}");
        }
    }
    #[test]
    fn snapshot_bounds_text_and_refs() {
        let v = bounded_snapshot(
            json!({"text":"x".repeat(30_000),"refs":(0..250).collect::<Vec<_>>()}),
        );
        assert_eq!(v["text"].as_str().unwrap().chars().count(), MAX_TEXT);
        assert_eq!(v["refs"].as_array().unwrap().len(), MAX_REFS);
    }
    #[test]
    fn action_payload_round_trips_as_json_data() {
        let payload = json!({"action":"fill","text":"\");window.location='https://bad'//"});
        let encoded = serde_json::to_string(&payload).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), payload);
        assert!(encoded.contains("\\\""));
    }
}
