//! Bind the verified import and private Git writer to the running service.
use super::*;
use crate::{
    catalog::Catalog,
    git_http::{GitHttp, GitHttpClient},
    migration::ImportBackup,
};
use brr_agent_core::{SharedRow, SharedSnapshot, SharedTable};
use brr_fleet::GitTransfer;
use brr_host::export::HistoryExport;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

struct PinnedGit(GitHttpClient);
impl GitTransfer for PinnedGit {
    fn fetch(&self, repo: &Path) -> Result<(), String> {
        self.0.fetch(repo)
    }
    fn push(&self, repo: &Path, branch: &str) -> Result<(), String> {
        self.0.push_node_branch(
            repo,
            branch
                .strip_prefix("nodes/")
                .ok_or("Only a node writer branch may be pushed remotely")?,
        )
    }
}
struct DetachedGit;
impl GitTransfer for DetachedGit {
    fn fetch(&self, _: &Path) -> Result<(), String> {
        Err("This writer is detached from its primary".into())
    }
    fn push(&self, _: &Path, _: &str) -> Result<(), String> {
        Err("This writer is detached from its primary".into())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Activation {
    version: u32,
    node_id: String,
    primary_node_id: String,
    #[serde(default)]
    git_origin: Option<String>,
    complete: bool,
    backup: ImportBackup,
}

impl Service {
    fn activation_path(&self) -> PathBuf {
        self.host.data_dir().join("node/fleet-activation.json")
    }

    fn activation(&self) -> Result<Option<Activation>, ApiError> {
        let path = self.activation_path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(host_error(error)),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 32 * 1024 * 1024
        {
            return Err(ApiError::new(
                "invalid_activation",
                "Fleet activation manifest is not a safe regular file",
            ));
        }
        let activation: Activation =
            serde_json::from_slice(&fs::read(path).map_err(host_error)?).map_err(host_error)?;
        if activation.version != 1
            || activation.node_id != self.node_id()
            || activation.backup.manifest.node_id != self.node_id()
        {
            return Err(ApiError::new(
                "invalid_activation",
                "Fleet activation belongs to another node",
            ));
        }
        Ok(Some(activation))
    }

    fn save_activation(&self, activation: &Activation) -> Result<(), ApiError> {
        let path = self.activation_path();
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<(), ApiError> {
            let mut file = options.open(&temp).map_err(host_error)?;
            file.write_all(&serde_json::to_vec(activation).map_err(host_error)?)
                .map_err(host_error)?;
            file.sync_all().map_err(host_error)?;
            fs::rename(&temp, &path).map_err(host_error)?;
            fs::File::open(path.parent().unwrap())
                .and_then(|dir| dir.sync_all())
                .map_err(host_error)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }

    async fn prepare_activation(
        &self,
        primary_node_id: &str,
    ) -> Result<(Activation, HistoryExport), ApiError> {
        if let Some(activation) = self.activation()? {
            if activation.primary_node_id != primary_node_id {
                return Err(ApiError::new(
                    "different_fleet",
                    "This node already has a preserved writer for another primary",
                ));
            }
            let history = activation
                .backup
                .history_export()
                .await
                .map_err(host_error)?;
            return Ok((activation, history));
        }
        // Fleet activation briefly closes the runtime. Avoid interrupting a
        // live turn or approval and leave existing task/worktree bindings intact.
        if self.host.is_connected().await {
            let tasks = self
                .host
                .agent_store()
                .list_tasks(None)
                .await
                .map_err(host_error)?;
            if tasks
                .iter()
                .any(|task| matches!(task.status.as_str(), "running" | "waiting_input"))
                || !self.approvals().await?.is_empty()
            {
                return Err(ApiError::new(
                    "fleet_busy",
                    "Finish live tasks and approvals before activating shared data",
                ));
            }
            let mut cursor = None;
            let mut seen = std::collections::HashSet::new();
            loop {
                let mut params = json!({"limit":100,"archived":false});
                if let Some(cursor) = cursor.take() {
                    params["cursor"] = Value::String(cursor);
                }
                let page = self
                    .host
                    .request("thread/list", params)
                    .await
                    .map_err(host_error)?;
                let threads = page["data"].as_array().ok_or_else(|| {
                    ApiError::new("fleet_busy", "Could not verify that the runtime is idle")
                })?;
                if threads.iter().any(|thread| {
                    thread["status"] == "active" || thread["status"]["type"] == "active"
                }) {
                    return Err(ApiError::new(
                        "fleet_busy",
                        "Finish the live session before activating shared data",
                    ));
                }
                match page["nextCursor"].as_str() {
                    Some(next) if seen.insert(next.to_owned()) => cursor = Some(next.to_owned()),
                    Some(_) => {
                        return Err(ApiError::new(
                            "fleet_busy",
                            "Could not verify all runtime sessions",
                        ))
                    }
                    None => break,
                }
            }
        }
        self.host.shutdown().await;
        let history = self
            .host
            .export_quiesced_history()
            .await
            .map_err(host_error)?;
        let backup = crate::migration::prepare(&self.host, self.node_id(), &history)
            .await
            .map_err(host_error)?;
        let activation = Activation {
            version: 1,
            node_id: self.node_id().into(),
            primary_node_id: primary_node_id.into(),
            git_origin: None,
            complete: false,
            backup,
        };
        self.save_activation(&activation)?;
        Ok((activation, history))
    }

    async fn node_git_transport(&self) -> Result<(String, Arc<dyn GitTransfer>), ApiError> {
        let client = GitHttpClient::new(self.peers.git_transport().await?).map_err(host_error)?;
        let origin = client.origin_url().to_owned();
        Ok((origin, Arc::new(PinnedGit(client))))
    }

    pub(super) async fn restore_catalog(&self) -> Result<(), ApiError> {
        let mode = self.peers.mode().await?;
        let activation = self.activation()?;
        if mode == PeerMode::Standalone && activation.is_none() {
            return Ok(());
        }
        let activation = activation.filter(|value| value.complete).ok_or_else(|| {
            ApiError::new(
                "shared_recovery_required",
                "Finish the interrupted fleet activation from the local service",
            )
        })?;
        let data = self.host.data_dir();
        let store = self.host.agent_store();
        let catalog = if mode == PeerMode::Primary {
            if activation.primary_node_id != self.node_id() {
                return Err(ApiError::new(
                    "different_fleet",
                    "Primary identity does not match its catalog",
                ));
            }
            Catalog::open_primary(data, self.node_id(), store).await
        } else if mode == PeerMode::Node {
            let transport = self.peers.git_transport().await?;
            if activation.primary_node_id != transport.node_id {
                return Err(ApiError::new(
                    "different_fleet",
                    "Registered primary does not match the preserved catalog",
                ));
            }
            let (origin, transfer) = self.node_git_transport().await?;
            Catalog::open_node(
                data,
                self.node_id(),
                activation.git_origin.as_deref().unwrap_or(&origin),
                Some(transfer),
                store,
            )
            .await
        } else if mode == PeerMode::Standalone && activation.primary_node_id != self.node_id() {
            let origin = activation.git_origin.as_deref().ok_or_else(|| {
                ApiError::new(
                    "invalid_activation",
                    "Detached writer has no preserved origin",
                )
            })?;
            Catalog::open_node(
                data,
                self.node_id(),
                origin,
                Some(Arc::new(DetachedGit)),
                store,
            )
            .await
        } else {
            return Err(ApiError::new(
                "invalid_activation",
                "Fleet role does not match its preserved catalog",
            ));
        }
        .map_err(host_error)?;
        self.install_catalog(catalog).await
    }

    pub(super) async fn activate_catalog(
        &self,
        primary_node_id: &str,
        join: Option<JoinConfig>,
    ) -> Result<Value, ApiError> {
        let was_connected = self.host.is_connected().await;
        let result = {
            let _runtime = self.runtime_guard.lock().await;
            self.activate_catalog_inner(primary_node_id, join).await
        };
        match &result {
            Ok(_) => {
                *self.last_shared_error.lock().await = None;
                let _ = self.connect_runtime().await;
            }
            Err(error) => {
                if self
                    .activation()
                    .ok()
                    .flatten()
                    .is_some_and(|marker| !marker.complete)
                {
                    *self.last_shared_error.lock().await = Some(error.message.clone());
                } else if was_connected && !self.host.is_connected().await {
                    let _ = self.connect_runtime().await;
                }
            }
        }
        result
    }

    async fn activate_catalog_inner(
        &self,
        primary_node_id: &str,
        join: Option<JoinConfig>,
    ) -> Result<Value, ApiError> {
        let primary = primary_node_id == self.node_id();
        if let Some(catalog) = self.catalog.lock().await.clone() {
            let mode = self.peers.mode().await?;
            if ((primary && mode == PeerMode::Primary) || (!primary && mode == PeerMode::Node))
                && self
                    .activation()?
                    .is_some_and(|state| state.complete && state.primary_node_id == primary_node_id)
            {
                return Ok(
                    json!({"mode":mode,"nodeId":self.node_id(),"primaryNodeId":primary_node_id,"sharedDataInitialized":true,"revision":catalog.status().map_err(host_error)?.revision}),
                );
            }
        }
        let (mut activation, history) = self.prepare_activation(primary_node_id).await?;
        if self.host.is_connected().await {
            self.host.shutdown().await;
        }
        let projects = self
            .host
            .project_store()
            .shared_snapshot(self.node_id())
            .await
            .map_err(host_error)?;
        let expected = if activation.complete {
            None
        } else {
            Some(source_snapshot(&self.host, self.node_id(), &projects).await?)
        };
        let data = self.host.data_dir();
        let store = self.host.agent_store();
        let catalog=if !primary {
            match self.peers.mode().await? {
                PeerMode::Standalone=>{self.peers.join(&self.auth,join.ok_or_else(||ApiError::new("invitation_required","Provide a fresh invitation from this primary to finish joining"))?).await?;}
                PeerMode::JoinPending=>{self.peers.resume_join().await?;}
                PeerMode::Node=>(),
                PeerMode::Primary=>return Err(ApiError::new("fleet_mode_conflict","A primary cannot join another fleet")),
            }
            let (origin,transfer)=self.node_git_transport().await?;
            if activation.complete {Catalog::open_node(data,self.node_id(),activation.git_origin.as_deref().unwrap_or(&origin),Some(transfer),store).await}
            else{
                let preserved_origin=activation.git_origin.clone().unwrap_or(origin);
                activation.git_origin=Some(preserved_origin.clone());self.save_activation(&activation)?;
                Catalog::init_node_with_projects(data,self.node_id(),&preserved_origin,Some(transfer),store,&projects).await
            }
        }else{
            self.peers.init_primary(self.node_id()).await?;
            if activation.complete {Catalog::open_primary(data,self.node_id(),store).await}
            else{Catalog::init_primary_with_projects(data,self.node_id(),store,&projects).await}
        }.map_err(host_error)?;
        if !activation.complete {
            // Raw backup bytes were verified before reconstruction. The typed
            // manifest normalizes legacy absent attachment lists to [] without
            // rewriting the preserved backup or weakening its original hash.
            let expected_history_sha = catalog
                .history_manifest_sha256(&history)
                .map_err(host_error)?;
            let history_result = catalog.publish_history(&history).map_err(host_error)?;
            if history_result.count != activation.backup.manifest.history_thread_count
                || history_result.sha256 != expected_history_sha
            {
                return Err(ApiError::new(
                    "import_mismatch",
                    "Committed Git history did not match the verified backup",
                ));
            }
            if let Some(expected) = expected {
                verify_import(&catalog, &expected)?;
            }
        }
        let sync = catalog.synchronize().await.map_err(host_error)?;
        activation.git_origin = Some(catalog.origin().map_err(host_error)?);
        self.install_catalog(catalog).await?;
        activation.complete = true;
        self.save_activation(&activation)?;
        *self.last_sync.lock().await = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        self.events
            .publish(json!({"method":"brr/fleet/changed","params":{}}));
        Ok(
            json!({"mode":self.peers.mode().await?,"nodeId":self.node_id(),"localNodeId":self.node_id(),"primaryNodeId":primary_node_id,"sharedDataInitialized":true,"revision":sync.revision,"historyCount":activation.backup.manifest.history_thread_count,"backupDirectory":activation.backup.directory}),
        )
    }

    async fn install_catalog(&self, catalog: Catalog) -> Result<(), ApiError> {
        self.materialize_projects(&catalog).await?;
        let projects = self.host.project_store();
        let writer = catalog.clone();
        projects.set_shared_commit_hook(self.node_id(), move |rows| writer.publish_projects(rows));
        if catalog.is_primary() {
            let origin = catalog.origin().map_err(host_error)?;
            let git = GitHttp::open(origin, self.peers.clone())
                .await
                .map_err(host_error)?;
            *self.git_http.lock().await = Some(Arc::new(git).callback());
        }
        *self.catalog.lock().await = Some(catalog);
        Ok(())
    }

    pub(super) async fn materialize_projects(&self, catalog: &Catalog) -> Result<(), ApiError> {
        let (revision, snapshot) = catalog.committed_shared().map_err(host_error)?;
        let rows = snapshot
            .rows
            .into_iter()
            .filter(|row| row.table == SharedTable::Projects)
            .collect::<Vec<_>>();
        self.host
            .project_store()
            .materialize_shared_snapshot(self.node_id(), &rows, &revision)
            .await
            .map_err(host_error)
    }

    pub(super) async fn historical_response(
        &self,
        request: &Request,
        owner: &str,
    ) -> Result<Option<Value>, ApiError> {
        if request.channel != Channel::Sessions
            || !matches!(
                request.method.as_str(),
                "read" | "turns" | "attachment.preview" | "attachment.read"
            )
        {
            return Ok(None);
        }
        let id = required(&request.params, "threadId")?;
        let Some(catalog) = self.catalog.lock().await.clone() else {
            return Ok(None);
        };
        if matches!(
            request.method.as_str(),
            "attachment.preview" | "attachment.read"
        ) {
            super::history_attachments::validate_request(request)?;
            let Some(pair) = catalog
                .historical_attachment(owner, id, required(&request.params, "sha256")?)
                .map_err(host_error)?
            else {
                return Ok(None);
            };
            return super::history_attachments::attachment_response(request, pair)
                .await
                .map(Some);
        }
        let Some(history) = catalog.historical_thread(owner, id).map_err(host_error)? else {
            return Ok(None);
        };
        if request.method == "read" {
            let mut thread = history["thread"].clone();
            thread["turns"] = history["turns"].clone();
            thread["attachments"] = history["attachments"].clone();
            thread["ownerNodeId"] = json!(owner);
            thread["readOnly"] = json!(true);
            thread["historical"] = json!(true);
            return Ok(Some(thread));
        }
        let turns = history["turns"].as_array().ok_or_else(|| {
            ApiError::new(
                "invalid_history",
                "Historical conversation has malformed turns",
            )
        })?;
        let offset = if let Some(cursor) = request.params["cursor"].as_str() {
            cursor
                .strip_prefix("history:")
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| {
                    ApiError::new(
                        "invalid_cursor",
                        "Refresh this historical conversation before paging",
                    )
                })?
        } else {
            0
        };
        let end = offset.saturating_add(100).min(turns.len());
        let page = turns.get(offset..end).ok_or_else(|| {
            ApiError::new("invalid_cursor", "Historical turn cursor is out of range")
        })?;
        Ok(Some(
            json!({"data":page,"nextCursor":(end<turns.len()).then(||format!("history:{end}")),"readOnly":true,"ownerNodeId":owner}),
        ))
    }

    pub(super) async fn append_historical_threads(&self, snapshot: &mut Snapshot) {
        let Some(catalog) = self.catalog.lock().await.clone() else {
            return;
        };
        let historical = match catalog.historical_threads() {
            Ok(rows) => rows,
            Err(error) => {
                snapshot.errors.push(SnapshotError {
                    scope: "history".into(),
                    message: error,
                });
                return;
            }
        };
        let mut seen = snapshot
            .threads
            .iter()
            .filter_map(|row| {
                Some((
                    row["ownerNodeId"].as_str()?.to_owned(),
                    row["id"].as_str()?.to_owned(),
                ))
            })
            .collect::<std::collections::HashSet<_>>();
        for history in historical {
            let Some(owner) = history["ownerNodeId"].as_str() else {
                continue;
            };
            let Some(id) = history["thread"]["id"].as_str() else {
                continue;
            };
            if let Some(thread) = snapshot
                .threads
                .iter_mut()
                .find(|thread| thread["ownerNodeId"] == owner && thread["id"] == id)
            {
                thread["attachments"] = history["attachments"].clone();
            }
            if seen.insert((owner.into(), id.into())) {
                let mut thread = history["thread"].clone();
                if let Some(object) = thread.as_object_mut() {
                    object.remove("turns");
                }
                thread["ownerNodeId"] = json!(owner);
                thread["attachments"] = history["attachments"].clone();
                thread["readOnly"] = json!(true);
                thread["historical"] = json!(true);
                snapshot.threads.push(thread);
            }
        }
    }

    pub(super) async fn tombstone_thread(&self, id: &str) -> Result<(), ApiError> {
        if let Some(catalog) = self.catalog.lock().await.clone() {
            catalog.tombstone_history(id).map_err(|_|ApiError::new("uncertain","The owner deleted the session, but its shared history update was not confirmed. Refresh before retrying."))?;
        }
        Ok(())
    }

    pub(super) async fn sync_view(&self) -> Result<Value, ApiError> {
        if let Some(error) = self.last_shared_error.lock().await.clone() {
            return Ok(
                json!({"configured":false,"state":"recoveryRequired","status":"Shared data needs recovery","error":error}),
            );
        }
        let Some(catalog) = self.catalog.lock().await.clone() else {
            return Ok(json!({"configured":false,"state":"notConfigured","status":"Not set up"}));
        };
        let status = catalog.status().map_err(host_error)?;
        let pending = pending_changes(&catalog)?;
        let mode = self.peers.mode().await?;
        Ok(
            json!({"configured":true,"state":if status.conflicts>0 {"conflicted"}else if mode==PeerMode::Standalone {"detached"}else if pending>0 {"pending"}else{"ready"},
            "status":if status.conflicts>0 {"Conflicts need review"}else if mode==PeerMode::Standalone {"Detached · local edits kept"}else if pending>0 {"Local changes pending"}else{"Up to date"},
            "pendingChanges":pending,"revision":status.revision,"records":status.records,"conflicts":status.conflicts,"lastSyncedAt":*self.last_sync.lock().await}),
        )
    }

    pub(super) async fn sync_request(
        &self,
        request: &Request,
        role: ClientRole,
    ) -> Result<Value, ApiError> {
        if request.method == "status" {
            return self.sync_view().await;
        }
        if request.method == "import" {
            if role != ClientRole::Desktop {
                return Err(ApiError::new(
                    "forbidden",
                    "Import recovery requires local IPC",
                ));
            }
            let state = self.activation()?.ok_or_else(|| {
                ApiError::new(
                    "not_configured",
                    "Run fleet init or join to prepare a verified import",
                )
            })?;
            return self.activate_catalog(&state.primary_node_id, None).await;
        }
        let catalog = self.catalog.lock().await.clone().ok_or_else(|| {
            ApiError::new("not_configured", "Shared Git is not set up on this node")
        })?;
        match request.method.as_str() {
            "records" => {
                let limit = request.params["limit"].as_u64().unwrap_or(50).clamp(1, 100) as usize;
                let offset = request.params["offset"]
                    .as_u64()
                    .unwrap_or(0)
                    .min(1_000_000) as usize;
                let mut bytes = 0usize;
                let mut data = Vec::new();
                let mut more = false;
                for record in catalog
                    .records()
                    .map_err(host_error)?
                    .into_iter()
                    .filter(|row| {
                        !matches!(
                            row.kind,
                            brr_fleet::RecordKind::Transcript | brr_fleet::RecordKind::Blob
                        )
                    })
                    .skip(offset)
                {
                    let size = serde_json::to_vec(&record).map_err(host_error)?.len();
                    if data.len() == limit || bytes + size > 16 * 1024 * 1024 {
                        more = true;
                        break;
                    }
                    bytes += size;
                    data.push(record);
                }
                Ok(json!({"nextOffset":more.then_some(offset+data.len()),"data":data}))
            }
            "history" => Ok(json!({"data":commit_history(&catalog)?})),
            "conflicts" => Ok(
                json!({"data":catalog.conflicts().map_err(host_error)?.iter().map(conflict_view).collect::<Vec<_>>()}),
            ),
            "synchronize" => self.synchronize_catalog(&catalog).await,
            "resolve" | "previewProfile" | "approveProfile" => {
                if self.peers.mode().await? != PeerMode::Primary {
                    return Err(ApiError::new(
                        "primary_required",
                        "Resolve shared conflicts on the primary",
                    ));
                }
                let id = required(&request.params, "conflictId")?;
                let expected = request.expected_revision.as_deref().ok_or_else(|| {
                    ApiError::new(
                        "revision_required",
                        "Refresh the conflict before resolving it",
                    )
                })?;
                let conflict = catalog
                    .conflicts()
                    .map_err(host_error)?
                    .into_iter()
                    .find(|conflict| conflict.id == id)
                    .ok_or_else(|| {
                        ApiError::new("revision_conflict", "The conflict has already changed")
                    })?;
                if conflict_view(&conflict)["revision"] != expected {
                    return Err(ApiError::new(
                        "revision_conflict",
                        "The conflict changed. Refresh before resolving it.",
                    ));
                }
                let result = match request.method.as_str() {
                    "previewProfile" | "approveProfile" => {
                        let candidate = serde_json::from_value(request.params["candidate"].clone())
                            .map_err(|_| {
                                ApiError::new(
                                    "invalid_request",
                                    "Supply the full profile to review",
                                )
                            })?;
                        if request.method == "previewProfile" {
                            return serde_json::to_value(
                                catalog
                                    .preview_profile_resolution(id, expected, candidate)
                                    .map_err(host_error)?,
                            )
                            .map_err(host_error);
                        }
                        let review = required(&request.params, "reviewHash")?;
                        catalog
                            .approve_profile_resolution(id, expected, candidate, review)
                            .await
                    }
                    _ => {
                        if crate::catalog::conflict_table(&conflict)
                            == Some(SharedTable::AgentProfiles)
                            && conflict
                                .current
                                .as_ref()
                                .is_some_and(|record| !record.deleted)
                            && conflict
                                .incoming
                                .as_ref()
                                .is_some_and(|record| !record.deleted)
                        {
                            return Err(ApiError::new(
                                "review_required",
                                "Review a new profile before applying the conflict resolution",
                            ));
                        }
                        let record = match required(&request.params, "choice")? {
                            "base" => conflict.base,
                            "current" => conflict.current,
                            "incoming" => conflict.incoming,
                            _ => None,
                        }
                        .ok_or_else(|| {
                            ApiError::new("invalid_request", "Choose an available conflict version")
                        })?;
                        catalog.resolve(id, record).await
                    }
                };
                self.materialize_projects(&catalog).await?;
                let resolved = result.map_err(host_error)?;
                self.events
                    .publish(json!({"method":"brr/sync/changed","params":{}}));
                Ok(json!({"record":resolved,"sync":self.sync_view().await?}))
            }
            _ => Err(ApiError::new(
                "unsupported_method",
                "Edit shared content through its profile, memory, skill or project controls",
            )),
        }
    }

    async fn synchronize_catalog(&self, catalog: &Catalog) -> Result<Value, ApiError> {
        if !matches!(self.peers.mode().await?, PeerMode::Primary | PeerMode::Node) {
            return Err(ApiError::new(
                "owner_unavailable",
                "Join the primary again to publish the retained local changes",
            ));
        }
        let before = catalog.status().map_err(host_error)?;
        let sync = catalog.synchronize().await;
        self.materialize_projects(catalog).await?;
        let sync = sync.map_err(|message| ApiError::new("sync_unavailable", message))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        *self.last_sync.lock().await = Some(now);
        let after = catalog.status().map_err(host_error)?;
        if sync.revision != before.revision || before.conflicts != after.conflicts {
            self.events
                .publish(json!({"method":"brr/sync/changed","params":{"revision":sync.revision}}));
        }
        Ok(self.sync_view().await?)
    }

    /// Keep the node branch synchronized when its fixed primary is reachable.
    /// Probe before taking the writer gate so a partition does not queue up
    /// network waits behind local edits or scheduled work.
    pub async fn run_catalog_sync(
        self: Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed=shutdown.changed()=>{if changed.is_err() || *shutdown.borrow(){break;}},
                _=interval.tick()=>{
                    if self.catalog.lock().await.is_none(){continue;}
                    match self.peers.mode().await {
                        Ok(PeerMode::Primary)=>(),
                        Ok(PeerMode::Node) if self.primary_git_available().await=>(),
                        _=>continue,
                    }
                    let Ok(_gate)=self.authority.try_write() else{continue;};
                    let Some(catalog)=self.catalog.lock().await.clone() else {continue;};
                    if self.last_shared_error.lock().await.is_none(){let _=self.synchronize_catalog(&catalog).await;}
                }
            }
        }
    }

