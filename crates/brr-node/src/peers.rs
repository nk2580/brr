//! Persistent topology and pinned remote-node routing for a personal Brr fleet.

use brr_node_auth::{DeviceRole, NodeAuthStore, Principal};
use brr_remote_protocol::{ApiError, Event, EventCursor, EventReplay, Reply, Request, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerMode {
    Standalone,
    JoinPending,
    Primary,
    Node,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub node_id: String,
    pub name: String,
    pub endpoint: String,
    pub fingerprint: String,
    pub online: bool,
    pub primary_device_id: String,
    pub remote_device_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSnapshot {
    pub node_id: String,
    pub online: bool,
    pub snapshot: Option<Value>,
}

/// Join material contains a bearer invite. It deliberately has no `Debug` or `Serialize`.
pub struct JoinConfig {
    pub primary_endpoint: String,
    pub primary_node_id: String,
    pub primary_ca_pem: String,
    pub primary_fingerprint: String,
    pub invite_code: String,
    pub name: String,
    pub node_endpoint: String,
    pub reverse_invite_ttl: Duration,
}

pub struct JoinReceipt {
    pub primary_node_id: String,
    pub local_node_id: String,
    pub primary_device_id: String,
}

/// Sensitive outbound bearer, intentionally redacted when formatted.
pub struct SecretString(String);
impl SecretString {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

pub struct PeerTransport {
    pub node_id: String,
    pub endpoint: String,
    pub trusted_ca_pem: String,
    pub fingerprint: String,
    pub credential: SecretString,
}

/// Result of removing a node. Local access and routing are disabled immediately;
/// `remote_revocation_pending` remains true until the node acknowledges leave.
pub struct PeerRemoval {
    pub node_id: String,
    pub remote_revocation_pending: bool,
}

#[derive(Clone)]
pub struct Peers {
    pool: SqlitePool,
    join_lock: Arc<Mutex<()>>,
}

impl Peers {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, ApiError> {
        let path = path.as_ref();
        ensure_roster_path(path)?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(storage)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS fleet_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),mode TEXT NOT NULL,local_node_id TEXT,owner_node_id TEXT,owner_endpoint TEXT,owner_ca_pem TEXT,owner_fingerprint TEXT,owner_device_id TEXT,owner_reverse_device_id TEXT,owner_secret TEXT,local_endpoint TEXT,local_ca_pem TEXT,local_fingerprint TEXT,local_name TEXT,pending_reverse_code TEXT)")
            .execute(&pool).await.map_err(storage)?;
        ensure_column(&pool, "fleet_state", "owner_reverse_device_id", "TEXT").await?;
        sqlx::query("INSERT OR IGNORE INTO fleet_state(singleton,mode) VALUES(1,'standalone')")
            .execute(&pool)
            .await
            .map_err(storage)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS peer_nodes(node_id TEXT PRIMARY KEY,principal_id TEXT NOT NULL UNIQUE,name TEXT NOT NULL,endpoint TEXT NOT NULL,ca_pem TEXT NOT NULL,fingerprint TEXT NOT NULL,primary_device_id TEXT NOT NULL,remote_device_id TEXT NOT NULL,secret TEXT NOT NULL,online INTEGER NOT NULL DEFAULT 1,removed INTEGER NOT NULL DEFAULT 0,latest_epoch TEXT,latest_sequence INTEGER NOT NULL DEFAULT 0,last_seen INTEGER NOT NULL)")
            .execute(&pool).await.map_err(storage)?;
        ensure_column(&pool, "peer_nodes", "removed", "INTEGER NOT NULL DEFAULT 0").await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS peer_snapshots(node_id TEXT PRIMARY KEY,snapshot_json TEXT NOT NULL,updated_at INTEGER NOT NULL,FOREIGN KEY(node_id) REFERENCES peer_nodes(node_id) ON DELETE CASCADE)")
            .execute(&pool).await.map_err(storage)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS peer_events(node_id TEXT NOT NULL,epoch TEXT NOT NULL,sequence INTEGER NOT NULL,event_json TEXT NOT NULL,PRIMARY KEY(node_id,epoch,sequence),FOREIGN KEY(node_id) REFERENCES peer_nodes(node_id) ON DELETE CASCADE)")
            .execute(&pool).await.map_err(storage)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS peer_event_state(node_id TEXT PRIMARY KEY,epoch TEXT,sequence INTEGER NOT NULL DEFAULT 0,requires_snapshot INTEGER NOT NULL DEFAULT 1,FOREIGN KEY(node_id) REFERENCES peer_nodes(node_id) ON DELETE CASCADE)")
            .execute(&pool).await.map_err(storage)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS peer_retired_epochs(node_id TEXT NOT NULL,epoch TEXT NOT NULL,PRIMARY KEY(node_id,epoch),FOREIGN KEY(node_id) REFERENCES peer_nodes(node_id) ON DELETE CASCADE)")
            .execute(&pool).await.map_err(storage)?;
        // Existing cache cursors lack enough evidence to prove there were no
        // gaps. Force a snapshot once after upgrading, then track contiguity.
        sqlx::query("INSERT OR IGNORE INTO peer_event_state(node_id,epoch,sequence,requires_snapshot) SELECT node_id,latest_epoch,latest_sequence,1 FROM peer_nodes")
            .execute(&pool).await.map_err(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(storage)?;
        }
        Ok(Self {
            pool,
            join_lock: Arc::new(Mutex::new(())),
        })
    }

    pub async fn mode(&self) -> Result<PeerMode, ApiError> {
        let value: String = sqlx::query_scalar("SELECT mode FROM fleet_state WHERE singleton=1")
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        match value.as_str() {
            "standalone" => Ok(PeerMode::Standalone),
            "join_pending" => Ok(PeerMode::JoinPending),
            "primary" => Ok(PeerMode::Primary),
            "node" => Ok(PeerMode::Node),
            _ => Err(storage("unknown persisted fleet mode")),
        }
    }

    pub async fn init_primary(&self, local_node_id: &str) -> Result<(), ApiError> {
        valid_id(local_node_id)?;
        let _guard = self.join_lock.lock().await;
        let row = sqlx::query("SELECT mode,local_node_id FROM fleet_state WHERE singleton=1")
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        let mode: String = row.get("mode");
        let current: Option<String> = row.get("local_node_id");
        if mode == "primary" && current.as_deref() == Some(local_node_id) {
            return Ok(());
        }
        if mode != "standalone" {
            return Err(ApiError::new(
                "fleet_mode_conflict",
                "This node is already configured for a different fleet role",
            ));
        }
        sqlx::query("UPDATE fleet_state SET mode='primary',local_node_id=? WHERE singleton=1")
            .bind(local_node_id)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    pub async fn join(
        &self,
        local_auth: &NodeAuthStore,
        config: JoinConfig,
    ) -> Result<JoinReceipt, ApiError> {
        let _guard = self.join_lock.lock().await;
        if self.mode().await? != PeerMode::Standalone {
            return Err(ApiError::new(
                "fleet_mode_conflict",
                "This node is already configured for a fleet",
            ));
        }
        let primary_url = validate_root_https(&config.primary_endpoint)?;
        verify_certificate_pin(&config.primary_ca_pem, &config.primary_fingerprint)?;
        ensure_host_in_certificate(&config.node_endpoint, &local_auth.identity_info().endpoints)?;
        if config.reverse_invite_ttl.is_zero()
            || config.name.trim().is_empty()
            || config.primary_node_id.is_empty()
            || config.invite_code.is_empty()
        {
            return Err(ApiError::new("invalid_join", "Join details are incomplete"));
        }
        let client = pinned_client(&config.primary_ca_pem, &config.primary_fingerprint)?;
        let pair_url = primary_url.join("v1/pair").map_err(endpoint_error)?;
        let pair_response = client
            .post(pair_url)
            .json(&json!({"code":config.invite_code,"requestedRole":"node","name":config.name}))
            .send()
            .await
            .map_err(|_| {
                ApiError::new(
                    "owner_unavailable",
                    "Could not reach the invited primary node",
                )
            })?;
        if !pair_response.status().is_success() {
            return Err(ApiError::new(
                "pairing_rejected",
                "The primary node rejected the pairing invitation",
            ));
        }
        let primary: PairResponse = pair_response.json().await.map_err(|_| {
            ApiError::new(
                "invalid_pair_response",
                "The primary returned an invalid pairing response",
            )
        })?;
        if primary.role != DeviceRole::Node || primary.node_id != config.primary_node_id {
            return Err(ApiError::new(
                "owner_identity_mismatch",
                "The invited endpoint did not identify as the selected primary",
            ));
        }
        verify_certificate_pin(&primary.trusted_server_cert_pem, &primary.fingerprint)?;
        if primary.fingerprint != config.primary_fingerprint
            || primary.trusted_server_cert_pem != config.primary_ca_pem
        {
            return Err(ApiError::new(
                "owner_identity_mismatch",
                "The primary pairing response changed its pinned TLS identity",
            ));
        }

        let reverse = local_auth
            .create_invite(DeviceRole::Node, config.reverse_invite_ttl)
            .await
            .map_err(auth_error)?;
        let local_id = local_auth.node_id().to_owned();
        sqlx::query("UPDATE fleet_state SET mode='join_pending',local_node_id=?,owner_node_id=?,owner_endpoint=?,owner_ca_pem=?,owner_fingerprint=?,owner_device_id=?,owner_secret=?,local_endpoint=?,local_ca_pem=?,local_fingerprint=?,local_name=?,pending_reverse_code=? WHERE singleton=1")
            .bind(&local_id).bind(&config.primary_node_id).bind(config.primary_endpoint.trim_end_matches('/'))
            .bind(&config.primary_ca_pem).bind(&config.primary_fingerprint).bind(&primary.device_id).bind(&primary.secret)
            .bind(&config.node_endpoint).bind(local_auth.certificate_pem()).bind(local_auth.fingerprint()).bind(&config.name)
            .bind(&reverse.code).execute(&self.pool).await.map_err(storage)?;
        self.complete_pending_join().await?;
        Ok(JoinReceipt {
            primary_node_id: config.primary_node_id,
            local_node_id: local_id,
            primary_device_id: primary.device_id,
        })
    }

    pub async fn resume_join(&self) -> Result<JoinReceipt, ApiError> {
        let _guard = self.join_lock.lock().await;
        self.complete_pending_join().await
    }

    async fn complete_pending_join(&self) -> Result<JoinReceipt, ApiError> {
        let row = sqlx::query("SELECT mode,local_node_id,owner_node_id,owner_endpoint,owner_ca_pem,owner_fingerprint,owner_device_id,owner_secret,local_endpoint,local_ca_pem,local_fingerprint,local_name,pending_reverse_code FROM fleet_state WHERE singleton=1")
            .fetch_one(&self.pool).await.map_err(storage)?;
        let mode: String = row.get("mode");
        if mode != "join_pending" {
            return Err(ApiError::new(
                "fleet_mode_conflict",
                "There is no pending node registration",
            ));
        }
        let local_id: String = row.get("local_node_id");
        let owner_id: String = row.get("owner_node_id");
        let endpoint: String = row.get("owner_endpoint");
        let ca: String = row.get("owner_ca_pem");
        let fingerprint: String = row.get("owner_fingerprint");
        let primary_device_id: String = row.get("owner_device_id");
        let primary_secret: String = row.get("owner_secret");
        let local_endpoint: String = row.get("local_endpoint");
        let reverse_code: String = row.get("pending_reverse_code");
        valid_id(&local_id)?;
        let url = validate_root_https(&endpoint)?;
        let client = pinned_client(&ca, &fingerprint)?;
        let params = json!({
            "nodeId": local_id,
            "endpoint": local_endpoint,
            "trustedServerCertPem": row.get::<String, _>("local_ca_pem"),
            "fingerprint": row.get::<String, _>("local_fingerprint"),
            "inviteCode": reverse_code,
            "name": row.get::<String, _>("local_name")
        });
        let request = Request {
            id: format!("register-{}", namespace_operation("node", &local_id)),
            channel: brr_remote_protocol::Channel::Fleet,
            method: "node.register".into(),
            params,
            operation_id: Some(format!(
                "fleet-register-{}",
                namespace_operation("node", &local_id)
            )),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        };
        let url = url.join("v1/request").map_err(endpoint_error)?;
        let response = client.post(url).bearer_auth(&primary_secret).json(&request).send().await
            .map_err(|_| ApiError::new("uncertain", "Primary registration outcome is unknown; retry the same registration operation"))?;
        if !response.status().is_success() {
            return Err(ApiError::new(
                "uncertain",
                "Primary registration outcome is unknown; retry the same registration operation",
            ));
        }
        let reply: Reply = response.json().await.map_err(|_| {
            ApiError::new(
                "uncertain",
                "Primary registration outcome is unknown; retry the same registration operation",
            )
        })?;
        if reply.id != request.id {
            return Err(ApiError::new(
                "uncertain",
                "Primary registration acknowledgement did not match the request",
            ));
        }
        if let Some(error) = reply.error {
            return Err(error);
        }
        let result = reply.result.unwrap_or(Value::Null);
        if result["nodeId"].as_str() != Some(local_id.as_str()) {
            return Err(ApiError::new(
                "owner_identity_mismatch",
                "Primary did not confirm the local node identity",
            ));
        }
        let reverse_device_id = result["reverseDeviceId"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ApiError::new(
                    "invalid_registration_acknowledgement",
                    "Primary did not confirm the reciprocal device identity",
                )
            })?;
        sqlx::query(
            "UPDATE fleet_state SET mode='node',owner_reverse_device_id=?,pending_reverse_code=NULL WHERE singleton=1",
        )
        .bind(reverse_device_id)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(JoinReceipt {
            primary_node_id: owner_id,
            local_node_id: local_id,
            primary_device_id,
        })
    }

    pub async fn register_reverse(
        &self,
        principal: &Principal,
        params: Value,
    ) -> Result<Value, ApiError> {
        let _guard = self.join_lock.lock().await;
        if self.mode().await? != PeerMode::Primary || principal.role != DeviceRole::Node {
            return Err(ApiError::new(
                "forbidden",
                "Only a paired node may register with the primary",
            ));
        }
        let owner_id: String =
            sqlx::query_scalar("SELECT local_node_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        if principal.node_id != owner_id {
            return Err(ApiError::new(
                "forbidden",
                "The registering device is not paired to this primary identity",
            ));
        }
        let (node_id, endpoint, ca_pem, fingerprint, invite_code, name) =
            parse_registration(&params)?;
        validate_root_https(&endpoint)?;
        verify_certificate_pin(&ca_pem, &fingerprint)?;
        if node_id == owner_id {
            return Err(ApiError::new(
                "invalid_node",
                "A primary cannot register itself as a peer",
            ));
        }

        if let Some(existing) = sqlx::query(
            "SELECT node_id,remote_device_id,removed FROM peer_nodes WHERE principal_id=?",
        )
        .bind(&principal.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        {
            let bound: String = existing.get("node_id");
            if bound == node_id {
                let removed: i64 = existing.get("removed");
                if removed != 0 {
                    return Err(ApiError::new(
                        "removal_pending",
                        "This node is awaiting reciprocal credential revocation",
                    ));
                }
                let reverse_device_id: String = existing.get("remote_device_id");
                return Ok(
                    json!({"nodeId":bound,"principalId":principal.id,"reverseDeviceId":reverse_device_id,"registered":true}),
                );
            }
            return Err(ApiError::new(
                "owner_conflict",
                "This paired device is already bound to a different node",
            ));
        }
        if sqlx::query("SELECT 1 FROM peer_nodes WHERE node_id=?")
            .bind(&node_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?
            .is_some()
        {
            return Err(ApiError::new(
                "owner_conflict",
                "This node identity is already bound to another paired device",
            ));
        }

        let client = pinned_client(&ca_pem, &fingerprint)?;
        let mut remote = validate_root_https(&endpoint)?;
        remote = remote.join("v1/pair").map_err(endpoint_error)?;
        let response = client.post(remote).json(&json!({"code":invite_code,"requestedRole":"node","name":name_for_primary(&principal.name)}))
            .send().await.map_err(|_| ApiError::new("owner_unavailable", "The joining node is not reachable"))?;
        if !response.status().is_success() {
            return Err(ApiError::new(
                "pairing_rejected",
                "The joining node rejected its reverse invitation",
            ));
        }
        let remote_credential: PairResponse = response.json().await.map_err(|_| {
            ApiError::new(
                "invalid_pair_response",
                "The joining node returned an invalid pairing response",
            )
        })?;
        if remote_credential.role != DeviceRole::Node || remote_credential.node_id != node_id {
            return Err(ApiError::new(
                "owner_identity_mismatch",
                "The reverse invitation endpoint did not identify as the claimed node",
            ));
        }
        verify_certificate_pin(
            &remote_credential.trusted_server_cert_pem,
            &remote_credential.fingerprint,
        )?;
        if remote_credential.fingerprint != fingerprint
            || remote_credential.trusted_server_cert_pem != ca_pem
        {
            return Err(ApiError::new(
                "owner_identity_mismatch",
                "The joining node changed its pinned TLS identity",
            ));
        }
        sqlx::query("INSERT INTO peer_nodes(node_id,principal_id,name,endpoint,ca_pem,fingerprint,primary_device_id,remote_device_id,secret,online,last_seen) VALUES(?,?,?,?,?,?,?,?,?,1,unixepoch())")
            .bind(&node_id).bind(&principal.id).bind(&name).bind(&endpoint).bind(&ca_pem).bind(&fingerprint)
            .bind(&principal.id).bind(&remote_credential.device_id).bind(&remote_credential.secret)
            .execute(&self.pool).await.map_err(storage)?;
        Ok(
            json!({"nodeId":node_id,"principalId":principal.id,"reverseDeviceId":remote_credential.device_id,"registered":true}),
        )
    }

    /// Authorize commands made with a Node-role bearer. A primary accepts
    /// registration from a paired node and status reads from already-bound
    /// workers. A joined node accepts scoped work only from its exact paired
    /// primary credential, plus the reciprocal removal command.
    pub async fn authorize_node_command(
        &self,
        principal: &Principal,
        request: &Request,
    ) -> Result<(), ApiError> {
        if principal.role != DeviceRole::Node {
            return Err(forbidden_node());
        }
        match self.mode().await? {
            PeerMode::Primary => {
                let owner: Option<String> =
                    sqlx::query_scalar("SELECT local_node_id FROM fleet_state WHERE singleton=1")
                        .fetch_one(&self.pool)
                        .await
                        .map_err(storage)?;
                if owner.as_deref() != Some(principal.node_id.as_str()) {
                    return Err(forbidden_node());
                }
                if request
                    .target_node_id
                    .as_deref()
                    .is_some_and(|target| target != principal.node_id)
                {
                    return Err(forbidden_node());
                }
                if request.channel == brr_remote_protocol::Channel::Fleet
                    && request.method == "node.register"
                {
                    return if self.registered_node_id_for(principal).await?.is_none()
                        || self
                            .reconcile_registration(principal, &request.params)
                            .await?
                            .is_some()
                    {
                        Ok(())
                    } else {
                        Err(forbidden_node())
                    };
                }
                if request.channel == brr_remote_protocol::Channel::System
                    && request.method == "status"
                    && self.registered_node_id_for(principal).await?.is_some()
                {
                    return Ok(());
                }
                Err(forbidden_node())
            }
            PeerMode::Node => {
                self.authorize_node_observer(principal).await?;
                if request.channel == brr_remote_protocol::Channel::Fleet
                    && request.method == "node.remove"
                {
                    return Ok(());
                }
                if request.channel == brr_remote_protocol::Channel::Fleet
                    || (request.channel == brr_remote_protocol::Channel::System
                        && !matches!(
                            request.method.as_str(),
                            "status" | "account" | "models" | "login.status"
                        ))
                {
                    return Err(forbidden_node());
                }
                if request
                    .target_node_id
                    .as_deref()
                    .is_some_and(|target| target != principal.node_id)
                {
                    return Err(forbidden_node());
                }
                request.validate(brr_remote_protocol::ClientRole::Node)?;
                Ok(())
            }
            PeerMode::Standalone | PeerMode::JoinPending => Err(forbidden_node()),
        }
    }

    /// Only the exact reciprocal primary credential may observe a joined
    /// worker. A Node credential on a primary is never an observer.
    pub async fn authorize_node_observer(&self, principal: &Principal) -> Result<(), ApiError> {
        if principal.role != DeviceRole::Node || self.mode().await? != PeerMode::Node {
            return Err(forbidden_node());
        }
        let row = sqlx::query(
            "SELECT local_node_id,owner_node_id,owner_reverse_device_id FROM fleet_state WHERE singleton=1",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        let local_id: Option<String> = row.get("local_node_id");
        let owner_id: Option<String> = row.get("owner_node_id");
        let reverse_id: Option<String> = row.get("owner_reverse_device_id");
        if local_id.as_deref() == Some(principal.node_id.as_str())
            && owner_id.as_deref().is_some_and(|id| !id.is_empty())
            && reverse_id.as_deref() == Some(principal.id.as_str())
        {
            Ok(())
        } else {
            Err(forbidden_node())
        }
    }

    /// Resolve an uncertain registration operation from durable local roster
    /// state only. This never redeems an invite or makes a network request.
    pub async fn reconcile_registration(
        &self,
        principal: &Principal,
        params: &Value,
    ) -> Result<Option<Value>, ApiError> {
        if self.mode().await? != PeerMode::Primary || principal.role != DeviceRole::Node {
            return Err(forbidden_node());
        }
        let owner: Option<String> =
            sqlx::query_scalar("SELECT local_node_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        if owner.as_deref() != Some(principal.node_id.as_str()) {
            return Err(forbidden_node());
        }
        let (node_id, endpoint, ca_pem, fingerprint, _, _) = parse_registration(params)?;
        validate_root_https(&endpoint)?;
        verify_certificate_pin(&ca_pem, &fingerprint)?;
        let row = sqlx::query(
            "SELECT node_id,principal_id,endpoint,ca_pem,fingerprint,remote_device_id,removed FROM peer_nodes WHERE principal_id=?",
        )
        .bind(&principal.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let matches = row.get::<String, _>("node_id") == node_id
            && row.get::<String, _>("principal_id") == principal.id
            && row.get::<String, _>("endpoint") == endpoint
            && row.get::<String, _>("ca_pem") == ca_pem
            && row.get::<String, _>("fingerprint") == fingerprint
            && row.get::<i64, _>("removed") == 0;
        if !matches {
            return Ok(None);
        }
        let reverse_device_id: String = row.get("remote_device_id");
        Ok(Some(json!({
            "nodeId": node_id,
            "principalId": principal.id,
            "reverseDeviceId": reverse_device_id,
            "registered": true
        })))
    }

    pub async fn route(
        &self,
        request: Request,
        originating_actor: &str,
    ) -> Result<Option<Reply>, ApiError> {
        let Some(target) = request.target_node_id.as_deref() else {
            return Ok(None);
        };
        let local_id: Option<String> =
            sqlx::query_scalar("SELECT local_node_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        if local_id.as_deref() == Some(target) {
            return Ok(None);
        }
        if self.mode().await? != PeerMode::Primary {
            return Err(ApiError::new(
                "owner_unavailable",
                "This node is not configured to route fleet work",
            ));
        }
        let row = sqlx::query(
            "SELECT endpoint,ca_pem,fingerprint,secret,online FROM peer_nodes WHERE node_id=? AND removed=0",
        )
        .bind(target)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or_else(|| {
            ApiError::new(
                "owner_unavailable",
                "The execution owner is not registered on this primary",
            )
        })?;
        if row.get::<i64, _>("online") == 0 {
            return Err(ApiError::new(
                "owner_offline",
                "The execution owner is offline; work was not dispatched",
            ));
        }
        let endpoint: String = row.get("endpoint");
        let ca: String = row.get("ca_pem");
        let fingerprint: String = row.get("fingerprint");
        let secret: String = row.get("secret");
        let client = pinned_client(&ca, &fingerprint)?;
        let mut forwarded = request.clone();
        if let Some(operation_id) = forwarded.operation_id.as_deref() {
            forwarded.operation_id = Some(namespace_operation(originating_actor, operation_id));
        }
        let url = validate_root_https(&endpoint)?
            .join("v1/request")
            .map_err(endpoint_error)?;
        let response = match client
            .post(url)
            .bearer_auth(secret)
            .json(&forwarded)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                self.set_peer_online(target, false).await?;
                return Err(ApiError::new(
                    "uncertain",
                    "The owner connection ended without an acknowledgement; the same operationId is safe to retry",
                ));
            }
        };
        if !response.status().is_success() {
            self.set_peer_online(target, false).await?;
            return Err(ApiError::new(
                "uncertain",
                "The owner returned no valid acknowledgement; do not submit this work with a new operationId",
            ));
        }
        let reply: Reply = match response.json().await {
            Ok(reply) => reply,
            Err(_) => {
                self.set_peer_online(target, false).await?;
                return Err(ApiError::new(
                    "uncertain",
                    "The owner acknowledgement was malformed; do not submit this work with a new operationId",
                ));
            }
        };
        if reply.id != request.id {
            self.set_peer_online(target, false).await?;
            return Err(ApiError::new(
                "uncertain",
                "The owner acknowledgement did not match the request id",
            ));
        }
        self.set_peer_online(target, true).await?;
        Ok(Some(reply))
    }

    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>, ApiError> {
        let rows = sqlx::query("SELECT node_id,name,endpoint,fingerprint,online,primary_device_id,remote_device_id FROM peer_nodes WHERE removed=0 ORDER BY name COLLATE NOCASE,node_id")
            .fetch_all(&self.pool).await.map_err(storage)?;
        rows.into_iter()
            .map(|row| {
                Ok(PeerInfo {
                    node_id: row.try_get("node_id").map_err(storage)?,
                    name: row.try_get("name").map_err(storage)?,
                    endpoint: row.try_get("endpoint").map_err(storage)?,
                    fingerprint: row.try_get("fingerprint").map_err(storage)?,
                    online: row.get::<i64, _>("online") != 0,
                    primary_device_id: row.try_get("primary_device_id").map_err(storage)?,
                    remote_device_id: row.try_get("remote_device_id").map_err(storage)?,
                })
            })
            .collect()
    }

    /// Disable a registered node locally before attempting reciprocal remote
    /// revocation. If the node is unreachable, its secret remains only in the
    /// private pending-removal row so a later service contact can finish leave.
    pub async fn remove(
        &self,
        node_id: &str,
        auth: &NodeAuthStore,
    ) -> Result<PeerRemoval, ApiError> {
        if self.mode().await? != PeerMode::Primary {
            return Err(ApiError::new(
                "fleet_mode_conflict",
                "Only the primary can remove a registered node",
            ));
        }
        let row = sqlx::query("SELECT primary_device_id FROM peer_nodes WHERE node_id=?")
            .bind(node_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
        let Some(row) = row else {
            return Ok(PeerRemoval {
                node_id: node_id.to_owned(),
                remote_revocation_pending: false,
            });
        };
        sqlx::query("UPDATE peer_nodes SET removed=1,online=0 WHERE node_id=?")
            .bind(node_id)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        let primary_device_id: String = row.get("primary_device_id");
        if auth
            .list_devices()
            .await
            .map_err(auth_error)?
            .iter()
            .any(|device| device.id == primary_device_id)
        {
            auth.revoke(&primary_device_id).await.map_err(auth_error)?;
        }
        let completed = self.finalize_pending_removal(node_id).await?;
        Ok(PeerRemoval {
            node_id: node_id.to_owned(),
            remote_revocation_pending: !completed,
        })
    }

    /// Retry reciprocal revocations for nodes already disabled by `remove`.
    /// Successful rows are deleted, cascading only fleet cache metadata; task,
    /// thread, and Git stores are outside this roster and are retained.
    pub async fn retry_pending_removals(&self) -> Result<usize, ApiError> {
        if self.mode().await? != PeerMode::Primary {
            return Ok(0);
        }
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT node_id FROM peer_nodes WHERE removed=1 ORDER BY node_id")
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
        let mut completed = 0;
        for node_id in ids {
            if self.finalize_pending_removal(&node_id).await? {
                completed += 1;
            }
        }
        Ok(completed)
    }

    /// Complete a leave requested by the exact reciprocal credential issued
    /// to the configured primary. Identity files and node-local data remain.
    pub async fn leave(
        &self,
        auth: &NodeAuthStore,
        principal: &Principal,
        params: &Value,
    ) -> Result<(), ApiError> {
        if principal.role != DeviceRole::Node {
            return Err(ApiError::new(
                "forbidden",
                "Only the paired primary may remove this node",
            ));
        }
        let local_id: Option<String> =
            sqlx::query_scalar("SELECT local_node_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        let owner_id: Option<String> =
            sqlx::query_scalar("SELECT owner_node_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        let reverse_device_id: Option<String> =
            sqlx::query_scalar("SELECT owner_reverse_device_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        if self.mode().await? != PeerMode::Node
            || local_id.as_deref() != Some(auth.node_id())
            || principal.node_id != auth.node_id()
            || principal.id != reverse_device_id.unwrap_or_default()
            || params["nodeId"].as_str() != Some(auth.node_id())
            || params["primaryNodeId"].as_str() != owner_id.as_deref()
            || params["primaryDeviceId"].as_str() != Some(principal.id.as_str())
        {
            return Err(ApiError::new(
                "forbidden",
                "The caller is not the configured primary credential for this node",
            ));
        }
        auth.revoke(&principal.id).await.map_err(auth_error)?;
        sqlx::query("UPDATE fleet_state SET mode='standalone',owner_node_id=NULL,owner_endpoint=NULL,owner_ca_pem=NULL,owner_fingerprint=NULL,owner_device_id=NULL,owner_reverse_device_id=NULL,owner_secret=NULL,pending_reverse_code=NULL WHERE singleton=1")
            .execute(&self.pool).await.map_err(storage)?;
        Ok(())
    }

    async fn finalize_pending_removal(&self, node_id: &str) -> Result<bool, ApiError> {
        let row = sqlx::query("SELECT endpoint,ca_pem,fingerprint,remote_device_id,secret FROM peer_nodes WHERE node_id=? AND removed=1")
            .bind(node_id).fetch_optional(&self.pool).await.map_err(storage)?;
        let Some(row) = row else { return Ok(true) };
        let endpoint: String = row.get("endpoint");
        let ca: String = row.get("ca_pem");
        let fingerprint: String = row.get("fingerprint");
        let remote_device_id: String = row.get("remote_device_id");
        let secret: String = row.get("secret");
        let Ok(client) = pinned_client(&ca, &fingerprint) else {
            return Ok(false);
        };
        let Ok(url) = validate_root_https(&endpoint)
            .and_then(|url| url.join("v1/request").map_err(endpoint_error))
        else {
            return Ok(false);
        };
        let local_primary: Option<String> =
            sqlx::query_scalar("SELECT local_node_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        let Some(primary_node_id) = local_primary else {
            return Ok(false);
        };
        let operation = namespace_operation("fleet-remove", node_id);
        let request = Request {
            id: format!("fleet-remove-{operation}"),
            channel: brr_remote_protocol::Channel::Fleet,
            method: "node.remove".into(),
            params: json!({
                "nodeId": node_id,
                "primaryNodeId": primary_node_id,
                "primaryDeviceId": remote_device_id,
            }),
            operation_id: Some(format!("fleet-remove-{operation}")),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        };
        let response = client
            .post(url)
            .bearer_auth(secret)
            .json(&request)
            .send()
            .await;
        let Ok(response) = response else {
            return Ok(false);
        };
        if !response.status().is_success() {
            return Ok(false);
        }
        let Ok(reply) = response.json::<Reply>().await else {
            return Ok(false);
        };
        if reply.id != request.id
            || reply.error.is_some()
            || reply.result.as_ref().is_none_or(|v| v["removed"] != true)
        {
            return Ok(false);
        }
        sqlx::query("DELETE FROM peer_nodes WHERE node_id=? AND removed=1")
            .bind(node_id)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(true)
    }

    /// Resolve a primary-side Node credential to its immutable registered
    /// owner node. Unknown, revoked, or removed principals have no association.
    pub async fn registered_node_id_for(
        &self,
        principal: &Principal,
    ) -> Result<Option<String>, ApiError> {
        if principal.role != DeviceRole::Node || self.mode().await? != PeerMode::Primary {
            return Ok(None);
        }
        let owner: Option<String> =
            sqlx::query_scalar("SELECT local_node_id FROM fleet_state WHERE singleton=1")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        if owner.as_deref() != Some(principal.node_id.as_str()) {
            return Ok(None);
        }
        sqlx::query_scalar("SELECT node_id FROM peer_nodes WHERE principal_id=? AND removed=0")
            .bind(&principal.id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)
    }

    pub async fn peer_transport(&self, node_id: &str) -> Result<PeerTransport, ApiError> {
        let row = sqlx::query(
            "SELECT endpoint,ca_pem,fingerprint,secret FROM peer_nodes WHERE node_id=? AND removed=0",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or_else(|| ApiError::new("owner_unavailable", "The peer is not registered"))?;
        Ok(PeerTransport {
            node_id: node_id.to_owned(),
            endpoint: row.get("endpoint"),
            trusted_ca_pem: row.get("ca_pem"),
            fingerprint: row.get("fingerprint"),
            credential: SecretString(row.get("secret")),
        })
    }

    /// Return the pinned HTTPS authority and non-debuggable bearer for this
    /// node's fixed primary, suitable for Sync/Git callbacks without putting
    /// credentials in remote URLs or `.git/config`.
    pub async fn git_transport(&self) -> Result<PeerTransport, ApiError> {
        if self.mode().await? != PeerMode::Node {
            return Err(ApiError::new(
                "fleet_mode_conflict",
                "This node has no configured primary Git transport",
            ));
        }
        let row = sqlx::query("SELECT owner_node_id,owner_endpoint,owner_ca_pem,owner_fingerprint,owner_secret FROM fleet_state WHERE singleton=1")
            .fetch_one(&self.pool).await.map_err(storage)?;
        Ok(PeerTransport {
            node_id: row.get("owner_node_id"),
            endpoint: row.get("owner_endpoint"),
            trusted_ca_pem: row.get("owner_ca_pem"),
            fingerprint: row.get("owner_fingerprint"),
            credential: SecretString(row.get("owner_secret")),
        })
    }

    pub async fn set_peer_online(&self, node_id: &str, online: bool) -> Result<(), ApiError> {
        let result = sqlx::query("UPDATE peer_nodes SET online=?,last_seen=CASE WHEN ?=1 THEN unixepoch() ELSE last_seen END WHERE node_id=? AND removed=0")
            .bind(i64::from(online)).bind(i64::from(online)).bind(node_id).execute(&self.pool).await.map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(ApiError::new(
                "owner_unavailable",
                "The peer is not registered",
            ));
        }
        Ok(())
    }

    /// Read only the peer's online bit while a long-lived event stream is open.
    /// A missing or removed peer is an error, never an implicit offline value.
    pub(crate) async fn peer_is_online(&self, node_id: &str) -> Result<bool, ApiError> {
        let online: Option<i64> = sqlx::query_scalar(
            "SELECT online FROM peer_nodes WHERE node_id=? AND removed=0",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        online
            .map(|value| value != 0)
            .ok_or_else(|| ApiError::new("owner_unavailable", "The peer is not registered"))
    }

    pub async fn cache_snapshot(&self, node_id: &str, snapshot: &Snapshot) -> Result<(), ApiError> {
        if snapshot.node_id != node_id {
            return Err(ApiError::new(
                "owner_identity_mismatch",
                "Snapshot belongs to a different node",
            ));
        }
        if snapshot.cursor.epoch != snapshot.epoch || snapshot.cursor.sequence > i64::MAX as u64 {
            return Err(ApiError::new(
                "invalid_snapshot_cursor",
                "Snapshot cursor does not match its owner epoch",
            ));
        }
        ensure_peer(&self.pool, node_id).await?;
        let serialized = serde_json::to_string(snapshot).map_err(storage)?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        if sqlx::query("SELECT 1 FROM peer_retired_epochs WHERE node_id=? AND epoch=?")
            .bind(node_id)
            .bind(&snapshot.cursor.epoch)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?
            .is_some()
        {
            return Err(ApiError::new(
                "retired_event_epoch",
                "Snapshot belongs to a retired owner epoch",
            ));
        }
        let previous: Option<String> =
            sqlx::query_scalar("SELECT epoch FROM peer_event_state WHERE node_id=?")
                .bind(node_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage)?
                .flatten();
        if let Some(previous) = previous.filter(|epoch| epoch != &snapshot.cursor.epoch) {
            sqlx::query("INSERT OR IGNORE INTO peer_retired_epochs(node_id,epoch) VALUES(?,?)")
                .bind(node_id)
                .bind(&previous)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            sqlx::query("DELETE FROM peer_events WHERE node_id=? AND epoch=?")
                .bind(node_id)
                .bind(previous)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        }
        // A snapshot is a new replay base. Drop cached deltas at and before
        // it (and any old deltas when repairing a gap) so later reads cannot
        // return rows beyond the snapshot's cursor.
        sqlx::query("DELETE FROM peer_events WHERE node_id=?")
            .bind(node_id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("INSERT INTO peer_snapshots(node_id,snapshot_json,updated_at) VALUES(?,?,unixepoch()) ON CONFLICT(node_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,updated_at=excluded.updated_at")
            .bind(node_id).bind(serialized).execute(&mut *tx).await.map_err(storage)?;
        sqlx::query("INSERT INTO peer_event_state(node_id,epoch,sequence,requires_snapshot) VALUES(?,?,?,0) ON CONFLICT(node_id) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence,requires_snapshot=0")
            .bind(node_id).bind(&snapshot.cursor.epoch).bind(snapshot.cursor.sequence as i64).execute(&mut *tx).await.map_err(storage)?;
        sqlx::query("UPDATE peer_nodes SET latest_epoch=?,latest_sequence=? WHERE node_id=?")
            .bind(&snapshot.cursor.epoch)
            .bind(snapshot.cursor.sequence as i64)
            .bind(node_id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        self.set_peer_online(node_id, true).await
    }

    pub async fn cached_snapshot(&self, node_id: &str) -> Result<CachedSnapshot, ApiError> {
        let online: i64 =
            sqlx::query_scalar("SELECT online FROM peer_nodes WHERE node_id=? AND removed=0")
                .bind(node_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?
                .ok_or_else(|| ApiError::new("owner_unavailable", "The peer is not registered"))?;
        let snapshot: Option<String> =
            sqlx::query_scalar("SELECT snapshot_json FROM peer_snapshots WHERE node_id=?")
                .bind(node_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?;
        Ok(CachedSnapshot {
            node_id: node_id.to_owned(),
            online: online != 0,
            snapshot: snapshot
                .map(|raw| serde_json::from_str(&raw).map_err(storage))
                .transpose()?,
        })
    }

    pub async fn cache_event(&self, event: &Event) -> Result<(), ApiError> {
        ensure_peer(&self.pool, &event.node_id).await?;
        if event.sequence == 0 || event.sequence > i64::MAX as u64 || event.epoch.is_empty() {
            return Err(ApiError::new(
                "invalid_event_cursor",
                "Peer event cursor is invalid",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(storage)?;
        if sqlx::query("SELECT 1 FROM peer_retired_epochs WHERE node_id=? AND epoch=?")
            .bind(&event.node_id)
            .bind(&event.epoch)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?
            .is_some()
        {
            return Err(ApiError::new(
                "retired_event_epoch",
                "Event belongs to a retired owner epoch",
            ));
        }
        let current = sqlx::query(
            "SELECT epoch,sequence,requires_snapshot FROM peer_event_state WHERE node_id=?",
        )
        .bind(&event.node_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let (previous_epoch, previous_sequence, previous_requires_snapshot) = match current {
            Some(row) => (
                row.try_get::<Option<String>, _>("epoch").map_err(storage)?,
                row.try_get::<i64, _>("sequence").map_err(storage)?,
                row.try_get::<i64, _>("requires_snapshot")
                    .map_err(storage)?
                    != 0,
            ),
            None => (None, 0, true),
        };
        let (latest_sequence, requires_snapshot) = if previous_epoch.as_deref()
            == Some(&event.epoch)
        {
            if event.sequence <= previous_sequence.max(0) as u64 {
                tx.rollback().await.map_err(storage)?;
                return Ok(());
            }
            let gap = event.sequence != previous_sequence.max(0) as u64 + 1;
            (event.sequence, previous_requires_snapshot || gap)
        } else {
            if let Some(previous) = previous_epoch {
                sqlx::query("INSERT OR IGNORE INTO peer_retired_epochs(node_id,epoch) VALUES(?,?)")
                    .bind(&event.node_id)
                    .bind(&previous)
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
                sqlx::query("DELETE FROM peer_events WHERE node_id=? AND epoch=?")
                    .bind(&event.node_id)
                    .bind(previous)
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
            }
            (event.sequence, event.sequence != 1)
        };
        let event_json = serde_json::to_string(event).map_err(storage)?;
        sqlx::query(
            "INSERT OR IGNORE INTO peer_events(node_id,epoch,sequence,event_json) VALUES(?,?,?,?)",
        )
        .bind(&event.node_id)
        .bind(&event.epoch)
        .bind(event.sequence as i64)
        .bind(event_json)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        sqlx::query("INSERT INTO peer_event_state(node_id,epoch,sequence,requires_snapshot) VALUES(?,?,?,?) ON CONFLICT(node_id) DO UPDATE SET epoch=excluded.epoch,sequence=excluded.sequence,requires_snapshot=excluded.requires_snapshot")
            .bind(&event.node_id).bind(&event.epoch).bind(latest_sequence as i64).bind(i64::from(requires_snapshot)).execute(&mut *tx).await.map_err(storage)?;
        sqlx::query("UPDATE peer_nodes SET latest_epoch=?,latest_sequence=?,online=1,last_seen=unixepoch() WHERE node_id=?")
            .bind(&event.epoch).bind(latest_sequence as i64).bind(&event.node_id).execute(&mut *tx).await.map_err(storage)?;
        sqlx::query("DELETE FROM peer_events WHERE node_id=? AND epoch=? AND sequence NOT IN (SELECT sequence FROM peer_events WHERE node_id=? AND epoch=? ORDER BY sequence DESC LIMIT 10000)")
            .bind(&event.node_id).bind(&event.epoch).bind(&event.node_id).bind(&event.epoch).execute(&mut *tx).await.map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    pub async fn cached_events(
        &self,
        node_id: &str,
        cursor: Option<&EventCursor>,
    ) -> Result<EventReplay, ApiError> {
        let peer = sqlx::query(
            "SELECT s.epoch,COALESCE(s.sequence,0) AS sequence,COALESCE(s.requires_snapshot,1) AS requires_snapshot,n.online FROM peer_nodes n LEFT JOIN peer_event_state s ON n.node_id=s.node_id WHERE n.node_id=? AND n.removed=0",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or_else(|| ApiError::new("owner_unavailable", "The peer is not registered"))?;
        let epoch: Option<String> = peer.get("epoch");
        let latest: i64 = peer.get("sequence");
        let requires_resync: bool = peer.get::<i64, _>("requires_snapshot") != 0;
        let epoch = epoch.unwrap_or_default();
        let oldest: Option<i64> =
            sqlx::query_scalar("SELECT MIN(sequence) FROM peer_events WHERE node_id=? AND epoch=?")
                .bind(node_id)
                .bind(&epoch)
                .fetch_one(&self.pool)
                .await
                .map_err(storage)?;
        let mut requires_snapshot = requires_resync
            || cursor.is_none_or(|cursor| {
                cursor.epoch != epoch
                    || cursor.sequence > latest as u64
                    || cursor.sequence < oldest.unwrap_or(latest + 1).saturating_sub(1) as u64
            });
        let events = if requires_snapshot {
            Vec::new()
        } else {
            let rows = sqlx::query("SELECT sequence,event_json FROM peer_events WHERE node_id=? AND epoch=? AND sequence>? ORDER BY sequence")
                .bind(node_id).bind(&epoch).bind(cursor.map_or(0, |cursor| cursor.sequence) as i64).fetch_all(&self.pool).await.map_err(storage)?;
            let mut expected = cursor.map_or(0, |cursor| cursor.sequence).saturating_add(1);
            let mut events = Vec::with_capacity(rows.len());
            for row in rows {
                let sequence: i64 = row.get("sequence");
                if sequence < 0 || sequence as u64 != expected {
                    requires_snapshot = true;
                    break;
                }
                events.push(
                    serde_json::from_str(&row.get::<String, _>("event_json")).map_err(storage)?,
                );
                expected = expected.saturating_add(1);
            }
            if expected <= latest as u64 {
                requires_snapshot = true;
            }
            if requires_snapshot {
                Vec::new()
            } else {
                events
            }
        };
        Ok(EventReplay {
            epoch,
            latest: latest.max(0) as u64,
            requires_snapshot,
            events,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairResponse {
    device_id: String,
    role: DeviceRole,
    node_id: String,
    secret: String,
    trusted_server_cert_pem: String,
    fingerprint: String,
}

fn ensure_roster_path(path: &Path) -> Result<(), ApiError> {
    if !path.is_absolute() {
        return Err(ApiError::new(
            "unsafe_roster",
            "Peer roster path must be absolute",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        ApiError::new(
            "unsafe_roster",
            "Peer roster needs a dedicated private parent directory",
        )
    })?;
    let parent_meta = std::fs::symlink_metadata(parent).map_err(storage)?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err(ApiError::new(
            "unsafe_roster",
            "Peer roster parent must be a real private directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if parent_meta.uid() != unsafe { libc::geteuid() }
            || parent_meta.permissions().mode() & 0o077 != 0
        {
            return Err(ApiError::new(
                "unsafe_roster",
                "Peer roster parent must be owned by this user and private",
            ));
        }
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(ApiError::new(
                "unsafe_roster",
                "Peer roster must be a regular file",
            ));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options.open(path).map_err(storage)?;
        let meta = file.metadata().map_err(storage)?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(ApiError::new(
                "unsafe_roster",
                "Peer roster must be an owned regular file",
            ));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(storage)?;
    }
    #[cfg(not(unix))]
    {
        let file = options.open(path).map_err(storage)?;
        if !file.metadata().map_err(storage)?.is_file() {
            return Err(ApiError::new(
                "unsafe_roster",
                "Peer roster must be a regular file",
            ));
        }
    }
    Ok(())
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &'static str,
    column: &'static str,
    declaration: &'static str,
) -> Result<(), ApiError> {
    // Callers pass only schema constants. PRAGMA identifiers cannot be bound,
    // so keep the query constructed from this closed internal set.
    let exists: Option<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info(?) WHERE name=?")
            .bind(table)
            .bind(column)
            .fetch_optional(pool)
            .await
            .map_err(storage)?;
    if exists.is_none() {
        match (table, column, declaration) {
            ("fleet_state", "owner_reverse_device_id", "TEXT") => {
                sqlx::query("ALTER TABLE fleet_state ADD COLUMN owner_reverse_device_id TEXT")
                    .execute(pool)
                    .await
                    .map_err(storage)?;
            }
            ("peer_nodes", "removed", "INTEGER NOT NULL DEFAULT 0") => {
                sqlx::query("ALTER TABLE peer_nodes ADD COLUMN removed INTEGER NOT NULL DEFAULT 0")
                    .execute(pool)
                    .await
                    .map_err(storage)?;
            }
            _ => return Err(storage("unsupported peer schema migration")),
        }
    }
    Ok(())
}

fn validate_root_https(endpoint: &str) -> Result<url::Url, ApiError> {
    let url = url::Url::parse(endpoint).map_err(endpoint_error)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::new(
            "invalid_endpoint",
            "Peer endpoints must be root HTTPS URLs without credentials or query parameters",
        ));
    }
    Ok(url)
}

fn ensure_host_in_certificate(endpoint: &str, sans: &[String]) -> Result<(), ApiError> {
    let url = validate_root_https(endpoint)?;
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    let matches = sans
        .iter()
        .any(|san| san.trim_matches(['[', ']']).eq_ignore_ascii_case(host));
    if !matches {
        return Err(ApiError::new(
            "invalid_endpoint",
            "The advertised endpoint host is not covered by the node TLS certificate",
        ));
    }
    Ok(())
}

fn verify_certificate_pin(pem: &str, expected: &str) -> Result<(), ApiError> {
    use base64::Engine as _;
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let mut parts = pem.trim().split(begin).skip(1);
    let body = parts
        .next()
        .ok_or_else(|| ApiError::new("invalid_trust_anchor", "Pinned CA certificate is missing"))?;
    if parts.next().is_some() {
        return Err(ApiError::new(
            "invalid_trust_anchor",
            "Provide exactly one pinned CA certificate",
        ));
    }
    let encoded = body
        .split(end)
        .next()
        .ok_or_else(|| ApiError::new("invalid_trust_anchor", "Pinned CA certificate is invalid"))?;
    let encoded = encoded
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let der = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::new("invalid_trust_anchor", "Pinned CA certificate is invalid"))?;
    if der.is_empty() {
        return Err(ApiError::new(
            "invalid_trust_anchor",
            "Pinned CA certificate is invalid",
        ));
    }
    let actual = format!("sha256:{:x}", Sha256::digest(&der));
    if actual != expected {
        return Err(ApiError::new(
            "fingerprint_mismatch",
            "Pinned CA certificate fingerprint does not match",
        ));
    }
    Ok(())
}

pub(crate) fn pinned_client(ca_pem: &str, fingerprint: &str) -> Result<reqwest::Client, ApiError> {
    verify_certificate_pin(ca_pem, fingerprint)?;
    let ca = reqwest::Certificate::from_pem(ca_pem.as_bytes())
        .map_err(|_| ApiError::new("invalid_trust_anchor", "Pinned CA certificate is invalid"))?;
    reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(ca)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| {
            ApiError::new(
                "tls_client_error",
                "Could not configure the pinned TLS client",
            )
        })
}

fn parse_registration(
    params: &Value,
) -> Result<(String, String, String, String, String, String), ApiError> {
    let object = params.as_object().ok_or_else(|| {
        ApiError::new(
            "invalid_registration",
            "Node registration parameters are invalid",
        )
    })?;
    let allowed = [
        "nodeId",
        "endpoint",
        "trustedServerCertPem",
        "fingerprint",
        "inviteCode",
        "name",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str())) || object.len() != allowed.len() {
        return Err(ApiError::new(
            "invalid_registration",
            "Node registration parameters are invalid",
        ));
    }
    let read = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                ApiError::new(
                    "invalid_registration",
                    "Node registration parameters are incomplete",
                )
            })
    };
    Ok((
        read("nodeId")?,
        read("endpoint")?,
        read("trustedServerCertPem")?,
        read("fingerprint")?,
        read("inviteCode")?,
        read("name")?,
    ))
}

fn namespace_operation(actor: &str, operation_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update((actor.len() as u64).to_be_bytes());
    hash.update(actor.as_bytes());
    hash.update((operation_id.len() as u64).to_be_bytes());
    hash.update(operation_id.as_bytes());
    format!("{:x}", hash.finalize())
}

async fn ensure_peer(pool: &SqlitePool, node_id: &str) -> Result<(), ApiError> {
    if sqlx::query("SELECT 1 FROM peer_nodes WHERE node_id=? AND removed=0")
        .bind(node_id)
        .fetch_optional(pool)
        .await
        .map_err(storage)?
        .is_none()
    {
        return Err(ApiError::new(
            "owner_unavailable",
            "The peer is not registered",
        ));
    }
    Ok(())
}

fn valid_id(value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err(ApiError::new("invalid_node", "Node identity is invalid"))
    } else {
        Ok(())
    }
}

fn endpoint_error(_error: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "invalid_endpoint",
        "Peer endpoint is not a valid root HTTPS URL",
    )
}

fn storage(_error: impl std::fmt::Display) -> ApiError {
    ApiError::new("storage_error", "Fleet peer storage operation failed")
}

fn forbidden_node() -> ApiError {
    ApiError::new(
        "forbidden",
        "This Node credential is not authorized for the requested operation",
    )
}

fn auth_error(_error: impl std::fmt::Display) -> ApiError {
    ApiError::new("pairing_error", "Node pairing could not be completed")
}

fn name_for_primary(peer_name: &str) -> String {
    let name = format!("Primary ({})", peer_name.trim());
    name.chars().take(128).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::EventHub,
        network::{serve, Dispatch, NetworkApi, SnapshotCallback},
    };
    use brr_remote_protocol::{Channel, NodeSummary, SnapshotError, PROTOCOL_VERSION};
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex as StdMutex},
        time::Duration,
    };
    use tempfile::tempdir;
    use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

    #[cfg(unix)]
    fn private(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    async fn start_node(
        auth: NodeAuthStore,
        dispatch: Dispatch,
    ) -> (
        SocketAddr,
        watch::Sender<bool>,
        JoinHandle<Result<(), String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let events = EventHub::new(auth.node_id().to_owned(), "runtime-epoch".into(), 64);
        let snapshot: SnapshotCallback = Arc::new(move |_principal| {
            Box::pin(async move { Err(ApiError::new("test", "unused")) })
        });
        let api = Arc::new(NetworkApi::new(auth, dispatch, snapshot, events, None));
        let task = tokio::spawn(async move { serve(listener, api, shutdown_rx).await });
        (address, shutdown_tx, task)
    }

    fn endpoint(address: SocketAddr, host: &str) -> String {
        format!("https://{host}:{}", address.port())
    }

    async fn auth(dir: &Path, node_name: &str, tls_host: &str) -> NodeAuthStore {
        let node_dir = dir.join(node_name);
        std::fs::create_dir_all(&node_dir).unwrap();
        #[cfg(unix)]
        private(&node_dir);
        NodeAuthStore::open(node_dir.join("auth.sqlite"), vec![tls_host.into()])
            .await
            .unwrap()
    }

    fn request(target: &str, operation: &str) -> Request {
        Request {
            id: "route-request-1".into(),
            channel: Channel::Agents,
            method: "tasks.create".into(),
            params: serde_json::json!({"title":"task"}),
            operation_id: Some(operation.into()),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(target.into()),
        }
    }

    #[tokio::test]
    async fn new_roster_is_standalone_and_primary_mode_persists_across_reopen() {
        let dir = tempdir().unwrap();
        #[cfg(unix)]
        private(dir.path());
        let path = dir.path().join("peers.sqlite");
        let peers = Peers::open(&path).await.unwrap();
        assert_eq!(peers.mode().await.unwrap(), PeerMode::Standalone);
        peers.init_primary("node-primary").await.unwrap();
        drop(peers);
        let reopened = Peers::open(&path).await.unwrap();
        assert_eq!(reopened.mode().await.unwrap(), PeerMode::Primary);
    }

    #[tokio::test]
    async fn two_pinned_tls_nodes_join_bind_route_and_cache_owner_epoch_without_replay() {
        let primary_dir = tempdir().unwrap();
        let child_dir = tempdir().unwrap();
        #[cfg(unix)]
        {
            private(primary_dir.path());
            private(child_dir.path());
        }
        let primary_auth = auth(primary_dir.path(), "node", "127.0.0.1").await;
        let child_auth = auth(child_dir.path(), "node", "localhost").await;
        let primary_peers = Peers::open(primary_dir.path().join("node/peers.sqlite"))
            .await
            .unwrap();
        let child_peers = Peers::open(child_dir.path().join("node/peers.sqlite"))
            .await
            .unwrap();
        primary_peers
            .init_primary(primary_auth.node_id())
            .await
            .unwrap();

        let primary_invite = primary_auth
            .create_invite(DeviceRole::Node, Duration::from_secs(120))
            .await
            .unwrap();
        let primary_for_dispatch = primary_peers.clone();
        let register_calls = Arc::new(StdMutex::new(Vec::<Principal>::new()));
        let calls_for_dispatch = register_calls.clone();
        let primary_dispatch: Dispatch = Arc::new(move |principal, incoming| {
            let peers = primary_for_dispatch.clone();
            let calls = calls_for_dispatch.clone();
            Box::pin(async move {
                let id = incoming.id.clone();
                if incoming.channel == Channel::Fleet && incoming.method == "node.register" {
                    calls.lock().unwrap().push(principal.clone());
                    Reply::from_result(
                        id,
                        peers.register_reverse(&principal, incoming.params).await,
                    )
                } else {
                    Reply::from_result(id, Err(ApiError::new("unexpected", "unexpected request")))
                }
            })
        });
        let (primary_addr, primary_shutdown, primary_task) =
            start_node(primary_auth.clone(), primary_dispatch).await;

        let route_observed = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let route_observed_child = route_observed.clone();
        let child_node_id = child_auth.node_id().to_owned();
        let child_peers_dispatch = child_peers.clone();
        let child_auth_dispatch = child_auth.clone();
        let child_dispatch: Dispatch = Arc::new(move |principal, incoming| {
            let observed = route_observed_child.clone();
            let node_id = child_node_id.clone();
            let peers = child_peers_dispatch.clone();
            let auth = child_auth_dispatch.clone();
            if incoming.channel == Channel::Fleet && incoming.method == "node.remove" {
                return Box::pin(async move {
                    let id = incoming.id;
                    let result = peers
                        .leave(&auth, &principal, &incoming.params)
                        .await
                        .map(|()| json!({"removed":true}));
                    Reply::from_result(id, result)
                });
            }
            Box::pin(async move {
                let params = incoming.params;
                let operation_id = incoming.operation_id;
                observed.lock().unwrap().push(serde_json::json!({"principalId":principal.id,"operationId":operation_id,"params":params}));
                Reply::from_result(
                    incoming.id,
                    Ok(
                        serde_json::json!({"nodeId":node_id,"principalId":principal.id,"operationId":operation_id,"params":params}),
                    ),
                )
            })
        });
        let (child_addr, child_shutdown, child_task) =
            start_node(child_auth.clone(), child_dispatch.clone()).await;

        let receipt = child_peers
            .join(
                &child_auth,
                JoinConfig {
                    primary_endpoint: endpoint(primary_addr, "127.0.0.1"),
                    primary_node_id: primary_auth.node_id().into(),
                    primary_ca_pem: primary_invite.server_certificate_pem.clone(),
                    primary_fingerprint: primary_invite.fingerprint.clone(),
                    invite_code: primary_invite.code,
                    name: "Desk node".into(),
                    node_endpoint: endpoint(child_addr, "localhost"),
                    reverse_invite_ttl: Duration::from_secs(120),
                },
            )
            .await
            .unwrap();
        assert_eq!(receipt.local_node_id, child_auth.node_id());
        assert_eq!(receipt.primary_node_id, primary_auth.node_id());
        assert_eq!(child_peers.mode().await.unwrap(), PeerMode::Node);
        let git = child_peers.git_transport().await.unwrap();
        assert_eq!(git.node_id, primary_auth.node_id());
        assert_eq!(git.endpoint, endpoint(primary_addr, "127.0.0.1"));
        assert_eq!(git.trusted_ca_pem, primary_invite.server_certificate_pem);
        assert!(!format!("{:?}", git.credential).contains(git.credential.expose_secret()));
        assert_eq!(primary_peers.list_peers().await.unwrap().len(), 1);
        let registration_principal = register_calls.lock().unwrap()[0].clone();
        assert_eq!(registration_principal.role, DeviceRole::Node);
        assert_eq!(registration_principal.node_id, primary_auth.node_id());

        let node_id = child_auth.node_id().to_owned();
        let reply = primary_peers
            .route(request(&node_id, "native-op-1"), "phone-device-9")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply.id, "route-request-1");
        assert_eq!(reply.result.as_ref().unwrap()["nodeId"], node_id);
        assert_eq!(reply.result.as_ref().unwrap()["params"]["title"], "task");
        let routed_op = reply.result.unwrap()["operationId"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(routed_op.len(), 64);
        assert_ne!(routed_op, "native-op-1");
        let before = route_observed.lock().unwrap()[0].clone();
        assert_eq!(
            before["principalId"],
            primary_peers.list_peers().await.unwrap()[0].remote_device_id
        );
        assert_eq!(before["params"]["title"], "task");
        assert!(before["params"].get("actor").is_none());

        let cached = Snapshot {
            protocol_version: PROTOCOL_VERSION,
            node_id: node_id.clone(),
            epoch: "primary-stream".into(),
            cursor: EventCursor {
                epoch: "primary-stream".into(),
                sequence: 4,
            },
            nodes: vec![NodeSummary {
                node_id: node_id.clone(),
                name: "Desk node".into(),
                platform: "linux".into(),
                online: true,
                primary: false,
                authenticated: true,
                runtime_connected: false,
                epoch: "owner-runtime".into(),
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
        };
        primary_peers
            .cache_snapshot(&node_id, &cached)
            .await
            .unwrap();
        primary_peers
            .set_peer_online(&node_id, false)
            .await
            .unwrap();
        let cached_again = primary_peers.cached_snapshot(&node_id).await.unwrap();
        assert!(!cached_again.online);
        assert_eq!(
            cached_again.snapshot.unwrap()["nodes"][0]["epoch"],
            "owner-runtime"
        );
        let event = Event {
            node_id: node_id.clone(),
            epoch: "node-stream".into(),
            sequence: 1,
            owner_epoch: Some("owner-runtime-2".into()),
            message: serde_json::json!({"method":"turn/started"}),
        };
        primary_peers.cache_event(&event).await.unwrap();
        let replay = primary_peers
            .cached_events(
                &node_id,
                Some(&EventCursor {
                    epoch: "node-stream".into(),
                    sequence: 0,
                }),
            )
            .await
            .unwrap();
        assert!(!replay.requires_snapshot);
        assert_eq!(replay.events[0].epoch, "node-stream");
        assert_eq!(
            replay.events[0].owner_epoch.as_deref(),
            Some("owner-runtime-2")
        );
        primary_peers
            .cache_event(&Event {
                node_id: node_id.clone(),
                epoch: "node-stream".into(),
                sequence: 3,
                owner_epoch: Some("owner-runtime-2".into()),
                message: serde_json::json!({"method":"turn/completed"}),
            })
            .await
            .unwrap();
        // A late sequence cannot move the watermark backwards or repair a gap.
        primary_peers
            .cache_event(&Event {
                node_id: node_id.clone(),
                epoch: "node-stream".into(),
                sequence: 2,
                owner_epoch: Some("owner-runtime-2".into()),
                message: serde_json::json!({"method":"turn/started"}),
            })
            .await
            .unwrap();
        let gap = primary_peers
            .cached_events(
                &node_id,
                Some(&EventCursor {
                    epoch: "node-stream".into(),
                    sequence: 1,
                }),
            )
            .await
            .unwrap();
        assert!(
            gap.requires_snapshot,
            "a missing interior cursor must not replay as complete"
        );
        assert!(gap.events.is_empty());

        primary_peers
            .cache_event(&Event {
                node_id: node_id.clone(),
                epoch: "node-stream-next".into(),
                sequence: 1,
                owner_epoch: Some("owner-runtime-3".into()),
                message: serde_json::json!({"method":"brr/epoch"}),
            })
            .await
            .unwrap();
        assert!(primary_peers
            .cache_event(&Event {
                node_id: node_id.clone(),
                epoch: "node-stream".into(),
                sequence: 4,
                owner_epoch: Some("owner-runtime-2".into()),
                message: serde_json::json!({"method":"late"}),
            })
            .await
            .is_err());
        let after_restart = primary_peers
            .cached_events(
                &node_id,
                Some(&EventCursor {
                    epoch: "node-stream-next".into(),
                    sequence: 0,
                }),
            )
            .await
            .unwrap();
        assert!(!after_restart.requires_snapshot);
        assert_eq!(after_restart.events.len(), 1);

        // A forged principal tied to the child identity cannot claim a different owner.
        let wrong_principal = Principal {
            id: "forged-device".into(),
            name: "forged".into(),
            role: DeviceRole::Node,
            node_id: node_id.clone(),
        };
        let reverse = child_auth
            .create_invite(DeviceRole::Node, Duration::from_secs(120))
            .await
            .unwrap();
        let wrong_params = serde_json::json!({"nodeId":"attacker-node","endpoint":endpoint(child_addr, "localhost"),"trustedServerCertPem":child_auth.certificate_pem(),"fingerprint":child_auth.fingerprint(),"inviteCode":reverse.code,"name":"attacker"});
        assert_eq!(
            primary_peers
                .register_reverse(&wrong_principal, wrong_params)
                .await
                .unwrap_err()
                .code,
            "forbidden"
        );
        assert_eq!(primary_peers.list_peers().await.unwrap().len(), 1);

        let first_pair = primary_peers.list_peers().await.unwrap().remove(0);
        assert!(child_auth
            .list_devices()
            .await
            .unwrap()
            .iter()
            .any(|device| device.id == first_pair.remote_device_id));
        let impostor = Principal {
            id: "unrelated-node-device".into(),
            name: "unrelated".into(),
            role: DeviceRole::Node,
            node_id: node_id.clone(),
        };
        let leave_params = json!({
            "nodeId": node_id,
            "primaryNodeId": primary_auth.node_id(),
            "primaryDeviceId": first_pair.remote_device_id,
        });
        assert_eq!(
            child_peers
                .leave(&child_auth, &impostor, &leave_params)
                .await
                .unwrap_err()
                .code,
            "forbidden"
        );
        assert_eq!(child_peers.mode().await.unwrap(), PeerMode::Node);
        let removed = primary_peers.remove(&node_id, &primary_auth).await.unwrap();
        assert!(!removed.remote_revocation_pending);
        assert_eq!(primary_peers.list_peers().await.unwrap().len(), 0);
        assert_eq!(child_peers.mode().await.unwrap(), PeerMode::Standalone);
        assert!(!primary_auth
            .list_devices()
            .await
            .unwrap()
            .iter()
            .any(|device| device.id == first_pair.primary_device_id));
        assert!(!child_auth
            .list_devices()
            .await
            .unwrap()
            .iter()
            .any(|device| device.id == first_pair.remote_device_id));
        assert_eq!(child_auth.node_id(), node_id);

        // Rejoining the same persisted identity mints fresh credentials.
        let second_invite = primary_auth
            .create_invite(DeviceRole::Node, Duration::from_secs(120))
            .await
            .unwrap();
        let second_receipt = child_peers
            .join(
                &child_auth,
                JoinConfig {
                    primary_endpoint: endpoint(primary_addr, "127.0.0.1"),
                    primary_node_id: primary_auth.node_id().into(),
                    primary_ca_pem: second_invite.server_certificate_pem,
                    primary_fingerprint: second_invite.fingerprint,
                    invite_code: second_invite.code,
                    name: "Desk node".into(),
                    node_endpoint: endpoint(child_addr, "localhost"),
                    reverse_invite_ttl: Duration::from_secs(120),
                },
            )
            .await
            .unwrap();
        assert_eq!(second_receipt.local_node_id, node_id);
        assert_eq!(child_peers.mode().await.unwrap(), PeerMode::Node);
        let second_pair = primary_peers.list_peers().await.unwrap().remove(0);
        assert_ne!(second_pair.primary_device_id, first_pair.primary_device_id);
        assert_ne!(second_pair.remote_device_id, first_pair.remote_device_id);

        child_shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), child_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let outcome = primary_peers
            .route(request(&node_id, "native-op-2"), "phone-device-9")
            .await
            .unwrap_err();
        assert_eq!(outcome.code, "uncertain");
        let offline = primary_peers
            .route(request(&node_id, "native-op-3"), "phone-device-9")
            .await
            .unwrap_err();
        assert_eq!(offline.code, "owner_offline");
        let pending = primary_peers.remove(&node_id, &primary_auth).await.unwrap();
        assert!(pending.remote_revocation_pending);
        assert!(primary_peers.list_peers().await.unwrap().is_empty());
        assert_eq!(child_peers.mode().await.unwrap(), PeerMode::Node);
        assert_eq!(primary_peers.retry_pending_removals().await.unwrap(), 0);

        let listener = TcpListener::bind(child_addr).await.unwrap();
        let (retry_shutdown, retry_shutdown_rx) = watch::channel(false);
        let retry_events = EventHub::new(child_auth.node_id().into(), "runtime-epoch".into(), 64);
        let retry_snapshot: SnapshotCallback = Arc::new(move |_principal| {
            Box::pin(async move { Err(ApiError::new("test", "unused")) })
        });
        let retry_api = Arc::new(NetworkApi::new(
            child_auth.clone(),
            child_dispatch.clone(),
            retry_snapshot,
            retry_events,
            None,
        ));
        let retry_task =
            tokio::spawn(async move { serve(listener, retry_api, retry_shutdown_rx).await });
        assert_eq!(primary_peers.retry_pending_removals().await.unwrap(), 1);
        assert!(primary_peers.list_peers().await.unwrap().is_empty());
        assert_eq!(child_peers.mode().await.unwrap(), PeerMode::Standalone);
        retry_shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), retry_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        primary_shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), primary_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    fn node_principal(id: &str, node_id: &str) -> Principal {
        Principal {
            id: id.into(),
            name: "node".into(),
            role: DeviceRole::Node,
            node_id: node_id.into(),
        }
    }

    fn scoped_request(
        channel: brr_remote_protocol::Channel,
        method: &str,
        target: Option<&str>,
    ) -> Request {
        Request {
            id: "auth-check".into(),
            channel,
            method: method.into(),
            params: json!({}),
            operation_id: Some("stable-operation".into()),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: target.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn primary_node_bearers_are_limited_to_registration_and_registered_status() {
        let dir = tempdir().unwrap();
        #[cfg(unix)]
        private(dir.path());
        let peers = Peers::open(dir.path().join("peers.sqlite")).await.unwrap();
        peers.init_primary("primary-node").await.unwrap();
        let joining = node_principal("joining-credential", "primary-node");
        assert!(peers
            .authorize_node_command(
                &joining,
                &scoped_request(brr_remote_protocol::Channel::Fleet, "node.register", None),
            )
            .await
            .is_ok());
        for command in [
            scoped_request(brr_remote_protocol::Channel::Fleet, "device.revoke", None),
            scoped_request(
                brr_remote_protocol::Channel::Fleet,
                "invitation.create",
                None,
            ),
            scoped_request(
                brr_remote_protocol::Channel::Agents,
                "tasks.create",
                Some("worker-node"),
            ),
            scoped_request(brr_remote_protocol::Channel::System, "status", None),
        ] {
            assert!(peers
                .authorize_node_command(&joining, &command)
                .await
                .is_err());
        }

        sqlx::query("INSERT INTO peer_nodes(node_id,principal_id,name,endpoint,ca_pem,fingerprint,primary_device_id,remote_device_id,secret,last_seen) VALUES('worker-node','worker-credential','worker','https://localhost:1','ca','sha256:pin','primary-id','reverse-id','opaque-secret',unixepoch())")
            .execute(&peers.pool).await.unwrap();
        let registered_worker = node_principal("worker-credential", "primary-node");
        assert!(peers
            .authorize_node_command(
                &registered_worker,
                &scoped_request(brr_remote_protocol::Channel::System, "status", None),
            )
            .await
            .is_ok());
        assert!(peers
            .authorize_node_command(
                &registered_worker,
                &scoped_request(brr_remote_protocol::Channel::Fleet, "node.register", None),
            )
            .await
            .is_err());
        assert!(peers
            .authorize_node_command(
                &node_principal("unknown-worker", "primary-node"),
                &scoped_request(brr_remote_protocol::Channel::System, "status", None),
            )
            .await
            .is_err());
        assert!(peers
            .authorize_node_command(
                &node_principal("wrong-owner", "another-primary"),
                &scoped_request(brr_remote_protocol::Channel::Fleet, "node.register", None),
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn joined_worker_only_accepts_reciprocal_primary_observation_and_scoped_routing() {
        let dir = tempdir().unwrap();
        #[cfg(unix)]
        private(dir.path());
        let peers = Peers::open(dir.path().join("peers.sqlite")).await.unwrap();
        sqlx::query("UPDATE fleet_state SET mode='node',local_node_id='worker-node',owner_node_id='primary-node',owner_reverse_device_id='reciprocal-primary-credential' WHERE singleton=1")
            .execute(&peers.pool).await.unwrap();
        let reciprocal = node_principal("reciprocal-primary-credential", "worker-node");
        peers.authorize_node_observer(&reciprocal).await.unwrap();
        assert!(peers
            .authorize_node_observer(&node_principal("other-credential", "worker-node"))
            .await
            .is_err());
        assert!(peers
            .authorize_node_observer(&node_principal(
                "reciprocal-primary-credential",
                "other-node"
            ))
            .await
            .is_err());

        assert!(peers
            .authorize_node_command(
                &reciprocal,
                &scoped_request(
                    brr_remote_protocol::Channel::Agents,
                    "tasks.create",
                    Some("worker-node")
                ),
            )
            .await
            .is_ok());
        assert!(peers
            .authorize_node_command(
                &reciprocal,
                &scoped_request(
                    brr_remote_protocol::Channel::Agents,
                    "tasks.create",
                    Some("other-node")
                ),
            )
            .await
            .is_err());
        assert!(peers
            .authorize_node_command(
                &reciprocal,
                &scoped_request(brr_remote_protocol::Channel::Fleet, "node.remove", None),
            )
            .await
            .is_ok());
        for method in ["device.revoke", "invitation.create", "node.register"] {
            assert!(peers
                .authorize_node_command(
                    &reciprocal,
                    &scoped_request(brr_remote_protocol::Channel::Fleet, method, None),
                )
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn registration_reconciliation_returns_only_matching_durable_roster_receipts() {
        let dir = tempdir().unwrap();
        #[cfg(unix)]
        private(dir.path());
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir(&auth_dir).unwrap();
        #[cfg(unix)]
        private(&auth_dir);
        let auth = NodeAuthStore::open(auth_dir.join("auth.sqlite"), vec!["localhost".into()])
            .await
            .unwrap();
        let peers = Peers::open(dir.path().join("peers.sqlite")).await.unwrap();
        peers.init_primary(auth.node_id()).await.unwrap();
        let principal = node_principal("primary-node-device", auth.node_id());
        let params = json!({
            "nodeId":"claimed-worker",
            "endpoint":"https://localhost:4321",
            "trustedServerCertPem":auth.certificate_pem(),
            "fingerprint":auth.fingerprint(),
            "inviteCode":"already-redeemed",
            "name":"Worker"
        });
        sqlx::query("INSERT INTO peer_nodes(node_id,principal_id,name,endpoint,ca_pem,fingerprint,primary_device_id,remote_device_id,secret,last_seen) VALUES(?,?,?,?,?,?,?,?,?,unixepoch())")
            .bind("claimed-worker").bind(&principal.id).bind("Worker").bind("https://localhost:4321")
            .bind(auth.certificate_pem()).bind(auth.fingerprint()).bind(&principal.id).bind("reverse-device")
            .bind("opaque-bearer").execute(&peers.pool).await.unwrap();

        let receipt = peers
            .reconcile_registration(&principal, &params)
            .await
            .unwrap()
            .unwrap();
        let mut replay = scoped_request(
            brr_remote_protocol::Channel::Fleet,
            "node.register",
            Some(auth.node_id()),
        );
        replay.params = params.clone();
        assert!(
            peers
                .authorize_node_command(&principal, &replay)
                .await
                .is_ok(),
            "A matching durable registration retry must reach receipt reconciliation"
        );
        replay.params["endpoint"] = json!("https://other.local:4321");
        assert!(peers
            .authorize_node_command(&principal, &replay)
            .await
            .is_err());
        let cross_owner = scoped_request(
            brr_remote_protocol::Channel::System,
            "status",
            Some("another-owner"),
        );
        assert!(peers
            .authorize_node_command(&principal, &cross_owner)
            .await
            .is_err());
        assert_eq!(receipt["nodeId"], "claimed-worker");
        assert_eq!(receipt["principalId"], principal.id);
        assert_eq!(receipt["reverseDeviceId"], "reverse-device");
        assert_eq!(receipt["registered"], true);
        let mut changed = params.clone();
        changed["endpoint"] = json!("https://other.local:4321");
        assert!(peers
            .reconcile_registration(&principal, &changed)
            .await
            .unwrap()
            .is_none());
        assert!(peers
            .reconcile_registration(&node_principal("other-device", auth.node_id()), &params)
            .await
            .unwrap()
            .is_none());
        assert!(peers
            .reconcile_registration(&node_principal(&principal.id, "not-primary"), &params)
            .await
            .is_err());
        let mut removed = params.clone();
        removed["nodeId"] = json!("different-worker");
        assert!(peers
            .reconcile_registration(&principal, &removed)
            .await
            .unwrap()
            .is_none());
    }
}
