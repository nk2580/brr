//! Dedicated deny-by-default transport for self-contained Intelligent UI apps.
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{oneshot, Mutex},
    time::{timeout, Duration},
};

const MAX_HTML: usize = 5 * 1024 * 1024;
const MAX_REQUEST: usize = 8192;
const ORIGINS: &[&str] = &[
    "http://127.0.0.1:1420",
    "http://localhost:1420",
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

#[derive(Default)]
pub(crate) struct SandboxManager {
    active: Mutex<HashMap<String, oneshot::Sender<()>>>,
}
impl SandboxManager {
    async fn destroy(&self, token: &str) {
        if let Some(stop) = self.active.lock().await.remove(token) {
            let _ = stop.send(());
        }
    }
}
pub(crate) async fn create_for_document(
    engine: &crate::bridge::Engine,
    p: &Value,
) -> Result<Value, String> {
    let thread = required(p, "threadId")?;
    let call = required(p, "callId")?;
    let hash = required(p, "documentHash")?;
    let host = required(p, "hostOrigin")?;
    if !ORIGINS.contains(&host) {
        return Err("Unsupported Brr host origin".into());
    }
    let resolved =
        crate::intelligent_ui::read_document(engine, &json!({"threadId":thread,"callId":call}))
            .await?;
    if resolved["documentHash"].as_str() != Some(hash) {
        return Err("Document identity is stale".into());
    }
    if resolved["document"]["kind"] != "app" {
        return Err("Only finalized app documents can be sandboxed".into());
    }
    let html = required(&resolved["document"], "html")?;
    if html.trim().is_empty() || html.len() > MAX_HTML {
        return Err("App HTML must be nonempty and no larger than 5 MB".into());
    }
    let state = crate::intelligent_ui::read_state(
        engine,
        &json!({"threadId":thread,"callId":call,"documentHash":hash}),
    )
    .await?["state"]
        .clone();
    let token = random_token(32)?;
    let nonce = random_token(18)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let url = format!("http://127.0.0.1:{port}/ui/{token}");
    let view_path = format!("/ui/{token}/view");
    let page = proxy_page(&nonce, host, &token, &view_path)?;
    let view = view_page(&token, &html, &state)?;
    let proxy_csp = format!("default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; frame-src 'self'; connect-src 'none'; img-src 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors {host}");
    let view_csp = format!("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; frame-src 'none'; navigate-to 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors 'self' {host}");
    let (stop, stopped) = oneshot::channel();
    tokio::spawn(async move {
        serve(
            listener,
            token.clone(),
            page,
            proxy_csp,
            view,
            view_csp,
            stopped,
        )
        .await;
    });
    engine
        .ui_sandboxes
        .active
        .lock()
        .await
        .insert(url_token(&url), stop);
    Ok(json!({"url":url,"token":url_token(&url)}))
}
pub(crate) async fn destroy_for_token(
    engine: &crate::bridge::Engine,
    p: &Value,
) -> Result<Value, String> {
    let token = required(p, "token")?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid Intelligent UI sandbox token".into());
    }
    engine.ui_sandboxes.destroy(token).await;
    Ok(Value::Null)
}
fn url_token(url: &str) -> String {
    url.rsplit('/').next().unwrap_or_default().to_owned()
}
fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{k} is required"))
}
fn random_token(n: usize) -> Result<String, String> {
    let mut b = vec![0; n];
    getrandom::fill(&mut b).map_err(|_| "Unable to create Intelligent UI capability")?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}
