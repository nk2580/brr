//! Restricted Git smart-HTTP transport for the primary fleet repository.

use crate::{
    network::GitCallback,
    peers::{PeerTransport, Peers, SecretString},
};
use axum::{
    body::{Body, Bytes},
    http::{header, HeaderValue, Request, Response, StatusCode},
};
use brr_node_auth::{DeviceRole, Principal};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task;
use url::Url;

const CGI_HEADER_LIMIT: usize = 64 * 1024;
const RESPONSE_CHUNK_SIZE: usize = 64 * 1024;
const RESPONSE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct GitHttp {
    bare_repo: PathBuf,
    project_root: PathBuf,
    peers: Peers,
    request_gate: Arc<Semaphore>,
}

struct PrivateBodyFile {
    path: PathBuf,
    file: File,
}

impl PrivateBodyFile {
    fn create() -> io::Result<Self> {
        let directory = std::env::temp_dir();
        for _ in 0..8 {
            let path = directory.join(format!("brr-git-body-{}.tmp", uuid::Uuid::new_v4()));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            match options.open(&path) {
                Ok(file) => return Ok(Self { path, file }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate private Git request storage",
        ))
    }
}

impl Drop for PrivateBodyFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub struct GitHttpClient {
    origin_url: String,
    trusted_ca_pem: String,
    fingerprint: String,
    credential: SecretString,
}

impl GitHttp {
    /// Bind the transport to one existing bare repository and a primary roster.
    /// The path is canonicalized once; no request component is ever used as a
    /// filesystem path or passed as a Git backend argument.
    pub async fn open(bare_repo: impl AsRef<Path>, peers: Peers) -> Result<Self, String> {
        if peers
            .mode()
            .await
            .map_err(|_| "could not read fleet role".to_string())?
            != crate::peers::PeerMode::Primary
        {
            return Err("Git HTTP can only serve from the primary node".into());
        }
        let path = bare_repo.as_ref();
        let metadata =
            fs::symlink_metadata(path).map_err(|_| "Git repository is unavailable".to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("Git repository must be a regular bare-repository directory".into());
        }
        let canonical =
            fs::canonicalize(path).map_err(|_| "Git repository is unavailable".to_string())?;
        let check = Command::new("git")
            .args([
                "-C",
                canonical.to_str().ok_or("non-UTF8 Git repository path")?,
                "rev-parse",
                "--is-bare-repository",
            ])
            .output()
            .map_err(|_| "Git is unavailable".to_string())?;
        if !check.status.success() || String::from_utf8_lossy(&check.stdout).trim() != "true" {
            return Err("Git repository must be bare".into());
        }
        let project_root = canonical
            .parent()
            .ok_or("bare repository has no project root")?
            .to_path_buf();
        let hooks = project_root.join("no-hooks");
        fs::create_dir_all(&hooks).map_err(|_| "could not configure Git hooks".to_string())?;
        set_git_config(
            &canonical,
            "core.hooksPath",
            hooks.to_str().ok_or("non-UTF8 hooks path")?,
        )?;
        set_git_config(&canonical, "http.receivepack", "true")?;
        set_git_config(&canonical, "receive.denyNonFastForwards", "true")?;
        Ok(Self {
            bare_repo: canonical,
            project_root,
            peers,
            request_gate: Arc::new(Semaphore::new(1)),
        })
    }

    pub fn callback(self: Arc<Self>) -> GitCallback {
        Arc::new(move |principal, request| {
            let server = self.clone();
            Box::pin(async move { server.handle(principal, request).await })
        })
    }