    /// Publish finalized owner-local history to the local writer even while
    /// the primary is unreachable. A completed turn is read, never resumed.
    pub async fn run_history_projection(
        self: Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        use std::time::{Duration, Instant};
        let mut events = self.events.subscribe();
        let mut interval = tokio::time::interval(Duration::from_millis(750));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending = BTreeMap::<String, HistoryChange>::new();
        let mut refresh_all = true;
        let mut next_full = Instant::now();
        loop {
            tokio::select! {
                changed=shutdown.changed()=>{if changed.is_err() || *shutdown.borrow(){break;}},
                received=events.recv()=>match received {
                    Ok(event) if event.node_id==self.node_id()=>{
                        let method=event.message["method"].as_str().unwrap_or_default();
                        let id=event.message["params"]["threadId"].as_str();
                        let change=match method {"turn/completed"|"thread/name/updated"=>Some(HistoryChange::Read(None)),"thread/archived"=>Some(HistoryChange::Read(Some(true))),"thread/unarchived"=>Some(HistoryChange::Read(Some(false))),"thread/deleted"=>Some(HistoryChange::Delete),_=>None};
                        if let (Some(id),Some(change))=(id,change) {
                            // Completion/rename refreshes cannot undo a known archive
                            // transition or resurrect a deleted transcript.
                            let keep = matches!(pending.get(id), Some(HistoryChange::Delete))
                                || (matches!(change, HistoryChange::Read(None))
                                    && matches!(pending.get(id), Some(HistoryChange::Read(Some(_)))));
                            if !keep { pending.insert(id.into(), change); }
                            if pending.len()>4096 {refresh_all=true;pending.retain(|_,change|matches!(change,HistoryChange::Delete));}
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>refresh_all=true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed)=>break,
                    _=>(),
                },
                _=interval.tick()=>{
                    if pending.is_empty() && !refresh_all {continue;}
                    let Ok(_gate)=self.authority.try_read() else{continue;};
                    let Some(catalog)=self.catalog.lock().await.clone() else {continue;};
                    if refresh_all && Instant::now()>=next_full && self.host.is_connected().await {
                        next_full=Instant::now()+Duration::from_secs(30);
                        let history=tokio::select!{_ = shutdown.changed()=>return, history=self.host.export_history()=>history};
                        if history.complete && catalog.publish_history(&history).is_ok(){refresh_all=false;}
                    }
                    let batch=pending.keys().take(8).cloned().collect::<Vec<_>>();
                    let mut rows=Vec::new();let mut completed=Vec::new();let mut blobs=BTreeMap::new();
                    for id in batch {
                        match pending.get(&id).copied() {
                            Some(HistoryChange::Delete)=>{if catalog.tombstone_history(&id).is_ok(){pending.remove(&id);}},
                            Some(HistoryChange::Read(archived))=>{
                                let archived=archived.or_else(|| {
                                    catalog.historical_thread(self.node_id(), &id).ok().flatten()
                                        .and_then(|history|history["thread"]["archived"].as_bool())
                                });
                                let exported=tokio::select!{_ = shutdown.changed()=>return, result=self.host.export_history_thread_with_blobs(&id,archived)=>result};
                                if let Ok((mut row,sources))=exported {
                                    // The closed transcript wrapper contains manifests only.
                                    // Verified local paths remain in the typed batch companion.
                                    if let Some(object)=row.as_object_mut(){object.remove("blobs");}
                                    for blob in sources {blobs.entry(blob.sha256.clone()).or_insert(blob);}
                                    rows.push(row);completed.push(id);
                                }
                            }
                            None=>(),
                        }
                    }
                    if !rows.is_empty() && catalog.publish_history(&HistoryExport{schema_version:1,owner_origin:"local-runtime".into(),complete:true,threads:rows,blobs:blobs.into_values().collect(),failures:vec![]}).is_ok(){for id in completed{pending.remove(&id);}}
                },
            }
        }
    }

    async fn primary_git_available(&self) -> bool {
        let Ok(peer) = self.peers.git_transport().await else {
            return false;
        };
        let Ok(client) = crate::peers::pinned_client(&peer.trusted_ca_pem, &peer.fingerprint)
        else {
            return false;
        };
        let request = Request {
            id: "git-availability".into(),
            channel: Channel::System,
            method: "status".into(),
            params: json!({}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(peer.node_id.clone()),
        };
        let Ok(response) = client
            .post(format!(
                "{}/v1/request",
                peer.endpoint.trim_end_matches('/')
            ))
            .timeout(std::time::Duration::from_secs(2))
            .bearer_auth(peer.credential.expose_secret())
            .json(&request)
            .send()
            .await
        else {
            return false;
        };
        let Ok(reply) = response.json::<Reply>().await else {
            return false;
        };
        reply.id == request.id
            && reply.error.is_none()
            && reply.result.is_some_and(|result| {
                result["nodeId"] == peer.node_id && result["sharedConfigured"] == true
            })
    }
}

#[derive(Clone, Copy)]
enum HistoryChange {
    Read(Option<bool>),
    Delete,
}

fn writer_path(catalog: &Catalog) -> PathBuf {
    catalog
        .data_dir()
        .join("fleet-catalog/writer/.brr-fleet/repo")
}

fn pending_changes(catalog: &Catalog) -> Result<usize, ApiError> {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(writer_path(catalog))
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "-z",
            "refs/remotes/origin/main",
            "HEAD",
            "--",
            "records",
        ])
        .output()
        .map_err(host_error)?;
    if !result.status.success() {
        return Err(ApiError::new(
            "sync_unavailable",
            "Could not compare the local Git writer with the last known main revision",
        ));
    }
    Ok(result
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .count())
}

