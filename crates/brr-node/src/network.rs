//! Authenticated, certificate-pinned HTTPS transport for the owner-managed node.
//! Pairing invitations are issued through local IPC; this module only redeems them.

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Query, State},
    http::{HeaderMap, Request as HttpRequest, Response, StatusCode, header},
    response::{IntoResponse, Json, Sse, sse::Event as SseEvent},
    routing::{any, get, post},
};
use brr_node_auth::{AuthError, DeviceRole, NodeAuthStore, Principal};
use brr_remote_protocol::{ApiError, Event, EventCursor, EventReplay, Reply, Request, Snapshot};
use futures_util::Stream;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque, convert::Infallible, future::Future, net::SocketAddr, pin::Pin,
    sync::Arc, time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
    time::{Instant, interval_at},
};
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

const PAIR_BODY_LIMIT: usize = 16 * 1024;
const REQUEST_BODY_LIMIT: usize = 32 * 1024 * 1024;
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);
const CONNECTION_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

pub type Dispatch =
    Arc<dyn Fn(Principal, Request) -> Pin<Box<dyn Future<Output = Reply> + Send>> + Send + Sync>;
pub type SnapshotCallback = Arc<
    dyn Fn(Principal) -> Pin<Box<dyn Future<Output = Result<Snapshot, ApiError>> + Send>>
        + Send
        + Sync,
>;
pub type GitCallback = Arc<
    dyn Fn(Principal, HttpRequest<Body>) -> Pin<Box<dyn Future<Output = Response<Body>> + Send>>
        + Send
        + Sync,
>;
pub type NodeObserverCallback = Arc<
    dyn Fn(Principal) -> Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send>> + Send + Sync,
>;

#[derive(Clone)]
pub struct NetworkApi {
    pub auth: NodeAuthStore,
    pub dispatch: Dispatch,
    pub snapshot: SnapshotCallback,
    pub events: crate::events::EventHub,
    pub git: Option<GitCallback>,
    node_observer: Option<NodeObserverCallback>,
}

impl NetworkApi {
    pub fn new(
        auth: NodeAuthStore,
        dispatch: Dispatch,
        snapshot: SnapshotCallback,
        events: crate::events::EventHub,
        git: Option<GitCallback>,
    ) -> Self {
        Self {
            auth,
            dispatch,
            snapshot,
            events,
            git,
            node_observer: None,
        }
    }

    pub fn with_node_observer(mut self, callback: NodeObserverCallback) -> Self {
        self.node_observer = Some(callback);
        self
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairInput {
    code: String,
    requested_role: DeviceRole,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairOutput {
    device_id: String,
    name: String,
    role: DeviceRole,
    node_id: String,
    secret: String,
    trusted_server_cert_pem: String,
    fingerprint: String,
}

#[derive(Deserialize)]
struct EventQuery {
    epoch: Option<String>,
    after: Option<u64>,
}

/// Serve TLS HTTP/1.1 and HTTP/2 on an already-bound listener. The TLS leaf and
/// chain come from the node's persistent `NodeAuthStore` identity.
pub async fn serve(
    listener: TcpListener,
    api: Arc<NetworkApi>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let tls = api
        .auth
        .tls_server_config()
        .map_err(|error| format!("Could not configure node TLS identity: {error}"))?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let router = router(api);
    let mut connections = JoinSet::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.map_err(|error| format!("Node listener failed: {error}"))?;
                let acceptor = acceptor.clone();
                let router = router.clone();
                let shutdown = shutdown.clone();
                connections.spawn(async move {
                    let _ = serve_connection(stream, peer, acceptor, router, shutdown).await;
                });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                let _ = completed;
            }
        }
    }
    let deadline = Instant::now() + CONNECTION_SHUTDOWN_GRACE;
    while !connections.is_empty() {
        if tokio::time::timeout_at(deadline, connections.join_next())
            .await
            .is_err()
        {
            break;
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

async fn serve_connection(
    stream: TcpStream,
    _peer: SocketAddr,
    acceptor: TlsAcceptor,
    router: Router,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    if *shutdown.borrow() {
        return Ok(());
    }
    let tls = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream))
        .await
        .map_err(|_| "TLS handshake timed out".to_string())?
        .map_err(|_| "TLS handshake failed".to_string())?;
    let io = TokioIo::new(tls);
    let service = TowerToHyperService::new(
        router.map_request(|request: HttpRequest<_>| request.map(Body::new)),
    );
    let builder = Builder::new(TokioExecutor::new());
    let connection = builder.serve_connection_with_upgrades(io, service);
    tokio::pin!(connection);
    if *shutdown.borrow() {
        connection.as_mut().graceful_shutdown();
        return connection
            .await
            .map_err(|_| "HTTPS connection shutdown failed".to_string());
    }
    loop {
        tokio::select! {
            result = &mut connection => return result.map_err(|_| "HTTPS connection ended with an error".to_string()),
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    connection.as_mut().graceful_shutdown();
                    return connection.await.map_err(|_| "HTTPS connection shutdown failed".to_string());
                }
            }
        }
    }
}

