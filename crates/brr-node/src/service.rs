use crate::{
    events::EventHub,
    instance::{private_directory, InstanceLock},
    operations::{Claim, Operations, Outcome},
    peers::{JoinConfig, PeerMode, Peers},
};
use brr_host::{Host, HostConfig};
use brr_node_auth::NodeAuthStore;
use brr_remote_protocol::{
    ApiError, Channel, ClientRole, NodeSummary, Reply, Request, Snapshot, SnapshotError,
    PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Weak},
};
use tokio::sync::{Mutex, RwLock};
#[path = "entity_api.rs"]
mod entity_api;
#[path = "fleet_service.rs"]
mod fleet_service;
#[path = "history_attachments.rs"]
mod history_attachments;
#[path = "attachment_upload.rs"]
mod attachment_upload;
#[path = "peer_monitor.rs"]
mod peer_monitor;

pub struct ServiceConfig {
    pub data_dir: PathBuf,
    pub runtime_path: PathBuf,
    pub name: String,
    pub endpoint_hosts: Vec<String>,
}

pub struct Service {
    _instance: InstanceLock,
    host: Host,
    auth: NodeAuthStore,
    operations: Operations,
    events: EventHub,
    name: String,
    runtime_guard: Mutex<()>,
    last_runtime_error: Mutex<Option<String>>,
    resources: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    endpoint: Mutex<Option<String>>,
    browser: Mutex<Option<brr_browser::BrowserHost>>,
    peers: Peers,
    authority: RwLock<()>,
    catalog: Mutex<Option<crate::catalog::Catalog>>,
    git_http: Mutex<Option<crate::network::GitCallback>>,
    last_shared_error: Mutex<Option<String>>,
    last_sync: Mutex<Option<u64>>,
    attachment_uploads: attachment_upload::AttachmentUploadManager,
}
impl Service {
    pub async fn open(mut config: ServiceConfig) -> Result<Arc<Self>, ApiError> {
        if !config.data_dir.is_absolute() {
            config.data_dir = std::env::current_dir()
                .map_err(host_error)?
                .join(&config.data_dir);
        }
        private_directory(&config.data_dir)?;
        let node_dir = config.data_dir.join("node");
        let instance = InstanceLock::acquire(&node_dir)?;
        let endpoints = if config.endpoint_hosts.is_empty() {
            vec!["localhost".into()]
        } else {
            config.endpoint_hosts
        };
        let auth = NodeAuthStore::open(node_dir.join("auth.sqlite"), endpoints)
            .await
            .map_err(host_error)?;
        let events = EventHub::new(
            auth.node_id().to_owned(),
            uuid::Uuid::new_v4().to_string(),
            10_000,
        );
        let sink = {
            let events = events.clone();
            Arc::new(move |message| events.publish(message))
        };
        let host = Host::open(
            HostConfig {
                data_dir: config.data_dir,
                runtime_path: config.runtime_path,
            },
            sink,
        )
        .await
        .map_err(host_error)?;
        let attachment_uploads = attachment_upload::AttachmentUploadManager::open(
            &host.data_dir().join("engine/agents/chat"),
        )
        .map_err(host_error)?;
        let operations = Operations::open(&node_dir.join("operations.sqlite")).await?;
        let peers = Peers::open(node_dir.join("peers.sqlite")).await?;
        let service = Arc::new(Self {
            _instance: instance,
            host,
            auth,
            operations,
            events,
            name: config.name,
            runtime_guard: Mutex::new(()),
            last_runtime_error: Mutex::new(Some("The owned runtime has not connected yet".into())),
            resources: Mutex::new(HashMap::new()),
            endpoint: Mutex::new(None),
            browser: Mutex::new(None),
            peers,
            authority: RwLock::new(()),
            catalog: Mutex::new(None),
            git_http: Mutex::new(None),
            last_shared_error: Mutex::new(None),
            last_sync: Mutex::new(None),
            attachment_uploads,
        });
        if let Err(error) = service.restore_catalog().await {
            *service.last_shared_error.lock().await = Some(error.message);
        }
        // The caller starts inference after IPC/TLS listeners and shared-data
        // recovery are ready. A slow engine cannot block pairing or stores.
        Ok(service)
    }

    pub fn node_id(&self) -> &str {
        self.auth.node_id()
    }
    pub fn events(&self) -> EventHub {
        self.events.clone()
    }
    pub fn auth(&self) -> &NodeAuthStore {
        &self.auth
    }
    pub fn instance(&self) -> &InstanceLock {
        &self._instance
    }
    pub fn host(&self) -> &Host {
        &self.host
    }
    pub async fn configure_browser(&self, executable: PathBuf) {
        let browser = brr_browser::BrowserHost::new(brr_browser::BrowserConfig {
            executable,
            profile_dir: self.host.data_dir().join("node/browser"),
        });
        let owned = browser.clone();
        self.host
            .set_browser_handler(Arc::new(move |thread, args| {
                let browser = owned.clone();
                Box::pin(async move { browser.request(&thread, args).await })
            }))
            .await;
        *self.browser.lock().await = Some(browser);
    }
    pub async fn set_endpoint(&self, endpoint: String) -> Result<(), ApiError> {
        crate::settings::endpoint_url(&endpoint)?;
        *self.endpoint.lock().await = Some(endpoint);
        Ok(())
    }

    pub async fn connect_runtime(&self) -> Result<(), ApiError> {
        let _guard = self.runtime_guard.lock().await;
        if let Some(error) = self.last_shared_error.lock().await.clone() {
            return Err(ApiError::new("shared_recovery_required", error));
        }
        if self.host.is_connected().await {
            return Ok(());
        }
        self.events.rotate_epoch(uuid::Uuid::new_v4().to_string());
        match self.host.connect().await {
            Ok(()) => {
                *self.last_runtime_error.lock().await = None;
                Ok(())
            }
            Err(error) => {
                *self.last_runtime_error.lock().await = Some(error.clone());
                Err(host_error(error))
            }
        }
    }

    pub async fn start_runtime(&self) -> Result<(), ApiError> {
        let _guard = self.authority.read().await;
        self.connect_runtime().await
    }

    pub async fn handle(&self, request: Request, role: ClientRole, actor: &str) -> Reply {
        let result = self.execute(&request, role, actor).await;
        Reply::from_result(request.id, result)
    }