fn commit_history(catalog: &Catalog) -> Result<Vec<Value>, ApiError> {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(writer_path(catalog))
        .args(["log", "-n", "30", "--format=%H%x00%ct%x00%ae%x00%s", "HEAD"])
        .output()
        .map_err(host_error)?;
    if !result.status.success() {
        return Err(ApiError::new(
            "sync_unavailable",
            "Could not read committed sync history",
        ));
    }
    let stdout = String::from_utf8(result.stdout).map_err(host_error)?;
    stdout.lines().map(|line|{
        let fields=line.splitn(4,'\0').collect::<Vec<_>>();
        if fields.len()!=4 {return Err(ApiError::new("sync_unavailable","Git returned a malformed history entry"));}
        Ok(json!({"id":fields[0],"createdAt":fields[1].parse::<u64>().map_err(host_error)?,"nodeId":fields[2].split('@').next().unwrap_or_default(),"summary":fields[3]}))
    }).collect()
}

fn conflict_view(conflict: &brr_fleet::Conflict) -> Value {
    let revision = crate::catalog::conflict_revision(conflict);
    let profile_review = crate::catalog::conflict_table(conflict)
        == Some(SharedTable::AgentProfiles)
        && conflict
            .current
            .as_ref()
            .is_some_and(|record| !record.deleted)
        && conflict
            .incoming
            .as_ref()
            .is_some_and(|record| !record.deleted);
    let kind = conflict
        .current
        .as_ref()
        .or(conflict.incoming.as_ref())
        .map(|record| record.kind.clone());
    json!({"id":conflict.id,"revision":revision,"kind":kind,"profileReview":profile_review,"table":crate::catalog::conflict_table(conflict),"base":conflict.base,"current":conflict.current,"incoming":conflict.incoming})
}