fn router(api: Arc<NetworkApi>) -> Router {
    Router::new()
        .route("/v1/pair", post(pair))
        .route("/v1/request", post(request))
        .route("/v1/snapshot", get(snapshot))
        .route("/v1/events", get(events))
        .route("/v1/git", any(git))
        .route("/v1/git/{*path}", any(git))
        .with_state(api)
}

type ApiResult<T> = Result<T, ApiResponseError>;

struct ApiResponseError(StatusCode, ApiError);

impl IntoResponse for ApiResponseError {
    fn into_response(self) -> Response<Body> {
        (self.0, Json(self.1)).into_response()
    }
}

fn api_error(status: StatusCode, code: &str, message: impl Into<String>) -> ApiResponseError {
    ApiResponseError(status, ApiError::new(code, message))
}

async fn pair(
    State(api): State<Arc<NetworkApi>>,
    request: HttpRequest<Body>,
) -> ApiResult<Json<PairOutput>> {
    let bytes = bounded_body(request.into_body(), PAIR_BODY_LIMIT).await?;
    let input: PairInput = serde_json::from_slice(&bytes).map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "invalid_pair_request",
            "Pairing request is invalid",
        )
    })?;
    if input.name.trim().is_empty() || input.name.len() > 128 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_pair_request",
            "Device name must contain 1 to 128 bytes",
        ));
    }
    let credential = api
        .auth
        .redeem_invite(&input.code, input.requested_role, &input.name)
        .await
        .map_err(pair_error)?;
    Ok(Json(PairOutput {
        device_id: credential.device_id,
        name: credential.name,
        role: credential.role,
        node_id: credential.node_id,
        secret: credential.secret,
        trusted_server_cert_pem: credential.trusted_server_certificate_pem,
        fingerprint: credential.fingerprint,
    }))
}

async fn request(
    State(api): State<Arc<NetworkApi>>,
    request: HttpRequest<Body>,
) -> ApiResult<Json<Reply>> {
    let principal = authorize(&api, request.headers()).await?;
    let bytes = bounded_body(request.into_body(), REQUEST_BODY_LIMIT).await?;
    let command: Request = serde_json::from_slice(&bytes).map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Request body is invalid",
        )
    })?;
    Ok(Json((api.dispatch)(principal, command).await))
}

async fn snapshot(
    State(api): State<Arc<NetworkApi>>,
    headers: HeaderMap,
) -> ApiResult<Json<Snapshot>> {
    let principal = authorize(&api, &headers).await?;
    authorize_node_observer(&api, &principal).await?;
    let snapshot = (api.snapshot)(principal)
        .await
        .map_err(api_response_error)?;
    Ok(Json(snapshot))
}