    async fn handle(&self, principal: Principal, request: Request<Body>) -> Response<Body> {
        if principal.role != DeviceRole::Node {
            return response(StatusCode::FORBIDDEN, "node credentials only");
        }
        let registered = match self.peers.registered_node_id_for(&principal).await {
            Ok(Some(node_id)) => node_id,
            Ok(None) => {
                return response(
                    StatusCode::FORBIDDEN,
                    "node is not registered for this repository",
                );
            }
            Err(_) => {
                return response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Git authorization failed",
                );
            }
        };
        let method = request.method().as_str().to_owned();
        let (path_info, query) = match map_git_route(request.uri()) {
            Ok(value) => value,
            Err(status) => return response(status, "unsupported Git route"),
        };
        let method_allowed = if path_info.ends_with("/info/refs") {
            method == "GET"
        } else if path_info.ends_with("/git-upload-pack") {
            method == "POST"
        } else if path_info.ends_with("/git-receive-pack") {
            method == "GET" || method == "POST"
        } else {
            false
        };
        if !method_allowed {
            return response(StatusCode::METHOD_NOT_ALLOWED, "unsupported Git method");
        }
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let receive_pack_post = method == "POST" && path_info.ends_with("/git-receive-pack");
        let upload_pack_post = method == "POST" && path_info.ends_with("/git-upload-pack");
        if receive_pack_post {
            if !content_type.starts_with("application/x-git-receive-pack-request") {
                return response(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "invalid receive-pack content type",
                );
            }
        } else if upload_pack_post {
            if !content_type.starts_with("application/x-git-upload-pack-request") {
                return response(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "invalid upload-pack content type",
                );
            }
        }
        let request_permit: Option<OwnedSemaphorePermit> =
            match self.request_gate.clone().acquire_owned().await {
                Ok(permit) => Some(permit),
                Err(_) => {
                    return response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Git transport is unavailable",
                    );
                }
            };
        let (mut body_file, body_length) =
            match spool_body_with_idle_timeout(request.into_body(), Duration::from_secs(120)).await
            {
                Ok(body) => body,
                Err((status, message)) => return response(status, message),
            };
        if receive_pack_post {
            if is_receive_pack_probe(&mut body_file.file, body_length).unwrap_or(false) {
                let mut response = Response::new(Body::empty());
                *response.status_mut() = StatusCode::OK;
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/x-git-receive-pack-result"),
                );
                return response;
            }
            if validate_receive_pack_file(&mut body_file.file, &registered).is_err() {
                return response(
                    StatusCode::FORBIDDEN,
                    "receive-pack may update only the registered node branch",
                );
            }
            if body_file.file.seek(SeekFrom::Start(0)).is_err() {
                return response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Git request storage failed",
                );
            }
        }
        let bare_repo = self.bare_repo.clone();
        let project_root = self.project_root.clone();
        let result = task::spawn_blocking(move || {
            let backend = run_backend(
                &project_root,
                &bare_repo,
                &method,
                &path_info,
                &query,
                &content_type,
                &mut body_file.file,
                body_length,
            );
            (backend, request_permit)
        })
        .await;
        match result {
            Ok((Ok((status, content_type, output_file, body_length)), permit)) => {
                let body = match backend_response_body(
                    output_file,
                    body_length,
                    permit,
                    RESPONSE_IDLE_TIMEOUT,
                ) {
                    Ok(body) => body,
                    Err(_) => {
                        return response(StatusCode::BAD_GATEWAY, "Git backend response failed");
                    }
                };
                let mut response = Response::new(body);
                *response.status_mut() = status;
                if let Ok(value) = HeaderValue::from_str(&content_type) {
                    response.headers_mut().insert(header::CONTENT_TYPE, value);
                }
                response
            }
            _ => response(StatusCode::BAD_GATEWAY, "Git backend failed"),
        }
    }
}

async fn spool_body_with_idle_timeout(
    body: Body,
    idle_timeout: Duration,
) -> Result<(PrivateBodyFile, u64), (StatusCode, &'static str)> {
    let mut temp = PrivateBodyFile::create().map_err(|_| {
        (
            StatusCode::INSUFFICIENT_STORAGE,
            "Git request storage is unavailable",
        )
    })?;
    let writer = temp.file.try_clone().map_err(|_| {
        (
            StatusCode::INSUFFICIENT_STORAGE,
            "Git request storage is unavailable",
        )
    })?;
    let mut writer = tokio::fs::File::from_std(writer);
    let mut stream = body.into_data_stream();
    let mut length = 0u64;
    loop {
        let next = tokio::time::timeout(idle_timeout, stream.next())
            .await
            .map_err(|_| (StatusCode::REQUEST_TIMEOUT, "Git request body stalled"))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|_| (StatusCode::BAD_REQUEST, "Git request body is invalid"))?;
        length = length
            .checked_add(chunk.len() as u64)
            .ok_or((StatusCode::BAD_REQUEST, "Git request body is too large"))?;
        writer.write_all(&chunk).await.map_err(|_| {
            (
                StatusCode::INSUFFICIENT_STORAGE,
                "Git request storage is full",
            )
        })?;
    }
    writer.flush().await.map_err(|_| {
        (
            StatusCode::INSUFFICIENT_STORAGE,
            "Git request storage is full",
        )
    })?;
    writer.sync_all().await.map_err(|_| {
        (
            StatusCode::INSUFFICIENT_STORAGE,
            "Git request storage is full",
        )
    })?;
    drop(writer);
    temp.file.seek(SeekFrom::Start(0)).map_err(|_| {
        (
            StatusCode::INSUFFICIENT_STORAGE,
            "Git request storage failed",
        )
    })?;
    Ok((temp, length))
}

fn backend_response_body(
    output: PrivateBodyFile,
    body_length: u64,
    permit: Option<OwnedSemaphorePermit>,
    idle_timeout: Duration,
) -> Result<Body, String> {
    let file = output
        .file
        .try_clone()
        .map_err(|_| "could not read Git backend response")?;
    let file = tokio::fs::File::from_std(file);
    let resources = Arc::new(tokio::sync::Mutex::new(Some(ResponseResources {
        _output: output,
        file,
        _permit: permit,
    })));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (progress_tx, mut progress_rx) = tokio::sync::watch::channel(0u64);
    let watchdog_resources = resources.clone();
    let watchdog_timed_out = timed_out.clone();
    let watchdog = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(idle_timeout, progress_rx.changed()).await {
                Ok(Ok(())) => continue,
                Ok(Err(_)) => return,
                Err(_) => {
                    watchdog_timed_out.store(true, Ordering::Release);
                    watchdog_resources.lock().await.take();
                    return;
                }
            }
        }
    });
    let lease = ResponseLease {
        resources,
        timed_out: timed_out.clone(),
        progress: progress_tx,
        watchdog,
    };
    let body =
        futures_util::stream::try_unfold((lease, body_length), |(lease, remaining)| async move {
            if lease.timed_out.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Git response body stalled",
                ));
            }
            if remaining == 0 {
                return Ok::<_, io::Error>(None);
            }
            let length = remaining.min(RESPONSE_CHUNK_SIZE as u64) as usize;
            let mut bytes = vec![0u8; length];
            let read = {
                let mut resources = lease.resources.lock().await;
                let Some(resources) = resources.as_mut() else {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Git response body stalled",
                    ));
                };
                resources.file.read(&mut bytes).await?
            };
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Git backend response was truncated",
                ));
            }
            bytes.truncate(read);
            let _ = lease.progress.send(remaining - read as u64);
            Ok(Some((Bytes::from(bytes), (lease, remaining - read as u64))))
        });
    Ok(Body::from_stream(body))
}