async fn source_snapshot(
    host: &Host,
    node: &str,
    projects: &[SharedRow],
) -> Result<SharedSnapshot, ApiError> {
    let mut source = host
        .agent_store()
        .shared_snapshot()
        .await
        .map_err(host_error)?;
    source.rows.retain(|row| row.table != SharedTable::Projects);
    source.rows.extend_from_slice(projects);
    for row in &mut source.rows {
        if row.origin_node_id.is_none() {
            row.origin_node_id = Some(node.into());
        }
    }
    source.validate(true).map_err(host_error)?;
    Ok(source)
}

fn verify_import(catalog: &Catalog, source: &SharedSnapshot) -> Result<(), ApiError> {
    let (_, committed) = catalog.committed_shared().map_err(host_error)?;
    let identity = |row: &SharedRow| (row.origin_node_id.clone(), row.table as u8, row.key.clone());
    let rows = committed
        .rows
        .iter()
        .map(|row| (identity(row), &row.data))
        .collect::<BTreeMap<_, _>>();
    if source.rows.iter().any(|row| {
        rows.get(&identity(row))
            .is_none_or(|data| **data != row.data)
    }) {
        return Err(ApiError::new(
            "import_mismatch",
            "Git records did not preserve every verified source row",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod attachment_history_tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn resumes_accepted_attachment_activation_from_backup_when_live_blob_is_missing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("fixture-runtime");
        fs::write(&runtime, include_str!("../tests/fixtures/runtime.py")).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: runtime,
            name: "Attachment recovery".into(),
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
        let imported = service.host.chat_request("attachments.importBytes",json!({"ownerId":id,"files":[{"name":"recovery.txt","mime":"text/plain","data":"cmVjb3ZlcmVkIGZyb20gdmVyaWZpZWQgYmFja3Vw"}]})).await.unwrap();
        let sent = service.host.chat_request("foreground.submit",json!({"id":"accepted-before-recovery","threadId":id,"document":{"version":1,"text":"saved","references":[],"quotes":[]},"attachmentIds":[imported[0]["id"]],"settings":{"model":"_runtime_default","mode":"default","effort":null}})).await.unwrap();
        assert_eq!(sent["submission"]["status"], "accepted");
        service
            .host
            .request(
                "turn/interrupt",
                json!({"threadId":id,"turnId":sent["submission"]["turnId"]}),
            )
            .await
            .unwrap();
        service.host.shutdown().await;
        let history = service.host.export_quiesced_history().await.unwrap();
        assert!(history.complete, "{:?}", history.failures);
        assert_eq!(history.blobs.len(), 1);
        let sha = history.blobs[0].sha256.clone();
        let backup = crate::migration::prepare(&service.host, service.node_id(), &history)
            .await
            .unwrap();
        service
            .save_activation(&Activation {
                version: 1,
                node_id: service.node_id().into(),
                primary_node_id: service.node_id().into(),
                git_origin: None,
                complete: false,
                backup: backup.clone(),
            })
            .unwrap();
        // Only a disposable fixture's live file is removed: retries must use
        // the independently verified backup, never a stale serialized path.
        fs::remove_file(history.blobs[0].verified_source_path().unwrap()).unwrap();
        let result = service.activate_catalog(service.node_id(), None).await;
        assert!(result.is_ok(), "{result:?}");
        // Normal successful activation reconnects its owner runtime. Stop that
        // fixture explicitly before proving catalog-only offline attachment reads.
        service.host.shutdown().await;
        let reply = service
            .handle(
                Request {
                    id: "recovered-preview".into(),
                    channel: Channel::Sessions,
                    method: "attachment.preview".into(),
                    params: json!({"threadId":id,"sha256":sha}),
                    operation_id: None,
                    expected_epoch: None,
                    expected_revision: None,
                    target_node_id: Some(service.node_id().into()),
                },
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(reply.error.is_none(), "{:?}", reply.error);
        assert_eq!(
            reply.result.unwrap()["preview"],
            "recovered from verified backup"
        );
        assert!(!service.host.is_connected().await);
        backup.verify().await.unwrap();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn resumes_verified_legacy_history_backup_without_rewriting_its_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::open(ServiceConfig {
            data_dir: dir.path().join("data"),
            runtime_path: dir.path().join("missing-runtime"),
            name: "Legacy recovery".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let sessions = service.host.data_dir().join("engine/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("prior.jsonl"), b"preserved legacy rollout\n").unwrap();
        let history = HistoryExport {
            schema_version: 1,
            owner_origin: "local-runtime".into(),
            complete: true,
            threads: vec![
                json!({"thread":{"id":"legacy-thread"},"turns":[],"origin":"local-runtime"}),
            ],
            blobs: vec![],
            failures: vec![],
        };
        let backup = crate::migration::prepare(&service.host, service.node_id(), &history)
            .await
            .unwrap();
        let raw_sha = backup.manifest.history_sha256.clone();
        let saved_history = fs::read(backup.directory.join("files/history/history.json")).unwrap();
        service
            .save_activation(&Activation {
                version: 1,
                node_id: service.node_id().into(),
                primary_node_id: service.node_id().into(),
                git_origin: None,
                complete: false,
                backup: backup.clone(),
            })
            .unwrap();
        let result = service.activate_catalog(service.node_id(), None).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(backup.manifest.history_sha256, raw_sha);
        assert_eq!(
            fs::read(backup.directory.join("files/history/history.json")).unwrap(),
            saved_history
        );
        backup.verify().await.unwrap();
        assert!(!service.host.is_connected().await);
        service.shutdown().await;
    }
}