async fn events(
    State(api): State<Arc<NetworkApi>>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let secret = bearer(&headers).map(str::to_owned).ok_or_else(|| {
        api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid device bearer credential is required",
        )
    })?;
    let principal = authorize(&api, &headers).await?;
    authorize_node_observer(&api, &principal).await?;
    let supplied_cursor = match (query.epoch, query.after) {
        (Some(epoch), Some(sequence)) => Some(EventCursor { epoch, sequence }),
        (None, None) => None,
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "Both epoch and after are required",
            ));
        }
    };
    let receiver = api.events.subscribe();
    let replay = api.events.replay(supplied_cursor.as_ref());
    let mut initial = VecDeque::new();
    let cursor = if replay.requires_snapshot {
        initial.push_back(snapshot_required_event(&replay));
        EventCursor {
            epoch: replay.epoch.clone(),
            sequence: replay.latest,
        }
    } else {
        let events = replay.events;
        let cursor = events
            .last()
            .map(|event| EventCursor {
                epoch: event.epoch.clone(),
                sequence: event.sequence,
            })
            .or(supplied_cursor)
            .unwrap_or(EventCursor {
                epoch: replay.epoch.clone(),
                sequence: replay.latest,
            });
        initial.extend(events.into_iter().map(remote_event));
        cursor
    };
    let state = EventStreamState {
        api,
        secret,
        principal,
        receiver,
        cursor,
        initial,
        heartbeat: interval_at(Instant::now() + SSE_HEARTBEAT, SSE_HEARTBEAT),
    };
    Ok(Sse::new(event_stream(state)))
}

async fn git(State(api): State<Arc<NetworkApi>>, request: HttpRequest<Body>) -> Response<Body> {
    let principal = match authorize(&api, request.headers()).await {
        Ok(principal) => principal,
        Err(error) => return error.into_response(),
    };
    if principal.role != DeviceRole::Node {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Git access is available to paired nodes only",
        )
        .into_response();
    }
    let Some(callback) = api.git.as_ref() else {
        return api_error(
            StatusCode::NOT_FOUND,
            "unsupported_method",
            "Git transport is unavailable",
        )
        .into_response();
    };
    let (mut parts, body) = request.into_parts();
    parts.headers.remove(header::AUTHORIZATION);
    let request = HttpRequest::from_parts(parts, body);
    callback(principal, request).await
}

async fn authorize(api: &NetworkApi, headers: &HeaderMap) -> ApiResult<Principal> {
    let value = bearer(headers).ok_or_else(|| {
        api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid device bearer credential is required",
        )
    })?;
    api.auth.authenticate(value).await.map_err(|_| {
        api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid device bearer credential is required",
        )
    })
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|secret| !secret.is_empty())
}

async fn bounded_body(body: Body, limit: usize) -> ApiResult<Bytes> {
    to_bytes(body, limit).await.map_err(|_| {
        api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            "Request body exceeds the allowed size",
        )
    })
}

fn pair_error(error: AuthError) -> ApiResponseError {
    let (status, code) = match &error {
        AuthError::WrongRole => (StatusCode::FORBIDDEN, "wrong_role"),
        AuthError::InviteExpired => (StatusCode::GONE, "invite_expired"),
        AuthError::InviteAlreadyUsed => (StatusCode::CONFLICT, "invite_used"),
        AuthError::InvalidInvite => (StatusCode::BAD_REQUEST, "invalid_invite"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "pairing_failed"),
    };
    api_error(status, code, error.to_string())
}