    async fn local_snapshot(&self) -> Result<Snapshot, ApiError> {
        // Replay begins at the start of the read so changes during this
        // multi-store snapshot cannot disappear behind its cursor.
        let snapshot_cursor = self.events.cursor();
        let stores = self.host.store_snapshot().await.map_err(host_error)?;
        let mut errors: Vec<SnapshotError> = stores
            .warnings
            .into_iter()
            .map(|message| SnapshotError {
                scope: "inventory".into(),
                message,
            })
            .collect();
        let mut agents = stores
            .shared
            .get("agent_profiles")
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(profile_view)
            .collect::<Vec<_>>();
        let mut tasks = stores
            .shared
            .get("tasks")
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(row_view)
            .collect::<Vec<_>>();
        let mut projects = self
            .host
            .project_request("list", json!({}))
            .await
            .map_err(host_error)?["data"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let connected = self.host.is_connected().await;
        let mut account = None;
        let mut models = Vec::new();
        let mut threads = Vec::new();
        let mut questions = Vec::new();
        let mut approvals = Vec::new();
        if connected {
            match self
                .host
                .request("account/read", json!({"refreshToken":false}))
                .await
            {
                Ok(value) if value["account"]["type"] == "chatgpt" => {
                    account = Some(value["account"].clone())
                }
                Ok(_) => {}
                Err(message) => errors.push(SnapshotError {
                    scope: "account".into(),
                    message,
                }),
            }
            match self
                .host
                .request("model/list", json!({"includeHidden":false}))
                .await
            {
                Ok(value) => models = value["data"].as_array().cloned().unwrap_or_default(),
                Err(message) => errors.push(SnapshotError {
                    scope: "models".into(),
                    message,
                }),
            }
            let mut cursor = None;
            let mut seen = std::collections::HashSet::new();
            loop {
                let mut params = json!({"limit":100,"sortKey":"updated_at"});
                if let Some(value) = cursor.take() {
                    params["cursor"] = Value::String(value);
                }
                match self.host.request("thread/list", params).await {
                    Ok(value) => {
                        threads.extend(value["data"].as_array().cloned().unwrap_or_default());
                        if let Some(next) = value["nextCursor"].as_str() {
                            if !seen.insert(next.to_owned()) {
                                errors.push(SnapshotError { scope:"threads".into(), message:"History pagination repeated a cursor; refresh before treating the list as complete".into() });
                                break;
                            }
                            cursor = Some(next.to_owned());
                        } else {
                            break;
                        }
                    }
                    Err(message) => {
                        errors.push(SnapshotError {
                            scope: "threads".into(),
                            message,
                        });
                        break;
                    }
                }
            }
            for (method, destination) in [
                ("agents.list", &mut agents),
                ("tasks.list", &mut tasks),
                ("questions.list", &mut questions),
            ] {
                match self
                    .host
                    .agents_request(
                        method,
                        if method == "questions.list" {
                            json!({"pendingOnly":true})
                        } else {
                            json!({})
                        },
                    )
                    .await
                {
                    Ok(value) => {
                        *destination = value["data"].as_array().cloned().unwrap_or_default()
                    }
                    Err(message) => errors.push(SnapshotError {
                        scope: method.into(),
                        message,
                    }),
                }
            }
            match self.approvals().await {
                Ok(value) => approvals = value,
                Err(error) => errors.push(SnapshotError {
                    scope: "approvals".into(),
                    message: error.message,
                }),
            }
        } else if let Some(message) = self.last_runtime_error.lock().await.clone() {
            errors.push(SnapshotError {
                scope: "runtime".into(),
                message,
            });
        }
        for items in [
            &mut agents,
            &mut tasks,
            &mut projects,
            &mut threads,
            &mut questions,
        ] {
            for item in items {
                self.annotate(item);
            }
        }
        let cursor = self.events.cursor();
        if cursor.epoch != snapshot_cursor.epoch {
            return Err(ApiError::new(
                "snapshot_changed",
                "The runtime restarted during refresh. Refresh again.",
            ));
        }
        let mut snapshot = Snapshot {
            protocol_version: PROTOCOL_VERSION,
            node_id: self.node_id().to_owned(),
            epoch: cursor.epoch.clone(),
            cursor: snapshot_cursor,
            nodes: vec![NodeSummary {
                node_id: self.node_id().to_owned(),
                name: self.name.clone(),
                platform: std::env::consts::OS.into(),
                online: true,
                primary: matches!(self.peers.mode().await?, PeerMode::Primary),
                authenticated: account.is_some(),
                runtime_connected: connected,
                epoch: cursor.epoch,
            }],
            account,
            models,
            threads,
            projects,
            agents,
            tasks,
            questions,
            approvals,
            sync: self.sync_view().await?,
            errors,
        };
        self.publicize_snapshot(&mut snapshot).await?;
        Ok(snapshot)
    }

    pub fn network_api(self: &Arc<Self>) -> Arc<crate::network::NetworkApi> {
        let dispatch = {
            let service = self.clone();
            Arc::new(
                move |principal: brr_node_auth::Principal, request: Request| {
                    let service = service.clone();
                    Box::pin(async move {
                        if principal.role == brr_node_auth::DeviceRole::Node {
                            if let Err(error) = service
                                .peers
                                .authorize_node_command(&principal, &request)
                                .await
                            {
                                return Reply::from_result(request.id, Err(error));
                            }
                        }
                        let role = match principal.role {
                            brr_node_auth::DeviceRole::Mobile => ClientRole::Mobile,
                            brr_node_auth::DeviceRole::Node => ClientRole::Node,
                        };
                        service.handle(request, role, &principal.id).await
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = Reply> + Send>>
                },
            )
        };
        let snapshot = {
            let service = self.clone();
            Arc::new(move |principal: brr_node_auth::Principal| {
                let service = service.clone();
                Box::pin(async move {
                    let _guard = service.authority.read().await;
                    if principal.role == brr_node_auth::DeviceRole::Node {
                        service.local_snapshot().await
                    } else {
                        service.snapshot().await
                    }
                })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<Snapshot, ApiError>> + Send>,
                    >
            })
        };
        let git: crate::network::GitCallback = {
            let service = self.clone();
            Arc::new(move |principal, request| {
                let service = service.clone();
                Box::pin(async move {
                    let callback = service.git_http.lock().await.clone();
                    if let Some(callback) = callback {
                        callback(principal, request).await
                    } else {
                        let mut response = axum::http::Response::new(axum::body::Body::from(
                            "Shared Git is not configured",
                        ));
                        *response.status_mut() = axum::http::StatusCode::NOT_FOUND;
                        response
                    }
                })
            })
        };
        let observer: crate::network::NodeObserverCallback = {
            let service = self.clone();
            Arc::new(move |principal| {
                let service = service.clone();
                Box::pin(async move { service.peers.authorize_node_observer(&principal).await })
            })
        };
        Arc::new(
            crate::network::NetworkApi::new(
                self.auth.clone(),
                dispatch,
                snapshot,
                self.events.clone(),
                Some(git),
            )
            .with_node_observer(observer),
        )
    }

    async fn execute(
        &self,
        request: &Request,
        role: ClientRole,
        actor: &str,
    ) -> Result<Value, ApiError> {
        request.validate(role)?;
        let policy = request.policy()?;
        let approval_identity = canonical_approval_identity(request)?;
        let exclusive = matches!(
            (request.channel, request.method.as_str()),
            (
                Channel::Fleet,
                "init" | "join" | "node.remove" | "device.revoke"
            )
        ) || (request.channel == Channel::Sync && policy.mutating);
        let _write = if exclusive {
            Some(self.authority.write().await)
        } else {
            None
        };
        let _read = if !exclusive {
            Some(self.authority.read().await)
        } else {
            None
        };
        if let Some(target) = request
            .target_node_id
            .as_deref()
            .filter(|node| *node != self.node_id())
        {
            if matches!(
                (request.channel, request.method.as_str()),
                (
                    Channel::Fleet,
                    "init"
                        | "join"
                        | "node.register"
                        | "node.remove"
                        | "device.revoke"
                        | "invitation.create"
                ) | (Channel::Sync, "import")
            ) {
                return Err(ApiError::new(
                    "invalid_target",
                    "Fleet setup and removal must be handled by this service",
                ));
            }
            let reply = match self.peers.route(request.clone(), actor).await {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    return self
                        .historical_response(request, target)
                        .await?
                        .ok_or_else(|| {
                            ApiError::new(
                                "owner_unavailable",
                                "The requested execution owner is unavailable",
                            )
                        })
                }
                Err(error) => {
                    if matches!(
                        error.code.as_str(),
                        "owner_offline" | "owner_unavailable" | "uncertain"
                    ) {
                        if let Some(history) = self.historical_response(request, target).await? {
                            return Ok(history);
                        }
                    }
                    return Err(error);
                }
            };
            if let Some(error) = reply.error {
                if matches!(
                    error.code.as_str(),
                    "owner_offline"
                        | "owner_unavailable"
                        | "shared_recovery_required"
                        | "unavailable"
                ) {
                    if let Some(history) = self.historical_response(request, target).await? {
                        return Ok(history);
                    }
                }
                return Err(error);
            }
            return reply.result.ok_or_else(|| {
                ApiError::new("invalid_reply", "The execution owner returned no result")
            });
        }
        if policy.mutating
            && !matches!(
                request.channel,
                Channel::Fleet | Channel::Sync | Channel::System
            )
        {
            if let Some(error) = self.last_shared_error.lock().await.clone() {
                return Err(ApiError::new("shared_recovery_required", error));
            }
        }
        let resource = if policy.mutating || request.channel == Channel::Desktop {
            Some(
                self.resource(request, approval_identity.as_deref())
                    .await
                    .lock_owned()
                    .await,
            )
        } else {
            None
        };
        if policy.mutating {
            match self.operations.begin(actor, request).await? {
                Claim::New => {}
                Claim::Replay(outcome) => return outcome.into_result(),
                Claim::Uncertain => {
                    if role == ClientRole::Node
                        && request.channel == Channel::Fleet
                        && request.method == "node.register"
                    {
                        let principal = brr_node_auth::Principal {
                            id: actor.into(),
                            name: "Fleet node".into(),
                            role: brr_node_auth::DeviceRole::Node,
                            node_id: self.node_id().into(),
                        };
                        if let Some(receipt) = self
                            .peers
                            .reconcile_registration(&principal, &request.params)
                            .await?
                        {
                            self.operations
                                .finish(actor, request, Outcome::from_result(Ok(receipt.clone())))
                                .await?;
                            return Ok(receipt);
                        }
                    }
                    return Err(ApiError::new("uncertain", "This operation was started but its result is unconfirmed. Reconcile its state before retrying."));
                }
            }
        }
        let generation = if policy.live {
            Some(self.runtime_guard.lock().await)
        } else {
            None
        };
        let result = if policy.live
            && request.expected_epoch.as_deref() != Some(self.events.cursor().epoch.as_str())
        {
            Err(ApiError::new(
                "stale_epoch",
                "The runtime restarted. Refresh this decision before responding.",
            ))
        } else {
            async {
                let localized=self.localize_scoped(request).await?;
                let mut response=self.dispatch(&localized,role,actor).await?;
                if let Err(error)=self.publicize_scoped(request,&mut response).await {
                    if policy.mutating {return Err(ApiError::new("uncertain","The owner handled the command but could not read its result metadata. Refresh before retrying."));}
                    return Err(error);
                }
                Ok(response)
            }.await
        };
        if policy.mutating {
            self.operations
                .finish(actor, request, Outcome::from_result(result.clone()))
                .await?;
        }
        drop(generation);
        drop(resource);
        result
    }

    async fn resource(&self, request: &Request, approval_identity: Option<&str>) -> Arc<Mutex<()>> {
        let mut resources = self.resources.lock().await;
        resources.retain(|_, lock| lock.strong_count() > 0);
        let params = if request.channel == Channel::Desktop
            && matches!(
                request.method.as_str(),
                "engine_request" | "chat_request" | "agents_request"
            ) {
            &request.params["params"]
        } else {
            &request.params
        };
        let key = if request.channel == Channel::Approvals {
            format!("approval:{}", approval_identity.unwrap_or("missing"))
        } else if request.channel == Channel::Desktop && request.method == "engine_respond" {
            if let Some(id) = params.get("requestId") {
                format!("approval:{}", approval_id(id))
            } else if let Some(id) = params.get("id") {
                format!("approval:{}", approval_id(id))
            } else {
                "approval:missing".into()
            }
        } else if let Some(thread) = params["threadId"].as_str() {
            format!("thread:{thread}")
        } else if let Some(task) = params["taskId"].as_str() {
            format!("task:{task}")
        } else if let Some(id) = params["requestId"].as_str() {
            format!("question:{id}")
        } else {
            format!(
                "{:?}:{}:{}",
                request.channel,
                request.method,
                request.operation_id.as_deref().unwrap_or(&request.id)
            )
        };
        if let Some(lock) = resources.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        resources.insert(key, Arc::downgrade(&lock));
        lock
    }

    async fn dispatch(
        &self,
        request: &Request,
        role: ClientRole,
        actor: &str,
    ) -> Result<Value, ApiError> {
        match (request.channel, request.method.as_str()) {
            (Channel::Fleet, "status" | "nodes") => Ok(
                json!({"mode":self.peers.mode().await?,"nodeId":self.node_id(),"data":self.peers.list_peers().await?}),
            ),
            (Channel::Fleet, "init") => self.activate_catalog(self.node_id(), None).await,
            (Channel::Fleet, "join") => {
                let invitation = request.params.get("invitation").unwrap_or(&request.params);
                let endpoint = self.endpoint.lock().await.clone().ok_or_else(|| {
                    ApiError::new(
                        "listener_unavailable",
                        "Start the remote TLS listener before joining a fleet",
                    )
                })?;
                let primary = required(invitation, "nodeId")?;
                if primary == self.node_id() {
                    return Err(ApiError::new("invalid_target", "A node cannot join itself"));
                }
                self.activate_catalog(
                    primary,
                    Some(JoinConfig {
                        primary_endpoint: required(invitation, "endpoint")?.into(),
                        primary_node_id: required(invitation, "nodeId")?.into(),
                        primary_ca_pem: required(invitation, "certificatePem")?.into(),
                        primary_fingerprint: required(invitation, "fingerprint")?.into(),
                        invite_code: required(invitation, "code")?.into(),
                        name: self.name.clone(),
                        node_endpoint: endpoint,
                        reverse_invite_ttl: std::time::Duration::from_secs(300),
                    }),
                )
                .await
            }
            (Channel::Fleet, "node.register") => {
                if role != ClientRole::Node {
                    return Err(ApiError::new(
                        "forbidden",
                        "Node registration requires a paired node credential",
                    ));
                }
                let result = self
                    .peers
                    .register_reverse(
                        &brr_node_auth::Principal {
                            id: actor.into(),
                            name: "Fleet node".into(),
                            role: brr_node_auth::DeviceRole::Node,
                            node_id: self.node_id().into(),
                        },
                        request.params.clone(),
                    )
                    .await?;
                self.events
                    .publish(json!({"method":"brr/fleet/changed","params":{}}));
                Ok(result)
            }
            (Channel::Fleet, "node.remove") => {
                let node = required(&request.params, "nodeId")?;
                if role == ClientRole::Node && self.peers.mode().await? == PeerMode::Node {
                    self.peers
                        .leave(
                            &self.auth,
                            &brr_node_auth::Principal {
                                id: actor.into(),
                                name: "Fleet primary".into(),
                                role: brr_node_auth::DeviceRole::Node,
                                node_id: self.node_id().into(),
                            },
                            &request.params,
                        )
                        .await?;
                    self.events
                        .publish(json!({"method":"brr/fleet/changed","params":{}}));
                    Ok(json!({"removed":true,"nodeId":node}))
                } else if role == ClientRole::Node {
                    Err(ApiError::new(
                        "forbidden",
                        "A joined node cannot remove another node from the primary",
                    ))
                } else {
                    let result = self.peers.remove(node, &self.auth).await?;
                    self.events
                        .publish(json!({"method":"brr/fleet/changed","params":{}}));
                    Ok(
                        json!({"removed":true,"nodeId":result.node_id,"remoteRevocationPending":result.remote_revocation_pending}),
                    )
                }
            }
            (Channel::System, "status") => Ok(json!({
                "protocolVersion":PROTOCOL_VERSION,"nodeId":self.node_id(),"name":self.name,"pid":std::process::id(),
                "platform":std::env::consts::OS,"epoch":self.events.cursor().epoch,
                "runtimeConnected":self.host.is_connected().await,
                "runtimeError":self.last_runtime_error.lock().await.clone(),
                "sharedConfigured":self.catalog.lock().await.is_some() && self.last_shared_error.lock().await.is_none(),
                "endpoint":self.endpoint.lock().await.clone(),
            })),
            (Channel::Fleet, "invitation.create") => {
                if role != ClientRole::Desktop {
                    return Err(ApiError::new(
                        "forbidden",
                        "Pairing invitations require local owner IPC",
                    ));
                }
                let device_role = match request.params["role"].as_str().unwrap_or("mobile") {
                    "mobile" => brr_node_auth::DeviceRole::Mobile,
                    "node" => brr_node_auth::DeviceRole::Node,
                    _ => {
                        return Err(ApiError::new(
                            "invalid_request",
                            "Choose mobile or node invitation role",
                        ))
                    }
                };
                let endpoint = request.params["endpoint"]
                    .as_str()
                    .map(str::to_owned)
                    .or(self.endpoint.lock().await.clone())
                    .ok_or_else(|| {
                        ApiError::new(
                            "listener_unavailable",
                            "The remote TLS listener is unavailable",
                        )
                    })?;
                let url = crate::settings::endpoint_url(&endpoint)?;
                if !self
                    .auth
                    .identity_info()
                    .endpoints
                    .iter()
                    .any(|host| url.host_str() == Some(host.trim_matches(['[', ']'])))
                {
                    return Err(ApiError::new("invalid_endpoint","The advertised host is missing from this node's TLS certificate. Configure --advertise-host and restart the service."));
                }
                let ttl = request.params["expiresInSeconds"]
                    .as_u64()
                    .unwrap_or(300)
                    .clamp(1, 900);
                let invitation = self
                    .auth
                    .create_invite(device_role, std::time::Duration::from_secs(ttl))
                    .await
                    .map_err(host_error)?;
                Ok(
                    json!({"endpoint":endpoint,"nodeId":invitation.node_id,"name":request.params["name"].as_str().unwrap_or(&self.name),"role":device_role,"code":invitation.code,"expiresAt":invitation.expires_at_unix,"certificatePem":invitation.server_certificate_pem,"fingerprint":invitation.fingerprint}),
                )
            }
            (Channel::Fleet, "devices") => {
                Ok(json!({"data":self.auth.list_devices().await.map_err(host_error)?}))
            }
            (Channel::Fleet, "device.revoke") => {
                let device = required(&request.params, "deviceId")?;
                if let Some(peer) = self
                    .peers
                    .list_peers()
                    .await?
                    .into_iter()
                    .find(|peer| peer.primary_device_id == device)
                {
                    self.peers.remove(&peer.node_id, &self.auth).await?;
                } else {
                    self.auth.revoke(device).await.map_err(host_error)?;
                }
                self.events
                    .publish(json!({"method":"brr/devices/changed","params":{}}));
                Ok(json!({"revoked":true}))
            }
            (Channel::Projects, "list") => {
                let mut result = self
                    .host
                    .project_request("list", json!({}))
                    .await
                    .map_err(host_error)?;
                if let Some(projects) = result["data"].as_array_mut() {
                    for project in projects {
                        self.annotate(project);
                    }
                }
                self.annotate_scoped_response(&mut result);
                Ok(result)
            }
            (Channel::Sync, _) => self.sync_request(request, role).await,
            (Channel::Projects, "create") => {
                let mut project = request.params.as_object().cloned().ok_or_else(|| {
                    ApiError::new("invalid_request", "Project parameters must be an object")
                })?;
                if role != ClientRole::Desktop
                    && (project
                        .get("folders")
                        .is_some_and(|value| value != &json!([]))
                        || project
                            .get("primaryFolder")
                            .is_some_and(|value| !value.is_null()))
                {
                    return Err(ApiError::new(
                        "local_setup_required",
                        "Configure this node's project folders using its desktop or CLI",
                    ));
                }
                for (key, value) in [
                    ("folders", json!([])),
                    ("primaryFolder", Value::Null),
                    ("model", Value::Null),
                    ("effort", Value::Null),
                    ("permission", Value::Null),
                ] {
                    project.entry(key).or_insert(value);
                }
                let mut result = self
                    .host
                    .project_request("create", Value::Object(project))
                    .await
                    .map_err(host_error)?;
                self.annotate(&mut result);
                self.annotate_scoped_response(&mut result);
                Ok(result)
            }
            (Channel::Projects, "save") => {
                let fields = request.params.as_object().ok_or_else(|| {
                    ApiError::new("invalid_request", "Project parameters must be an object")
                })?;
                if fields.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "id" | "name" | "model" | "effort" | "permission"
                    )
                }) {
                    return Err(ApiError::new(
                        "invalid_request",
                        "Only project metadata fields may be saved from a remote client",
                    ));
                }
                let id = required(&request.params, "id")?;
                let expected_revision = expected_project_revision(request)?;
                let existing = self
                    .host
                    .project_request("list", json!({}))
                    .await
                    .map_err(host_error)?["data"]
                    .as_array()
                    .and_then(|projects| projects.iter().find(|project| project["id"] == id))
                    .cloned()
                    .ok_or_else(|| ApiError::new("not_found", "Project does not exist"))?;
                let name = match request.params.get("name") {
                    Some(Value::String(value)) => value.clone(),
                    None => existing["name"]
                        .as_str()
                        .ok_or_else(|| ApiError::new("invalid_request", "Missing project name"))?
                        .to_owned(),
                    Some(_) => {
                        return Err(ApiError::new(
                            "invalid_request",
                            "Project name must be a string",
                        ));
                    }
                };
                let model = optional_project_string(&request.params, &existing, "model")?;
                let effort = optional_project_string(&request.params, &existing, "effort")?;
                let permission = match request.params.get("permission") {
                    None => {
                        serde_json::from_value(existing["permission"].clone()).map_err(|_| {
                            ApiError::new("invalid_request", "Invalid project permission")
                        })?
                    }
                    Some(Value::Null) => None,
                    Some(value) => Some(serde_json::from_value(value.clone()).map_err(|_| {
                        ApiError::new("invalid_request", "Invalid project permission")
                    })?),
                };
                let mut result = serde_json::to_value(
                    self.host
                        .project_save_metadata(
                            id,
                            expected_revision,
                            name,
                            model,
                            effort,
                            permission,
                        )
                        .await
                        .map_err(host_error)?,
                )
                .map_err(host_error)?;
                self.annotate(&mut result);
                self.annotate_scoped_response(&mut result);
                Ok(result)
            }
            (Channel::Projects, "delete") => {
                let id = required(&request.params, "projectId")?;
                self.host
                    .project_delete_expected_revision(id, expected_project_revision(request)?)
                    .await
                    .map_err(host_error)?;
                Ok(Value::Null)
            }
            (Channel::Projects, "moveChat") => {
                let thread_id = required(&request.params, "threadId")?;
                self.connect_runtime().await?;
                let read = self
                    .host
                    .request("thread/read", json!({"threadId":thread_id}))
                    .await
                    .map_err(host_error)?;
                let actual_id = read["thread"]["id"]
                    .as_str()
                    .or(read["id"].as_str())
                    .ok_or_else(|| {
                        ApiError::new("not_found", "Thread does not exist on this node")
                    })?;
                if actual_id != thread_id {
                    return Err(ApiError::new(
                        "not_found",
                        "Thread does not exist on this node",
                    ));
                }
                self.host
                    .project_request(
                        "move",
                        json!({"threadId":thread_id,"projectId":request.params.get("projectId").cloned().unwrap_or(Value::Null)}),
                    )
                    .await
                    .map_err(host_error)
            }
            (Channel::Sessions, _) => self.session_request(request, actor).await,
            (Channel::Chat, method) => {
                self.connect_runtime().await?;
                let mut params = request.params.clone();
                if method == "foreground.submit" {
                    params["id"] = json!(submission_id(
                        actor,
                        request.operation_id.as_deref().unwrap_or(&request.id)
                    ));
                }
                let mut result = self
                    .host
                    .chat_request(method, params)
                    .await
                    .map_err(host_error)?;
                self.annotate_scoped_response(&mut result);
                Ok(result)
            }
            (Channel::Agents, method) => {
                self.connect_runtime().await?;
                let mut result = self
                    .host
                    .agents_request_with_expected_revision(
                        method,
                        request.params.clone(),
                        request.expected_revision.as_deref(),
                    )
                    .await
                    .map_err(host_error)?;
                self.annotate_scoped_response(&mut result);
                Ok(result)
            }
            (Channel::Questions, method) => {
                self.connect_runtime().await?;
                let method = if method == "draft" {
                    "questions.saveDraft".to_owned()
                } else {
                    format!("questions.{method}")
                };
                let mut result = self
                    .host
                    .agents_request_with_expected_revision(
                        &method,
                        request.params.clone(),
                        request.expected_revision.as_deref(),
                    )
                    .await
                    .map_err(host_error)?;
                self.annotate_scoped_response(&mut result);
                Ok(result)
            }
            (Channel::Approvals, "list") => Ok(json!({"data":self.approvals().await?})),
            (Channel::Approvals, "answer") => self.answer_approval(request).await,
            (Channel::System, "account" | "models" | "login.status") => {
                self.connect_runtime().await?;
                let (method, params) = if request.method == "models" {
                    ("model/list", json!({"includeHidden":false}))
                } else {
                    ("account/read", json!({"refreshToken":false}))
                };
                self.host.request(method, params).await.map_err(host_error)
            }
            (Channel::System, "login.start") => {
                if request.params.as_object().is_none_or(|p| !p.is_empty()) {
                    return Err(ApiError::new("invalid_request", "Managed ChatGPT device login accepts no credentials or provider configuration"));
                }
                self.connect_runtime().await?;
                self.host
                    .request("account/login/start", json!({"type":"chatgptDeviceCode"}))
                    .await
                    .map_err(host_error)
            }
            (Channel::System, "login.cancel" | "logout") => {
                self.connect_runtime().await?;
                let method = if request.method == "logout" {
                    "account/logout"
                } else {
                    "account/login/cancel"
                };
                self.host
                    .request(method, request.params.clone())
                    .await
                    .map_err(host_error)
            }
            (Channel::Desktop, _) => self.desktop_request(request).await,
            _ => Err(ApiError::new(
                "unsupported_method",
                "This service command is unavailable",
            )),
        }
    }

    async fn session_request(&self, request: &Request, actor: &str) -> Result<Value, ApiError> {
        let p = &request.params;
        if matches!(
            request.method.as_str(),
            "attachment.preview" | "attachment.read"
        ) {
            return self.local_attachment_response(request).await;
        }
        if matches!(request.method.as_str(), "stop" | "steer") {
            if !self.host.is_connected().await {
                return Err(ApiError::new(
                    "unavailable",
                    "The owning runtime is disconnected",
                ));
            }
        } else if let Err(error) = self.connect_runtime().await {
            if let Some(history) = self.historical_response(request, self.node_id()).await? {
                return Ok(history);
            }
            return Err(error);
        }
        match request.method.as_str() {
            "list" => {
                let mut params = json!({"limit":p["limit"].as_u64().unwrap_or(100).clamp(1,100),"sortKey":"updated_at"});
                if let Some(cursor) = p["cursor"].as_str() {
                    params["cursor"] = json!(cursor);
                }
                let mut result = self
                    .host
                    .request("thread/list", params)
                    .await
                    .map_err(host_error)?;
                if let Some(threads) = result["data"].as_array_mut() {
                    for thread in threads {
                        self.annotate(thread);
                    }
                }
                Ok(result)
            }
            "read" => {
                let result = self
                    .host
                    .request(
                        "thread/read",
                        json!({"threadId":required(p,"threadId")?,"includeTurns":true}),
                    )
                    .await
                    .map_err(host_error)?;
                let mut thread = result.get("thread").cloned().unwrap_or(result);
                match self.host.export_history_thread(required(p, "threadId")?, None).await {
                    Ok(history) => thread["attachments"] = history["attachments"].clone(),
                    Err(_) => thread["attachmentError"] = json!("Saved attachments could not be verified. Refresh this conversation to try again."),
                }
                self.annotate(&mut thread);
                Ok(thread)
            }
            "turns" => {
                let mut params = json!({"threadId":required(p,"threadId")?,"limit":100,"sortDirection":"asc","itemsView":"full"});
                if let Some(cursor) = p["cursor"].as_str() {
                    params["cursor"] = json!(cursor);
                }
                self.host
                    .request("thread/turns/list", params)
                    .await
                    .map_err(host_error)
            }
            "start" | "resume" => {
                let mut params = if request.method == "start" {
                    json!({"projectId":p.get("projectId").cloned().unwrap_or(Value::Null),"sandbox":if p["readOnly"]==true {"read-only"} else {"workspace-write"}})
                } else {
                    json!({"threadId":required(p,"threadId")?})
                };
                params["modelProvider"] = json!("openai");
                params["approvalPolicy"] = json!("on-request");
                if let Some(model) = p["model"].as_str().filter(|model| !model.is_empty()) {
                    params["model"] = json!(model);
                }
                let method = if request.method == "start" {
                    "thread/start"
                } else {
                    "thread/resume"
                };
                let mut result = self
                    .host
                    .request(method, params)
                    .await
                    .map_err(host_error)?;
                if request.method == "start" {
                    if let Some(text) = p["text"].as_str().filter(|text| !text.trim().is_empty()) {
                        let id = required(&result["thread"], "id")?.to_owned();
                        let submission = self.submit(&id, text, p, actor, request, false).await?;
                        result["submission"] = submission;
                        if let Ok(read) = self
                            .host
                            .request("thread/read", json!({"threadId":id,"includeTurns":true}))
                            .await
                        {
                            if read["thread"].is_object() {
                                result["thread"] = read["thread"].clone();
                            }
                        }
                    }
                }
                self.annotate(&mut result["thread"]);
                Ok(result)
            }
            "send" | "steer" => {
                self.submit(
                    required(p, "threadId")?,
                    required(p, "text")?,
                    p,
                    actor,
                    request,
                    request.method == "steer",
                )
                .await
            }
            "stop" => self
                .host
                .request(
                    "turn/interrupt",
                    json!({"threadId":required(p,"threadId")?,"turnId":required(p,"turnId")?}),
                )
                .await
                .map_err(host_error),
            "rename" => self
                .host
                .request(
                    "thread/name/set",
                    json!({"threadId":required(p,"threadId")?,"name":required(p,"name")?}),
                )
                .await
                .map_err(host_error),
            "archive" => self
                .host
                .request(
                    &format!("thread/{}", request.method),
                    json!({"threadId":required(p,"threadId")?}),
                )
                .await
                .map_err(host_error),
            "delete" => {
                let id = required(p, "threadId")?;
                let result = self
                    .host
                    .request("thread/delete", json!({"threadId":id}))
                    .await
                    .map_err(host_error)?;
                self.tombstone_thread(id).await?;
                Ok(result)
            }
            _ => Err(ApiError::new(
                "unsupported_method",
                "Unknown session operation",
            )),
        }
    }

    async fn submit(
        &self,
        thread: &str,
        text: &str,
        params: &Value,
        actor: &str,
        request: &Request,
        steer: bool,
    ) -> Result<Value, ApiError> {
        let mode = params["mode"].as_str().unwrap_or("default");
        if !matches!(mode, "default" | "plan") {
            return Err(ApiError::new(
                "invalid_request",
                "Choose Default or Plan mode",
            ));
        }
        let message_id = submission_id(
            actor,
            request.operation_id.as_deref().unwrap_or(&request.id),
        );
        let binding = self.host.agent_store().session_binding(thread).await.ok();
        if binding
            .as_ref()
            .is_some_and(|binding| matches!(binding.kind.as_str(), "interview" | "profile"))
        {
            let mut turn_params = json!({
                "threadId": thread,
                "input": [{"type":"text", "text":text, "text_elements":[]}],
                "clientUserMessageId": message_id,
            });
            if steer {
                turn_params["expectedTurnId"] = json!(required(params, "expectedTurnId")?);
            } else {
                let selected_model = params["model"]
                    .as_str()
                    .filter(|model| !model.is_empty() && *model != "_runtime_default")
                    .map(str::to_owned);
                let model = if selected_model.is_some() {
                    selected_model
                } else if mode == "plan" {
                    let thread = self
                        .host
                        .request("thread/read", json!({"threadId":thread}))
                        .await
                        .ok()
                        .and_then(|response| {
                            response["thread"]["model"]
                                .as_str()
                                .filter(|model| !model.is_empty() && *model != "_runtime_default")
                                .map(str::to_owned)
                        });
                    match thread {
                        Some(model) => Some(model),
                        None => {
                            let models = self
                                .host
                                .request("model/list", json!({"includeHidden":false}))
                                .await
                                .map_err(host_error)?;
                            models["data"]
                                .as_array()
                                .and_then(|models| {
                                    models
                                        .iter()
                                        .find(|model| model["isDefault"] == true)
                                        .or_else(|| models.first())
                                })
                                .and_then(|model| {
                                    model["model"].as_str().or_else(|| model["id"].as_str())
                                })
                                .filter(|model| !model.is_empty() && *model != "_runtime_default")
                                .map(str::to_owned)
                        }
                    }
                } else {
                    None
                };
                if let Some(model) = model {
                    let model = model.as_str();
                    let effort = params["effort"].as_str().unwrap_or("medium");
                    turn_params["model"] = json!(model);
                    turn_params["effort"] = json!(effort);
                    turn_params["collaborationMode"] = json!({
                        "mode": mode,
                        "settings": {"model": model, "reasoning_effort": effort, "developer_instructions": null}
                    });
                } else if mode == "plan" {
                    return Err(ApiError::new(
                        "invalid_request",
                        "Plan mode requires an available model",
                    ));
                }
            }
            let method = if steer { "turn/steer" } else { "turn/start" };
            return self
                .host
                .request(method, turn_params)
                .await
                .map_err(host_error);
        }
        let model = params["model"]
            .as_str()
            .filter(|model| !model.is_empty())
            .unwrap_or("_runtime_default");
        let mut submission = json!({"id":message_id,"threadId":thread,"document":{"version":1,"text":text,"references":[],"quotes":[]},"attachmentIds":[],"settings":{"model":model,"effort":params["effort"].as_str().unwrap_or("medium"),"mode":mode,"readOnly":params["readOnly"].as_bool().unwrap_or(false)},"delivery":if steer {"steer"} else {"send"}});
        if steer {
            submission["expectedTurnId"] = json!(required(params, "expectedTurnId")?);
        }
        let result = self
            .host
            .chat_request("foreground.submit", submission)
            .await
            .map_err(host_error)?;
        if result["submission"]["status"] == "failed" {
            return Err(ApiError::new(
                "submission_failed",
                result["submission"]["error"]
                    .as_str()
                    .unwrap_or("The message was retained but was not delivered"),
            ));
        }
        Ok(result)
    }

    pub fn annotate(&self, value: &mut Value) {
        if let Some(object) = value.as_object_mut() {
            object.insert("ownerNodeId".into(), json!(self.node_id()));
            object.insert("ownerEpoch".into(), json!(self.events.cursor().epoch));
        }
    }

    fn annotate_scoped_response(&self, value: &mut Value) {
        annotate_owned_records(value, self.node_id(), &self.events.cursor().epoch);
    }

    async fn approvals(&self) -> Result<Vec<Value>, ApiError> {
        if !self.host.is_connected().await {
            return Ok(Vec::new());
        }
        let decisions = self
            .host
            .agents_request("agents.decisions", json!({}))
            .await
            .map_err(host_error)?;
        let mut approvals = decisions["runtimeRequests"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for approval in &mut approvals {
            let id = approval["id"].clone();
            approval["requestId"] = id.clone();
            approval["id"] = json!(approval_id(&id));
            approval["epoch"] = json!(self.events.cursor().epoch);
            self.annotate(approval);
        }
        Ok(approvals)
    }

    async fn answer_approval(&self, request: &Request) -> Result<Value, ApiError> {
        if !self.host.is_connected().await {
            return Err(ApiError::new(
                "unavailable",
                "The owning runtime is disconnected",
            ));
        }
        let p = &request.params;
        let identity = canonical_approval_identity(request)?
            .ok_or_else(|| ApiError::new("invalid_request", "An approval identity is required"))?;
        let approvals = self.approvals().await?;
        let approval = approvals
            .iter()
            .find(|pending| approval_id(&pending["requestId"]) == identity)
            .ok_or_else(|| ApiError::new("stale_decision", "This approval is no longer pending"))?;
        for field in ["threadId", "turnId"] {
            if p.get(field)
                .is_some_and(|value| value != &approval["params"][field])
            {
                return Err(ApiError::new(
                    "stale_decision",
                    "The approval context changed",
                ));
            }
        }
        let result = if p["result"].is_object() {
            p["result"].clone()
        } else {
            json!({"decision":required(p,"decision")?})
        };
        self.host
            .respond(approval["requestId"].clone(), result)
            .await
            .map_err(host_error)?;
        self.events
            .publish(json!({"method":"brr/approvals/changed","params":{}}));
        Ok(json!({"answered":true}))
    }

    async fn desktop_request(&self, request: &Request) -> Result<Value, ApiError> {
        let p = &request.params;
        match request.method.as_str() {
            "browser_request" => {
                let browser=self.browser.lock().await.clone().ok_or_else(||ApiError::new("browser_unavailable","The managed Brr browser is unavailable. Build or reinstall the packaged browser assets."))?;
                if p["acknowledgeUncertain"] == true && p["args"]["action"] == "reset" {
                    browser
                        .acknowledge_uncertain_and_reset()
                        .await
                        .map_err(host_error)?;
                    return Ok(json!({"reset":true,"previousActionReplayed":false}));
                }
                browser
                    .request(required(p, "threadId")?, p["args"].clone())
                    .await
                    .map_err(|message| {
                        ApiError::new(
                            if message.contains("uncertain") {
                                "uncertain"
                            } else {
                                "browser_error"
                            },
                            message,
                        )
                    })
            }
            "engine_connect" => {
                self.connect_runtime().await?;
                Ok(Value::Null)
            }
            "project_request" => self
                .host
                .project_request(required(p, "method")?, p["params"].clone())
                .await
                .map_err(host_error),
            "tickets_request" => self
                .host
                .tickets_request(required(p, "method")?, p["params"].clone())
                .await
                .map_err(host_error),
            "engine_respond" => Err(ApiError::new(
                "epoch_required",
                "Use the scoped approval command with its original runtime epoch",
            )),
            "engine_request" | "chat_request" | "agents_request" | "intelligent_ui_request" => {
                let method = required(p, "method")?;
                let params = p["params"].clone();
                let local_upload = request.method == "chat_request"
                    && method.starts_with("attachments.upload.");
                if local_upload {
                    return self
                        .attachment_uploads
                        .dispatch(&self.host, method, params)
                        .await
                        .map_err(host_error);
                }
                if request.policy()?.live {
                    if !self.host.is_connected().await {
                        return Err(ApiError::new(
                            "unavailable",
                            "The owning runtime is disconnected",
                        ));
                    }
                } else {
                    self.connect_runtime().await?;
                }
                let deleted_thread =
                    if request.method == "engine_request" && method == "thread/delete" {
                        Some(required(&params, "threadId")?.to_owned())
                    } else {
                        None
                    };
                let mut result = match request.method.as_str() {
                    "engine_request" => self.host.request(method, params).await,
                    "chat_request" => self.host.chat_request(method, params).await,
                    "agents_request" => self.host.agents_request(method, params).await,
                    _ => self.host.intelligent_ui_request(method, params).await,
                }
                .map_err(host_error)?;
                if let Some(thread) = deleted_thread {
                    self.tombstone_thread(&thread).await?;
                }
                if request.method == "agents_request" && method == "agents.decisions" {
                    if let Some(items) = result["runtimeRequests"].as_array_mut() {
                        for item in items {
                            item["brrEpoch"] = json!(self.events.cursor().epoch);
                        }
                    }
                }
                Ok(result)
            }
            _ => Err(ApiError::new(
                "unsupported_method",
                "Unknown local desktop operation",
            )),
        }
    }

    pub async fn shutdown(&self) {
        self.host.shutdown().await;
        if let Some(browser) = self.browser.lock().await.take() {
            browser.shutdown().await;
        }
    }
}

