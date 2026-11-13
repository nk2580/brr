use super::*;
use brr_remote_protocol::Event;
use futures_util::StreamExt;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::sync::watch;

impl Service {
    pub async fn snapshot(&self) -> Result<Snapshot, ApiError> {
        let mut snapshot = self.local_snapshot().await?;
        if self.peers.mode().await? != PeerMode::Primary {
            self.append_historical_threads(&mut snapshot).await;
            return Ok(snapshot);
        }
        self.refresh_peers_once().await?;
        for peer in self.peers.list_peers().await? {
            let cached = self.peers.cached_snapshot(&peer.node_id).await?;
            let remote = cached
                .snapshot
                .and_then(|v| serde_json::from_value::<Snapshot>(v).ok());
            if let Some(mut remote) = remote {
                if let Some(mut owner) = remote
                    .nodes
                    .into_iter()
                    .find(|owner| owner.node_id == peer.node_id)
                {
                    owner.online = cached.online;
                    owner.primary = false;
                    snapshot.nodes.push(owner);
                }
                snapshot.threads.append(&mut remote.threads);
                snapshot.projects.append(&mut remote.projects);
                snapshot.agents.append(&mut remote.agents);
                snapshot.tasks.append(&mut remote.tasks);
                snapshot.questions.append(&mut remote.questions);
                snapshot.approvals.append(&mut remote.approvals);
                snapshot
                    .errors
                    .extend(remote.errors.into_iter().map(|mut e| {
                        e.scope = format!("{}:{}", peer.node_id, e.scope);
                        e
                    }));
            } else {
                snapshot.nodes.push(NodeSummary {
                    node_id: peer.node_id.clone(),
                    name: peer.name,
                    platform: "unknown".into(),
                    online: false,
                    primary: false,
                    authenticated: false,
                    runtime_connected: false,
                    epoch: String::new(),
                });
            }
            if !cached.online {
                snapshot.errors.push(SnapshotError{scope:peer.node_id,message:"Execution owner is offline. Cached history remains readable; live controls require reconnection.".into()});
            }
        }
        deduplicate_shared(&mut snapshot.agents);
        deduplicate_shared(&mut snapshot.projects);
        self.append_historical_threads(&mut snapshot).await;
        Ok(snapshot)
    }

    /// A bounded read of each registered owner. Failed reads keep the last
    /// durable projection and mark that owner offline; no execution is moved.
    pub async fn refresh_peers_once(&self) -> Result<(), ApiError> {
        if self.peers.mode().await? != PeerMode::Primary {
            return Ok(());
        }
        let peers = self.peers.list_peers().await?;
        futures_util::future::join_all(peers.into_iter().map(|peer| async move {
            if hydrate(self, &peer.node_id).await.is_err() {
                let _ = self.peers.set_peer_online(&peer.node_id, false).await;
            }
        }))
        .await;
        Ok(())
    }

    pub async fn run_peer_monitor(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _=shutdown.changed()=>{if *shutdown.borrow() || shutdown.has_changed().is_err(){break;}},
                _=interval.tick()=>{
                    let peers=if matches!(self.peers.mode().await,Ok(PeerMode::Primary)){self.peers.list_peers().await.unwrap_or_default()}else{vec![]};
                    let present=peers.iter().map(|p|p.node_id.clone()).collect::<HashSet<_>>();
                    tasks.retain(|id,task|{if !present.contains(id) || task.is_finished(){task.abort();false}else{true}});
                    for peer in peers {
                        if !tasks.contains_key(&peer.node_id) {
                            let service=self.clone();let id=peer.node_id.clone();
                            tasks.insert(peer.node_id,tokio::spawn(async move {
                                loop {
                                    if service.peers.peer_transport(&id).await.is_err(){break;}
                                    let result=match hydrate(&service,&id).await {
                                        Ok(cursor)=>stream_until_peer_offline(&service,&id,cursor).await,
                                        Err(e)=>Err(e),
                                    };
                                    if result.is_err(){let _=service.peers.set_peer_online(&id,false).await;service.events.publish(json!({"method":"brr/fleet/changed","params":{"nodeId":id}}));}
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                }
                            }));
                        }
                    }
                    let _=self.peers.retry_pending_removals().await;
                }
            }
        }
        for task in tasks.into_values() {
            task.abort();
            let _ = task.await;
        }
    }
}

fn deduplicate_shared(rows: &mut Vec<Value>) {
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    let mut unique = Vec::<Value>::with_capacity(rows.len());
    for mut row in rows.drain(..) {
        let Some(identity) = row["id"]
            .as_str()
            .filter(|id| id.starts_with("fleet_"))
            .zip(row["originNodeId"].as_str())
            .map(|(id, origin)| (id.to_owned(), origin.to_owned()))
        else {
            unique.push(row);
            continue;
        };
        let owner = row["ownerNodeId"].as_str().map(str::to_owned);
        if let Some(position) = index.get(&identity) {
            if let Some(owner) = owner {
                if let Some(available) = unique[*position]["availableNodeIds"].as_array_mut() {
                    if !available.iter().any(|node| node.as_str() == Some(&owner)) {
                        available.push(json!(owner));
                    }
                }
            }
        } else {
            row["availableNodeIds"] = json!(owner.into_iter().collect::<Vec<_>>());
            index.insert(identity, unique.len());
            unique.push(row);
        }
    }
    *rows = unique;
}

