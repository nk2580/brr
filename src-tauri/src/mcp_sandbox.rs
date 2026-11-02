use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tauri::State;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{oneshot, Mutex},
    time::{timeout, Duration},
};

const MAX_HTML_BYTES: usize = 5_000_000;
const MAX_REQUEST_BYTES: usize = 8_192;
const MAX_TOKEN_BYTES: usize = 32;
const HOST_ORIGINS: &[&str] = &[
    "http://127.0.0.1:1420",
    "http://localhost:1420",
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppCsp {
    #[serde(default)]
    pub connect_domains: Vec<String>,
    #[serde(default)]
    pub resource_domains: Vec<String>,
    #[serde(default)]
    pub frame_domains: Vec<String>,
    #[serde(default)]
    pub base_uri_domains: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct McpSandboxAddress {
    pub url: String,
    pub token: String,
}

struct SandboxHandle {
    stop: oneshot::Sender<()>,
}

#[derive(Default)]
pub struct SandboxManager {
    active: Mutex<HashMap<String, SandboxHandle>>,
}

impl SandboxManager {
    pub async fn destroy(&self, token: &str) -> Result<(), String> {
        let handle = self.active.lock().await.remove(token);
        if let Some(handle) = handle {
            let _ = handle.stop.send(());
        }
        Ok(())
    }
}

#[tauri::command]
pub async fn create_mcp_sandbox(
    manager: State<'_, SandboxManager>,
    html: String,
    csp: AppCsp,
    host_origin: String,
) -> Result<McpSandboxAddress, String> {
    if !HOST_ORIGINS.contains(&host_origin.as_str()) {
        return Err("Unsupported MCP App host origin".into());
    }
    if html.trim().is_empty() || html.len() > MAX_HTML_BYTES {
        return Err("MCP App HTML must be non-empty and no larger than 5 MB".into());
    }
    let csp = validate_csp(csp)?;
    let mut token_bytes = [0_u8; MAX_TOKEN_BYTES];
    getrandom::fill(&mut token_bytes).map_err(|_| "Unable to create isolated MCP App token")?;
    let token = token_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut nonce_bytes = [0_u8; 18];
    getrandom::fill(&mut nonce_bytes).map_err(|_| "Unable to create MCP App proxy nonce")?;
    let nonce = nonce_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("Unable to bind MCP App sandbox: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let url = format!("http://127.0.0.1:{port}/widget/{token}");
    let view_url = format!("{url}/view");
    let html = Arc::<str>::from(html);
    let page = proxy_page(&nonce, &host_origin, &view_url);
    let proxy_csp = format!("default-src 'none'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; frame-src 'self'; connect-src 'none'; img-src data: blob:; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors {host_origin}");
    let view_csp = app_csp_header(&csp, &host_origin, &format!("http://127.0.0.1:{port}"));
    let (stop, stopped) = oneshot::channel();
    let server_token = token.clone();
    tokio::spawn(async move {
        serve(
            listener,
            server_token,
            page,
            proxy_csp,
            html,
            view_csp,
            stopped,
        )
        .await;
    });
    manager
        .active
        .lock()
        .await
        .insert(token.clone(), SandboxHandle { stop });
    Ok(McpSandboxAddress { url, token })
}

#[tauri::command]
pub async fn destroy_mcp_sandbox(
    manager: State<'_, SandboxManager>,
    token: String,
) -> Result<(), String> {
    if token.len() != MAX_TOKEN_BYTES * 2 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid MCP App sandbox token".into());
    }
    manager.destroy(&token).await
}

fn validate_csp(mut csp: AppCsp) -> Result<AppCsp, String> {
    for (domains, allows_websocket) in [
        (&mut csp.connect_domains, true),
        (&mut csp.resource_domains, false),
        (&mut csp.frame_domains, false),
        (&mut csp.base_uri_domains, false),
    ] {
        for domain in domains.iter() {
            let parsed =
                url::Url::parse(domain).map_err(|_| "MCP App CSP contains an invalid origin")?;
            let scheme_allowed = matches!(parsed.scheme(), "http" | "https")
                || (allows_websocket && matches!(parsed.scheme(), "ws" | "wss"));
            let host = parsed.host_str().unwrap_or_default();
            let wildcard = host.starts_with("*.");
            let suffix = if wildcard { &host[2..] } else { host };
            let ipv6_literal = !wildcard && host.starts_with('[') && host.ends_with(']');
            let valid_dns_name = ipv6_literal
                || (suffix.len() <= 253
                    && suffix.split('.').count() >= if wildcard { 2 } else { 1 }
                    && suffix.split('.').all(|label| {
                        !label.is_empty()
                            && label.len() <= 63
                            && label.as_bytes()[0].is_ascii_alphanumeric()
                            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                            && label
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    }));
            if domain
                .chars()
                .any(|character| character.is_whitespace() || character == ';')
                || host.contains('*') != wildcard
                || !valid_dns_name
                || !scheme_allowed
                || parsed.origin().ascii_serialization() != *domain
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err("MCP App CSP must contain valid exact or wildcard HTTP(S) origins (and WebSocket origins for connections), without credentials".into());
            }
        }
        domains.sort();
        domains.dedup();
    }
    Ok(csp)
}

fn app_csp_header(csp: &AppCsp, host_origin: &str, sandbox_origin: &str) -> String {
    fn sources(values: &[String]) -> String {
        values.iter().map(|value| format!(" {value}")).collect()
    }
    format!("default-src 'none'; script-src 'self' 'unsafe-inline'{}; style-src 'self' 'unsafe-inline'{}; img-src 'self' data: blob:{}; font-src 'self'{}; media-src 'self'{}; connect-src 'self'{}; frame-src{}; base-uri{}; form-action 'none'; object-src 'none'; frame-ancestors {host_origin} {sandbox_origin}",
        sources(&csp.resource_domains), sources(&csp.resource_domains), sources(&csp.resource_domains), sources(&csp.resource_domains), sources(&csp.resource_domains), sources(&csp.connect_domains), if csp.frame_domains.is_empty() { " 'none'".into() } else { sources(&csp.frame_domains) }, if csp.base_uri_domains.is_empty() { " 'self'".into() } else { sources(&csp.base_uri_domains) })
}

fn proxy_page(nonce: &str, host_origin: &str, view_url: &str) -> String {
    let host = serde_json::to_string(host_origin).unwrap_or_else(|_| "null".into());
    let view = serde_json::to_string(view_url).unwrap_or_else(|_| "null".into());
    let script = include_str!("mcp-sandbox/proxy.js")
        .replace("__HOST_ORIGIN__", &host)
        .replace("__VIEW_URL__", &view);
    format!("<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><style>html,body,iframe{{width:100%;height:100%;margin:0;border:0}}iframe{{display:block}}</style><iframe id=\"view\" title=\"MCP App\" sandbox=\"allow-scripts allow-same-origin allow-forms allow-downloads\"></iframe><script nonce=\"{nonce}\">{script}</script>")
}

async fn serve(
    listener: TcpListener,
    token: String,
    page: String,
    proxy_csp: String,
    html: Arc<str>,
    view_csp: String,
    mut stopped: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => {
                let Ok((mut stream, _)) = accepted else { continue; };
                let token = token.clone(); let page = page.clone(); let proxy_csp = proxy_csp.clone(); let html = html.clone(); let view_csp = view_csp.clone();
                tokio::spawn(async move { let _ = respond(&mut stream, &token, &page, &proxy_csp, &html, &view_csp).await; });
            }
        }
    }
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    token: &str,
    page: &str,
    proxy_csp: &str,
    html: &str,
    view_csp: &str,
) -> Result<(), String> {
    let mut request = vec![0; MAX_REQUEST_BYTES + 1];
    let read = timeout(Duration::from_secs(2), async {
        let mut total = 0;
        while total < request.len() {
            let bytes = stream.read(&mut request[total..]).await?;
            if bytes == 0 {
                break;
            }
            total += bytes;
            if total >= 4
                && request[..total]
                    .windows(4)
                    .any(|window| window == b"\r\n\r\n")
            {
                break;
            }
        }
        std::io::Result::Ok(total)
    })
    .await
    .map_err(|_| "MCP App sandbox request timed out")?
    .map_err(|e| e.to_string())?;
    if read == 0 || read > MAX_REQUEST_BYTES {
        return write_response(
            stream,
            431,
            "text/plain",
            "default-src 'none'",
            "Request too large",
        )
        .await;
    }
    let header = std::str::from_utf8(&request[..read]).map_err(|_| "Invalid HTTP request")?;
    let line = header.lines().next().unwrap_or_default();
    let mut parts = line.split_ascii_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method != "GET" {
        return write_response(
            stream,
            405,
            "text/plain",
            "default-src 'none'",
            "Method not allowed",
        )
        .await;
    }
    let expected = format!("/widget/{token}");
    if path == expected {
        write_response(stream, 200, "text/html; charset=utf-8", proxy_csp, page).await
    } else if path == format!("{expected}/view") {
        write_response(stream, 200, "text/html; charset=utf-8", view_csp, html).await
    } else {
        write_response(stream, 404, "text/plain", "default-src 'none'", "Not found").await
    }
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    csp: &str,
    body: &str,
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Request Header Fields Too Large",
    };
    let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Security-Policy: {csp}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nPermissions-Policy: camera=(), microphone=(), geolocation=(), clipboard-write=()\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.shutdown().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_csp_accepts_spec_wildcard_origins_and_rejects_unsafe_sources() {
        let good: AppCsp = serde_json::from_value(
            serde_json::json!({ "connectDomains":["https://api.example.test"] }),
        )
        .unwrap();
        assert_eq!(
            validate_csp(good).unwrap().connect_domains,
            ["https://api.example.test"]
        );
        let ipv6: AppCsp =
            serde_json::from_value(serde_json::json!({ "connectDomains":["wss://[::1]:8080"] }))
                .unwrap();
        assert_eq!(
            validate_csp(ipv6).unwrap().connect_domains,
            ["wss://[::1]:8080"]
        );
        let wildcard: AppCsp = serde_json::from_value(serde_json::json!({
            "connectDomains":["wss://*.api.example.test"], "resourceDomains":["https://*.cdn.example.test"]
        })).unwrap();
        assert!(validate_csp(wildcard).is_ok());
        for value in [
            "https://*",
            "https://*.test",
            "https://*.example.test/path",
            "https://user:pass@example.test",
            "file:///tmp",
        ] {
            let csp: AppCsp =
                serde_json::from_value(serde_json::json!({"resourceDomains":[value]})).unwrap();
            assert!(validate_csp(csp).is_err());
        }
    }

    #[tokio::test]
    async fn serves_expected_proxy_and_immutable_view_only_then_tears_down_listener() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let token = "a".repeat(64);
        let host = "http://127.0.0.1:1420";
        let view_url = format!("http://{address}/widget/{token}/view");
        let page = proxy_page("nonce", host, &view_url);
        let proxy_policy = format!("default-src 'none'; script-src 'nonce-nonce'; frame-src 'self'; frame-ancestors {host}");
        let html = Arc::<str>::from("<html>immutable app</html>");
        let app_policy = app_csp_header(
            &AppCsp {
                connect_domains: vec![],
                resource_domains: vec![],
                frame_domains: vec![],
                base_uri_domains: vec![],
            },
            host,
            &format!("http://{address}"),
        );
        let (stop, stopped) = oneshot::channel();
        let server_token = token.clone();
        let task = tokio::spawn(async move {
            serve(
                listener,
                server_token,
                page,
                proxy_policy,
                html,
                app_policy,
                stopped,
            )
            .await
        });

        async fn get(address: std::net::SocketAddr, path: &str) -> String {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            String::from_utf8(response).unwrap()
        }

        let proxy = get(address, &format!("/widget/{token}")).await;
        assert!(proxy.starts_with("HTTP/1.1 200 OK"));
        assert!(proxy.contains(&format!("frame-ancestors {host}")));
        assert!(proxy.contains(&format!("const VIEW_URL = \"{view_url}\"")));
        let view = get(address, &format!("/widget/{token}/view")).await;
        assert!(view.starts_with("HTTP/1.1 200 OK"));
        assert!(view.contains("<html>immutable app</html>"));
        assert!(view.contains(&format!("frame-ancestors {host} http://{address}")));
        assert!(get(address, &format!("/widget/{}/view", "b".repeat(64)))
            .await
            .starts_with("HTTP/1.1 404"));

        let _ = stop.send(());
        task.await.unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
}