fn proxy_page(nonce: &str, host: &str, token: &str, view_path: &str) -> Result<String, String> {
    let host = js_json(host)?;
    let token = js_json(token)?;
    let view_path = js_json(view_path)?;
    Ok(format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>html,body,iframe{{width:100%;height:100%;margin:0;border:0}}iframe{{display:block}}</style><iframe id="app" title="Interactive app" sandbox="allow-scripts" src={view_path}></iframe><script nonce="{nonce}">
const HOST={host},TOKEN={token},f=document.getElementById('app');
window.addEventListener('message',e=>{{if(e.source!==f.contentWindow||e.origin!=='null'||e.data?.channel!=='brr-ui'||e.data?.token!==TOKEN)return;parent.postMessage(e.data,HOST)}});
        window.addEventListener('message',e=>{{if(e.source!==parent||e.origin!==HOST||e.data?.token!==TOKEN)return;if(e.data?.channel==='brr-ui-result'||e.data?.channel==='brr-ui-state')f.contentWindow.postMessage(e.data,'*')}});
</script>"#
    ))
}

fn view_page(token: &str, html: &str, state: &Value) -> Result<String, String> {
    let token = js_json(token)?;
    let state = js_json(&state.to_string())?;
    // This is a separate HTTP document, so its strict deny-network policy does not
    // inherit the outer proxy's nonce-only policy. The opaque frame starts this
    // helper before parsing any model-authored app script.
    Ok(format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><script>
(()=>{{const TOKEN={token};let state=JSON.parse({state});let seq=0;const pending=new Map();const send=(method,args)=>new Promise((resolve,reject)=>{{const id=String(++seq);pending.set(id,{{resolve,reject}});parent.postMessage({{channel:'brr-ui',token:TOKEN,id,method,args}},'*')}});window.addEventListener('message',e=>{{if(e.source!==parent||e.data?.token!==TOKEN)return;if(e.data?.channel==='brr-ui-state'){{state=e.data.state;window.dispatchEvent(new CustomEvent('brr-ui-state',{{detail:{{state,revision:e.data.revision}}}}));return}}if(e.data?.channel!=='brr-ui-result')return;const p=pending.get(e.data.id);if(!p)return;pending.delete(e.data.id);e.data.ok?p.resolve(e.data.value):p.reject(new Error(e.data.error||'Request failed'))}});Object.defineProperty(window,'brrUI',{{value:Object.freeze({{getState:()=>JSON.parse(JSON.stringify(state)),setState:async v=>{{const saved=await send('setState',v);state=v;return saved}},requestAction:id=>send('requestAction',id),resize:h=>send('resize',h)}}),writable:false,configurable:false}})}})();
</script>{html}"#
    ))
}