struct ResponseLease {
    resources: Arc<tokio::sync::Mutex<Option<ResponseResources>>>,
    timed_out: Arc<AtomicBool>,
    progress: tokio::sync::watch::Sender<u64>,
    watchdog: tokio::task::JoinHandle<()>,
}

struct ResponseResources {
    _output: PrivateBodyFile,
    file: tokio::fs::File,
    _permit: Option<OwnedSemaphorePermit>,
}

impl Drop for ResponseLease {
    fn drop(&mut self) {
        self.watchdog.abort();
        if let Ok(mut resources) = self.resources.try_lock() {
            resources.take();
        }
    }
}

impl GitHttpClient {
    pub fn new(transport: PeerTransport) -> Result<Self, String> {
        let mut endpoint = Url::parse(&transport.endpoint)
            .map_err(|_| "invalid pinned Git endpoint".to_string())?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.username() != ""
            || endpoint.password().is_some()
        {
            return Err("Git endpoint must be a credential-free HTTPS URL".into());
        }
        let der = parse_single_pem_certificate(&transport.trusted_ca_pem)?;
        let actual = format!("sha256:{}", hex(&Sha256::digest(&der)));
        if actual != transport.fingerprint {
            return Err("trusted Git certificate fingerprint does not match its pin".into());
        }
        endpoint.set_path("/v1/git/origin.git");
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        Ok(Self {
            origin_url: endpoint.to_string(),
            trusted_ca_pem: transport.trusted_ca_pem,
            fingerprint: transport.fingerprint,
            credential: transport.credential,
        })
    }

    pub fn origin_url(&self) -> &str {
        &self.origin_url
    }
    pub fn pinned_fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn clone_repository(&self, destination: &Path) -> Result<(), String> {
        if destination.exists()
            && fs::read_dir(destination)
                .map_err(|_| "clone destination is unavailable")?
                .next()
                .is_some()
        {
            return Err("clone destination must not contain files".into());
        }
        self.run_git(
            None,
            &[
                "clone",
                "--",
                &self.origin_url,
                destination.to_str().ok_or("non-UTF8 clone path")?,
            ],
        )
    }
    pub fn fetch(&self, repo: &Path) -> Result<(), String> {
        ensure_safe_client_repo(repo)?;
        self.run_git(
            Some(repo),
            &[
                "fetch",
                "--no-recurse-submodules",
                &self.origin_url,
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        )
    }
    pub fn push_node_branch(&self, repo: &Path, node_id: &str) -> Result<(), String> {
        validate_node_id(node_id)?;
        ensure_safe_client_repo(repo)?;
        let branch = format!("refs/heads/nodes/{node_id}");
        self.run_git(
            Some(repo),
            &[
                "push",
                "--porcelain",
                &self.origin_url,
                &format!("{branch}:{branch}"),
            ],
        )
    }

    fn run_git(&self, repo: Option<&Path>, args: &[&str]) -> Result<(), String> {
        let ca_path = write_private_ca(&self.trusted_ca_pem)?;
        let mut command = Command::new("git");
        clear_git_environment(&mut command);
        if let Some(repo) = repo {
            command.arg("-C").arg(repo);
        }
        command
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_TRACE", "0")
            .env("GIT_TRACE_PACKET", "0")
            .env("GIT_TRACE_CURL", "0")
            .env("GIT_CURL_VERBOSE", "0")
            .env("GIT_TRACE2", "0")
            .env("GIT_TRACE2_EVENT", "0")
            .env("GIT_CONFIG_COUNT", "7")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env("GIT_CONFIG_VALUE_0", "")
            .env("GIT_CONFIG_KEY_1", "http.extraHeader")
            .env(
                "GIT_CONFIG_VALUE_1",
                format!("Authorization: Bearer {}", self.credential.expose_secret()),
            )
            .env("GIT_CONFIG_KEY_2", "http.sslCAInfo")
            .env("GIT_CONFIG_VALUE_2", &ca_path)
            .env("GIT_CONFIG_KEY_3", "http.sslVerify")
            .env("GIT_CONFIG_VALUE_3", "true")
            .env("GIT_CONFIG_KEY_4", "http.followRedirects")
            .env("GIT_CONFIG_VALUE_4", "false")
            .env("GIT_CONFIG_KEY_5", "http.proxy")
            .env("GIT_CONFIG_VALUE_5", "")
            .env("GIT_CONFIG_KEY_6", "core.hooksPath")
            .env("GIT_CONFIG_VALUE_6", "/dev/null")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("GIT_TRACE_CURL_NO_DATA")
            .env_remove("GIT_TRACE_REDACT");
        let result = run_git_command(&mut command, std::time::Duration::from_secs(30));
        let _ = fs::remove_file(ca_path);
        result
    }
}

fn run_git_command(command: &mut Command, deadline: std::time::Duration) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "could not start Git".to_owned())?;
    let status = wait_child(&mut child, deadline)?;
    if status.success() {
        Ok(())
    } else {
        Err("Git transport operation failed".into())
    }
}

