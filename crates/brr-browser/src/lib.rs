//! Headless, service-owned Chromium driver for per-thread browser tools.
//!
//! The browser is launched as a separate process with an isolated profile and
//! Chromium's inherited-FD DevTools pipe. It does not inspect or control desktop tabs.

use serde_json::{json, Value};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
    time::{timeout, timeout_at, Instant as TokioInstant},
};
use url::Url;

const PAGE_SCRIPT: &str = include_str!("../../../src-tauri/src/workspace-browser/page.js");
const MAX_TEXT: usize = 24_000;
const MAX_REFS: usize = 200;
const MAX_TABS: usize = 12;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const ACTION_UNCERTAIN: &str =
    "Browser action outcome is uncertain; inspect the page before retrying.";

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    pub executable: PathBuf,
    pub profile_dir: PathBuf,
}

#[derive(Clone)]
pub struct BrowserHost {
    config: BrowserConfig,
    state: Arc<Mutex<HostState>>,
}

#[derive(Default)]
struct HostState {
    pipe: Option<CdpPipe>,
    unavailable: Option<String>,
    uncertain_action: bool,
    threads: HashMap<String, ThreadContext>,
    next_id: u64,
}

struct ThreadContext {
    browser_context_id: String,
    tabs: BTreeMap<String, Tab>,
    active_tab: Option<String>,
}

#[derive(Clone)]
struct Tab {
    target_id: String,
    session_id: String,
    url: String,
    title: String,
}

struct CdpPipe {
    child: Child,
    reader: BufReader<File>,
    writer: File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorKind {
    Spawn,
    Protocol,
    Transport,
    Timeout,
}

#[derive(Debug)]
struct DriverError {
    kind: ErrorKind,
    message: String,
}

impl DriverError {
    fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Protocol,
            message: message.into(),
        }
    }
    fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Transport,
            message: message.into(),
        }
    }
    fn lost_transport(&self) -> bool {
        matches!(self.kind, ErrorKind::Transport | ErrorKind::Timeout)
    }
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<io::Error> for DriverError {
    fn from(error: io::Error) -> Self {
        Self::transport(format!("Browser DevTools pipe failed: {error}"))
    }
}

impl BrowserHost {
    pub fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(HostState::default())),
        }
    }

    /// Execute the existing `brr_browser` action envelope for this thread.
    /// Tabs and Chromium browser contexts are isolated by thread ID.
    pub async fn request(&self, thread: &str, args: Value) -> Result<Value, String> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or("Missing browser action")?;
        if !matches!(
            action,
            "list"
                | "open"
                | "navigate"
                | "snapshot"
                | "click"
                | "fill"
                | "press"
                | "scroll"
                | "close"
        ) {
            return Err("Unsupported browser action".into());
        }
        let mutating = matches!(
            action,
            "open" | "navigate" | "click" | "fill" | "press" | "scroll" | "close"
        );
        let mut state = self.state.lock().await;
        if let Some(error) = state.unavailable.as_ref() {
            return Err(error.clone());
        }
        if state.pipe.is_none() {
            match CdpPipe::launch(&self.config).await {
                Ok(pipe) => state.pipe = Some(pipe),
                Err(error) => {
                    let message = error.to_string();
                    state.unavailable = Some(message.clone());
                    return Err(message);
                }
            }
        }
        let result = dispatch(&mut state, thread, &args, action).await;
        match result {
            Ok(value) => Ok(value),
            Err(error) if error.lost_transport() => {
                state.pipe.take();
                if mutating {
                    state.unavailable = Some("Browser action outcome is uncertain; acknowledge it before resetting the browser.".into());
                    state.uncertain_action = true;
                    Err(ACTION_UNCERTAIN.into())
                } else {
                    state.unavailable = Some("Browser process or DevTools pipe was lost; create a new BrowserHost to recover.".into());
                    Err(error.to_string())
                }
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// The owner has inspected the uncertain outcome and chosen to discard the
    /// old browser context. No previous action is retried in the fresh process.
    pub async fn acknowledge_uncertain_and_reset(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if !state.uncertain_action {
            return Err("No uncertain browser action awaits acknowledgement".into());
        }
        if let Some(mut pipe) = state.pipe.take() {
            let _ = pipe.child.start_kill();
            let _ = timeout(Duration::from_secs(3), pipe.child.wait()).await;
        }
        state.threads.clear();
        state.next_id = 0;
        state.unavailable = None;
        state.uncertain_action = false;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        state.threads.clear();
        if let Some(mut pipe) = state.pipe.take() {
            let _ = pipe.child.start_kill();
            let _ = timeout(Duration::from_secs(3), pipe.child.wait()).await;
        }
        state.unavailable = Some("BrowserHost has been shut down.".into());
        state.uncertain_action = false;
    }
}

pub fn validate_url(raw: &str) -> Result<Url, String> {
    if raw == "about:blank" {
        return Url::parse(raw).map_err(|error| error.to_string());
    }
    let parsed =
        Url::parse(raw).map_err(|_| "URL must be absolute and use http or https".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Only http, https, and about:blank URLs are allowed".into());
    }
    Ok(parsed)
}

async fn dispatch(
    state: &mut HostState,
    thread: &str,
    args: &Value,
    action: &str,
) -> Result<Value, DriverError> {
    match action {
        "open" => {
            open_tab(
                state,
                thread,
                args.get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("about:blank"),
            )
            .await
        }
        "list" => list_tabs(state, thread).await,
        "navigate" => {
            navigate(
                state,
                thread,
                select_tab(state, thread, args)?,
                args.get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DriverError::protocol("Missing URL"))?,
            )
            .await
        }
        "snapshot" => snapshot(state, thread, select_tab(state, thread, args)?).await,
        "click" | "fill" | "press" | "scroll" => {
            page_action(state, thread, select_tab(state, thread, args)?, args).await
        }
        "close" => close_tab(state, thread, select_tab(state, thread, args)?).await,
        _ => Err(DriverError::protocol("Unsupported browser action")),
    }
}