fn api_response_error(error: ApiError) -> ApiResponseError {
    let status = match error.code.as_str() {
        "unauthorized" => StatusCode::UNAUTHORIZED,
        "forbidden" => StatusCode::FORBIDDEN,
        "not_found" => StatusCode::NOT_FOUND,
        "invalid_request" | "invalid_cursor" | "operation_id_required" | "epoch_required" => {
            StatusCode::BAD_REQUEST
        }
        "unsupported_method" => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiResponseError(status, error)
}

struct EventStreamState {
    api: Arc<NetworkApi>,
    secret: String,
    principal: Principal,
    receiver: tokio::sync::broadcast::Receiver<Event>,
    cursor: EventCursor,
    initial: VecDeque<SseEvent>,
    heartbeat: tokio::time::Interval,
}

fn event_stream(state: EventStreamState) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    futures_util::stream::unfold(state, |mut state| async move {
        if !stream_authorized(&state).await {
            return None;
        }
        if let Some(event) = state.initial.pop_front() {
            return Some((Ok(event), state));
        }
        loop {
            tokio::select! {
                _ = state.heartbeat.tick() => {
                    if !stream_authorized(&state).await { return None; }
                    return Some((Ok(SseEvent::default().comment("keep-alive")), state));
                }
                received = state.receiver.recv() => match received {
                    Ok(event) => {
                        if event.epoch != state.cursor.epoch || event.sequence > state.cursor.sequence.saturating_add(1) {
                            enqueue_replay(&mut state);
                            if let Some(event) = state.initial.pop_front() { return Some((Ok(event), state)); }
                            continue;
                        }
                        if event.epoch != state.cursor.epoch || event.sequence <= state.cursor.sequence { continue; }
                        state.cursor = EventCursor {epoch:event.epoch.clone(),sequence:event.sequence};
                        if !stream_authorized(&state).await { return None; }
                        return Some((Ok(remote_event(event)),state));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        enqueue_replay(&mut state);
                        if let Some(event) = state.initial.pop_front() { return Some((Ok(event),state)); }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    })
}

fn enqueue_replay(state: &mut EventStreamState) {
    let replay = state.api.events.replay(Some(&state.cursor));
    if replay.requires_snapshot {
        state.cursor = EventCursor {
            epoch: replay.epoch.clone(),
            sequence: replay.latest,
        };
        state.initial.push_back(snapshot_required_event(&replay));
    } else {
        for event in replay.events {
            if event.epoch == state.cursor.epoch && event.sequence > state.cursor.sequence {
                state.cursor = EventCursor {
                    epoch: event.epoch.clone(),
                    sequence: event.sequence,
                };
                state.initial.push_back(remote_event(event));
            }
        }
    }
}

async fn stream_authorized(state: &EventStreamState) -> bool {
    let Ok(principal) = state.api.auth.authenticate(&state.secret).await else {
        return false;
    };
    if principal.id != state.principal.id
        || principal.node_id != state.principal.node_id
        || principal.role != state.principal.role
    {
        return false;
    }
    if principal.role == DeviceRole::Node {
        let Some(callback) = state.api.node_observer.as_ref() else {
            return false;
        };
        callback(principal).await.is_ok()
    } else {
        true
    }
}

async fn authorize_node_observer(api: &NetworkApi, principal: &Principal) -> ApiResult<()> {
    if principal.role != DeviceRole::Node {
        return Ok(());
    }
    let Some(callback) = api.node_observer.as_ref() else {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Node observation is not configured for this endpoint",
        ));
    };
    callback(principal.clone())
        .await
        .map_err(api_response_error)
}

fn remote_event(event: Event) -> SseEvent {
    let id = format!("{}:{}", event.epoch, event.sequence);
    let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
    SseEvent::default().id(id).event("message").data(data)
}

fn snapshot_required_event(replay: &EventReplay) -> SseEvent {
    let id = format!("{}:{}", replay.epoch, replay.latest);
    let data = serde_json::to_string(replay).unwrap_or_else(|_| "{}".into());
    SseEvent::default()
        .id(id)
        .event("snapshot_required")
        .data(data)
}

#[cfg(test)]
mod observer_tests {
    use super::*;
    use axum::body::Bytes;
    use brr_remote_protocol::{EventCursor, NodeSummary, PROTOCOL_VERSION, SnapshotError};
    use futures_util::{StreamExt, stream};
    use std::{
        path::Path,
        sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        time::Duration,
    };
    use tempfile::tempdir;

    static GIT_BODY_CHUNK: [u8; 64 * 1024] = [0; 64 * 1024];
    const PRIOR_GIT_BODY_LIMIT: usize = 512 * 1024 * 1024;

    #[cfg(unix)]
    fn private(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn test_snapshot(node_id: String) -> Snapshot {
        Snapshot {
            protocol_version: PROTOCOL_VERSION,
            node_id: node_id.clone(),
            epoch: "runtime-1".into(),
            cursor: EventCursor {
                epoch: "runtime-1".into(),
                sequence: 0,
            },
            nodes: vec![NodeSummary {
                node_id,
                name: "worker".into(),
                platform: "test".into(),
                online: true,
                primary: false,
                authenticated: true,
                runtime_connected: false,
                epoch: "runtime-1".into(),
            }],
            account: None,
            models: vec![],
            threads: vec![],
            projects: vec![],
            agents: vec![],
            tasks: vec![],
            questions: vec![],
            approvals: vec![],
            sync: serde_json::json!({}),
            errors: vec![SnapshotError {
                scope: "runtime".into(),
                message: "offline".into(),
            }],
        }
    }

    async fn request(api: Arc<NetworkApi>, uri: &str, bearer: &str) -> StatusCode {
        let response = router(api)
            .oneshot(
                HttpRequest::builder()
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        response.status()
    }

    #[tokio::test]
    async fn git_route_forwards_large_authenticated_body_as_a_stream_without_bearer_header() {
        let dir = tempdir().unwrap();
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir(&auth_dir).unwrap();
        #[cfg(unix)]
        private(&auth_dir);
        let auth = NodeAuthStore::open(auth_dir.join("auth.sqlite"), vec!["localhost".into()])
            .await
            .unwrap();
        let invite = auth
            .create_invite(DeviceRole::Node, Duration::from_secs(60))
            .await
            .unwrap();
        let credential = auth
            .redeem_invite(&invite.code, DeviceRole::Node, "worker")
            .await
            .unwrap();
        let received = Arc::new(AtomicU64::new(0));
        let received_by_git = received.clone();
        let saw_bearer = Arc::new(AtomicBool::new(false));
        let saw_bearer_by_git = saw_bearer.clone();
        let callback: GitCallback = Arc::new(move |principal, request| {
            let received = received_by_git.clone();
            let saw_bearer = saw_bearer_by_git.clone();
            Box::pin(async move {
                assert_eq!(principal.role, DeviceRole::Node);
                saw_bearer.store(
                    request.headers().contains_key(header::AUTHORIZATION),
                    Ordering::SeqCst,
                );
                let mut body = request.into_body().into_data_stream();
                let mut count = 0u64;
                while let Some(chunk) = body.next().await {
                    count += chunk.unwrap().len() as u64;
                }
                received.store(count, Ordering::SeqCst);
                Response::new(Body::empty())
            })
        });
        let snapshot_node_id = auth.node_id().to_owned();
        let snapshot: SnapshotCallback = Arc::new(move |_| {
            let snapshot = test_snapshot(snapshot_node_id.clone());
            Box::pin(async move { Ok(snapshot) })
        });
        let dispatch: Dispatch = Arc::new(|_, request| {
            Box::pin(async move { Reply::from_result(request.id, Ok(serde_json::json!({}))) })
        });
        let events = crate::events::EventHub::new(auth.node_id().into(), "runtime-1".into(), 8);
        let api = Arc::new(NetworkApi::new(
            auth,
            dispatch,
            snapshot,
            events,
            Some(callback),
        ));
        let length = PRIOR_GIT_BODY_LIMIT as u64 + 1;
        let body = stream::unfold(length, |remaining| async move {
            if remaining == 0 {
                return None;
            }
            let count = remaining.min(GIT_BODY_CHUNK.len() as u64) as usize;
            Some((
                Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(&GIT_BODY_CHUNK[..count])),
                remaining - count as u64,
            ))
        });
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/v1/git/origin.git/git-receive-pack")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", credential.secret),
            )
            .body(Body::from_stream(body))
            .unwrap();
        let response = router(api).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(received.load(Ordering::SeqCst), length);
        assert!(!saw_bearer.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn node_snapshot_and_sse_require_configured_observer_authority() {
        let dir = tempdir().unwrap();
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir(&auth_dir).unwrap();
        #[cfg(unix)]
        private(&auth_dir);
        let auth = NodeAuthStore::open(auth_dir.join("auth.sqlite"), vec!["localhost".into()])
            .await
            .unwrap();
        let invite = auth
            .create_invite(DeviceRole::Node, Duration::from_secs(60))
            .await
            .unwrap();
        let node_credential = auth
            .redeem_invite(&invite.code, DeviceRole::Node, "worker")
            .await
            .unwrap();
        let node_secret = node_credential.secret;
        let mobile_invite = auth
            .create_invite(DeviceRole::Mobile, Duration::from_secs(60))
            .await
            .unwrap();
        let mobile_credential = auth
            .redeem_invite(&mobile_invite.code, DeviceRole::Mobile, "phone")
            .await
            .unwrap();
        let events = crate::events::EventHub::new(auth.node_id().into(), "runtime-1".into(), 16);
        let dispatch: Dispatch = Arc::new(|_, request| {
            Box::pin(async move { Reply::from_result(request.id, Ok(serde_json::json!({}))) })
        });
        let node_id = auth.node_id().to_owned();
        let snapshot: SnapshotCallback = Arc::new(move |_| {
            let snapshot = test_snapshot(node_id.clone());
            Box::pin(async move { Ok(snapshot) })
        });
        let api = Arc::new(NetworkApi::new(
            auth.clone(),
            dispatch.clone(),
            snapshot.clone(),
            events.clone(),
            None,
        ));
        assert_eq!(
            request(api.clone(), "/v1/snapshot", &node_secret).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(api, "/v1/events", &node_secret).await,
            StatusCode::FORBIDDEN
        );
        let api = Arc::new(NetworkApi::new(
            auth.clone(),
            dispatch.clone(),
            snapshot.clone(),
            events.clone(),
            None,
        ));
        assert_eq!(
            request(api, "/v1/snapshot", &mobile_credential.secret).await,
            StatusCode::OK
        );

        let called = Arc::new(AtomicUsize::new(0));
        let called_by_observer = called.clone();
        let observer: NodeObserverCallback = Arc::new(move |_| {
            called_by_observer.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
        let api = Arc::new(
            NetworkApi::new(
                auth.clone(),
                dispatch.clone(),
                snapshot.clone(),
                events.clone(),
                None,
            )
            .with_node_observer(observer),
        );
        assert_eq!(
            request(api.clone(), "/v1/snapshot", &node_secret).await,
            StatusCode::OK
        );
        assert_eq!(
            request(api.clone(), "/v1/events", &node_secret).await,
            StatusCode::OK
        );
        assert!(called.load(Ordering::SeqCst) >= 2);

        let rejecting: NodeObserverCallback = Arc::new(|_| {
            Box::pin(async { Err(ApiError::new("forbidden", "not the configured primary")) })
        });
        let api = Arc::new(
            NetworkApi::new(auth, dispatch, snapshot, events, None).with_node_observer(rejecting),
        );
        assert_eq!(
            request(api.clone(), "/v1/snapshot", &node_secret).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(api, "/v1/events", &node_secret).await,
            StatusCode::FORBIDDEN
        );
    }
}