fn wait_child(child: &mut Child, deadline: std::time::Duration) -> Result<ExitStatus, String> {
    if deadline.is_zero() {
        return Err("Git child deadline is invalid".into());
    }
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => (),
            Err(_) => {
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err("Could not observe Git child completion".into());
            }
        }
        if started.elapsed() >= deadline {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err("Git child timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn map_git_route(uri: &axum::http::Uri) -> Result<(String, String), StatusCode> {
    let path = uri.path();
    let suffix = path.strip_prefix("/v1/git/").ok_or(StatusCode::NOT_FOUND)?;
    if !suffix.starts_with("origin.git/")
        || suffix.contains("..")
        || suffix.contains('\\')
        || suffix.contains('%')
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let operation = suffix
        .strip_prefix("origin.git")
        .ok_or(StatusCode::NOT_FOUND)?;
    if !matches!(
        operation,
        "/info/refs" | "/git-upload-pack" | "/git-receive-pack"
    ) {
        return Err(StatusCode::NOT_FOUND);
    }
    let query = uri.query().unwrap_or("");
    if operation == "/info/refs" {
        if query != "service=git-upload-pack" && query != "service=git-receive-pack" {
            return Err(StatusCode::BAD_REQUEST);
        }
    } else if !query.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((format!("/origin.git{operation}"), query.to_owned()))
}

fn run_backend(
    root: &Path,
    _bare: &Path,
    method: &str,
    path_info: &str,
    query: &str,
    content_type: &str,
    body: &mut File,
    body_length: u64,
) -> Result<(StatusCode, String, PrivateBodyFile, u64), String> {
    let mut git = Command::new("git");
    clear_git_environment(&mut git);
    let exec_path = git
        .arg("--exec-path")
        .output()
        .map_err(|_| "git not available")?;
    let exec_path = String::from_utf8(exec_path.stdout).map_err(|_| "invalid git exec path")?;
    let backend = Path::new(exec_path.trim()).join("git-http-backend");
    let mut output =
        PrivateBodyFile::create().map_err(|_| "could not create Git response storage")?;
    let stdin = Stdio::from(
        body.try_clone()
            .map_err(|_| "Git request storage is unavailable")?,
    );
    let stdout = Stdio::from(
        output
            .file
            .try_clone()
            .map_err(|_| "Git response storage is unavailable")?,
    );
    let mut command = Command::new(backend);
    clear_git_environment(&mut command);
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", path_info)
        .env("REQUEST_METHOD", method)
        .env("QUERY_STRING", query)
        .env("CONTENT_TYPE", content_type)
        .env("CONTENT_LENGTH", body_length.to_string())
        .env("SERVER_PROTOCOL", "HTTP/1.1")
        .env("SERVER_NAME", "localhost")
        .env("SERVER_PORT", "443")
        .stdin(stdin)
        .stdout(stdout)
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "could not start Git HTTP backend")?;
    let status = wait_child(&mut child, std::time::Duration::from_secs(300))
        .map_err(|_| "Git HTTP backend timed out")?;
    if !status.success() {
        return Err("Git backend failed".into());
    }
    let (status, response_type, body_length) = parse_cgi_response(&mut output)?;
    Ok((status, response_type, output, body_length))
}

fn parse_cgi_response(output: &mut PrivateBodyFile) -> Result<(StatusCode, String, u64), String> {
    output
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|_| "could not seek Git backend response")?;
    let mut headers = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    let split = loop {
        let read = output
            .file
            .read(&mut chunk)
            .map_err(|_| "could not read Git backend headers")?;
        if read == 0 {
            return Err("invalid Git backend response".into());
        }
        headers.extend_from_slice(&chunk[..read]);
        if let Some(split) = headers.windows(4).position(|window| window == b"\r\n\r\n") {
            if split > CGI_HEADER_LIMIT {
                return Err("Git backend response headers too large".into());
            }
            break split;
        }
        if headers.len() > CGI_HEADER_LIMIT + 4 {
            return Err("Git backend response headers too large".into());
        }
    };
    let header_end = split + 4;
    let total_length = output
        .file
        .metadata()
        .map_err(|_| "could not inspect Git backend response")?
        .len();
    if total_length < header_end as u64 {
        return Err("invalid Git backend response length".into());
    }
    let body_length = total_length - header_end as u64;
    output
        .file
        .seek(SeekFrom::Start(header_end as u64))
        .map_err(|_| "could not seek Git backend response body")?;
    let headers =
        std::str::from_utf8(&headers[..split]).map_err(|_| "invalid Git backend headers")?;
    let mut status = StatusCode::OK;
    let mut response_type = "application/octet-stream".to_owned();
    for line in headers.split("\r\n") {
        if let Some(value) = line.strip_prefix("Status: ") {
            status = StatusCode::from_u16(
                value
                    .split_whitespace()
                    .next()
                    .ok_or("invalid CGI status")?
                    .parse()
                    .map_err(|_| "invalid CGI status")?,
            )
            .map_err(|_| "invalid CGI status")?;
        } else if let Some(value) = line.strip_prefix("Content-Type: ") {
            if value.len() > 128 || value.bytes().any(|b| b.is_ascii_control()) {
                return Err("invalid CGI content type".into());
            }
            response_type = value.to_owned();
        }
    }
    Ok((status, response_type, body_length))
}