async fn open_tab(
    state: &mut HostState,
    thread: &str,
    raw_url: &str,
) -> Result<Value, DriverError> {
    let url = validate_url(raw_url).map_err(DriverError::protocol)?;
    let current_count: usize = state
        .threads
        .values()
        .map(|context| context.tabs.len())
        .sum();
    if current_count >= MAX_TABS {
        return Err(DriverError::protocol(
            "Close a browser tab before opening another (limit 12)",
        ));
    }
    if !state.threads.contains_key(thread) {
        let result = state
            .send(
                "Target.createBrowserContext",
                json!({"disposeOnDetach":true}),
                None,
            )
            .await?;
        let context_id = result
            .get("browserContextId")
            .and_then(Value::as_str)
            .ok_or_else(|| DriverError::protocol("Chromium did not create a browser context"))?
            .to_owned();
        state.threads.insert(
            thread.to_owned(),
            ThreadContext {
                browser_context_id: context_id,
                tabs: BTreeMap::new(),
                active_tab: None,
            },
        );
    }
    let context_id = state
        .threads
        .get(thread)
        .expect("thread inserted")
        .browser_context_id
        .clone();
    let target_result = state
        .send(
            "Target.createTarget",
            json!({"url":"about:blank","browserContextId":context_id}),
            None,
        )
        .await?;
    let target_id = target_result
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| DriverError::protocol("Chromium did not create a page"))?
        .to_owned();
    let tab = initialize_target(state, &target_id).await?;
    let mut tab = tab;
    if url.as_str() != "about:blank" {
        navigate_target(state, &mut tab, url.as_str()).await?;
    }
    let id = target_id.clone();
    let context = state.threads.get_mut(thread).expect("thread exists");
    context.active_tab = Some(id.clone());
    context.tabs.insert(id.clone(), tab.clone());
    Ok(json!({"id":id,"url":tab.url}))
}

async fn initialize_target(state: &mut HostState, target_id: &str) -> Result<Tab, DriverError> {
    let attached = state
        .send(
            "Target.attachToTarget",
            json!({"targetId":target_id,"flatten":true}),
            None,
        )
        .await?;
    let session_id = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| DriverError::protocol("Chromium did not attach the page"))?
        .to_owned();
    state
        .send("Page.enable", json!({}), Some(&session_id))
        .await?;
    state
        .send("Runtime.enable", json!({}), Some(&session_id))
        .await?;
    state
        .send(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":PAGE_SCRIPT}),
            Some(&session_id),
        )
        .await?;
    evaluate(state, &session_id, PAGE_SCRIPT, false).await?;
    let meta = evaluate(
        state,
        &session_id,
        "JSON.stringify({url:location.href,title:document.title})",
        false,
    )
    .await?;
    let parsed: Value =
        serde_json::from_str(meta.as_str().unwrap_or("{}")).unwrap_or_else(|_| json!({}));
    Ok(Tab {
        target_id: target_id.to_owned(),
        session_id,
        url: parsed["url"].as_str().unwrap_or("about:blank").to_owned(),
        title: parsed["title"].as_str().unwrap_or("").to_owned(),
    })
}