fn js_json(v: &str) -> Result<String, String> {
    let s = serde_json::to_string(v).map_err(|e| e.to_string())?;
    Ok(s.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026"))
}
async fn serve(
    listener: TcpListener,
    token: String,
    proxy: String,
    proxy_csp: String,
    view: String,
    view_csp: String,
    mut stopped: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => {
                let Ok((mut stream, _)) = accepted else { continue };
                let (token, proxy, proxy_csp, view, view_csp) = (token.clone(), proxy.clone(), proxy_csp.clone(), view.clone(), view_csp.clone());
                tokio::spawn(async move { let _ = respond(&mut stream, &token, &proxy, &proxy_csp, &view, &view_csp).await; });
            }
        }
    }
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    token: &str,
    proxy: &str,
    proxy_csp: &str,
    view: &str,
    view_csp: &str,
) -> Result<(), String> {
    let mut buffer = vec![0; MAX_REQUEST + 1];
    let read = timeout(Duration::from_secs(2), async {
        let mut total = 0;
        while total < buffer.len() {
            let count = stream.read(&mut buffer[total..]).await?;
            if count == 0 {
                break;
            }
            total += count;
            if total >= 4 && buffer[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        std::io::Result::Ok(total)
    })
    .await
    .map_err(|_| "Sandbox request timed out")?
    .map_err(|e| e.to_string())?;
    if read == 0 || read > MAX_REQUEST {
        return response(
            stream,
            431,
            "text/plain",
            "default-src 'none'",
            "Request too large",
        )
        .await;
    }
    let header = std::str::from_utf8(&buffer[..read]).map_err(|_| "Invalid request")?;
    let mut parts = header
        .lines()
        .next()
        .unwrap_or_default()
        .split_ascii_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method != "GET" {
        return response(stream, 404, "text/plain", "default-src 'none'", "Not found").await;
    }
    if path == format!("/ui/{token}") {
        return response(stream, 200, "text/html; charset=utf-8", proxy_csp, proxy).await;
    }
    if path == format!("/ui/{token}/view") {
        return response(stream, 200, "text/html; charset=utf-8", view_csp, view).await;
    }
    response(stream, 404, "text/plain", "default-src 'none'", "Not found").await
}
async fn response(
    s: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    csp: &str,
    body: &str,
) -> Result<(), String> {
    let reason = if status == 200 {
        "OK"
    } else if status == 431 {
        "Request Header Fields Too Large"
    } else {
        "Not Found"
    };
    let h=format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nContent-Security-Policy: {csp}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",body.len());
    s.write_all(h.as_bytes()).await.map_err(|e| e.to_string())?;
    s.write_all(body.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_is_opaque_and_exposes_only_the_fixed_bridge() {
        let page = proxy_page("n", "http://127.0.0.1:1420", "aabb", "/ui/aabb/view").unwrap();
        let view = view_page(
            "aabb",
            "<script>window.appRan=true</script>",
            &json!({"x":1}),
        )
        .unwrap();
        assert!(page.contains("sandbox=\"allow-scripts\""));
        assert!(page.contains("e.source!==f.contentWindow||e.origin!=='null'"));
        assert!(view.contains("window.appRan=true"));
        assert!(view.contains("requestAction:id=>send('requestAction',id)"));
        assert!(view.contains("state=v;return saved"));
        assert!(view.contains("channel==='brr-ui-state'"));
        assert!(view.contains("new CustomEvent('brr-ui-state'"));
        assert!(!page.contains("window.appRan"));
        assert!(page.contains("parent.postMessage(e.data,HOST)"));
        assert!(page.contains("e.source!==parent||e.origin!==HOST"));
    }
    #[test]
    fn host_origins_are_an_exact_allowlist() {
        assert!(ORIGINS.contains(&"tauri://localhost"));
        assert!(!ORIGINS.contains(&"https://attacker.invalid"));
    }
    #[tokio::test]
    async fn loopback_server_emits_fixed_deny_policy_and_tears_down() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let token = "cap";
        let nonce = "n";
        let host = "http://localhost:1420";
        let page = proxy_page(nonce, host, token, "/ui/cap/view").unwrap();
        let view = view_page(
            token,
            "<script>window.appRan=true</script>",
            &json!({"x":1}),
        )
        .unwrap();
        let proxy_csp = format!("default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; frame-src 'self'; connect-src 'none'; img-src 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors {host}");
        let view_csp = format!("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; frame-src 'none'; navigate-to 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors 'self' {host}");
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve(
            listener,
            token.into(),
            page,
            proxy_csp,
            view,
            view_csp,
            stopped,
        ));
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /ui/cap HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await.unwrap();
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK"));
        assert!(reply.contains("Content-Security-Policy: default-src 'none'; script-src 'nonce-n'; style-src 'unsafe-inline'; frame-src 'self'; connect-src 'none'; img-src 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors http://localhost:1420"));
        assert!(reply.contains("/ui/cap/view"));
        assert!(reply.contains("sandbox=\"allow-scripts\""));
        assert!(reply.contains("e.source!==f.contentWindow||e.origin!=='null'"));
        let mut inner = tokio::net::TcpStream::connect(address).await.unwrap();
        inner
            .write_all(b"GET /ui/cap/view HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut inner_reply = Vec::new();
        inner.read_to_end(&mut inner_reply).await.unwrap();
        let inner_reply = String::from_utf8(inner_reply).unwrap();
        assert!(inner_reply.starts_with("HTTP/1.1 200 OK"));
        assert!(inner_reply.contains("script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; frame-src 'none'; navigate-to 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors 'self' http://localhost:1420"));
        assert!(inner_reply.contains("window.appRan=true"));
        assert!(inner_reply.contains("Object.defineProperty(window,'brrUI'"));
        let mut wrong = tokio::net::TcpStream::connect(address).await.unwrap();
        wrong
            .write_all(b"GET /other HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut not_found = Vec::new();
        wrong.read_to_end(&mut not_found).await.unwrap();
        assert!(String::from_utf8(not_found)
            .unwrap()
            .starts_with("HTTP/1.1 404 Not Found"));
        let _ = stop.send(());
        task.await.unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    /// Manual browser QA fixture. Start with
    /// `BRR_UI_FIXTURE_DIR=/tmp/brr-ui-fixture cargo test --manifest-path src-tauri/Cargo.toml generated_ui_sandbox::tests::browser_fixture -- --ignored --nocapture`
    /// and open the URL written to ready.json. Create `<dir>/stop` to tear it down.
    #[tokio::test]
    #[ignore = "manual browser QA fixture"]
    async fn browser_fixture() {
        let dir = std::env::var("BRR_UI_FIXTURE_DIR").expect("set BRR_UI_FIXTURE_DIR");
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        let fixture_host =
            std::env::var("BRR_UI_FIXTURE_HOST").unwrap_or_else(|_| "http://localhost:1420".into());
        let host = fixture_host.as_str();
        let token = random_token(32).unwrap();
        let nonce = random_token(18).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/ui/{token}");
        let view_path = format!("/ui/{token}/view");
        let html = r#"<!doctype html><meta charset="utf-8"><main style="font:16px system-ui;padding:24px"><h1>Sandbox state fixture</h1><label>Hue <input id="hue" type="range" min="0" max="360"></label><output id="state"></output><p id="network">network: checking</p><p id="native"></p><button id="action">Prepare follow-up</button> <button id="resize">Resize panel</button><script>
const hue=document.querySelector('#hue'),out=document.querySelector('#state');
function paint(){const s=window.brrUI.getState();hue.value=s.hue;document.body.style.background=`hsl(${s.hue} 50% 92%)`;out.textContent=`saved hue: ${s.hue}`}
paint();hue.addEventListener('input',async()=>{try{await window.brrUI.setState({hue:Number(hue.value)});paint()}catch(e){out.textContent='save failed: '+e.message}});
window.addEventListener('brr-ui-state',paint);
document.querySelector('#native').textContent='native API: '+(window.__TAURI__===undefined?'absent':'unexpected');
fetch('https://example.invalid/').then(()=>document.querySelector('#network').textContent='network: unexpectedly reachable').catch(()=>document.querySelector('#network').textContent='network: blocked');
document.querySelector('#action').onclick=()=>window.brrUI.requestAction('record').then(()=>{}).catch(e=>alert(e.message));
document.querySelector('#resize').onclick=()=>window.brrUI.resize(260);
</script></main>"#;
        let page = proxy_page(&nonce, host, &token, &view_path).unwrap();
        let view = view_page(&token, html, &json!({"hue":210})).unwrap();
        let proxy_csp = format!("default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; frame-src 'self'; connect-src 'none'; img-src 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors {host}");
        let view_csp = format!("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; frame-src 'none'; navigate-to 'none'; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors 'self' {host}");
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve(
            listener,
            token.clone(),
            page,
            proxy_csp,
            view,
            view_csp,
            stopped,
        ));
        let ready = json!({"url":url,"token":token,"hostOrigin":host});
        std::fs::write(
            format!("{dir}/ready.json"),
            serde_json::to_vec_pretty(&ready).unwrap(),
        )
        .unwrap();
        println!("Intelligent UI browser fixture: {url}");
        for _ in 0..4500 {
            if std::path::Path::new(&format!("{dir}/stop")).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let _ = stop.send(());
        task.await.unwrap();
    }
}