fn host_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new("host_error", error.to_string())
}

fn annotate_owned_records(value: &mut Value, node_id: &str, owner_epoch: &str) {
    const OWNED_ROWS: &[&str] = &[
        "data",
        "thread",
        "agent",
        "task",
        "workflow",
        "proposal",
        "draft",
        "submission",
        "question",
        "project",
        "memory",
        "skill",
        "threads",
        "agents",
        "tasks",
        "workflows",
        "proposals",
        "drafts",
        "submissions",
        "questions",
        "projects",
        "memories",
        "skills",
        "sessions",
    ];
    if let Value::Object(object) = value {
        for key in OWNED_ROWS {
            if let Some(rows) = object.get_mut(*key) {
                match rows {
                    Value::Array(items) => {
                        for item in items {
                            annotate_one_owned_record(item, node_id, owner_epoch);
                        }
                    }
                    Value::Object(_) => annotate_one_owned_record(rows, node_id, owner_epoch),
                    _ => {}
                }
            }
        }
    }
}

fn annotate_one_owned_record(value: &mut Value, node_id: &str, owner_epoch: &str) {
    if let Some(object) = value.as_object_mut() {
        object.insert("ownerNodeId".into(), json!(node_id));
        object.insert("ownerEpoch".into(), json!(owner_epoch));
    }
    annotate_owned_records(value, node_id, owner_epoch);
}