async fn navigate(
    state: &mut HostState,
    thread: &str,
    tab_id: String,
    raw_url: &str,
) -> Result<Value, DriverError> {
    let url = validate_url(raw_url).map_err(DriverError::protocol)?;
    let mut tab = state
        .threads
        .get(thread)
        .and_then(|context| context.tabs.get(&tab_id))
        .cloned()
        .ok_or_else(|| DriverError::protocol("Browser tab is not owned by this thread"))?;
    navigate_target(state, &mut tab, url.as_str()).await?;
    let context = state.threads.get_mut(thread).expect("tab owner exists");
    context.active_tab = Some(tab_id.clone());
    context.tabs.insert(tab_id.clone(), tab.clone());
    Ok(json!({"id":tab_id,"url":tab.url}))
}

async fn navigate_target(
    state: &mut HostState,
    tab: &mut Tab,
    url: &str,
) -> Result<(), DriverError> {
    let result = state
        .send("Page.navigate", json!({"url":url}), Some(&tab.session_id))
        .await?;
    if let Some(error) = result.get("errorText").and_then(Value::as_str) {
        return Err(DriverError::protocol(format!(
            "Browser navigation failed: {error}"
        )));
    }
    let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(DriverError {
                kind: ErrorKind::Timeout,
                message: "Browser navigation timed out".into(),
            });
        }
        let state_value = evaluate(state, &tab.session_id, "document.readyState", false).await?;
        if state_value
            .as_str()
            .is_some_and(|ready| ready == "complete" || ready == "interactive")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let meta = evaluate(
        state,
        &tab.session_id,
        "JSON.stringify({url:location.href,title:document.title})",
        false,
    )
    .await?;
    let parsed: Value =
        serde_json::from_str(meta.as_str().unwrap_or("{}")).unwrap_or_else(|_| json!({}));
    tab.url = parsed["url"].as_str().unwrap_or(url).to_owned();
    tab.title = parsed["title"].as_str().unwrap_or("").to_owned();
    Ok(())
}

async fn list_tabs(state: &mut HostState, thread: &str) -> Result<Value, DriverError> {
    let Some(context_id) = state
        .threads
        .get(thread)
        .map(|context| context.browser_context_id.clone())
    else {
        return Ok(Value::Array(Vec::new()));
    };
    let targets = state.send("Target.getTargets", json!({}), None).await?;
    let infos = targets
        .get("targetInfos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for info in infos {
        if info.get("type").and_then(Value::as_str) != Some("page")
            || info.get("browserContextId").and_then(Value::as_str) != Some(context_id.as_str())
        {
            continue;
        }
        let Some(target_id) = info.get("targetId").and_then(Value::as_str) else {
            continue;
        };
        if state
            .threads
            .get(thread)
            .is_some_and(|context| context.tabs.contains_key(target_id))
        {
            continue;
        }
        if state
            .threads
            .values()
            .map(|context| context.tabs.len())
            .sum::<usize>()
            >= MAX_TABS
        {
            break;
        }
        let Ok(mut tab) = initialize_target(state, target_id).await else {
            continue;
        };
        tab.url = info
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(&tab.url)
            .to_owned();
        tab.title = info
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&tab.title)
            .to_owned();
        state
            .threads
            .get_mut(thread)
            .expect("thread exists")
            .tabs
            .insert(target_id.to_owned(), tab);
    }
    let context = state.threads.get(thread).expect("thread exists");
    let mut result = context
        .tabs
        .iter()
        .map(|(id, tab)| json!({"id":id,"url":tab.url,"title":tab.title}))
        .collect::<Vec<_>>();
    result.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    Ok(Value::Array(result))
}

async fn snapshot(
    state: &mut HostState,
    thread: &str,
    tab_id: String,
) -> Result<Value, DriverError> {
    let tab = state
        .threads
        .get(thread)
        .and_then(|context| context.tabs.get(&tab_id))
        .cloned()
        .ok_or_else(|| DriverError::protocol("Browser tab is not owned by this thread"))?;
    let mut value = evaluate_json(state, &tab.session_id, "window.__brrBrowser ? window.__brrBrowser.snapshot(false) : ({url:location.href,title:document.title,text:'',refs:[],ready:false})", false).await?;
    bound_snapshot(&mut value);
    if let Some(object) = value.as_object_mut() {
        object.insert("id".into(), json!(tab_id));
    }
    Ok(value)
}