fn validate_receive_pack(body: &[u8], node_id: &str) -> Result<(), String> {
    validate_receive_pack_reader(&mut io::Cursor::new(body), node_id)
}

fn is_receive_pack_probe(body: &mut File, body_length: u64) -> io::Result<bool> {
    if body_length != 4 {
        return Ok(false);
    }
    body.seek(SeekFrom::Start(0))?;
    let mut packet = [0u8; 4];
    body.read_exact(&mut packet)?;
    Ok(&packet == b"0000")
}

fn validate_receive_pack_file(body: &mut File, node_id: &str) -> Result<(), String> {
    body.seek(SeekFrom::Start(0))
        .map_err(|_| "could not seek receive-pack request")?;
    validate_receive_pack_reader(body, node_id)
}

fn validate_receive_pack_reader(reader: &mut impl Read, node_id: &str) -> Result<(), String> {
    validate_node_id(node_id)?;
    let allowed_ref = format!("refs/heads/nodes/{node_id}");
    let mut updates = HashSet::new();
    loop {
        let mut header = [0u8; 4];
        reader
            .read_exact(&mut header)
            .map_err(|_| "truncated receive-pack packet header")?;
        if &header == b"0000" {
            break;
        }
        let length_text =
            std::str::from_utf8(&header).map_err(|_| "invalid receive-pack packet length")?;
        let length = usize::from_str_radix(length_text, 16)
            .map_err(|_| "invalid receive-pack packet length")?;
        if length < 4 || length > 65_520 {
            return Err("invalid receive-pack command packet length".into());
        }
        let mut payload = vec![0u8; length - 4];
        reader
            .read_exact(&mut payload)
            .map_err(|_| "truncated receive-pack command packet")?;
        let mut payload = payload.as_slice();
        if payload.last() == Some(&b'\n') {
            payload = &payload[..payload.len() - 1];
        }
        let mut fields = payload.splitn(3, |byte| *byte == b' ');
        let old = fields.next().ok_or("malformed receive-pack ref update")?;
        let new = fields.next().ok_or("malformed receive-pack ref update")?;
        let reference = fields.next().ok_or("malformed receive-pack ref update")?;
        validate_object_id(old)?;
        validate_object_id(new)?;
        if old.len() != new.len() || new.iter().all(|byte| *byte == b'0') {
            return Err("deleting or malformed fleet refs is forbidden".into());
        }
        let reference = reference
            .split(|byte| *byte == 0)
            .next()
            .ok_or("malformed receive-pack ref update")?;
        let reference = std::str::from_utf8(reference).map_err(|_| "non-UTF8 receive-pack ref")?;
        if reference != allowed_ref {
            return Err("node credential may update only its own fleet branch".into());
        }
        if !updates.insert(reference.to_owned()) {
            return Err("duplicate fleet ref update".into());
        }
    }
    if updates.is_empty() {
        return Err("receive-pack request has no complete ref update section".into());
    }
    Ok(())
}