fn required<'a>(params: &'a Value, field: &str) -> Result<&'a str, ApiError> {
    params[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::new("invalid_request", format!("{field} is required")))
}
fn submission_id(actor: &str, operation: &str) -> String {
    format!(
        "remote_{:x}",
        Sha256::digest(format!("{actor}\0{operation}"))
    )
}

fn canonical_approval_identity(request: &Request) -> Result<Option<String>, ApiError> {
    if request.channel != Channel::Approvals || request.method != "answer" {
        return Ok(None);
    }

    let params = &request.params;
    let raw_identity = match params.get("requestId") {
        Some(Value::String(value)) => Some(approval_id(&Value::String(value.clone()))),
        Some(Value::Number(value)) => Some(approval_id(&Value::Number(value.clone()))),
        Some(_) => {
            return Err(ApiError::new(
                "invalid_request",
                "The approval requestId must be a string or number",
            ));
        }
        None => None,
    };
    let canonical_alias = match params.get("id") {
        Some(Value::String(value)) if is_canonical_approval_id(value) => Some(value.clone()),
        Some(_) => {
            return Err(ApiError::new(
                "invalid_request",
                "The approval id must be a canonical approval identity",
            ));
        }
        None => None,
    };

    match (raw_identity, canonical_alias) {
        (Some(raw), Some(canonical)) if raw != canonical => Err(ApiError::new(
            "invalid_request",
            "The approval aliases identify different pending requests",
        )),
        (Some(raw), _) => Ok(Some(raw)),
        (None, Some(canonical)) => Ok(Some(canonical)),
        (None, None) => Err(ApiError::new(
            "invalid_request",
            "An approval identity is required",
        )),
    }
}