async fn page_action(
    state: &mut HostState,
    thread: &str,
    tab_id: String,
    args: &Value,
) -> Result<Value, DriverError> {
    let tab = state
        .threads
        .get(thread)
        .and_then(|context| context.tabs.get(&tab_id))
        .cloned()
        .ok_or_else(|| DriverError::protocol("Browser tab is not owned by this thread"))?;
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    let payload = json!({"action":action,"ref":args.get("ref"),"text":args.get("text"),"key":args.get("key"),"deltaY":args.get("deltaY")});
    let payload = serde_json::to_string(&payload)
        .map_err(|error| DriverError::protocol(error.to_string()))?;
    let mut value = evaluate_json(
        state,
        &tab.session_id,
        &format!("window.__brrBrowser.action({payload})"),
        true,
    )
    .await?;
    bound_snapshot(&mut value);
    if let Some(object) = value.as_object_mut() {
        object.insert("id".into(), json!(tab_id));
    }
    Ok(value)
}

async fn close_tab(
    state: &mut HostState,
    thread: &str,
    tab_id: String,
) -> Result<Value, DriverError> {
    let target_id = state
        .threads
        .get(thread)
        .and_then(|context| context.tabs.get(&tab_id))
        .map(|tab| tab.target_id.clone())
        .ok_or_else(|| DriverError::protocol("Browser tab is not owned by this thread"))?;
    state
        .send("Target.closeTarget", json!({"targetId":target_id}), None)
        .await?;
    let context = state.threads.get_mut(thread).expect("tab owner exists");
    context.tabs.remove(&tab_id);
    if context.active_tab.as_deref() == Some(tab_id.as_str()) {
        context.active_tab = context.tabs.keys().next().cloned();
    }
    if context.tabs.is_empty() {
        let browser_context_id = context.browser_context_id.clone();
        state
            .send(
                "Target.disposeBrowserContext",
                json!({"browserContextId":browser_context_id}),
                None,
            )
            .await?;
        state.threads.remove(thread);
    }
    Ok(json!({"id":tab_id,"closed":true}))
}

fn select_tab(state: &HostState, thread: &str, args: &Value) -> Result<String, DriverError> {
    let context = state
        .threads
        .get(thread)
        .ok_or_else(|| DriverError::protocol("No browser tab is open for this thread"))?;
    let id = args
        .get("tabId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| context.active_tab.clone())
        .ok_or_else(|| DriverError::protocol("No active browser tab"))?;
    if !context.tabs.contains_key(&id) {
        return Err(DriverError::protocol(
            "Browser tab is not owned by this thread",
        ));
    }
    Ok(id)
}

async fn evaluate(
    state: &mut HostState,
    session: &str,
    expression: &str,
    user_gesture: bool,
) -> Result<Value, DriverError> {
    let result = state
        .send(
            "Runtime.evaluate",
            json!({"expression":expression,"returnByValue":true,"awaitPromise":true,"userGesture":user_gesture}),
            Some(session),
        )
        .await?;
    if let Some(exception) = result.get("exceptionDetails") {
        let text = exception
            .pointer("/exception/description")
            .and_then(Value::as_str)
            .unwrap_or("Browser JavaScript evaluation failed");
        return Err(DriverError::protocol(text.to_owned()));
    }
    Ok(result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null))
}

async fn evaluate_json(
    state: &mut HostState,
    session: &str,
    expression: &str,
    user_gesture: bool,
) -> Result<Value, DriverError> {
    let value = evaluate(state, session, expression, user_gesture).await?;
    if value.is_string() {
        serde_json::from_str(value.as_str().unwrap_or(""))
            .map_err(|_| DriverError::protocol("Browser returned an invalid snapshot"))
    } else {
        Ok(value)
    }
}

fn bound_snapshot(value: &mut Value) {
    if let Some(text) = value.get("text").and_then(Value::as_str).map(str::to_owned) {
        value["text"] = Value::String(text.chars().take(MAX_TEXT).collect());
    }
    if let Some(refs) = value.get_mut("refs").and_then(Value::as_array_mut) {
        refs.truncate(MAX_REFS);
    }
}