async fn hydrate(
    service: &Service,
    node: &str,
) -> Result<brr_remote_protocol::EventCursor, ApiError> {
    let peer = service.peers.peer_transport(node).await?;
    let client = crate::peers::pinned_client(&peer.trusted_ca_pem, &peer.fingerprint)?;
    let response = client
        .get(format!(
            "{}/v1/snapshot",
            peer.endpoint.trim_end_matches('/')
        ))
        .timeout(Duration::from_secs(5))
        .bearer_auth(peer.credential.expose_secret())
        .send()
        .await
        .map_err(remote_error)?;
    if !response.status().is_success() {
        return Err(ApiError::new(
            "owner_unavailable",
            "Owner snapshot was rejected",
        ));
    }
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(remote_error)?;
        if body.len() + chunk.len() > 32 * 1024 * 1024 {
            return Err(ApiError::new(
                "snapshot_too_large",
                "Owner snapshot exceeds the supported limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let snapshot: Snapshot = serde_json::from_slice(&body).map_err(remote_error)?;
    if snapshot.node_id != node || snapshot.protocol_version != PROTOCOL_VERSION {
        return Err(ApiError::new(
            "owner_mismatch",
            "Snapshot identity or protocol does not match the registered owner",
        ));
    }
    let previous = service.peers.cached_snapshot(node).await?;
    let changed = !previous.online
        || previous
            .snapshot
            .as_ref()
            .is_none_or(|old| old["epoch"] != snapshot.epoch);
    let cursor = snapshot.cursor.clone();
    service.peers.cache_snapshot(node, &snapshot).await?;
    service.peers.set_peer_online(node, true).await?;
    if changed {
        service
            .events
            .publish(json!({"method":"brr/fleet/changed","params":{"nodeId":node}}));
    }
    Ok(cursor)
}

async fn stream(
    service: &Service,
    node: &str,
    cursor: brr_remote_protocol::EventCursor,
) -> Result<(), ApiError> {
    let peer = service.peers.peer_transport(node).await?;
    let client = crate::peers::pinned_client(&peer.trusted_ca_pem, &peer.fingerprint)?;
    let response = client
        .get(format!("{}/v1/events", peer.endpoint.trim_end_matches('/')))
        .query(&[
            ("epoch", cursor.epoch),
            ("after", cursor.sequence.to_string()),
        ])
        .timeout(Duration::from_secs(3600))
        .bearer_auth(peer.credential.expose_secret())
        .send()
        .await
        .map_err(remote_error)?;
    if !response.status().is_success() {
        return Err(ApiError::new(
            "owner_unavailable",
            "Owner event stream was rejected",
        ));
    }
    let mut chunks = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut name = String::new();
    let mut data = String::new();
    while let Some(chunk) = chunks.next().await {
        buffer.extend_from_slice(&chunk.map_err(remote_error)?);
        if buffer.len() + data.len() > 32 * 1024 * 1024 {
            return Err(ApiError::new(
                "event_too_large",
                "Owner event exceeds the supported limit",
            ));
        }
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=end).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .map_err(remote_error)?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if name == "snapshot_required" {
                    return Err(ApiError::new(
                        "event_gap",
                        "Owner event history requires a fresh snapshot",
                    ));
                }
                if name == "message" && !data.is_empty() {
                    let event: Event = serde_json::from_str(&data).map_err(remote_error)?;
                    if event.node_id != node {
                        return Err(ApiError::new(
                            "owner_mismatch",
                            "Event belongs to another execution owner",
                        ));
                    }
                    service.peers.cache_event(&event).await?;
                    service.events.publish_remote(event);
                }
                name.clear();
                data.clear();
            } else if let Some(value) = line.strip_prefix("event:") {
                name = value.trim().into();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
    }
    Err(ApiError::new(
        "owner_unavailable",
        "Owner event stream ended",
    ))
}

/// Keep the authenticated SSE stream for normal event delivery, but stop
/// waiting on it when routed API activity has marked this owner offline. The
/// caller then follows its normal bounded rehydrate cadence. This poll only
/// reads the online bit; it never deserializes snapshots or retries owner work.
async fn stream_until_peer_offline(
    service: &Service,
    node: &str,
    cursor: brr_remote_protocol::EventCursor,
) -> Result<(), ApiError> {
    let mut event_stream = Box::pin(stream(service, node, cursor));
    loop {
        tokio::select! {
            result = &mut event_stream => return result,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                if !service.peers.peer_is_online(node).await? {
                    return Ok(());
                }
            }
        }
    }
}

fn remote_error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "owner_unavailable",
        "Could not read the registered owner over its pinned connection",
    )
}