fn validate_object_id(bytes: &[u8]) -> Result<(), String> {
    if !matches!(bytes.len(), 40 | 64) || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err("malformed receive-pack object id".into());
    }
    Ok(())
}
fn validate_node_id(node_id: &str) -> Result<(), String> {
    if node_id.is_empty()
        || !node_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("invalid registered node identity".into());
    }
    Ok(())
}
fn set_git_config(repo: &Path, key: &str, value: &str) -> Result<(), String> {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    let output = command
        .args([
            "-C",
            repo.to_str().ok_or("non-UTF8 Git repository path")?,
            "config",
            key,
            value,
        ])
        .output()
        .map_err(|_| "could not configure Git repository".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err("could not configure Git repository".into())
    }
}
fn clear_git_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
}
fn ensure_safe_client_repo(repo: &Path) -> Result<(), String> {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    let output = command
        .arg("-C")
        .arg(repo)
        .args([
            "config",
            "--local",
            "--no-includes",
            "--name-only",
            "--get-regexp",
            r"^(url\..*\.insteadof|http\..*\.(extraheader|proxy)|credential\.helper|include\..*\.path|includeif\..*\.path)$",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .map_err(|_| "could not inspect local Git configuration".to_string())?;
    if output.status.code() == Some(1) && output.stdout.is_empty() {
        Ok(())
    } else if output.status.success() && output.stdout.is_empty() {
        Ok(())
    } else {
        Err("repository contains unsafe transport configuration".into())
    }
}
fn write_private_ca(pem: &str) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("brr-git-ca-{}.pem", uuid::Uuid::new_v4()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| "could not create private TLS trust file")?;
        file.write_all(pem.as_bytes())
            .map_err(|_| "could not write private TLS trust file")?;
    }
    #[cfg(not(unix))]
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "could not create private TLS trust file")?;
        file.write_all(pem.as_bytes())
            .map_err(|_| "could not write private TLS trust file")?;
    }
    Ok(path)
}
fn parse_single_pem_certificate(pem: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let pem = pem.trim();
    let body = pem
        .strip_prefix(begin)
        .and_then(|s| {
            s.split_once(end)
                .filter(|(_, trailing)| trailing.trim().is_empty())
                .map(|(body, _)| body)
        })
        .ok_or("invalid trusted Git certificate")?;
    if body.contains("-----") {
        return Err("expected one trusted Git certificate".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(
            body.bytes()
                .filter(|b| !b.is_ascii_whitespace())
                .collect::<Vec<_>>(),
        )
        .map_err(|_| "invalid trusted Git certificate".into())
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}
fn response(status: StatusCode, message: &'static str) -> Response<Body> {
    let mut response = Response::new(Body::from(message));
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use futures_util::stream;
    use futures_util::StreamExt;
    use std::collections::BTreeSet;
    use std::convert::Infallible;

    static ZERO_CHUNK: [u8; 64 * 1024] = [0; 64 * 1024];
    const PREVIOUS_MAX_GIT_BODY: usize = 512 * 1024 * 1024;

    fn body_tempfiles() -> BTreeSet<PathBuf> {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("brr-git-body-") && name.ends_with(".tmp"))
            })
            .collect()
    }

    #[tokio::test]
    async fn stalled_request_body_times_out_and_removes_private_spool() {
        let before = body_tempfiles();
        let body_stream = stream::iter([Ok::<Bytes, Infallible>(Bytes::from_static(b"part"))])
            .chain(stream::pending());
        let error = match spool_body_with_idle_timeout(
            Body::from_stream(body_stream),
            Duration::from_millis(20),
        )
        .await
        {
            Ok(_) => panic!("stalled upload unexpectedly completed"),
            Err(error) => error,
        };
        assert_eq!(error.0, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(body_tempfiles(), before);
    }

    #[tokio::test]
    async fn backend_response_uses_bounded_chunks_and_removes_tempfile_on_cancel() {
        let mut output = PrivateBodyFile::create().unwrap();
        let payload = vec![0x5a; RESPONSE_CHUNK_SIZE * 3];
        output
            .file
            .write_all(b"Status: 200 OK\r\nContent-Type: application/octet-stream\r\n\r\n")
            .unwrap();
        output.file.write_all(&payload).unwrap();
        output.file.seek(SeekFrom::Start(0)).unwrap();
        let path = output.path.clone();
        let gate = Arc::new(Semaphore::new(1));
        let permit = gate.clone().acquire_owned().await.unwrap();
        let (status, content_type, body_length) = parse_cgi_response(&mut output).unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "application/octet-stream");
        assert_eq!(body_length, payload.len() as u64);
        let body = backend_response_body(output, body_length, Some(permit), RESPONSE_IDLE_TIMEOUT)
            .unwrap();
        let mut chunks = body.into_data_stream();
        let first = chunks.next().await.unwrap().unwrap();
        assert!(first.len() <= RESPONSE_CHUNK_SIZE);
        assert_eq!(
            gate.available_permits(),
            0,
            "request gate must stay held while response is active"
        );
        drop(chunks);
        assert!(
            !path.exists(),
            "canceled response must remove its temporary file"
        );
        assert_eq!(
            gate.available_permits(),
            1,
            "canceling response must release request gate"
        );
    }

    #[tokio::test]
    async fn unpolled_response_idle_timeout_releases_tempfile_and_request_gate() {
        let mut output = PrivateBodyFile::create().unwrap();
        output.file.write_all(b"response body").unwrap();
        output.file.seek(SeekFrom::Start(0)).unwrap();
        let path = output.path.clone();
        let gate = Arc::new(Semaphore::new(1));
        let permit = gate.clone().acquire_owned().await.unwrap();
        let body =
            backend_response_body(output, 13, Some(permit), Duration::from_millis(20)).unwrap();
        assert_eq!(gate.available_permits(), 0);
        let mut chunks = body.into_data_stream();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(gate.available_permits(), 1);
        assert!(!path.exists());
        assert!(chunks
            .next()
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("Git response body stalled"));
        drop(chunks);
    }
    #[test]
    fn unavailable_git_transport_has_a_bounded_deadline() {
        let started = std::time::Instant::now();
        let mut command = Command::new("python3");
        command.args(["-c", "import time; time.sleep(2)"]);
        assert!(run_git_command(&mut command, std::time::Duration::from_millis(100)).is_err());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "Offline synchronization must release shared SQL authority promptly"
        );
    }
    fn packet(payload: &[u8]) -> Vec<u8> {
        let mut out = format!("{:04x}", payload.len() + 4).into_bytes();
        out.extend_from_slice(payload);
        out
    }
    fn update(old: &str, new: &str, reference: &str, capabilities: bool) -> Vec<u8> {
        let command = if capabilities {
            format!("{old} {new} {reference}\0report-status side-band-64k\n")
        } else {
            format!("{old} {new} {reference}\n")
        };
        let mut body = packet(command.as_bytes());
        body.extend_from_slice(b"0000PACK\0fake-pack-payload");
        body
    }

    async fn registered_receive_pack_server(
        directory: &tempfile::TempDir,
    ) -> (Arc<GitHttp>, Principal, PathBuf) {
        let peers_path = directory.path().join("probe-peers.sqlite");
        let peers = Peers::open(&peers_path).await.unwrap();
        peers.init_primary("node-primary").await.unwrap();
        let peer_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&peers_path))
            .await
            .unwrap();
        sqlx::query("INSERT INTO peer_nodes(node_id,principal_id,name,endpoint,ca_pem,fingerprint,primary_device_id,remote_device_id,secret,last_seen) VALUES('node-worker','principal-worker','Worker','https://worker.invalid','ca','pin','primary-device','worker-device','secret',1)")
            .execute(&peer_pool).await.unwrap();
        let bare = directory.path().join("origin.git");
        let output = Command::new("git")
            .args(["init", "--bare", bare.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success());
        let server = Arc::new(GitHttp::open(&bare, peers).await.unwrap());
        let principal = Principal {
            id: "principal-worker".into(),
            name: "Worker".into(),
            role: DeviceRole::Node,
            node_id: "node-primary".into(),
        };
        (server, principal, bare)
    }

    #[tokio::test]
    async fn authenticated_exact_receive_pack_flush_probe_returns_empty_success_without_backend_mutation(
    ) {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let (server, principal, bare) = registered_receive_pack_server(&directory).await;
        let before = body_tempfiles();
        let request = Request::builder()
            .method("POST")
            .uri("/v1/git/origin.git/git-receive-pack")
            .header(
                header::CONTENT_TYPE,
                "application/x-git-receive-pack-request",
            )
            .body(Body::from("0000"))
            .unwrap();
        let response = server.handle(principal, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-git-receive-pack-result"
        );
        assert!(axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap()
            .is_empty());
        let refs = Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap();
        assert!(refs.stdout.is_empty(), "the probe must not update refs");
        assert_eq!(
            body_tempfiles(),
            before,
            "probe request storage must be removed"
        );
    }

    #[tokio::test]
    async fn receive_pack_probe_requires_a_registered_node_identity() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let (server, principal, _) = registered_receive_pack_server(&directory).await;
        let before = body_tempfiles();
        for mut principal in [
            Principal {
                id: "unregistered-principal".into(),
                ..principal.clone()
            },
            Principal {
                role: DeviceRole::Mobile,
                ..principal
            },
        ] {
            principal.name = "Probe test".into();
            let request = Request::builder()
                .method("POST")
                .uri("/v1/git/origin.git/git-receive-pack")
                .header(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-request",
                )
                .body(Body::from("0000"))
                .unwrap();
            let response = server.handle(principal, request).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(
            body_tempfiles(),
            before,
            "unauthorized probes must not spool a body"
        );
    }

    #[tokio::test]
    async fn receive_pack_probe_does_not_allow_trailing_malformed_or_foreign_updates() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let (server, principal, _) = registered_receive_pack_server(&directory).await;
        let before = body_tempfiles();
        let invalid_bodies = [
            Vec::new(),
            b"0000x".to_vec(),
            b"0009bad0000PACK".to_vec(),
            update(&"0".repeat(40), &"1".repeat(40), "refs/heads/main", true),
        ];
        for body in invalid_bodies {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/git/origin.git/git-receive-pack")
                .header(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-request",
                )
                .body(Body::from(body))
                .unwrap();
            let response = server.handle(principal.clone(), request).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(body_tempfiles(), before, "rejected bodies must be removed");
    }

    #[tokio::test]
    async fn real_git_large_push_probes_then_uploads_to_the_authenticated_node_branch() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let (server, principal, bare) = registered_receive_pack_server(&directory).await;
        let probe_seen = Arc::new(AtomicBool::new(false));
        let upload_length = Arc::new(AtomicU64::new(0));
        let app = axum::Router::new().fallback(axum::routing::any({
            let server = server.clone();
            let principal = principal.clone();
            let probe_seen = probe_seen.clone();
            let upload_length = upload_length.clone();
            move |request: Request<Body>| {
                let server = server.clone();
                let principal = principal.clone();
                let probe_seen = probe_seen.clone();
                let upload_length = upload_length.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let bytes = axum::body::to_bytes(body, 2 * 1024 * 1024).await.unwrap();
                    if parts.uri.path().ends_with("/git-receive-pack") {
                        if bytes.as_ref() == b"0000" {
                            probe_seen.store(true, Ordering::SeqCst);
                        } else {
                            upload_length.store(bytes.len() as u64, Ordering::SeqCst);
                        }
                    }
                    server
                        .handle(principal, Request::from_parts(parts, Body::from(bytes)))
                        .await
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let local = directory.path().join("worker.git");
        for args in [
            vec!["init", local.to_str().unwrap()],
            vec![
                "-C",
                local.to_str().unwrap(),
                "config",
                "user.name",
                "Fleet Test",
            ],
            vec![
                "-C",
                local.to_str().unwrap(),
                "config",
                "user.email",
                "fleet-test@example.invalid",
            ],
        ] {
            let result = Command::new("git").args(args).output().unwrap();
            assert!(result.status.success(), "Git setup failed: {result:?}");
        }
        let mut content = vec![0u8; 128 * 1024];
        let mut state = 0x7a51_29d3_u32;
        for byte in &mut content {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        std::fs::write(local.join("payload.bin"), content).unwrap();
        for args in [
            vec!["-C", local.to_str().unwrap(), "add", "payload.bin"],
            vec![
                "-C",
                local.to_str().unwrap(),
                "commit",
                "-m",
                "large probe test",
            ],
        ] {
            let result = Command::new("git").args(args).output().unwrap();
            assert!(result.status.success(), "Git setup failed: {result:?}");
        }
        let endpoint = format!("http://{address}/v1/git/origin.git");
        let mut push = tokio::process::Command::new("git");
        push.args([
            "-c",
            "http.postBuffer=1024",
            "-c",
            "core.compression=0",
            "-c",
            "pack.window=0",
            "-c",
            "pack.depth=0",
            "-c",
            "http.proxy=",
            "-C",
            local.to_str().unwrap(),
            "push",
            &endpoint,
            "HEAD:refs/heads/nodes/node-worker",
        ]);
        push.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY");
        push.kill_on_drop(true);
        let result = tokio::time::timeout(Duration::from_secs(30), push.output())
            .await
            .expect("Git client push exceeded its test deadline")
            .unwrap();
        server_task.abort();
        let _ = server_task.await;
        assert!(
            result.status.success(),
            "Git push failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            probe_seen.load(Ordering::SeqCst),
            "Git did not send the exact probe"
        );
        assert!(
            upload_length.load(Ordering::SeqCst) > 128 * 1024,
            "Git did not send the large receive-pack request"
        );
        let refs = Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap();
        assert!(refs.status.success());
        assert!(String::from_utf8_lossy(&refs.stdout).contains("refs/heads/nodes/node-worker"));
    }

    #[test]
    fn receive_pack_allows_only_the_authenticated_nodes_branch_and_handles_capabilities_nul() {
        let old = "0".repeat(40);
        let new = "1".repeat(40);
        assert!(validate_receive_pack(
            &update(&old, &new, "refs/heads/nodes/node_abc123", true),
            "node_abc123"
        )
        .is_ok());
    }
    #[test]
    fn receive_pack_rejects_main_other_node_tags_deletes_and_malformed_commands() {
        let old = "0".repeat(40);
        let new = "1".repeat(40);
        for reference in [
            "refs/heads/main",
            "refs/heads/nodes/another",
            "refs/tags/release",
        ] {
            assert!(
                validate_receive_pack(&update(&old, &new, reference, true), "node_abc123").is_err()
            );
        }
        assert!(validate_receive_pack(
            &update(&new, &old, "refs/heads/nodes/node_abc123", true),
            "node_abc123"
        )
        .is_err());
        assert!(validate_receive_pack(b"0009bad0000PACK", "node_abc123").is_err());
    }

    #[tokio::test]
    async fn authenticated_receive_pack_larger_than_512_mib_reaches_git_backend() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let peers_path = directory.path().join("peers.sqlite");
        let peers = Peers::open(&peers_path).await.unwrap();
        peers.init_primary("node-primary").await.unwrap();
        let peer_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&peers_path))
            .await
            .unwrap();
        sqlx::query("INSERT INTO peer_nodes(node_id,principal_id,name,endpoint,ca_pem,fingerprint,primary_device_id,remote_device_id,secret,last_seen) VALUES('node-worker','principal-worker','Worker','https://worker.invalid','ca','pin','primary-device','worker-device','secret',1)")
            .execute(&peer_pool).await.unwrap();

        let bare = directory.path().join("origin.git");
        let output = Command::new("git")
            .args(["init", "--bare", bare.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success());
        let server = Arc::new(GitHttp::open(&bare, peers).await.unwrap());
        let principal = Principal {
            id: "principal-worker".into(),
            name: "Worker".into(),
            role: DeviceRole::Node,
            node_id: "node-primary".into(),
        };
        let prefix = Bytes::from(update(
            &"0".repeat(40),
            &"1".repeat(40),
            "refs/heads/nodes/node-worker",
            true,
        ));
        let remaining = PREVIOUS_MAX_GIT_BODY + 1 - prefix.len();
        let body_stream = stream::unfold(
            (Some(prefix), remaining),
            |(prefix, remaining)| async move {
                if let Some(prefix) = prefix {
                    return Some((Ok::<Bytes, Infallible>(prefix), (None, remaining)));
                }
                if remaining == 0 {
                    return None;
                }
                let length = remaining.min(ZERO_CHUNK.len());
                Some((
                    Ok(Bytes::from_static(&ZERO_CHUNK[..length])),
                    (None, remaining - length),
                ))
            },
        );
        let request = Request::builder()
            .method("POST")
            .uri("/v1/git/origin.git/git-receive-pack")
            .header(
                header::CONTENT_TYPE,
                "application/x-git-receive-pack-request",
            )
            .body(Body::from_stream(body_stream))
            .unwrap();

        let before = body_tempfiles();
        let response = server.handle(principal, request).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            body_tempfiles(),
            before,
            "spooled request temp file must be removed"
        );
        let refs = Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap();
        assert!(
            refs.stdout.is_empty(),
            "malformed pack must not update a branch"
        );
    }
}