impl HostState {
    async fn send(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, DriverError> {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.pipe
            .as_mut()
            .ok_or_else(|| DriverError::transport("Browser process is unavailable"))?
            .send(id, method, params, session)
            .await
    }
}

impl CdpPipe {
    async fn launch(config: &BrowserConfig) -> Result<Self, DriverError> {
        prepare_profile(&config.profile_dir)?;
        #[cfg(unix)]
        let (child_input, host_output, host_input, child_output) = {
            let (input_read, input_write) = pipe_pair().map_err(|error| DriverError {
                kind: ErrorKind::Spawn,
                message: format!("Could not create browser DevTools pipe: {error}"),
            })?;
            let (output_read, output_write) = pipe_pair().map_err(|error| DriverError {
                kind: ErrorKind::Spawn,
                message: format!("Could not create browser DevTools pipe: {error}"),
            })?;
            (input_read, input_write, output_read, output_write)
        };
        let mut command = Command::new(&config.executable);
        command
            .arg("--headless=new")
            .arg("--remote-debugging-pipe")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--user-data-dir={}", config.profile_dir.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let child_read = child_input.as_raw_fd();
            let child_write = child_output.as_raw_fd();
            unsafe {
                command.as_std_mut().pre_exec(move || {
                    let safe_read = libc::fcntl(child_read, libc::F_DUPFD_CLOEXEC, 10);
                    if safe_read < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let safe_write = libc::fcntl(child_write, libc::F_DUPFD_CLOEXEC, 10);
                    if safe_write < 0 {
                        libc::close(safe_read);
                        return Err(io::Error::last_os_error());
                    }
                    if libc::dup2(safe_read, 3) < 0 || libc::dup2(safe_write, 4) < 0 {
                        let error = io::Error::last_os_error();
                        libc::close(safe_read);
                        libc::close(safe_write);
                        return Err(error);
                    }
                    libc::close(safe_read);
                    libc::close(safe_write);
                    Ok(())
                });
            }
        }
        let child = command.spawn().map_err(|error| DriverError {
            kind: ErrorKind::Spawn,
            message: format!("Could not launch configured browser executable: {error}"),
        })?;
        #[cfg(unix)]
        {
            drop(child_input);
            drop(child_output);
            let reader =
                unsafe { File::from_std(std::fs::File::from_raw_fd(host_input.into_raw_fd())) };
            let writer =
                unsafe { File::from_std(std::fs::File::from_raw_fd(host_output.into_raw_fd())) };
            let mut pipe = Self {
                child,
                reader: BufReader::new(reader),
                writer,
            };
            let deadline = TokioInstant::now() + STARTUP_TIMEOUT;
            timeout_at(
                deadline,
                pipe.send_inner(0, "Browser.getVersion", json!({}), None, None),
            )
            .await
            .map_err(|_| DriverError {
                kind: ErrorKind::Timeout,
                message: "Browser startup version handshake timed out".into(),
            })??;
            Ok(pipe)
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Err(DriverError {
                kind: ErrorKind::Spawn,
                message: "Independent browser pipe is supported on macOS and Linux only".into(),
            })
        }
    }