fn is_canonical_approval_id(value: &str) -> bool {
    if value.starts_with("s:") {
        return true;
    }
    let Some(raw) = value.strip_prefix("n:") else {
        return false;
    };
    serde_json::from_str::<Value>(raw)
        .ok()
        .filter(|value| value.is_number())
        .is_some_and(|id| approval_id(&id) == value)
}

fn approval_id(id: &Value) -> String {
    match id {
        Value::String(value) => format!("s:{value}"),
        _ => format!("n:{id}"),
    }
}

fn row_view(value: Value) -> Value {
    let Value::Object(row) = value else {
        return value;
    };
    let mut result = serde_json::Map::new();
    for (key, mut value) in row {
        if matches!(
            key.as_str(),
            "acceptance_criteria" | "dependencies" | "nodes" | "task"
        ) {
            if let Some(text) = value.as_str() {
                if let Ok(decoded) = serde_json::from_str(text) {
                    value = decoded;
                }
            }
        }
        let mut parts = key.split('_');
        let mut key = parts.next().unwrap_or_default().to_owned();
        for part in parts {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                key.extend(first.to_uppercase());
                key.extend(chars);
            }
        }
        result.insert(key, value);
    }
    Value::Object(result)
}
fn profile_view(value: Value) -> Value {
    let mut profile = row_view(value);
    if let Some(candidate) = profile["candidate"]
        .as_str()
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
    {
        if let (Some(profile), Value::Object(candidate)) = (profile.as_object_mut(), candidate) {
            profile.remove("candidate");
            profile.extend(candidate);
        }
    }
    profile
}

fn expected_project_revision(request: &Request) -> Result<i64, ApiError> {
    request
        .expected_revision
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|revision| *revision > 0)
        .ok_or_else(|| {
            ApiError::new(
                "revision_required",
                "A positive expectedRevision is required for this project change",
            )
        })
}