    async fn send(
        &mut self,
        id: u64,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, DriverError> {
        self.send_inner(id, method, params, session, Some(COMMAND_TIMEOUT))
            .await
    }

    async fn send_inner(
        &mut self,
        id: u64,
        method: &str,
        params: Value,
        session: Option<&str>,
        per_read_timeout: Option<Duration>,
    ) -> Result<Value, DriverError> {
        if self.child.try_wait().map_err(DriverError::from)?.is_some() {
            return Err(DriverError::transport("Browser process exited"));
        }
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|error| DriverError::protocol(error.to_string()))?;
        bytes.push(0);
        self.writer
            .write_all(&bytes)
            .await
            .map_err(DriverError::from)?;
        self.writer.flush().await.map_err(DriverError::from)?;
        loop {
            let mut frame = Vec::new();
            let read = match per_read_timeout {
                Some(duration) => timeout(duration, self.reader.read_until(0, &mut frame))
                    .await
                    .map_err(|_| DriverError {
                        kind: ErrorKind::Timeout,
                        message: "Browser DevTools response timed out".into(),
                    })?
                    .map_err(DriverError::from)?,
                None => self
                    .reader
                    .read_until(0, &mut frame)
                    .await
                    .map_err(DriverError::from)?,
            };
            if read == 0 {
                return Err(DriverError::transport("Browser DevTools pipe closed"));
            }
            frame.pop();
            let response: Value = serde_json::from_slice(&frame)
                .map_err(|_| DriverError::transport("Browser returned invalid DevTools data"))?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(DriverError::protocol(
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Browser command failed")
                        .to_owned(),
                ));
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

fn prepare_profile(path: &std::path::Path) -> Result<(), DriverError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent).map_err(profile_error)?;
    std::fs::create_dir_all(path).map_err(profile_error)?;
    let parent_meta = std::fs::symlink_metadata(parent).map_err(profile_error)?;
    let profile_meta = std::fs::symlink_metadata(path).map_err(profile_error)?;
    if parent_meta.file_type().is_symlink()
        || !parent_meta.is_dir()
        || profile_meta.file_type().is_symlink()
        || !profile_meta.is_dir()
    {
        return Err(DriverError {
            kind: ErrorKind::Spawn,
            message: "Browser profile path must use owned, non-symlink directories".into(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let uid = unsafe { libc::geteuid() };
        if parent_meta.uid() != uid || profile_meta.uid() != uid {
            return Err(DriverError {
                kind: ErrorKind::Spawn,
                message: "Browser profile directory must be owned by this service user".into(),
            });
        }
        let mut parent_permissions = parent_meta.permissions();
        parent_permissions.set_mode(0o700);
        std::fs::set_permissions(parent, parent_permissions).map_err(profile_error)?;
        let mut profile_permissions = profile_meta.permissions();
        profile_permissions.set_mode(0o700);
        std::fs::set_permissions(path, profile_permissions).map_err(profile_error)?;
    }
    let marker = path.join(".brr-managed-profile");
    if marker.exists() {
        let metadata = std::fs::symlink_metadata(&marker).map_err(profile_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(DriverError {
                kind: ErrorKind::Spawn,
                message: "Browser profile marker is invalid".into(),
            });
        }
    } else {
        let mut entries = std::fs::read_dir(path).map_err(profile_error)?;
        if entries.next().is_some() {
            return Err(DriverError {
                kind: ErrorKind::Spawn,
                message: "Refusing to use a non-empty profile not created for the service browser"
                    .into(),
            });
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        options
            .open(marker)
            .and_then(|mut file| file.write_all(b"brr-managed-browser-profile-v1\n"))
            .map_err(profile_error)?;
    }
    Ok(())
}

fn profile_error(error: io::Error) -> DriverError {
    DriverError {
        kind: ErrorKind::Spawn,
        message: format!("Could not prepare isolated browser profile: {error}"),
    }
}

#[cfg(unix)]
fn pipe_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(error);
        }
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    #[test]
    fn url_policy_allows_only_web_and_blank() {
        assert_eq!(
            validate_url("https://example.com").unwrap().as_str(),
            "https://example.com/"
        );
        assert!(validate_url("about:blank").is_ok());
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "tauri://localhost",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn profile_preparation_refuses_unmanaged_and_symlink_profiles() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let unmanaged = dir.path().join("unmanaged");
        std::fs::create_dir(&unmanaged).unwrap();
        std::fs::write(unmanaged.join("Cookies"), b"existing profile data").unwrap();
        assert!(prepare_profile(&unmanaged).is_err());

        #[cfg(unix)]
        {
            let target = dir.path().join("target");
            std::fs::create_dir(&target).unwrap();
            let link = dir.path().join("linked-profile");
            symlink(&target, &link).unwrap();
            assert!(prepare_profile(&link).is_err());
        }
    }

    #[tokio::test]
    async fn missing_browser_reports_start_error_without_faking_a_browser() {
        let dir = tempfile::tempdir().unwrap();
        let host = BrowserHost::new(BrowserConfig {
            executable: dir.path().join("missing-browser"),
            profile_dir: dir.path().join("profile"),
        });
        let error = host
            .request("thread-a", json!({"action":"open","url":"about:blank"}))
            .await
            .unwrap_err();
        assert!(error.contains("configured browser executable"));
        host.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn browser_startup_accepts_version_response_after_ten_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let executable = fake_cdp_browser(&dir, 10_200, 0);
        let diagnostic = dir.path().join("fake-cdp-browser.log");
        let profile = dir.path().join("profile");
        let result = CdpPipe::launch(&BrowserConfig {
            executable,
            profile_dir: profile,
        })
        .await;
        assert!(
            result.is_ok(),
            "startup version handshake failed: {}",
            result
                .as_ref()
                .err()
                .map(|error| {
                    format!(
                        "{error}; fake browser: {}",
                        std::fs::read_to_string(diagnostic).unwrap_or_default()
                    )
                })
                .unwrap_or_default()
        );
        drop(result);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ordinary_browser_command_keeps_ten_second_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let executable = fake_cdp_browser(&dir, 0, 10_200);
        let mut pipe = CdpPipe::launch(&BrowserConfig {
            executable,
            profile_dir: dir.path().join("profile"),
        })
        .await
        .unwrap();

        let error = pipe
            .send(1, "Test.slow", json!({}), None)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Timeout);
        assert_eq!(error.to_string(), "Browser DevTools response timed out");
    }

    #[cfg(unix)]
    fn fake_cdp_browser(
        dir: &tempfile::TempDir,
        startup_delay_ms: u64,
        command_delay_ms: u64,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let executable = dir.path().join("fake-cdp-browser");
        let python_script = dir.path().join("fake-cdp-browser.py");
        let diagnostic = dir.path().join("fake-cdp-browser.log");
        let script = format!(
            concat!(
                "#!/usr/bin/env python3\n",
                "import json, os, time\n",
                "buf = bytearray()\n",
                "while True:\n",
                "    chunk = os.read(3, 1)\n",
                "    if not chunk:\n",
                "        break\n",
                "    if chunk != b'\\0':\n",
                "        buf.extend(chunk)\n",
                "        continue\n",
                "    request = json.loads(buf)\n",
                "    buf.clear()\n",
                "    if request['method'] == 'Browser.getVersion':\n",
                "        time.sleep({startup_delay_ms} / 1000)\n",
                "    elif request['method'] == 'Test.slow':\n",
                "        time.sleep({command_delay_ms} / 1000)\n",
                "    response = {{'id': request['id'], 'result': {{'product': 'FakeChrome/1.0'}}}}\n",
                "    os.write(4, json.dumps(response).encode() + b'\\0')\n"
            ),
            startup_delay_ms = startup_delay_ms,
            command_delay_ms = command_delay_ms
        );
        std::fs::write(&python_script, script).unwrap();
        let launcher = format!(
            "#!/bin/sh\nexec /usr/bin/env python3 '{}' 2>'{}'\n",
            python_script.display(),
            diagnostic.display()
        );
        std::fs::write(&executable, launcher).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        executable
    }

    #[tokio::test]
    async fn real_chromium_supports_owned_tabs_forms_navigation_and_popups() {
        let Some(executable) = installed_browser() else {
            eprintln!(
                "Skipping real browser smoke: set BRR_BROWSER_EXECUTABLE to a Chromium binary"
            );
            return;
        };
        let (base_url, _server) = fixture_server(None).await;
        let profile = tempfile::tempdir().unwrap();
        let host = BrowserHost::new(BrowserConfig {
            executable,
            profile_dir: profile.path().join("service-profile"),
        });
        let opened = host
            .request("chat-one", json!({"action":"open","url":base_url.clone()}))
            .await
            .unwrap();
        let tab_id = opened["id"].as_str().unwrap().to_owned();
        let first = host
            .request("chat-one", json!({"action":"snapshot"}))
            .await
            .unwrap();
        let refs = first["refs"].as_array().unwrap();
        let input_ref = refs.iter().find(|r| r["label"] == "Search").unwrap()["id"].clone();
        let submit_ref = refs.iter().find(|r| r["label"] == "Submit").unwrap()["id"].clone();
        host.request(
            "chat-one",
            json!({"action":"fill","ref":input_ref,"text":"Ada"}),
        )
        .await
        .unwrap();
        host.request("chat-one", json!({"action":"click","ref":submit_ref}))
            .await
            .unwrap();
        let submitted = host
            .request("chat-one", json!({"action":"snapshot"}))
            .await
            .unwrap();
        assert!(submitted["text"].as_str().unwrap().contains("Hello Ada"));

        let popup_ref = submitted["refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == "Popup")
            .unwrap()["id"]
            .clone();
        host.request("chat-one", json!({"action":"click","ref":popup_ref}))
            .await
            .unwrap();
        let mut tabs = Value::Array(Vec::new());
        for _ in 0..30 {
            tabs = host
                .request("chat-one", json!({"action":"list"}))
                .await
                .unwrap();
            if tabs.as_array().is_some_and(|tabs| tabs.len() > 1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(tabs
            .as_array()
            .unwrap()
            .iter()
            .any(|tab| tab["url"].as_str().unwrap_or("").ends_with("/popup")));
        assert!(host
            .request("chat-two", json!({"action":"snapshot","tabId":tab_id}))
            .await
            .is_err());

        host.request(
            "chat-one",
            json!({"action":"navigate","tabId":tab_id,"url":format!("{base_url}next")}),
        )
        .await
        .unwrap();
        let destination = host
            .request("chat-one", json!({"action":"snapshot","tabId":tab_id}))
            .await
            .unwrap();
        assert!(destination["text"]
            .as_str()
            .unwrap()
            .contains("Destination Page"));
        host.shutdown().await;
    }

    #[tokio::test]
    async fn lost_browser_response_after_real_page_action_is_uncertain() {
        let Some(executable) = installed_browser() else {
            eprintln!(
                "Skipping action uncertainty smoke: set BRR_BROWSER_EXECUTABLE to a Chromium binary"
            );
            return;
        };
        let (observed, observed_url) = oneshot::channel();
        let observed = Arc::new(Mutex::new(Some(observed)));
        let (base_url, _server) = fixture_server(Some(observed)).await;
        let profile = tempfile::tempdir().unwrap();
        let host = BrowserHost::new(BrowserConfig {
            executable,
            profile_dir: profile.path().join("service-profile"),
        });
        let opened = host
            .request("chat-hang", json!({"action":"open","url":base_url.clone()}))
            .await
            .unwrap();
        let button = host
            .request("chat-hang", json!({"action":"snapshot"}))
            .await
            .unwrap()["refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == "Hang")
            .unwrap()["id"]
            .clone();
        let pid = host
            .state
            .lock()
            .await
            .pipe
            .as_ref()
            .unwrap()
            .child
            .id()
            .unwrap() as libc::pid_t;
        let action_host = host.clone();
        let tab_id = opened["id"].clone();
        let action = tokio::spawn(async move {
            action_host
                .request(
                    "chat-hang",
                    json!({"action":"click","tabId":tab_id,"ref":button}),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), observed_url)
            .await
            .unwrap()
            .unwrap();
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let error = tokio::time::timeout(Duration::from_secs(5), action)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error, ACTION_UNCERTAIN);
        assert!(host
            .request("chat-hang", json!({"action":"list"}))
            .await
            .is_err());
        host.acknowledge_uncertain_and_reset().await.unwrap();
        assert_eq!(
            host.request("chat-hang", json!({"action":"list"}))
                .await
                .unwrap(),
            json!([]),
            "the previous tab and uncertain click must not be replayed"
        );
        let fresh = host
            .request("chat-hang", json!({"action":"open","url":"about:blank"}))
            .await
            .unwrap();
        assert_eq!(fresh["url"], "about:blank");
        host.shutdown().await;
    }

    fn installed_browser() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("BRR_BROWSER_EXECUTABLE") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        [
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/google-chrome",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
    }

    async fn fixture_server(
        observed: Option<Arc<Mutex<Option<oneshot::Sender<String>>>>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let observed = observed.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let size = stream.read(&mut request).await.unwrap_or(0);
                    let first = String::from_utf8_lossy(&request[..size]);
                    let path = first
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if path.starts_with("/observed") {
                        if let Some(sender) = observed
                            .as_ref()
                            .and_then(|slot| slot.try_lock().ok()?.take())
                        {
                            let _ = sender.send(path.to_owned());
                        }
                    }
                    let (content_type, body) = if path.starts_with("/next") {
                        (
                            "text/html",
                            "<title>Destination</title><h1>Destination Page</h1>",
                        )
                    } else if path.starts_with("/popup") {
                        ("text/html", "<title>Popup</title><h1>Popup Page</h1>")
                    } else {
                        (
                            "text/html",
                            "<title>Fixture</title><input placeholder='Search'><button id='submit' onclick=\"document.querySelector('#result').textContent='Hello '+document.querySelector('input').value\">Submit</button><a href='/popup' target='_blank'>Popup</a><button onclick=\"fetch('/observed');while(true){}\">Hang</button><p id='result'></p>",
                        )
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{address}/"), handle)
    }
}