fn optional_project_string(
    params: &Value,
    existing: &Value,
    field: &str,
) -> Result<Option<String>, ApiError> {
    match params.get(field) {
        None => Ok(existing[field].as_str().map(str::to_owned)),
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ApiError::new(
            "invalid_request",
            format!("Project {field} must be a string or null"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saved_attachment_reads_are_scoped_verified_and_need_no_runtime_or_mutation() {
        use brr_host::export::{HistoryAttachment, HistoryBlob, HistoryExport};
        let dir = tempfile::tempdir().unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: dir.path().join("missing-runtime"),
            name: "Offline owner".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let bytes = b"saved attachment <script>inert</script>\n";
        let sha = format!("{:x}", Sha256::digest(bytes));
        let source = std::fs::canonicalize(dir.path()).unwrap().join("file");
        std::fs::write(&source, bytes).unwrap();
        let attachment = HistoryAttachment {
            id: "file-1".into(),
            name: "notes.txt".into(),
            mime: "text/plain".into(),
            kind: "text".into(),
            size_bytes: bytes.len() as u64,
            sha256: sha.clone(),
            submission_id: "accepted-submission".into(),
            turn_id: None,
            item_id: None,
        };
        let catalog = crate::catalog::Catalog::init_primary(
            service.host.data_dir(),
            service.node_id(),
            service.host.agent_store(),
        )
        .await
        .unwrap();
        catalog.publish_history(&HistoryExport {
            schema_version: 1, owner_origin: "local-runtime".into(), complete: true,
            threads: vec![json!({"thread":{"id":"saved-thread"},"turns":[],"origin":"local-runtime","attachments":[attachment]})],
            blobs: vec![HistoryBlob::from_verified_path(&source, sha.clone(), bytes.len() as u64).unwrap()], failures: vec![],
        }).unwrap();
        let committed_head = catalog.status().unwrap().revision;
        *service.catalog.lock().await = Some(catalog);
        let request = |method: &str, params: Value| Request {
            id: format!("{method}-read"),
            channel: Channel::Sessions,
            method: method.into(),
            params,
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(service.node_id().into()),
        };
        let preview = service
            .handle(
                request(
                    "attachment.preview",
                    json!({"threadId":"saved-thread","sha256":sha}),
                ),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let preview = preview.result.unwrap();
        assert_eq!(
            preview["preview"],
            String::from_utf8(bytes.to_vec()).unwrap()
        );
        assert_eq!(preview["attachment"]["sha256"], sha);
        assert!(!service.host.is_connected().await);
        let read = service
            .handle(
                request(
                    "attachment.read",
                    json!({"threadId":"saved-thread","sha256":sha,"offset":6,"length":10}),
                ),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(read.error.is_none(), "{:?}", read.error);
        use base64::Engine;
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(read.result.unwrap()["dataBase64"].as_str().unwrap())
                .unwrap(),
            &bytes[6..16]
        );
        for params in [
            json!({"threadId":"other-thread","sha256":sha}),
            json!({"threadId":"saved-thread","sha256":"0".repeat(64)}),
            json!({"threadId":"saved-thread","sha256":sha,"offset":-1}),
            json!({"threadId":"saved-thread","sha256":sha,"length":262145}),
        ] {
            assert!(service
                .handle(
                    request("attachment.read", params),
                    ClientRole::Mobile,
                    "phone"
                )
                .await
                .error
                .is_some());
        }
        let listed = service
            .handle(
                Request {
                    channel: Channel::Sync,
                    method: "records".into(),
                    params: json!({}),
                    ..request("records", json!({}))
                },
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(listed.error.is_none(), "{:?}", listed.error);
        assert!(listed.result.unwrap()["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["kind"] != "blob" && row["kind"] != "transcript"));
        // The metadata cache only trusts the immutable Git tree; requested bytes
        // are still checked against their digest on every preview and read.
        let blob_path = service
            .host
            .data_dir()
            .join("fleet-catalog/writer/.brr-fleet/repo/blobs")
            .join(&sha);
        std::fs::write(&blob_path, vec![b'!'; bytes.len()]).unwrap();
        for method in ["attachment.preview", "attachment.read"] {
            let response = service
                .handle(
                    request(
                        method,
                        json!({"threadId":"saved-thread","sha256":sha,"offset":0,"length":16}),
                    ),
                    ClientRole::Mobile,
                    "phone",
                )
                .await;
            assert!(response.error.is_some(), "tampered blob was served by {method}");
            assert!(response.result.is_none());
        }
        let catalog = service.catalog.lock().await.clone().unwrap();
        assert_eq!(catalog.status().unwrap().revision, committed_head);
        service.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completed_attachment_turn_publishes_verified_sources_without_full_refresh() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("fixture-runtime");
        std::fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: runtime,
            name: "Owner".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        service.connect_runtime().await.unwrap();
        let started = service
            .host
            .request("thread/start", json!({"cwd":dir.path()}))
            .await
            .unwrap();
        let id = started["thread"]["id"].as_str().unwrap();
        let catalog = crate::catalog::Catalog::init_primary(
            service.host.data_dir(),
            service.node_id(),
            service.host.agent_store(),
        )
        .await
        .unwrap();
        *service.catalog.lock().await = Some(catalog.clone());
        let (shutdown, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(service.clone().run_history_projection(rx));
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if catalog
                    .historical_thread(service.node_id(), id)
                    .unwrap()
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("initial full refresh must finish before the new accepted message");
        let files=service.host.chat_request("attachments.importBytes",json!({"ownerId":id,"files":[{"name":"saved.txt","mime":"text/plain","data":"c2F2ZWQgYnl0ZXM="}]})).await.unwrap();
        let sent=service.host.chat_request("foreground.submit",json!({"id":"accepted-event","threadId":id,"document":{"version":1,"text":"saved","references":[],"quotes":[]},"attachmentIds":[files[0]["id"]],"settings":{"model":"_runtime_default","mode":"default","effort":null}})).await.unwrap();
        assert_eq!(sent["submission"]["status"], "accepted");
        service
            .host
            .request(
                "turn/interrupt",
                json!({"threadId":id,"turnId":sent["submission"]["turnId"]}),
            )
            .await
            .unwrap();
        let published = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let history = catalog
                    .historical_thread(service.node_id(), id)
                    .unwrap()
                    .unwrap();
                if history["attachments"]
                    .as_array()
                    .is_some_and(|rows| rows.len() == 1)
                {
                    break history;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await;
        let live = service
            .handle(
                Request {
                    id: "read-with-files".into(),
                    channel: Channel::Sessions,
                    method: "read".into(),
                    params: json!({"threadId":id}),
                    operation_id: None,
                    expected_epoch: None,
                    expected_revision: None,
                    target_node_id: Some(service.node_id().into()),
                },
                ClientRole::Mobile,
                "phone",
            )
            .await;
        let snapshot = service.snapshot().await.unwrap();
        shutdown.send(true).unwrap();
        task.await.unwrap();
        service.shutdown().await;
        let published =
            published.expect("completed message attachment must publish before any full refresh");
        let (_, blob) = catalog
            .historical_attachment(
                service.node_id(),
                id,
                published["attachments"][0]["sha256"].as_str().unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(blob.read_verified_bytes().unwrap(), b"saved bytes");
        assert!(live.error.is_none(), "{:?}", live.error);
        assert_eq!(
            live.result.unwrap()["attachments"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            snapshot
                .threads
                .iter()
                .find(|thread| thread["id"] == id)
                .unwrap()["attachments"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
    }
    use brr_remote_protocol::Channel;
    use serde_json::json;

    #[test]
    fn approval_identity_preserves_empty_string_runtime_ids() {
        let raw_typed = Request {
            id: "empty-approval".into(),
            channel: Channel::Approvals,
            method: "answer".into(),
            params: json!({"requestId":""}),
            operation_id: Some("empty-approval-once".into()),
            expected_epoch: Some("epoch".into()),
            expected_revision: None,
            target_node_id: None,
        };
        assert_eq!(
            canonical_approval_identity(&raw_typed).unwrap().as_deref(),
            Some("s:")
        );

        let canonical = Request {
            params: json!({"id":"s:"}),
            ..raw_typed
        };
        assert_eq!(
            canonical_approval_identity(&canonical).unwrap().as_deref(),
            Some("s:")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn opening_service_does_not_wait_for_a_hung_runtime_handshake() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("hanging-runtime");
        std::fs::write(
            &runtime,
            "#!/usr/bin/env python3\nimport time\ntime.sleep(10)\n",
        )
        .unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let opened = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            Service::open(ServiceConfig {
                data_dir: dir.path().join("data"),
                runtime_path: runtime,
                name: "Slow runtime".into(),
                endpoint_hosts: vec!["localhost".into()],
            }),
        )
        .await;
        assert!(
            opened.is_ok(),
            "Local IPC and remote listeners must start without waiting for inference"
        );
        opened.unwrap().unwrap().shutdown().await;
    }

    #[test]
    fn scoped_response_owner_metadata_excludes_historical_source_records() {
        let mut response = json!({
            "data": [{"id":"task-1"}],
            "thread": {"id":"thread-1"},
            "sources": [{"sourceId":"source-from-history","threadId":"old-thread"}]
        });
        annotate_owned_records(&mut response, "worker-1", "epoch-4");
        assert_eq!(response["data"][0]["ownerNodeId"], "worker-1");
        assert_eq!(response["data"][0]["ownerEpoch"], "epoch-4");
        assert_eq!(response["thread"]["ownerNodeId"], "worker-1");
        assert!(response["sources"][0].get("ownerNodeId").is_none());
    }

    #[tokio::test]
    async fn a_headless_node_never_falls_back_to_a_desktop_browser() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().to_owned(),
            runtime_path: dir.path().join("missing-runtime"),
            name: "Headless".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let request = Request {
            id: "browser".into(),
            channel: Channel::Desktop,
            method: "browser_request".into(),
            params: json!({"threadId":"chat-one","args":{"action":"open","url":"about:blank"}}),
            operation_id: Some("browser-open".into()),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        };
        let reply = service
            .handle(request, ClientRole::Desktop, "local-owner")
            .await;
        assert_eq!(reply.error.unwrap().code, "browser_unavailable");
    }

    #[tokio::test]
    async fn local_service_retains_store_access_without_an_installed_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().to_owned(),
            runtime_path: dir.path().join("missing-owned-runtime"),
            name: "Test Mac".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let request = Request {
            id: "status".into(),
            channel: Channel::System,
            method: "status".into(),
            params: json!({}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        };
        let status = service
            .handle(request, ClientRole::Desktop, "local-owner")
            .await
            .result
            .unwrap();
        assert_eq!(status["runtimeConnected"], false);
        assert_eq!(status["name"], "Test Mac");
        let request = Request {
            id: "projects".into(),
            channel: Channel::Projects,
            method: "list".into(),
            params: json!({}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        };
        assert!(service
            .handle(request, ClientRole::Desktop, "local-owner")
            .await
            .result
            .unwrap()["data"]
            .is_array());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn approval_aliases_must_agree_and_concurrent_typed_aliases_resolve_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("node")).unwrap();
        std::fs::write(
            data.join("node/fixture-runtime.json"),
            json!({
                "threads":{},
                "calls":[],
                "approvals":[],
                "seedApprovals":[
                    {"id":901,"method":"item/commandExecution/requestApproval","params":{"threadId":"numeric-thread","turnId":"numeric-turn"}},
                    {"id":"901","method":"item/commandExecution/requestApproval","params":{"threadId":"string-thread","turnId":"string-turn"}}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let runtime = dir.path().join("fixture-runtime");
        std::fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: data.clone(),
            runtime_path: runtime,
            name: "Fixture approval owner".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        service.start_runtime().await.unwrap();

        let mut pending = Vec::new();
        for _ in 0..50 {
            pending = service.approvals().await.unwrap();
            if pending.len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(pending.len(), 2, "fixture approvals were not registered");
        assert!(pending.iter().any(|approval| approval["id"] == "n:901"));
        assert!(pending.iter().any(|approval| approval["id"] == "s:901"));

        let epoch = service.events().cursor().epoch;
        let make_request = |id: &str, operation_id: &str, params: Value| Request {
            id: id.into(),
            channel: Channel::Approvals,
            method: "answer".into(),
            params,
            operation_id: Some(operation_id.into()),
            expected_epoch: Some(epoch.clone()),
            expected_revision: None,
            target_node_id: Some(service.node_id().into()),
        };
        let conflicting = service
            .handle(
                make_request(
                    "approval-conflict",
                    "approval-conflict-once",
                    json!({"requestId":901,"id":"s:901","decision":"accept"}),
                ),
                ClientRole::Desktop,
                "desktop-owner",
            )
            .await;
        assert_eq!(
            conflicting.error.as_ref().map(|error| error.code.as_str()),
            Some("invalid_request")
        );
        let state: Value =
            serde_json::from_slice(&std::fs::read(data.join("node/fixture-runtime.json")).unwrap())
                .unwrap();
        assert!(state["approvals"].as_array().unwrap().is_empty());
        let legacy_desktop = service
            .handle(
                Request {
                    id: "legacy-engine-respond".into(),
                    channel: Channel::Desktop,
                    method: "engine_respond".into(),
                    params: json!({"requestId":901,"result":{"decision":"accept"}}),
                    operation_id: Some("legacy-engine-respond-once".into()),
                    expected_epoch: Some(epoch.clone()),
                    expected_revision: None,
                    target_node_id: Some(service.node_id().into()),
                },
                ClientRole::Desktop,
                "desktop-owner",
            )
            .await;
        assert_eq!(
            legacy_desktop
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("epoch_required")
        );
        let state: Value =
            serde_json::from_slice(&std::fs::read(data.join("node/fixture-runtime.json")).unwrap())
                .unwrap();
        assert!(state["approvals"].as_array().unwrap().is_empty());

        let (numeric_desktop, numeric_mobile) = tokio::join!(
            service.handle(
                make_request(
                    "numeric-canonical",
                    "numeric-canonical-once",
                    json!({"id":"n:901","decision":"accept"})
                ),
                ClientRole::Desktop,
                "desktop-owner",
            ),
            service.handle(
                make_request(
                    "numeric-typed",
                    "numeric-typed-once",
                    json!({"requestId":901,"decision":"accept"})
                ),
                ClientRole::Mobile,
                "phone-owner",
            ),
        );
        assert_eq!(
            (numeric_desktop.error.is_none() as usize) + (numeric_mobile.error.is_none() as usize),
            1
        );
        assert_eq!(
            [numeric_desktop.error, numeric_mobile.error]
                .into_iter()
                .flatten()
                .filter(|error| error.code == "stale_decision")
                .count(),
            1
        );

        let (string_desktop, string_mobile) = tokio::join!(
            service.handle(
                make_request(
                    "string-canonical",
                    "string-canonical-once",
                    json!({"id":"s:901","decision":"accept"})
                ),
                ClientRole::Desktop,
                "desktop-owner",
            ),
            service.handle(
                make_request(
                    "string-typed",
                    "string-typed-once",
                    json!({"requestId":"901","decision":"accept"})
                ),
                ClientRole::Mobile,
                "phone-owner",
            ),
        );
        assert_eq!(
            (string_desktop.error.is_none() as usize) + (string_mobile.error.is_none() as usize),
            1
        );
        assert_eq!(
            [string_desktop.error, string_mobile.error]
                .into_iter()
                .flatten()
                .filter(|error| error.code == "stale_decision")
                .count(),
            1
        );

        let state: Value =
            serde_json::from_slice(&std::fs::read(data.join("node/fixture-runtime.json")).unwrap())
                .unwrap();
        let responses = state["approvals"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses
                .iter()
                .filter(|response| response["id"].as_u64() == Some(901))
                .count(),
            1
        );
        assert_eq!(
            responses
                .iter()
                .filter(|response| response["id"].as_str() == Some("901"))
                .count(),
            1
        );
        service.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_numeric_memory_revision_from_outer_request_does_not_forget_source() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("fixture-runtime");
        std::fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: runtime,
            name: "Fixture node".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let store = service.host.agent_store();
        let access = store.memory_access_for_user(None, None).await.unwrap();
        store
            .record_source(&access, "source-cas", 1, "bounded evidence")
            .await
            .unwrap();
        let reply = service
            .handle(
                Request {
                    id: "forget-stale".into(),
                    channel: Channel::Agents,
                    method: "memory.forget".into(),
                    params: json!({"id":"source-cas"}),
                    operation_id: Some("forget-stale-once".into()),
                    expected_epoch: None,
                    expected_revision: Some("2".into()),
                    target_node_id: Some(service.node_id().into()),
                },
                ClientRole::Desktop,
                "desktop",
            )
            .await;
        assert!(
            reply
                .error
                .as_ref()
                .is_some_and(|error| error.message.contains("revision changed")),
            "{:?}",
            reply.error
        );
        assert!(store
            .list_sources(&access, 10)
            .await
            .unwrap()
            .iter()
            .any(|source| source.source_id == "source-cas"));
        service.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn desktop_raw_turn_start_replays_the_stored_outcome_for_same_operation_id() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("fixture-runtime");
        std::fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: runtime,
            name: "Fixture node".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        service.connect_runtime().await.unwrap();
        let started = service
            .host
            .request(
                "thread/start",
                json!({"cwd":dir.path().to_string_lossy(),"model":"fixture-model"}),
            )
            .await
            .unwrap();
        let thread_id = started["thread"]["id"].as_str().unwrap();
        let request = Request {
            id: "raw-turn-start-1".into(),
            channel: Channel::Desktop,
            method: "engine_request".into(),
            params: json!({"method":"turn/start","params":{"threadId":thread_id,"input":[{"type":"text","text":"run once"}]}}),
            operation_id: Some("desktop-turn-start-once".into()),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(service.node_id().into()),
        };
        let local_server = crate::local::LocalServer::bind(service.instance()).unwrap();
        let local_path = local_server.path().to_owned();
        let local_service = service.clone();
        let dispatch: crate::local::Dispatch = Arc::new(move |request| {
            let service = local_service.clone();
            Box::pin(async move {
                service
                    .handle(request, ClientRole::Desktop, "desktop-owner")
                    .await
            })
        });
        let (stop, shutdown) = tokio::sync::watch::channel(false);
        let local_task = tokio::spawn(local_server.run(dispatch, service.events(), shutdown));
        let local_client = crate::local::LocalClient::new(local_path.clone());
        let first = local_client.request(request.clone()).await.unwrap();
        assert!(first.error.is_none(), "{:?}", first.error);
        let mut retry = request;
        retry.id = "raw-turn-start-retry".into();
        let replay = crate::local::LocalClient::new(local_path)
            .request(retry)
            .await
            .unwrap();
        assert_eq!(first.result, replay.result);
        let read = service
            .host
            .request("thread/read", json!({"threadId":thread_id}))
            .await
            .unwrap();
        assert_eq!(read["thread"]["turns"].as_array().unwrap().len(), 1);
        let live = Request {
            id: "raw-stop".into(),
            channel: Channel::Desktop,
            method: "engine_request".into(),
            params: json!({"method":"turn/interrupt","params":{"threadId":thread_id,"turnId":"fixture-turn"}}),
            operation_id: Some("raw-stop-once".into()),
            expected_epoch: Some(service.events.cursor().epoch),
            expected_revision: None,
            target_node_id: None,
        };
        let stopped = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            service.handle(live, ClientRole::Desktop, "local-owner"),
        )
        .await;
        assert!(
            stopped.is_ok(),
            "A live desktop call must not reacquire the epoch guard it already holds"
        );
        let stopped = stopped.unwrap();
        assert!(stopped.error.is_none(), "{:?}", stopped.error);
        stop.send(true).unwrap();
        local_task.await.unwrap();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn paired_client_retries_do_not_create_duplicate_projects() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().to_owned(),
            runtime_path: dir.path().join("missing-owned-runtime"),
            name: "Test node".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let request = Request {
            id: "create-one".into(),
            channel: Channel::Projects,
            method: "create".into(),
            params: json!({"name":"Shared notes"}),
            operation_id: Some("create-notes".into()),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(service.node_id().to_owned()),
        };
        let first = service
            .handle(request.clone(), ClientRole::Mobile, "phone-one")
            .await;
        assert!(first.error.is_none(), "{:?}", first.error);
        let mut retry = request;
        retry.id = "create-retry".into();
        let replay = service.handle(retry, ClientRole::Mobile, "phone-one").await;
        assert_eq!(first.result, replay.result);
        let projects = service
            .host
            .project_request("list", json!({}))
            .await
            .unwrap();
        assert_eq!(projects["data"].as_array().unwrap().len(), 1);
        assert_eq!(projects["data"][0]["folders"], json!([]));
    }

    #[tokio::test]
    async fn scoped_project_metadata_and_delete_use_outer_revision_cas() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().to_owned(),
            runtime_path: dir.path().join("missing-owned-runtime"),
            name: "Test node".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let project = service
            .host
            .project_request(
                "create",
                json!({"name":"Local folder","folders":["/owner/work"],"primaryFolder":"/owner/work"}),
            )
            .await
            .unwrap();
        let project_id = project["id"].as_str().unwrap().to_owned();
        service
            .host
            .project_request(
                "move",
                json!({"threadId":"owner-thread","projectId":project_id}),
            )
            .await
            .unwrap();

        let save = Request {
            id: "project-save".into(),
            channel: Channel::Projects,
            method: "save".into(),
            params: json!({"id":project_id,"name":"Shared metadata","model":"fixture-model","effort":"high","permission":"read-only"}),
            operation_id: Some("project-save-once".into()),
            expected_epoch: None,
            expected_revision: Some("1".into()),
            target_node_id: Some(service.node_id().to_owned()),
        };
        let saved = service
            .handle(save.clone(), ClientRole::Mobile, "phone")
            .await;
        assert!(saved.error.is_none(), "{:?}", saved.error);
        assert_eq!(saved.result.as_ref().unwrap()["name"], "Shared metadata");
        assert_eq!(
            saved.result.as_ref().unwrap()["ownerNodeId"],
            service.node_id()
        );

        let local = service
            .host
            .project_request("list", json!({}))
            .await
            .unwrap()["data"][0]
            .clone();
        assert_eq!(local["folders"], json!(["/owner/work"]));
        assert_eq!(local["primaryFolder"], "/owner/work");
        let listed = service
            .handle(
                Request {
                    id: "projects-after-save".into(),
                    channel: Channel::Projects,
                    method: "list".into(),
                    params: json!({}),
                    operation_id: None,
                    expected_epoch: None,
                    expected_revision: None,
                    target_node_id: Some(service.node_id().to_owned()),
                },
                ClientRole::Mobile,
                "phone",
            )
            .await
            .result
            .unwrap();
        assert_eq!(listed["data"][0]["ownerNodeId"], service.node_id());
        let mut stale_delete = Request {
            id: "project-stale-delete".into(),
            channel: Channel::Projects,
            method: "delete".into(),
            params: json!({"projectId":project_id}),
            operation_id: Some("project-delete-stale".into()),
            expected_epoch: None,
            expected_revision: Some("1".into()),
            target_node_id: Some(service.node_id().to_owned()),
        };
        let rejected = service
            .handle(stale_delete.clone(), ClientRole::Mobile, "phone")
            .await;
        assert!(rejected.error.is_some());
        assert_eq!(
            service
                .host
                .project_request("list", json!({}))
                .await
                .unwrap()["data"][0]["hidden"],
            false
        );

        stale_delete.id = "project-delete".into();
        stale_delete.operation_id = Some("project-delete-once".into());
        stale_delete.expected_revision = Some("2".into());
        let deleted = service
            .handle(stale_delete, ClientRole::Mobile, "phone")
            .await;
        assert!(deleted.error.is_none(), "{:?}", deleted.error);
        assert_eq!(
            service
                .host
                .project_request("list", json!({}))
                .await
                .unwrap()["data"],
            json!([{
                "id":project_id,
                "name":"Shared metadata",
                "folders":["/owner/work"],
                "primaryFolder":"/owner/work",
                "revision":3,
                "model":"fixture-model",
                "effort":"high",
                "permission":"read-only",
                "hidden":true
            }])
        );
        service.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scoped_move_chat_reads_exact_owner_thread_without_resuming_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("node")).unwrap();
        std::fs::write(
            data.join("node/fixture-runtime.json"),
            json!({"threads":{"owner-thread":{"id":"owner-thread","turns":[],"status":{"type":"idle"}}},"calls":[],"approvals":[]}).to_string(),
        )
        .unwrap();
        let runtime = dir.path().join("fixture-runtime");
        std::fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: data.clone(),
            runtime_path: runtime,
            name: "Fixture owner".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let project = service
            .handle(
                Request {
                    id: "create-project".into(),
                    channel: Channel::Projects,
                    method: "create".into(),
                    params: json!({"name":"Project"}),
                    operation_id: Some("create-project-once".into()),
                    expected_epoch: None,
                    expected_revision: None,
                    target_node_id: Some(service.node_id().to_owned()),
                },
                ClientRole::Mobile,
                "phone",
            )
            .await
            .result
            .unwrap();
        assert_eq!(project["ownerNodeId"], service.node_id());
        assert_eq!(project["originNodeId"], service.node_id());
        let moved = service
            .handle(
                Request {
                    id: "move-owner-thread".into(),
                    channel: Channel::Projects,
                    method: "moveChat".into(),
                    params: json!({"threadId":"owner-thread","projectId":project["id"]}),
                    operation_id: Some("move-owner-thread-once".into()),
                    expected_epoch: None,
                    expected_revision: None,
                    target_node_id: Some(service.node_id().to_owned()),
                },
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(moved.error.is_none(), "{:?}", moved.error);

        let foreign = service
            .handle(
                Request {
                    id: "move-foreign-thread".into(),
                    channel: Channel::Projects,
                    method: "moveChat".into(),
                    params: json!({"threadId":"foreign-thread","projectId":project["id"]}),
                    operation_id: Some("move-foreign-thread-once".into()),
                    expected_epoch: None,
                    expected_revision: None,
                    target_node_id: Some(service.node_id().to_owned()),
                },
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(foreign.error.is_some());
        let state: Value =
            serde_json::from_slice(&std::fs::read(data.join("node/fixture-runtime.json")).unwrap())
                .unwrap();
        let calls = state["calls"].as_array().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call["method"] == "thread/read")
                .count(),
            1
        );
        assert!(!calls.iter().any(|call| call["method"] == "thread/resume"));
        assert!(!calls.iter().any(|call| call["method"] == "thread/start"));
        service.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_mobile_session_start_delivers_its_first_message_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("fixture-runtime");
        std::fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: runtime,
            name: "Fixture node".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let start = Request {
            id: "first".into(),
            channel: Channel::Sessions,
            method: "start".into(),
            params: json!({"model":"fixture-model","text":"hello from the phone","effort":"medium","mode":"default"}),
            operation_id: Some("start-once".into()),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(service.node_id().to_owned()),
        };
        let first = service
            .handle(start.clone(), ClientRole::Mobile, "phone")
            .await;
        assert!(first.error.is_none(), "{:?}", first.error);
        let replay = service.handle(start, ClientRole::Mobile, "phone").await;
        assert_eq!(first.result, replay.result);
        let id = first.result.unwrap()["thread"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let read = Request {
            id: "read".into(),
            channel: Channel::Sessions,
            method: "read".into(),
            params: json!({"threadId":id}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(service.node_id().to_owned()),
        };
        let thread = service
            .handle(read, ClientRole::Mobile, "phone")
            .await
            .result
            .unwrap();
        assert_eq!(thread["turns"].as_array().unwrap().len(), 1);
        assert_eq!(
            thread["turns"][0]["items"][0]["content"][0]["text"],
            "hello from the phone"
        );
        assert_eq!(thread["ownerNodeId"], service.node_id());
        service.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mobile_interview_and_profile_messages_use_the_owned_workflow_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("fixture-runtime");
        std::fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: runtime,
            name: "Fixture node".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        service.connect_runtime().await.unwrap();

        let store = service.host.agent_store();
        let draft = store
            .propose_profile(
                "profile-seed",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Fixture agent".into(),
                    description: "test".into(),
                    instructions: "test".into(),
                    soul: "test".into(),
                    model: "fixture-model".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, "fixture-approve")
            .await
            .unwrap();
        let applied = store
            .apply_profile(&draft.id, &draft.hash, "fixture-apply")
            .await
            .unwrap();

        let interview = service
            .host
            .agents_request(
                "agents.interview",
                json!({"model":"fixture-model","effort":"medium"}),
            )
            .await
            .unwrap();
        let profile = service
            .host
            .agents_request("agents.edit", json!({"agentId":applied.id}))
            .await
            .unwrap();
        let send = |id: &str,
                    thread_id: &str,
                    text: &str,
                    operation_id: &str,
                    model: &str,
                    mode: &str| Request {
            id: id.into(),
            channel: Channel::Sessions,
            method: "send".into(),
            params: json!({"threadId":thread_id,"text":text,"model":model,"effort":"low","mode":mode,"readOnly":true}),
            operation_id: Some(operation_id.into()),
            expected_epoch: Some(service.events.cursor().epoch),
            expected_revision: None,
            target_node_id: Some(service.node_id().into()),
        };
        for (name, session, content) in [
            ("interview", &interview, "ask interview"),
            ("profile", &profile, "edit profile"),
        ] {
            let thread_id = session["thread"]["id"].as_str().unwrap();
            let request = send(
                &format!("{name}-send"),
                thread_id,
                content,
                &format!("{name}-send-once"),
                "fixture-model",
                "plan",
            );
            let first = service
                .handle(request.clone(), ClientRole::Mobile, "phone")
                .await;
            assert!(first.error.is_none(), "{name}: {:?}", first.error);
            let mut retry = request;
            retry.id.push_str("-retry");
            let replay = service.handle(retry, ClientRole::Mobile, "phone").await;
            assert_eq!(first.result, replay.result);
        }

        let state: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("data/node/fixture-runtime.json")).unwrap(),
        )
        .unwrap();
        let turns = state["threads"]
            .as_object()
            .unwrap()
            .values()
            .map(|thread| thread["turns"].as_array().unwrap().len())
            .sum::<usize>();
        assert_eq!(
            turns, 2,
            "operation retry must not start a second runtime turn"
        );
        let turn_calls = state["calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "turn/start")
            .collect::<Vec<_>>();
        assert_eq!(turn_calls.len(), 2);
        for call in turn_calls {
            assert_eq!(call["params"]["collaborationMode"]["mode"], "plan");
            assert_eq!(call["params"]["effort"], "low");
            assert_eq!(
                call["params"]["clientUserMessageId"]
                    .as_str()
                    .unwrap()
                    .starts_with("remote_"),
                true
            );
        }
        assert!(!state["calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "foreground.submit"));

        let default_interview = service
            .host
            .agents_request(
                "agents.interview",
                json!({"model":"fixture-model","effort":"medium"}),
            )
            .await
            .unwrap();
        let plan_interview = service
            .host
            .agents_request(
                "agents.interview",
                json!({"model":"fixture-model","effort":"medium"}),
            )
            .await
            .unwrap();
        let default_sentinel_interview = service
            .host
            .agents_request(
                "agents.interview",
                json!({"model":"fixture-model","effort":"medium"}),
            )
            .await
            .unwrap();
        let plan_sentinel_interview = service
            .host
            .agents_request(
                "agents.interview",
                json!({"model":"fixture-model","effort":"medium"}),
            )
            .await
            .unwrap();
        let default_thread = default_interview["thread"]["id"].as_str().unwrap();
        let plan_thread = plan_interview["thread"]["id"].as_str().unwrap();
        let default_sentinel_thread = default_sentinel_interview["thread"]["id"].as_str().unwrap();
        let plan_sentinel_thread = plan_sentinel_interview["thread"]["id"].as_str().unwrap();
        let default_empty = service
            .handle(
                send(
                    "default-empty-model",
                    default_thread,
                    "runtime default",
                    "default-empty-model-once",
                    "",
                    "default",
                ),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(
            default_empty.error.is_none(),
            "empty model in Default mode: {:?}",
            default_empty.error
        );
        let plan_empty = service
            .handle(
                send(
                    "plan-empty-model",
                    plan_thread,
                    "runtime model in Plan",
                    "plan-empty-model-once",
                    "",
                    "plan",
                ),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(
            plan_empty.error.is_none(),
            "Plan mode should resolve a canonical runtime model: {:?}",
            plan_empty.error
        );
        let default_sentinel = service
            .handle(
                send(
                    "default-sentinel-model",
                    default_sentinel_thread,
                    "runtime default sentinel",
                    "default-sentinel-model-once",
                    "_runtime_default",
                    "default",
                ),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(
            default_sentinel.error.is_none(),
            "runtime-default sentinel in Default mode: {:?}",
            default_sentinel.error
        );
        let plan_sentinel = service
            .handle(
                send(
                    "plan-sentinel-model",
                    plan_sentinel_thread,
                    "runtime model sentinel in Plan",
                    "plan-sentinel-model-once",
                    "_runtime_default",
                    "plan",
                ),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(
            plan_sentinel.error.is_none(),
            "Plan mode should resolve the runtime-default sentinel: {:?}",
            plan_sentinel.error
        );
        let plan_call =
            std::fs::read_to_string(dir.path().join("data/node/fixture-runtime.json")).unwrap();
        let plan_state: Value = serde_json::from_str(&plan_call).unwrap();
        let plan_call = plan_state["calls"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|call| {
                call["method"] == "turn/start" && call["params"]["threadId"] == plan_thread
            })
            .unwrap();
        assert_eq!(plan_call["params"]["collaborationMode"]["mode"], "plan");
        assert_eq!(
            plan_call["params"]["collaborationMode"]["settings"]["model"],
            "fixture-model"
        );
        let plan_sentinel_call = plan_state["calls"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|call| {
                call["method"] == "turn/start" && call["params"]["threadId"] == plan_sentinel_thread
            })
            .unwrap();
        assert_eq!(
            plan_sentinel_call["params"]["collaborationMode"]["mode"],
            "plan"
        );
        assert_eq!(
            plan_sentinel_call["params"]["collaborationMode"]["settings"]["model"],
            "fixture-model"
        );
        let default_call = plan_state["calls"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|call| {
                call["method"] == "turn/start" && call["params"]["threadId"] == default_thread
            })
            .unwrap();
        assert!(default_call["params"].get("model").is_none());
        assert!(default_call["params"].get("collaborationMode").is_none());
        let default_sentinel_call = plan_state["calls"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|call| {
                call["method"] == "turn/start"
                    && call["params"]["threadId"] == default_sentinel_thread
            })
            .unwrap();
        assert!(default_sentinel_call["params"].get("model").is_none());
        assert!(default_sentinel_call["params"]
            .get("collaborationMode")
            .is_none());

        let profile_thread = profile["thread"]["id"].as_str().unwrap();
        let steer = Request {
            id: "profile-steer".into(),
            channel: Channel::Sessions,
            method: "steer".into(),
            params: json!({"threadId":profile_thread,"text":"continue profile edit","expectedTurnId":"turn-1","model":"fixture-model","effort":"low","mode":"default"}),
            operation_id: Some("profile-steer-once".into()),
            expected_epoch: Some(service.events.cursor().epoch),
            expected_revision: None,
            target_node_id: Some(service.node_id().into()),
        };
        let steered = service
            .handle(steer.clone(), ClientRole::Mobile, "phone")
            .await;
        assert!(steered.error.is_none(), "steer: {:?}", steered.error);
        let mut steer_retry = steer;
        steer_retry.id = "profile-steer-retry".into();
        let replay = service
            .handle(steer_retry, ClientRole::Mobile, "phone")
            .await;
        assert_eq!(steered.result, replay.result);

        for kind in ["task", "learning"] {
            let id = format!("fixture-{kind}");
            store
                .bind_session(brr_agent_core::SessionBinding {
                    thread_id: id.clone(),
                    agent_id: None,
                    kind: kind.into(),
                    project_id: None,
                    cwd: None,
                    task_run_id: None,
                    learning_eligible: false,
                })
                .await
                .unwrap();
            let rejected = service
                .handle(
                    send(
                        &format!("{kind}-send"),
                        &id,
                        "must remain owned",
                        &format!("{kind}-send-once"),
                        "fixture-model",
                        "default",
                    ),
                    ClientRole::Mobile,
                    "phone",
                )
                .await;
            assert!(
                rejected
                    .error
                    .as_ref()
                    .is_some_and(|error| error.message.contains("owning workflow controls")),
                "{kind}: {:?}",
                rejected.error
            );
        }

        let state: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("data/node/fixture-runtime.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            state["calls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|call| call["method"] == "turn/steer")
                .count(),
            1
        );
        assert_eq!(
            state["calls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|call| call["method"] == "turn/steer")
                .unwrap()["params"]["expectedTurnId"],
            "turn-1"
        );
        assert_eq!(
            state["threads"]
                .as_object()
                .unwrap()
                .values()
                .map(|thread| thread["turns"].as_array().unwrap().len())
                .sum::<usize>(),
            6
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn healthy_peer_stream_recovers_online_after_one_uncertain_routed_read() {
        use crate::network::{Dispatch, NetworkApi, SnapshotCallback};
        use brr_node_auth::{DeviceRole, Principal};
        use brr_remote_protocol::{EventCursor, NodeSummary, PROTOCOL_VERSION};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tokio::{net::TcpListener, sync::watch};

        let primary_dir = tempfile::tempdir().unwrap();
        let worker_dir = tempfile::tempdir().unwrap();
        let primary_data = primary_dir.path().join("primary-data");
        let service = Service::open(ServiceConfig {
            data_dir: primary_data.clone(),
            runtime_path: primary_dir.path().join("missing-owned-runtime"),
            name: "Monitor recovery primary".into(),
            endpoint_hosts: vec!["127.0.0.1".into()],
        })
        .await
        .unwrap();
        service
            .peers
            .init_primary(service.node_id())
            .await
            .unwrap();

        let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary_address = primary_listener.local_addr().unwrap();
        let primary_endpoint = format!("https://127.0.0.1:{}", primary_address.port());
        service
            .set_endpoint(primary_endpoint.clone())
            .await
            .unwrap();
        let (primary_network_stop, primary_network_shutdown) = watch::channel(false);
        let primary_network = tokio::spawn(crate::network::serve(
            primary_listener,
            service.network_api(),
            primary_network_shutdown,
        ));
        let (monitor_stop, monitor_shutdown) = watch::channel(false);
        let monitor = tokio::spawn(service.clone().run_peer_monitor(monitor_shutdown));

        let primary_invite = service
            .auth()
            .create_invite(DeviceRole::Node, std::time::Duration::from_secs(120))
            .await
            .unwrap();

        let worker_node_dir = worker_dir.path().join("worker/node");
        std::fs::create_dir_all(&worker_node_dir).unwrap();
        private_directory(&worker_node_dir).unwrap();
        let worker_auth = NodeAuthStore::open(
            worker_node_dir.join("auth.sqlite"),
            vec!["localhost".into()],
        )
        .await
        .unwrap();
        let worker_id = worker_auth.node_id().to_owned();
        let worker_peers = Peers::open(worker_node_dir.join("peers.sqlite"))
            .await
            .unwrap();
        let worker_events = EventHub::new(worker_id.clone(), "worker-stream".into(), 64);
        let mut primary_events = service.events().subscribe();
        let reject_first_status = Arc::new(AtomicBool::new(true));
        let status_dispatches = Arc::new(AtomicUsize::new(0));
        let snapshot_requests = Arc::new(AtomicUsize::new(0));
        let dispatch_reject = reject_first_status.clone();
        let dispatch_count = status_dispatches.clone();
        let dispatch_peers = worker_peers.clone();
        let dispatch: Dispatch = Arc::new(move |principal: Principal, request: Request| {
            let reject_first_status = dispatch_reject.clone();
            let status_dispatches = dispatch_count.clone();
            let peers = dispatch_peers.clone();
            Box::pin(async move {
                let id = request.id.clone();
                if let Err(error) = peers
                    .authorize_node_command(&principal, &request)
                    .await
                {
                    return Reply::from_result(id, Err(error));
                }
                if request.channel == Channel::System && request.method == "status" {
                    status_dispatches.fetch_add(1, Ordering::SeqCst);
                    if reject_first_status.swap(false, Ordering::SeqCst) {
                        return Reply {
                            id: "deliberately-mismatched-ack".into(),
                            result: Some(json!({"runtimeConnected":true})),
                            error: None,
                        };
                    }
                }
                Reply::from_result(id, Ok(json!({"runtimeConnected":true})))
            })
        });
        let snapshot_id = worker_id.clone();
        let snapshot_count = snapshot_requests.clone();
        let snapshot_events = worker_events.clone();
        let snapshot: SnapshotCallback = Arc::new(move |_principal| {
            let node_id = snapshot_id.clone();
            let cursor = snapshot_events.cursor();
            snapshot_count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(Snapshot {
                    protocol_version: PROTOCOL_VERSION,
                    node_id: node_id.clone(),
                    epoch: cursor.epoch.clone(),
                    cursor,
                    nodes: vec![NodeSummary {
                        node_id,
                        name: "Recovery worker".into(),
                        platform: "test".into(),
                        online: true,
                        primary: false,
                        authenticated: true,
                        runtime_connected: true,
                        epoch: "worker-runtime".into(),
                    }],
                    account: None,
                    models: vec![],
                    threads: vec![],
                    projects: vec![],
                    agents: vec![],
                    tasks: vec![],
                    questions: vec![],
                    approvals: vec![],
                    sync: json!({"state":"ready"}),
                    errors: vec![],
                })
            })
        });
        let observer_peers = worker_peers.clone();
        let observer: crate::network::NodeObserverCallback = Arc::new(move |principal| {
            let peers = observer_peers.clone();
            Box::pin(async move { peers.authorize_node_observer(&principal).await })
        });
        let worker_api = Arc::new(
            NetworkApi::new(
                worker_auth.clone(),
                dispatch,
                snapshot,
                worker_events.clone(),
                None,
            )
            .with_node_observer(observer),
        );
        let worker_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker_address = worker_listener.local_addr().unwrap();
        let worker_endpoint = format!("https://localhost:{}", worker_address.port());
        let (worker_network_stop, worker_network_shutdown) = watch::channel(false);
        let worker_network = tokio::spawn(crate::network::serve(
            worker_listener,
            worker_api,
            worker_network_shutdown,
        ));

        worker_peers
            .join(
                &worker_auth,
                JoinConfig {
                    primary_endpoint,
                    primary_node_id: service.node_id().into(),
                    primary_ca_pem: primary_invite.server_certificate_pem.clone(),
                    primary_fingerprint: primary_invite.fingerprint.clone(),
                    invite_code: primary_invite.code,
                    name: "Recovery worker".into(),
                    node_endpoint: worker_endpoint,
                    reverse_invite_ttl: std::time::Duration::from_secs(120),
                },
            )
            .await
            .unwrap();

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut initial_stream_ready = false;
        while tokio::time::Instant::now() < ready_deadline {
            let cached = service.peers.cached_snapshot(&worker_id).await.unwrap();
            if cached.online && cached.snapshot.is_some() {
                initial_stream_ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Prove the monitor has consumed an event on the live SSE stream
        // before inducing the uncertain route. This setup marker is included
        // in the worker snapshot cursor and is the only owner event published
        // during the test.
        worker_events.publish(json!({
            "method":"test/setup",
            "params":{"marker":"stream-ready"}
        }));
        let setup_cursor = worker_events.cursor();
        let setup_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut setup_event_cached = false;
        while tokio::time::Instant::now() < setup_deadline {
            let replay = service
                .peers
                .cached_events(
                    &worker_id,
                    Some(&EventCursor {
                        epoch: "worker-stream".into(),
                        sequence: 0,
                    }),
                )
                .await
                .unwrap();
            if replay.events.iter().any(|event| {
                event.message["method"] == "test/setup"
                    && event.message["params"]["marker"] == "stream-ready"
            }) {
                setup_event_cached = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(setup_event_cached, "worker setup event was not consumed from SSE");

        let setup_notification_deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut setup_notification_seen = false;
        while tokio::time::Instant::now() < setup_notification_deadline {
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                primary_events.recv(),
            )
            .await
            {
                Ok(Ok(event))
                    if event.node_id == worker_id
                        && event.message["method"] == "test/setup"
                        && event.message["params"]["marker"] == "stream-ready" =>
                {
                    setup_notification_seen = true;
                    break;
                }
                Ok(Ok(_)) | Err(_) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            }
        }
        assert!(setup_notification_seen, "primary did not publish the streamed setup event");
        while primary_events.try_recv().is_ok() {}

        // Route one read-only status request. The worker intentionally returns
        // an acknowledgement with the wrong id. That is treated as uncertain
        // and marks the peer offline, while its independent SSE stream stays
        // open and emits no events.
        let routed = service
            .handle(
                Request {
                    id: "probe-status-once".into(),
                    channel: Channel::System,
                    method: "status".into(),
                    params: json!({}),
                    operation_id: None,
                    expected_epoch: None,
                    expected_revision: None,
                    target_node_id: Some(worker_id.clone()),
                },
                ClientRole::Mobile,
                "test-mobile-device",
            )
            .await;
        let route_was_uncertain = routed
            .error
            .as_ref()
            .is_some_and(|error| error.code == "uncertain");
        let offline_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut marked_offline = false;
        while tokio::time::Instant::now() < offline_deadline {
            if !service.peers.cached_snapshot(&worker_id).await.unwrap().online {
                marked_offline = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let snapshots_before_recovery = snapshot_requests.load(Ordering::SeqCst);
        let online_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(22);
        let mut recovered_online = false;
        let mut online_changed_notification = false;
        while tokio::time::Instant::now() < online_deadline {
            match tokio::time::timeout(
                std::time::Duration::from_millis(100),
                primary_events.recv(),
            )
            .await
            {
                Ok(Ok(event)) => {
                    if event.message["method"] == "brr/fleet/changed"
                        && event.message["params"]["nodeId"] == worker_id
                    {
                        let is_fresh_and_online = snapshot_requests.load(Ordering::SeqCst)
                            > snapshots_before_recovery
                            && service
                                .peers
                                .cached_snapshot(&worker_id)
                                .await
                                .unwrap()
                                .online;
                        if is_fresh_and_online {
                            online_changed_notification = true;
                            recovered_online = true;
                            break;
                        }
                    }
                }
                Err(_) => {
                    if snapshot_requests.load(Ordering::SeqCst) > snapshots_before_recovery
                        && service.peers.cached_snapshot(&worker_id).await.unwrap().online
                        && online_changed_notification
                    {
                        recovered_online = true;
                        break;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            }
        }
        let attempts_without_replay = status_dispatches.load(Ordering::SeqCst);
        let no_owner_events_after_failure = worker_events.cursor().sequence == setup_cursor.sequence;

        // Shut down before assertions so the intentional RED does not leave
        // listener or monitor tasks running in the test runtime.
        let _ = monitor_stop.send(true);
        let _ = primary_network_stop.send(true);
        let _ = worker_network_stop.send(true);
        let _ = monitor.await;
        let _ = primary_network.await;
        let _ = worker_network.await;
        service.shutdown().await;

        assert!(initial_stream_ready, "worker snapshot did not hydrate before the test");
        assert!(
            route_was_uncertain,
            "the one mismatched acknowledgement was not uncertain: {:?}",
            routed.error
        );
        assert!(marked_offline, "the uncertain route did not mark the peer offline");
        assert!(
            recovered_online,
            "the still-open healthy SSE stream never triggered a fresh snapshot and peer recovery without an explicit refresh"
        );
        assert!(
            online_changed_notification,
            "primary did not publish brr/fleet/changed after fresh snapshot restored the peer"
        );
        assert!(
            no_owner_events_after_failure,
            "an owner event after the uncertain route could have triggered recovery"
        );
        assert_eq!(attempts_without_replay, 1, "the original read request was replayed");
    }
}
