//! Strict bridge between the local AgentStore projection and FleetRepo Git.
//!
//! The writer checkout is the execution authority. On a primary, `gateway`
//! owns `main` and its bare origin while `writer` owns
//! `nodes/<primary-node-id>`; SQL always follows the writer HEAD, including
//! while the gateway has an unresolved merge.

use brr_agent_core::{AgentStore, ProfileCandidate, SharedRow, SharedSnapshot, SharedTable};
use brr_fleet::{BlobFile, Conflict, FleetRepo, GitTransfer, Record, RecordKind, SyncOutcome};
use brr_host::export::{HistoryAttachment, HistoryBlob, HistoryExport};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[path = "profile_merge.rs"]
mod profile_merge;

const CATALOG_DIR: &str = "fleet-catalog";
const GATEWAY_DIR: &str = "gateway";
const WRITER_DIR: &str = "writer";

#[derive(Clone)]
pub struct Catalog {
    data_dir: PathBuf,
    node_id: String,
    store: AgentStore,
    repos: Arc<Mutex<Repos>>,
}

#[derive(Clone)]
struct Repos {
    writer: FleetRepo,
    gateway: Option<FleetRepo>,
    primary: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogStatus {
    pub node_id: String,
    pub role: String,
    pub revision: String,
    pub records: usize,
    pub conflicts: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSync {
    pub writer: SyncOutcome,
    pub gateway: Option<SyncOutcome>,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMergeReview {
    pub candidate: ProfileCandidate,
    pub review_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryPublication {
    pub revision: String,
    pub count: usize,
    pub sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExportedHistoryThread {
    thread: Value,
    turns: Vec<Value>,
    origin: String,
    #[serde(default)]
    attachments: Vec<HistoryAttachment>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryRecordContent {
    owner_node_id: String,
    read_only: bool,
    thread: Value,
    turns: Vec<Value>,
    #[serde(default)]
    attachments: Vec<HistoryAttachment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryBlobRecord {
    sha256: String,
    size_bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SharedRecord {
    table: SharedTable,
    key: String,
    data: Value,
}

impl Catalog {
    /// Create a primary's separate gateway/main and local writer/node branch,
    /// seed the writer from the existing shared SQLite snapshot, and recover
    /// SQL from its committed HEAD before enabling the shared hook.
    pub async fn init_primary(
        data_dir: impl AsRef<Path>,
        node_id: impl Into<String>,
        store: AgentStore,
    ) -> Result<Self, String> {
        Self::init_primary_with_projects(data_dir, node_id, store, &[]).await
    }

    pub async fn init_primary_with_projects(
        data_dir: impl AsRef<Path>,
        node_id: impl Into<String>,
        store: AgentStore,
        projects: &[SharedRow],
    ) -> Result<Self, String> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let node_id = node_id.into();
        let root = data_dir.join(CATALOG_DIR);
        std::fs::create_dir_all(&root).map_err(err)?;
        let gateway =
            FleetRepo::init_primary(root.join(GATEWAY_DIR), node_id.clone()).map_err(err)?;
        let origin = gateway.origin().to_owned();
        let mut gateway = gateway;
        install_validator(&mut gateway);
        let writer =
            FleetRepo::init_node(root.join(WRITER_DIR), node_id.clone(), &origin).map_err(err)?;
        let catalog = Self::new(data_dir, node_id, store, writer, Some(gateway), true);
        let candidate = catalog.local_candidate(projects).await?;
        let candidate = catalog.additive_seed(&candidate)?;
        catalog.commit_candidate(&candidate, RecordDomain::AllShared)?;
        catalog.synchronize().await?;
        catalog.recover().await?;
        catalog.install_hook();
        Ok(catalog)
    }

    /// Join an existing fleet origin. `transfer` is the pinned Git HTTP
    /// transport when the origin is remote; local bare repositories may pass
    /// `None` and use ordinary local Git operations.
    pub async fn init_node(
        data_dir: impl AsRef<Path>,
        node_id: impl Into<String>,
        origin: &str,
        transfer: Option<Arc<dyn GitTransfer>>,
        store: AgentStore,
    ) -> Result<Self, String> {
        Self::init_node_with_projects(data_dir, node_id, origin, transfer, store, &[]).await
    }

    pub async fn init_node_with_projects(
        data_dir: impl AsRef<Path>,
        node_id: impl Into<String>,
        origin: &str,
        transfer: Option<Arc<dyn GitTransfer>>,
        store: AgentStore,
        projects: &[SharedRow],
    ) -> Result<Self, String> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let node_id = node_id.into();
        let root = data_dir.join(CATALOG_DIR);
        std::fs::create_dir_all(&root).map_err(err)?;
        let writer = match transfer {
            Some(transport) => FleetRepo::init_node_with_transport(
                root.join(WRITER_DIR),
                node_id.clone(),
                origin,
                transport,
            ),
            None => FleetRepo::init_node(root.join(WRITER_DIR), node_id.clone(), origin),
        }
        .map_err(err)?;
        let catalog = Self::new(data_dir, node_id, store, writer, None, false);
        // Existing local rows are imported into the node writer with their
        // local origin, while already-shared rows retain their Git origins.
        // Build this snapshot before recovery so an interrupted activation
        // cannot discard pre-Git SQL data; the additive seed below compares it
        // with the committed tree and rejects conflicting identities.
        let candidate = catalog.local_candidate(projects).await?;
        let candidate = catalog.additive_seed(&candidate)?;
        catalog.commit_candidate(&candidate, RecordDomain::AllShared)?;
        catalog.recover().await?;
        catalog.install_hook();
        Ok(catalog)
    }

    pub async fn open_primary(
        data_dir: impl AsRef<Path>,
        node_id: impl Into<String>,
        store: AgentStore,
    ) -> Result<Self, String> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let node_id = node_id.into();
        let root = data_dir.join(CATALOG_DIR);
        let gateway =
            FleetRepo::open_primary(root.join(GATEWAY_DIR), node_id.clone()).map_err(err)?;
        let origin = gateway.origin().to_owned();
        let mut gateway = gateway;
        install_validator(&mut gateway);
        let writer =
            FleetRepo::open_node(root.join(WRITER_DIR), node_id.clone(), &origin).map_err(err)?;
        let catalog = Self::new(data_dir, node_id, store, writer, Some(gateway), true);
        catalog.recover().await?;
        catalog.install_hook();
        Ok(catalog)
    }

    pub async fn open_node(
        data_dir: impl AsRef<Path>,
        node_id: impl Into<String>,
        origin: &str,
        transfer: Option<Arc<dyn GitTransfer>>,
        store: AgentStore,
    ) -> Result<Self, String> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let node_id = node_id.into();
        let root = data_dir.join(CATALOG_DIR);
        let writer = match transfer {
            Some(transport) => {
                FleetRepo::open_with_transport(root.join(WRITER_DIR), node_id.clone(), transport)
            }
            None => FleetRepo::open_node(root.join(WRITER_DIR), node_id.clone(), origin),
        }
        .map_err(err)?;
        if writer.origin() != origin {
            return Err("persisted fleet origin does not match the requested origin".into());
        }
        let catalog = Self::new(data_dir, node_id, store, writer, None, false);
        catalog.recover().await?;
        catalog.install_hook();
        Ok(catalog)
    }

    fn new(
        data_dir: PathBuf,
        node_id: String,
        store: AgentStore,
        writer: FleetRepo,
        gateway: Option<FleetRepo>,
        primary: bool,
    ) -> Self {
        Self {
            data_dir,
            node_id,
            store,
            repos: Arc::new(Mutex::new(Repos {
                writer,
                gateway,
                primary,
            })),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Return every validated record from the writer's committed Git tree,
    /// including tombstones needed for synchronization and recovery.
    pub fn records(&self) -> Result<Vec<Record>, String> {
        let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        let records = repos.writer.list_records().map_err(err)?;
        validate_record_set(&records)?;
        Ok(records)
    }

    /// Read the current writer HEAD as a validated SQL materialization input.
    pub fn committed_shared(&self) -> Result<(String, SharedSnapshot), String> {
        let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        committed_snapshot(&repos.writer)
    }

    /// Hash the validated, normalized history manifest before publication.
    pub fn history_manifest_sha256(&self, export: &HistoryExport) -> Result<String, String> {
        let (threads, _, _) = history_records(export, &self.node_id)?;
        let manifest = serde_json::json!({"schemaVersion":export.schema_version,"ownerOrigin":export.owner_origin,"threads":threads});
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&manifest).map_err(err)?)
        ))
    }

    /// Commit a complete, read-only history export as one Git batch owned by
    /// this node. Runtime history is intentionally excluded from SharedSnapshot
    /// and is never materialized into AgentStore.
    pub fn publish_history(&self, export: &HistoryExport) -> Result<HistoryPublication, String> {
        let (manifest_threads, records, blob_files) = history_records(export, &self.node_id)?;
        let manifest = serde_json::json!({
            "schemaVersion": export.schema_version,
            "ownerOrigin": export.owner_origin,
            "threads": manifest_threads,
        });
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(err)?;
        let sha256 = format!("{:x}", Sha256::digest(&manifest_bytes));
        let raw_thread_ids = records
            .iter()
            .map(|record| {
                record.content["thread"]["id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("history record has no runtime thread ID")
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        let prior = repos.writer.list_records().map_err(err)?;
        validate_record_set(&prior)?;
        let mut by_id = prior
            .iter()
            .cloned()
            .map(|record| (record.entity_id(), record))
            .collect::<BTreeMap<_, _>>();
        let mut changes = Vec::new();
        for mut record in records {
            let entity_id = record.entity_id();
            let old = by_id.get(&entity_id).cloned();
            if let Some(old) = old {
                if old.kind != RecordKind::Transcript {
                    return Err("history record ID collides with another fleet record".into());
                }
                if old.deleted {
                    return Err("a tombstoned history record cannot be resurrected".into());
                }
                if old.content == record.content {
                    continue;
                }
                record.revision = String::new();
                record.base_revision = Some(old.revision.clone());
                changes.push((record.clone(), Some(old.revision.clone())));
                by_id.insert(entity_id, record);
            } else {
                changes.push((record.clone(), None));
                by_id.insert(entity_id, record);
            }
        }
        for blob in &export.blobs {
            let blob_record = blob_record(&self.node_id, blob.sha256.clone(), blob.size_bytes)?;
            let entity_id = blob_record.entity_id();
            match by_id.get(&entity_id) {
                Some(old) if old.kind != RecordKind::Blob => {
                    return Err("history blob ID collides with another fleet record".into());
                }
                Some(old) if old.deleted => {
                    return Err("a content-addressed history blob cannot be tombstoned".into());
                }
                Some(old) if old.content == blob_record.content => {}
                Some(_) => {
                    return Err("content-addressed history blob metadata is immutable".into());
                }
                None => {
                    changes.push((blob_record.clone(), None));
                    by_id.insert(entity_id, blob_record);
                }
            }
        }
        let revision = if changes.is_empty() {
            repos.writer.head().map_err(err)?
        } else {
            repos
                .writer
                .commit_records_with_blobs(changes, blob_files)
                .map_err(err)?;
            repos.writer.head().map_err(err)?
        };
        let readback = repos.writer.list_records().map_err(err)?;
        validate_record_set(&readback)?;
        let mut readback_threads = Vec::with_capacity(raw_thread_ids.len());
        for raw_id in &raw_thread_ids {
            let id = history_record_id(raw_id);
            let record = readback
                .iter()
                .find(|record| {
                    record.kind == RecordKind::Transcript
                        && !record.deleted
                        && record.origin_node_id == self.node_id
                        && record.id == id
                })
                .ok_or_else(|| format!("committed history record `{raw_id}` is missing"))?;
            let content: HistoryRecordContent =
                serde_json::from_value(record.content.clone()).map_err(err)?;
            readback_threads.push(serde_json::json!({
                "thread": content.thread,
                "turns": content.turns,
                "origin": "local-runtime",
                "attachments": content.attachments,
            }));
        }
        let count = readback_threads.len();
        let readback_manifest = serde_json::json!({
            "schemaVersion": export.schema_version,
            "ownerOrigin": export.owner_origin,
            "threads": readback_threads,
        });
        let readback_sha256 = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&readback_manifest).map_err(err)?)
        );
        if readback_sha256 != sha256 {
            return Err("committed history readback does not match the export manifest".into());
        }
        Ok(HistoryPublication {
            revision,
            count,
            sha256: readback_sha256,
        })
    }

    /// Fetch a node-owned historical conversation by its original runtime ID.
    pub fn historical_thread(
        &self,
        origin: &str,
        thread_id: &str,
    ) -> Result<Option<Value>, String> {
        let records = self.records()?;
        let id = history_record_id(thread_id);
        let Some(record) = records.iter().find(|record| {
            record.kind == RecordKind::Transcript
                && !record.deleted
                && record.origin_node_id == origin
                && record.id == id
        }) else {
            return Ok(None);
        };
        Ok(Some(history_view(record)?))
    }

    /// Read one immutable saved-history attachment only when its digest is
    /// referenced by the exact origin-qualified thread manifest.
    pub fn historical_attachment(
        &self,
        origin: &str,
        thread_id: &str,
        sha256: &str,
    ) -> Result<Option<(HistoryAttachment, HistoryBlob)>, String> {
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("attachment digest must be lowercase SHA-256".into());
        }
        let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        let records = repos.writer.list_records().map_err(err)?;
        validate_record_set(&records)?;
        let transcript_id = history_record_id(thread_id);
        let Some(transcript) = records.iter().find(|record| {
            record.kind == RecordKind::Transcript
                && !record.deleted
                && record.origin_node_id == origin
                && record.id == transcript_id
        }) else {
            return Ok(None);
        };
        let history: HistoryRecordContent =
            serde_json::from_value(transcript.content.clone()).map_err(err)?;
        let Some(attachment) = history.attachments.into_iter().find(|a| a.sha256 == sha256) else {
            return Ok(None);
        };
        let Some(blob_record) = records.iter().find(|record| {
            record.kind == RecordKind::Blob
                && !record.deleted
                && record.origin_node_id == origin
                && record.id == sha256
        }) else {
            return Err("history attachment references a missing owner-local blob".into());
        };
        let size_bytes = blob_record.content["sizeBytes"]
            .as_u64()
            .ok_or("history blob has invalid size metadata")?;
        if attachment.size_bytes != size_bytes {
            return Err("history attachment size differs from its blob metadata".into());
        }
        let repo_path = std::fs::canonicalize(repos.writer.repo_path()).map_err(err)?;
        let source_path = repo_path.join("blobs").join(sha256);
        let blob = HistoryBlob::from_verified_path(source_path, sha256, size_bytes).map_err(err)?;
        Ok(Some((attachment, blob)))
    }

    /// Return all committed historical conversations in stable origin/ID order.
    pub fn historical_threads(&self) -> Result<Vec<Value>, String> {
        let mut records = self
            .records()?
            .into_iter()
            .filter(|record| record.kind == RecordKind::Transcript && !record.deleted)
            .collect::<Vec<_>>();
        records.sort_by(|a, b| (&a.origin_node_id, &a.id).cmp(&(&b.origin_node_id, &b.id)));
        records.iter().map(history_view).collect()
    }

    /// Tombstone a transcript owned by this node after the source chat is
    /// deleted. A repeated delete, or an ID owned by another node, is a no-op.
    pub fn tombstone_history(&self, runtime_thread_id: &str) -> Result<Option<String>, String> {
        if runtime_thread_id.trim().is_empty() {
            return Err("runtime thread ID must be non-empty".into());
        }
        let mut repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        let records = repos.writer.list_records().map_err(err)?;
        validate_record_set(&records)?;
        let id = history_record_id(runtime_thread_id);
        let Some(record) = records
            .iter()
            .find(|record| record.origin_node_id == self.node_id && record.id == id)
        else {
            return Ok(None);
        };
        if record.kind != RecordKind::Transcript {
            return Err("history identity is occupied by a non-transcript record".into());
        }
        let content: HistoryRecordContent =
            serde_json::from_value(record.content.clone()).map_err(err)?;
        if content.thread["id"].as_str() != Some(runtime_thread_id) {
            return Err("history transcript ID does not match its runtime thread".into());
        }
        if record.deleted {
            return Ok(None);
        }
        let mut tombstone = record.clone();
        tombstone.deleted = true;
        tombstone.revision.clear();
        tombstone.base_revision = None;
        repos
            .writer
            .commit_record(tombstone, Some(&record.revision))
            .map_err(err)?;
        Ok(Some(repos.writer.head().map_err(err)?))
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn is_primary(&self) -> bool {
        self.repos.lock().expect("catalog lock").primary
    }

    /// Publish the AgentStore transaction candidate to the writer branch.
    /// This synchronous hook intentionally does not read SQLite or contact the
    /// remote; Git commit is durable before the SQL transaction may commit.
    pub fn install_hook(&self) {
        let repos = Arc::clone(&self.repos);
        let node_id = self.node_id.clone();
        self.store.set_shared_commit_hook(move |snapshot| {
            let candidate = qualify_snapshot(snapshot, &node_id)?;
            candidate.validate(false)?;
            commit_snapshot_locked(&repos, &candidate, RecordDomain::Agent)
        });
    }

    /// Replace only project metadata records, preserving AgentStore records
    /// and any other catalog domains. The project rows must not contain local
    /// folder paths, worktrees, or cwd values; SharedTable::Projects has a
    /// closed field schema that rejects those fields.
    pub fn publish_projects(&self, rows: &[SharedRow]) -> Result<String, String> {
        let mut projects = Vec::with_capacity(rows.len());
        for row in rows {
            if row.table != SharedTable::Projects {
                return Err("project catalog publication accepts only projects rows".into());
            }
            let mut row = row.clone();
            if row.origin_node_id.is_none() {
                row.origin_node_id = Some(self.node_id.clone());
            }
            projects.push(row);
        }
        let current = {
            let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
            records_snapshot(&repos.writer.list_records().map_err(err)?)?
        };
        let candidate = overlay_domain(current, projects, RecordDomain::Projects)?;
        candidate.validate(true)?;
        self.commit_candidate(&candidate, RecordDomain::Projects)
    }

    /// Read committed Git HEAD and restore the SQL projection before execution.
    /// The head is checked again after materialization to close the race with a
    /// concurrent Git-before-SQL writer commit.
    pub async fn recover(&self) -> Result<String, String> {
        self.store
            .materialize_shared_from(&self.node_id, || {
                let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
                committed_snapshot(&repos.writer)
            })
            .await
    }

    pub async fn synchronize(&self) -> Result<CatalogSync, String> {
        let mut outcome = None;
        let revision = self
            .store
            .materialize_shared_from(&self.node_id, || {
                let mut repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
                // Even a failed push may follow a successful local merge. Restore
                // the resulting valid HEAD before releasing the SQL writer lock.
                outcome = Some(synchronize_repos(&mut repos));
                committed_snapshot(&repos.writer)
            })
            .await?;
        let (writer, gateway) = outcome.ok_or("Missing Git synchronization outcome")??;
        Ok(CatalogSync {
            writer,
            gateway,
            revision,
        })
    }

    pub fn conflicts(&self) -> Result<Vec<Conflict>, String> {
        let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        let target = repos.gateway.as_ref().unwrap_or(&repos.writer);
        target.conflicts().map_err(err)
    }

    pub fn status(&self) -> Result<CatalogStatus, String> {
        let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        let records = repos.writer.list_records().map_err(err)?;
        let target = repos.gateway.as_ref().unwrap_or(&repos.writer);
        Ok(CatalogStatus {
            node_id: self.node_id.clone(),
            role: if repos.primary { "primary" } else { "node" }.into(),
            revision: repos.writer.head().map_err(err)?,
            records: records.len(),
            conflicts: target.conflicts().map_err(err)?.len(),
        })
    }

    pub async fn resolve(&self, id: &str, record: Record) -> Result<Record, String> {
        let mut result = None;
        self.store
            .materialize_shared_from(&self.node_id, || {
                let mut repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
                result = Some(if let Some(gateway) = repos.gateway.as_mut() {
                    gateway.resolve(id, record).map_err(err)
                } else {
                    repos.writer.resolve(id, record).map_err(err)
                });
                if result.as_ref().is_some_and(Result::is_ok) {
                    // Resolution on main must be pulled into the local writer.
                    // Network failure is reported after projecting its valid HEAD.
                    if let Err(error) = synchronize_repos(&mut repos) {
                        result = Some(Err(error));
                    }
                }
                committed_snapshot(&repos.writer)
            })
            .await?;
        result.ok_or("Missing Git resolution outcome")?
    }

    pub fn preview_profile_resolution(
        &self,
        id: &str,
        revision: &str,
        candidate: ProfileCandidate,
    ) -> Result<ProfileMergeReview, String> {
        Ok(
            profile_merge::ProfileMerge::prepare(&self.conflicts()?, id, revision, candidate)?
                .review,
        )
    }

    pub async fn approve_profile_resolution(
        &self,
        id: &str,
        revision: &str,
        candidate: ProfileCandidate,
        review_hash: &str,
    ) -> Result<Record, String> {
        let mut result = None;
        self.store
            .materialize_shared_from(&self.node_id, || {
                let mut repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
                let gateway = repos
                    .gateway
                    .as_mut()
                    .ok_or("Review conflicts on the primary")?;
                let merge = profile_merge::ProfileMerge::prepare(
                    &gateway.conflicts().map_err(err)?,
                    id,
                    revision,
                    candidate,
                )?;
                if merge.review.review_hash != review_hash {
                    return Err("The profile differs from the exact content reviewed".into());
                }
                let (resolutions, additions) = merge.records(&self.node_id)?;
                result = Some(
                    gateway
                        .resolve_batch(resolutions, additions)
                        .map_err(err)
                        .and_then(|rows| {
                            rows.into_iter()
                                .find(|row| row.id == id)
                                .ok_or("Missing resolved profile".into())
                        }),
                );
                if result.as_ref().is_some_and(Result::is_ok) {
                    if let Err(error) = synchronize_repos(&mut repos) {
                        result = Some(Err(error));
                    }
                }
                committed_snapshot(&repos.writer)
            })
            .await?;
        result.ok_or("Missing profile resolution outcome")?
    }

    pub fn origin(&self) -> Result<String, String> {
        let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
        Ok(repos
            .gateway
            .as_ref()
            .map(FleetRepo::origin)
            .unwrap_or_else(|| repos.writer.origin())
            .to_owned())
    }

    async fn local_candidate(&self, projects: &[SharedRow]) -> Result<SharedSnapshot, String> {
        let snapshot = self.store.shared_snapshot().await?;
        let mut candidate = qualify_snapshot(&snapshot, &self.node_id)?;
        let mut rows = candidate
            .rows
            .into_iter()
            .map(|row| (row_identity(&row), row))
            .collect::<BTreeMap<_, _>>();
        for row in projects {
            if row.table != SharedTable::Projects {
                return Err("Expected portable project metadata".into());
            }
            let mut row = row.clone();
            if row.origin_node_id.is_none() {
                row.origin_node_id = Some(self.node_id.clone());
            }
            rows.insert(row_identity(&row), row);
        }
        candidate.rows = rows.into_values().collect();
        candidate.validate(true)?;
        Ok(candidate)
    }

    fn commit_candidate(
        &self,
        candidate: &SharedSnapshot,
        domain: RecordDomain,
    ) -> Result<String, String> {
        commit_snapshot_locked(&self.repos, candidate, domain)
    }

    fn additive_seed(&self, candidate: &SharedSnapshot) -> Result<SharedSnapshot, String> {
        let committed = {
            let repos = self.repos.lock().map_err(|_| "catalog lock poisoned")?;
            records_snapshot(&repos.writer.list_records().map_err(err)?)?
        };
        let mut rows = committed
            .rows
            .into_iter()
            .map(|row| (row_identity(&row), row))
            .collect::<BTreeMap<_, _>>();
        for row in &candidate.rows {
            let identity = row_identity(row);
            match rows.get(&identity) {
                Some(existing) if existing.data == row.data => {}
                Some(_) => {
                    return Err(format!(
                        "activation source conflicts with committed shared row {}:{}",
                        row.origin_node_id.as_deref().unwrap_or(""),
                        row.key
                    ));
                }
                None => {
                    rows.insert(identity, row.clone());
                }
            }
        }
        let snapshot = SharedSnapshot {
            schema_version: 1,
            rows: rows.into_values().collect(),
        };
        snapshot.validate(true)?;
        Ok(snapshot)
    }
}

fn committed_snapshot(writer: &FleetRepo) -> Result<(String, SharedSnapshot), String> {
    let revision = writer.head().map_err(err)?;
    let snapshot = records_snapshot(&writer.list_records().map_err(err)?)?;
    snapshot.validate(true)?;
    Ok((revision, snapshot))
}

fn synchronize_repos(repos: &mut Repos) -> Result<(SyncOutcome, Option<SyncOutcome>), String> {
    let writer = repos.writer.sync().map_err(err)?;
    let gateway = if let Some(gateway) = repos.gateway.as_mut() {
        Some(gateway.sync().map_err(err)?)
    } else {
        return Ok((writer, None));
    };
    let after = repos.writer.sync().map_err(err)?;
    Ok((
        if matches!(after, SyncOutcome::Conflicted { .. }) {
            after
        } else {
            writer
        },
        gateway,
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordDomain {
    Agent,
    Projects,
    AllShared,
}

fn qualify_snapshot(
    snapshot: &SharedSnapshot,
    local_node_id: &str,
) -> Result<SharedSnapshot, String> {
    let mut snapshot = snapshot.clone();
    for row in &mut snapshot.rows {
        match row.origin_node_id.as_deref() {
            None | Some("") => row.origin_node_id = Some(local_node_id.to_owned()),
            Some(_) => {}
        }
    }
    snapshot.validate(false)?;
    Ok(snapshot)
}

fn commit_snapshot_locked(
    repos: &Arc<Mutex<Repos>>,
    incoming: &SharedSnapshot,
    domain: RecordDomain,
) -> Result<String, String> {
    incoming.validate(false)?;
    let mut repos = repos.lock().map_err(|_| "catalog lock poisoned")?;
    let prior = repos.writer.list_records().map_err(err)?;
    let current = records_snapshot(&prior)?;
    let rows = if domain == RecordDomain::AllShared {
        overlay_domain(current, incoming.rows.clone(), domain)?
    } else {
        overlay_domain(current, incoming.rows.clone(), domain)?
    };
    rows.validate(true)?;
    commit_snapshot_to_repo(&mut repos.writer, &prior, &rows, domain)
}

fn commit_snapshot_to_repo(
    writer: &mut FleetRepo,
    prior: &[Record],
    candidate: &SharedSnapshot,
    domain: RecordDomain,
) -> Result<String, String> {
    let mut desired = prior
        .iter()
        .cloned()
        .map(|record| (record.entity_id(), record))
        .collect::<BTreeMap<_, _>>();
    let included = candidate
        .rows
        .iter()
        .map(|row| row_identity(row))
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for row in &candidate.rows {
        if !in_domain(row.table, domain) {
            continue;
        }
        let record = row_record(row)?;
        let identity = record.entity_id();
        let old = desired.get(&identity);
        if old.is_some_and(|old| old.deleted) {
            return Err(format!(
                "shared tombstone prevents resurrection of {}:{}",
                row.origin_node_id.as_deref().unwrap_or(""),
                row.key
            ));
        }
        if old.is_some_and(|old| {
            old.kind == record.kind && old.content == record.content && !old.deleted
        }) {
            continue;
        }
        let expected = old.map(|old| old.revision.clone());
        desired.insert(identity, record.clone());
        changes.push((record, expected));
    }

    // Missing owner-local rows and evidence links are tombstoned, never
    // physically removed from Git history. Foreign projections are retained
    // when the local store cannot author them.
    for old in prior {
        if old.deleted || !in_domain_record(old, domain) {
            continue;
        }
        let envelope = decode_record(old)?;
        let identity = (
            old.origin_node_id.clone(),
            envelope.table as u8,
            envelope.key.clone(),
        );
        if included.contains(&identity) {
            continue;
        }
        if domain == RecordDomain::Projects && old.origin_node_id != writer.node_id() {
            continue;
        }
        let mut tombstone = old.clone();
        tombstone.deleted = true;
        desired.insert(tombstone.entity_id(), tombstone.clone());
        changes.push((tombstone, Some(old.revision.clone())));
    }
    if changes.is_empty() {
        return writer.head().map_err(err);
    }
    validate_record_set(&desired.values().cloned().collect::<Vec<_>>())?;
    writer.commit_records(changes).map_err(err)?;
    writer.head().map_err(err)
}

fn overlay_domain(
    current: SharedSnapshot,
    incoming: Vec<SharedRow>,
    domain: RecordDomain,
) -> Result<SharedSnapshot, String> {
    let mut rows = current
        .rows
        .into_iter()
        .map(|row| (row_identity(&row), row))
        .collect::<BTreeMap<_, _>>();
    // AgentStore and ProjectStore snapshots are complete for their own
    // domains. Remove stale rows from that domain before applying the
    // candidate so deleted evidence links become Git tombstones downstream.
    rows.retain(|_, row| match domain {
        RecordDomain::Agent => row.table == SharedTable::Projects,
        RecordDomain::Projects => row.table != SharedTable::Projects,
        RecordDomain::AllShared => false,
    });
    for row in incoming {
        if !in_domain(row.table, domain) {
            continue;
        }
        let identity = row_identity(&row);
        rows.insert(identity, row);
    }
    let snapshot = SharedSnapshot {
        schema_version: 1,
        rows: rows.into_values().collect(),
    };
    snapshot.validate(true)?;
    Ok(snapshot)
}

fn records_snapshot(records: &[Record]) -> Result<SharedSnapshot, String> {
    let mut blobs = BTreeMap::<(String, String), u64>::new();
    for record in records
        .iter()
        .filter(|record| record.kind == RecordKind::Blob)
    {
        validate_record_envelope(record)?;
        let blob: HistoryBlobRecord =
            serde_json::from_value(record.content.clone()).map_err(err)?;
        if blobs
            .insert(
                (record.origin_node_id.clone(), blob.sha256),
                blob.size_bytes,
            )
            .is_some()
        {
            return Err("duplicate owner-local history blob metadata".into());
        }
    }
    let mut rows = Vec::new();
    let mut referenced_blobs = BTreeSet::<(String, String)>::new();
    for record in records {
        validate_record_envelope(record)?;
        if record.kind == RecordKind::Transcript {
            let history: HistoryRecordContent =
                serde_json::from_value(record.content.clone()).map_err(err)?;
            for attachment in &history.attachments {
                let identity = (record.origin_node_id.clone(), attachment.sha256.clone());
                if blobs.get(&identity) != Some(&attachment.size_bytes) {
                    return Err(
                        "history attachment has no matching blob owned by the same origin".into(),
                    );
                }
                referenced_blobs.insert(identity);
            }
            continue;
        }
        if record.kind == RecordKind::Blob {
            continue;
        }
        if record.deleted {
            continue;
        }
        let envelope = decode_record(record)?;
        rows.push(SharedRow {
            origin_node_id: Some(record.origin_node_id.clone()),
            table: envelope.table,
            key: envelope.key,
            data: envelope.data,
        });
    }
    if referenced_blobs.len() != blobs.len()
        || blobs
            .keys()
            .any(|identity| !referenced_blobs.contains(identity))
    {
        return Err("fleet contains an unreferenced history blob".into());
    }
    let snapshot = SharedSnapshot {
        schema_version: 1,
        rows,
    };
    snapshot.validate(true)?;
    Ok(snapshot)
}

fn row_record(row: &SharedRow) -> Result<Record, String> {
    let origin = row
        .origin_node_id
        .as_deref()
        .ok_or("shared record has no origin")?;
    if origin.trim().is_empty() || row.key.trim().is_empty() {
        return Err("shared record origin and key must be non-empty".into());
    }
    let table_name = serde_json::to_value(row.table)
        .map_err(err)?
        .as_str()
        .ok_or("invalid shared table identity")?
        .to_owned();
    let mut digest = Sha256::new();
    digest.update(origin.as_bytes());
    digest.update([0]);
    digest.update(table_name.as_bytes());
    digest.update([0]);
    digest.update(row.key.as_bytes());
    let id = format!("shared-{}", hex(&digest.finalize()));
    Ok(Record {
        schema_version: 1,
        id,
        kind: record_kind(row.table),
        origin_node_id: origin.to_owned(),
        revision: String::new(),
        base_revision: None,
        deleted: false,
        content: serde_json::to_value(SharedRecord {
            table: row.table,
            key: row.key.clone(),
            data: row.data.clone(),
        })
        .map_err(err)?,
    })
}

fn decode_record(record: &Record) -> Result<SharedRecord, String> {
    validate_record_envelope(record)?;
    let envelope: SharedRecord = serde_json::from_value(record.content.clone()).map_err(err)?;
    if record.id
        != row_record(&SharedRow {
            origin_node_id: Some(record.origin_node_id.clone()),
            table: envelope.table,
            key: envelope.key.clone(),
            data: envelope.data.clone(),
        })?
        .id
    {
        return Err("shared record id does not match its origin, table, and key".into());
    }
    Ok(envelope)
}

fn validate_record_envelope(record: &Record) -> Result<(), String> {
    if record.schema_version != 1 {
        return Err("unsupported fleet record schema".into());
    }
    if record.kind == RecordKind::Transcript {
        let history: HistoryRecordContent =
            serde_json::from_value(record.content.clone()).map_err(err)?;
        let raw_id = history
            .thread
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or("history transcript has no runtime thread ID")?;
        if history.owner_node_id != record.origin_node_id
            || !history.read_only
            || record.id != history_record_id(raw_id)
        {
            return Err("history transcript owner, read-only flag, or ID is invalid".into());
        }
        if !history.thread.is_object() {
            return Err("history transcript summary must be an object".into());
        }
        return Ok(());
    }
    if record.kind == RecordKind::Blob {
        let blob: HistoryBlobRecord =
            serde_json::from_value(record.content.clone()).map_err(err)?;
        if record.deleted
            || !is_sha256(&blob.sha256)
            || record.id != blob.sha256
            || blob.size_bytes > 50 * 1024 * 1024
        {
            return Err("history blob identity or size metadata is invalid".into());
        }
        return Ok(());
    }
    let envelope: SharedRecord = serde_json::from_value(record.content.clone()).map_err(err)?;
    if record.kind != record_kind(envelope.table) {
        return Err("fleet record kind does not match its shared table".into());
    }
    if envelope.key.trim().is_empty() || !envelope.data.is_object() {
        return Err("shared record has an invalid key or payload".into());
    }
    Ok(())
}

fn history_records(
    export: &HistoryExport,
    node_id: &str,
) -> Result<(Vec<Value>, Vec<Record>, Vec<BlobFile>), String> {
    if export.schema_version != 1 {
        return Err("unsupported runtime history export schema".into());
    }
    if !export.complete || !export.failures.is_empty() {
        return Err("runtime history export is incomplete".into());
    }
    if export.owner_origin != "local-runtime" {
        return Err("runtime history export has an unexpected owner origin".into());
    }
    let mut seen = BTreeSet::new();
    let mut manifest_threads = Vec::with_capacity(export.threads.len());
    let mut records = Vec::with_capacity(export.threads.len());
    let mut referenced_blobs = BTreeMap::<String, u64>::new();
    let mut submissions = BTreeMap::<String, (usize, u64)>::new();
    for value in &export.threads {
        // The wrapper is closed, even though app-server thread/turn payloads
        // remain runtime-defined values inside its fields.
        let thread: ExportedHistoryThread = serde_json::from_value(value.clone()).map_err(err)?;
        if thread.origin != "local-runtime" || !thread.thread.is_object() {
            return Err("runtime history thread has an invalid origin or summary".into());
        }
        let raw_id = thread
            .thread
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or("runtime history thread has no ID")?
            .to_owned();
        if !seen.insert(raw_id.clone()) {
            return Err(format!(
                "runtime history export repeats thread ID `{raw_id}`"
            ));
        }
        for attachment in &thread.attachments {
            if attachment.id.trim().is_empty()
                || attachment.submission_id.trim().is_empty()
                || !is_sha256(&attachment.sha256)
                || attachment.size_bytes > 50 * 1024 * 1024
            {
                return Err("history attachment metadata is invalid".into());
            }
            match referenced_blobs.insert(attachment.sha256.clone(), attachment.size_bytes) {
                Some(size) if size != attachment.size_bytes => {
                    return Err("the same history blob is referenced with different sizes".into());
                }
                _ => {}
            }
            let entry = submissions
                .entry(attachment.submission_id.clone())
                .or_default();
            entry.0 += 1;
            entry.1 = entry
                .1
                .checked_add(attachment.size_bytes)
                .ok_or("attachment submission size overflow")?;
            if entry.0 > 20 || entry.1 > 200 * 1024 * 1024 {
                return Err(
                    "history submission attachments exceed the 20-file or 200 MiB limit".into(),
                );
            }
        }
        let content = HistoryRecordContent {
            owner_node_id: node_id.to_owned(),
            read_only: true,
            thread: thread.thread,
            turns: thread.turns,
            attachments: thread.attachments,
        };
        let manifest_thread = serde_json::json!({
            "thread": content.thread.clone(),
            "turns": content.turns.clone(),
            "origin": "local-runtime",
            "attachments": content.attachments.clone(),
        });
        let record = Record {
            schema_version: 1,
            id: history_record_id(&raw_id),
            kind: RecordKind::Transcript,
            origin_node_id: node_id.to_owned(),
            revision: String::new(),
            base_revision: None,
            deleted: false,
            content: serde_json::to_value(&content).map_err(err)?,
        };
        validate_record_envelope(&record)?;
        manifest_threads.push(manifest_thread);
        records.push(record);
    }
    let mut blobs = BTreeMap::<String, &HistoryBlob>::new();
    for blob in &export.blobs {
        if !is_sha256(&blob.sha256) || blob.size_bytes > 50 * 1024 * 1024 {
            return Err("history blob metadata is invalid".into());
        }
        if blobs.insert(blob.sha256.clone(), blob).is_some() {
            return Err("runtime history export repeats a content-addressed blob".into());
        }
    }
    if referenced_blobs.len() != blobs.len()
        || referenced_blobs
            .iter()
            .any(|(sha, size)| blobs.get(sha).is_none_or(|blob| blob.size_bytes != *size))
    {
        return Err("history attachment manifests and exported blobs do not match exactly".into());
    }
    let blob_files = blobs
        .into_iter()
        .map(|(sha256, blob)| {
            Ok(BlobFile {
                sha256,
                size_bytes: blob.size_bytes,
                source_path: blob.verified_source_path().map_err(err)?.to_path_buf(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((manifest_threads, records, blob_files))
}

fn blob_record(node_id: &str, sha256: String, size_bytes: u64) -> Result<Record, String> {
    if !is_sha256(&sha256) || size_bytes > 50 * 1024 * 1024 {
        return Err("invalid history blob identity or size".into());
    }
    Ok(Record {
        schema_version: 1,
        id: sha256.clone(),
        kind: RecordKind::Blob,
        origin_node_id: node_id.to_owned(),
        revision: String::new(),
        base_revision: None,
        deleted: false,
        content: serde_json::json!({"sha256": sha256, "sizeBytes": size_bytes}),
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn history_record_id(runtime_thread_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(runtime_thread_id.as_bytes());
    format!("transcript-{}", hex(&digest.finalize()))
}

fn history_view(record: &Record) -> Result<Value, String> {
    validate_record_envelope(record)?;
    if record.kind != RecordKind::Transcript || record.deleted {
        return Err("record is not a live historical transcript".into());
    }
    let history: HistoryRecordContent =
        serde_json::from_value(record.content.clone()).map_err(err)?;
    Ok(serde_json::json!({
        "ownerNodeId": history.owner_node_id,
        "readOnly": true,
        "thread": history.thread,
        "turns": history.turns,
        "attachments": history.attachments,
    }))
}

fn validate_record_set(records: &[Record]) -> Result<(), String> {
    let snapshot = records_snapshot(records)?;
    snapshot.validate(true)
}

fn record_kind(table: SharedTable) -> RecordKind {
    match table {
        SharedTable::AgentProfiles
        | SharedTable::ProfileRevisions
        | SharedTable::ProfileDrafts
        | SharedTable::ProfileReviewReceipts => RecordKind::Agent,
        SharedTable::MemorySources
        | SharedTable::MemorySourceRevisions
        | SharedTable::MemoryRecords
        | SharedTable::MemoryRecordSources => RecordKind::Memory,
        SharedTable::LearnedSkills | SharedTable::SkillSources => RecordKind::Skill,
        SharedTable::Projects => RecordKind::Project,
        SharedTable::TaskDefinitions => RecordKind::Task,
        SharedTable::WorkflowDefinitions => RecordKind::Workflow,
    }
}

fn in_domain(table: SharedTable, domain: RecordDomain) -> bool {
    match domain {
        RecordDomain::Agent => table != SharedTable::Projects,
        RecordDomain::Projects => table == SharedTable::Projects,
        RecordDomain::AllShared => true,
    }
}

fn in_domain_record(record: &Record, domain: RecordDomain) -> bool {
    decode_record(record).is_ok_and(|envelope| in_domain(envelope.table, domain))
}

fn row_identity(row: &SharedRow) -> (String, u8, String) {
    (
        row.origin_node_id.clone().unwrap_or_default(),
        row.table as u8,
        row.key.clone(),
    )
}

fn install_validator(repo: &mut FleetRepo) {
    repo.set_domain_validator(|records| validate_record_set(records));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn conflict_table(conflict: &Conflict) -> Option<SharedTable> {
    let record = conflict
        .current
        .as_ref()
        .or(conflict.incoming.as_ref())
        .or(conflict.base.as_ref())?;
    decode_record(record).ok().map(|record| record.table)
}

pub(crate) fn conflict_revision(conflict: &Conflict) -> String {
    hex(&Sha256::digest(
        serde_json::to_vec(&serde_json::json!([
            conflict.entity_id,
            conflict.base,
            conflict.current,
            conflict.incoming,
        ]))
        .expect("record variants are serializable"),
    ))
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn primary_activation_retry_preserves_extra_committed_rows() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("primary");
        let first = SharedRow {
            origin_node_id: None,
            table: SharedTable::Projects,
            key: "alpha".into(),
            data: serde_json::json!({"id":"alpha","name":"Alpha","revision":1,"model":null,"effort":null,"permission":null,"hidden":false}),
        };
        let second = SharedRow {
            origin_node_id: Some("primary".into()),
            table: SharedTable::Projects,
            key: "beta".into(),
            data: serde_json::json!({"id":"beta","name":"Beta","revision":1,"model":null,"effort":null,"permission":null,"hidden":false}),
        };
        let store = AgentStore::open(data.join("engine/agents")).await.unwrap();
        let catalog = Catalog::init_primary_with_projects(
            &data,
            "primary",
            store,
            std::slice::from_ref(&first),
        )
        .await
        .unwrap();
        let mut rows = vec![first.clone(), second.clone()];
        for row in &mut rows {
            if row.origin_node_id.is_none() {
                row.origin_node_id = Some("primary".into());
            }
        }
        catalog.publish_projects(&rows).unwrap();
        drop(catalog);

        let store = AgentStore::open(data.join("engine/agents")).await.unwrap();
        let resumed = Catalog::init_primary_with_projects(
            &data,
            "primary",
            store,
            std::slice::from_ref(&first),
        )
        .await
        .unwrap();
        let records = resumed.records().unwrap();
        let snapshot = records_snapshot(&records).unwrap();
        assert_eq!(snapshot.rows.len(), 2);
        assert!(snapshot.rows.iter().any(|row| row.key == "alpha"));
        assert!(snapshot.rows.iter().any(|row| row.key == "beta"));
    }

    #[tokio::test]
    async fn import_joins_project_metadata_before_qualifying_project_memory() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("primary");
        let store = AgentStore::open(data.join("engine/agents")).await.unwrap();
        let access = store
            .memory_access_for_user(None, Some("local-project"))
            .await
            .unwrap();
        store
            .record_source(&access, "project-source", 1, "Project evidence")
            .await
            .unwrap();
        store
            .publish_memory(
                &access,
                brr_agent_core::MemoryScope::Project,
                "fact",
                "Portable project fact",
                &["project-source@1".into()],
            )
            .await
            .unwrap();
        let project = SharedRow {
            origin_node_id: None,
            table: SharedTable::Projects,
            key: "local-project".into(),
            data: serde_json::json!({"id":"local-project","name":"Research","revision":1,"model":null,"effort":null,"permission":null,"hidden":false}),
        };
        let primary = Catalog::init_primary_with_projects(&data, "primary", store, &[project])
            .await
            .unwrap();
        let node_data = directory.path().join("node");
        let node_store = AgentStore::open(node_data.join("engine/agents"))
            .await
            .unwrap();
        let _node = Catalog::init_node(
            &node_data,
            "worker",
            &primary.origin().unwrap(),
            None,
            node_store.clone(),
        )
        .await
        .unwrap();
        let project_id =
            brr_agent_core::shared_entity_id("primary", SharedTable::Projects, "local-project");
        let access = node_store
            .memory_access_for_user(None, Some(&project_id))
            .await
            .unwrap();
        let memories = node_store.search_memory(&access, "portable").await.unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].scope_id, project_id);
        assert!(node_store
            .shared_snapshot()
            .await
            .unwrap()
            .rows
            .iter()
            .any(|row| row.table == SharedTable::Projects
                && row.origin_node_id.as_deref() == Some("primary")));
    }
    fn candidate(name: &str) -> ProfileCandidate {
        ProfileCandidate {
            name: name.into(),
            description: "profile used by the catalog integration test".into(),
            instructions: "use committed shared data".into(),
            soul: "steady".into(),
            model: "gpt-5-codex".into(),
            effort: "high".into(),
        }
    }

    async fn publish_profile(
        store: &AgentStore,
        key: &str,
        profile: Option<String>,
        value: ProfileCandidate,
    ) -> brr_agent_core::AgentProfile {
        let draft = store.propose_profile(key, profile, value).await.unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, &format!("approve-{key}"))
            .await
            .unwrap();
        store
            .apply_profile(&draft.id, &draft.hash, &format!("apply-{key}"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn competing_reviewed_profile_edits_require_a_fresh_exact_review_and_preserve_bindings() {
        use brr_agent_core::{shared_entity_id, SessionBinding};
        let directory = tempfile::tempdir().unwrap();
        let primary_data = directory.path().join("primary");
        let primary_store = AgentStore::open(primary_data.join("engine/agents"))
            .await
            .unwrap();
        let primary = Catalog::init_primary(&primary_data, "primary", primary_store.clone())
            .await
            .unwrap();
        let original =
            publish_profile(&primary_store, "original", None, candidate("Researcher")).await;
        primary.synchronize().await.unwrap();

        let worker_data = directory.path().join("worker");
        let worker_store = AgentStore::open(worker_data.join("engine/agents"))
            .await
            .unwrap();
        let worker = Catalog::init_node(
            &worker_data,
            "worker",
            &primary.origin().unwrap(),
            None,
            worker_store.clone(),
        )
        .await
        .unwrap();
        let worker_id = shared_entity_id("primary", SharedTable::AgentProfiles, &original.id);
        let binding = SessionBinding {
            thread_id: "existing-thread".into(),
            agent_id: Some(worker_id.clone()),
            kind: "agent".into(),
            project_id: None,
            cwd: Some("/worker/original".into()),
            task_run_id: None,
            learning_eligible: false,
        };
        worker_store.bind_session(binding.clone()).await.unwrap();
        let mut current = candidate("Researcher");
        current.instructions = "Verify the author".into();
        let mut incoming = candidate("Researcher");
        incoming.instructions = "Verify the date".into();
        publish_profile(
            &worker_store,
            "worker-edit",
            Some(worker_id.clone()),
            incoming.clone(),
        )
        .await;
        publish_profile(
            &primary_store,
            "primary-edit",
            Some(original.id.clone()),
            current.clone(),
        )
        .await;
        primary.synchronize().await.unwrap();
        worker.synchronize().await.unwrap();
        assert!(matches!(
            primary.synchronize().await.unwrap().gateway,
            Some(SyncOutcome::Conflicted { .. })
        ));
        let conflicts = primary.conflicts().unwrap();
        let conflict = conflicts
            .iter()
            .find(|conflict| {
                conflict.current.as_ref().is_some_and(|record| {
                    decode_record(record).unwrap().table == SharedTable::AgentProfiles
                })
            })
            .unwrap();
        let revision = conflict_revision(conflict);
        assert_eq!(
            primary_store
                .read_profile(&original.id)
                .await
                .unwrap()
                .instructions,
            current.instructions
        );
        assert_eq!(
            worker_store
                .read_profile(&worker_id)
                .await
                .unwrap()
                .instructions,
            incoming.instructions
        );

        let mut merged = current;
        merged.instructions = "Verify both the author and the date".into();
        let review = primary
            .preview_profile_resolution(&conflict.id, &revision, merged.clone())
            .unwrap();
        let mut unreviewed = merged.clone();
        unreviewed.instructions.push_str(" and publish immediately");
        assert!(primary
            .approve_profile_resolution(&conflict.id, &revision, unreviewed, &review.review_hash)
            .await
            .is_err());
        assert_eq!(primary.conflicts().unwrap(), conflicts);
        primary
            .approve_profile_resolution(
                &conflict.id,
                &revision,
                merged.clone(),
                &review.review_hash,
            )
            .await
            .unwrap();
        assert!(primary.conflicts().unwrap().is_empty());
        worker.synchronize().await.unwrap();
        let profile = worker_store.read_profile(&worker_id).await.unwrap();
        assert_eq!(profile.revision, 3);
        assert_eq!(profile.instructions, merged.instructions);
        assert_eq!(
            worker_store
                .session_binding("existing-thread")
                .await
                .unwrap(),
            binding
        );
        assert!(primary_store
            .session_binding("existing-thread")
            .await
            .is_err());
        assert_eq!(
            primary_store
                .read_profile(&original.id)
                .await
                .unwrap()
                .instructions,
            merged.instructions
        );
    }

    #[tokio::test]
    async fn committed_git_candidate_recovers_after_sql_transaction_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("data");
        let store = AgentStore::open(data_dir.join("engine/agents"))
            .await
            .unwrap();
        let catalog = Catalog::init_primary(&data_dir, "primary", store.clone())
            .await
            .unwrap();
        let draft = store
            .propose_profile("interview-thread", None, candidate("Reviewed agent"))
            .await
            .unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, "approval-op")
            .await
            .unwrap();

        // Model a process crash after the authority commit but before SQLite
        // can commit: the hook publishes to Git, then returns an error so the
        // enclosing SQL transaction rolls back.
        let repos = Arc::clone(&catalog.repos);
        let node_id = catalog.node_id.clone();
        store.set_shared_commit_hook(move |snapshot| {
            let candidate = qualify_snapshot(snapshot, &node_id)?;
            candidate.validate(false)?;
            commit_snapshot_locked(&repos, &candidate, RecordDomain::Agent)?;
            Err("simulated stop after Git commit".into())
        });
        assert!(store
            .apply_profile(&draft.id, &draft.hash, "apply-op")
            .await
            .unwrap_err()
            .contains("simulated stop"));
        assert!(store.list_profiles().await.unwrap().is_empty());

        let reopened_store = AgentStore::open(data_dir.join("engine/agents"))
            .await
            .unwrap();
        let recovered = Catalog::open_primary(&data_dir, "primary", reopened_store.clone())
            .await
            .unwrap();
        assert_eq!(recovered.status().unwrap().role, "primary");
        let profiles = reopened_store.list_profiles().await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Reviewed agent");
        assert_eq!(
            recovered.status().unwrap().revision,
            recovered.recover().await.unwrap()
        );
    }

    #[tokio::test]
    async fn node_shared_profile_reaches_primary_through_real_local_git() {
        let directory = tempfile::tempdir().unwrap();
        let primary_data = directory.path().join("primary-data");
        let primary_store = AgentStore::open(primary_data.join("engine/agents"))
            .await
            .unwrap();
        let primary = Catalog::init_primary(&primary_data, "primary", primary_store.clone())
            .await
            .unwrap();

        let node_data = directory.path().join("node-data");
        let node_store = AgentStore::open(node_data.join("engine/agents"))
            .await
            .unwrap();
        let node = Catalog::init_node(
            &node_data,
            "laptop",
            &primary.origin().unwrap(),
            None,
            node_store.clone(),
        )
        .await
        .unwrap();
        let draft = node_store
            .propose_profile("laptop-interview", None, candidate("Laptop agent"))
            .await
            .unwrap();
        node_store
            .approve_profile(&draft.id, &draft.hash, "laptop-approval")
            .await
            .unwrap();
        node_store
            .apply_profile(&draft.id, &draft.hash, "laptop-apply")
            .await
            .unwrap();

        node.synchronize().await.unwrap();
        let sync = primary.synchronize().await.unwrap();
        assert!(matches!(sync.gateway, Some(SyncOutcome::Merged { .. })));
        assert_eq!(sync.revision, primary.status().unwrap().revision);
        let profiles = primary_store.list_profiles().await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Laptop agent");
    }

    #[tokio::test]
    async fn removing_memory_evidence_commits_a_tombstone_and_recovery_keeps_only_live_links() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("data");
        let store = AgentStore::open(data_dir.join("engine/agents"))
            .await
            .unwrap();
        let catalog = Catalog::init_primary(&data_dir, "primary", store.clone())
            .await
            .unwrap();
        catalog
            .publish_projects(&[SharedRow {
                origin_node_id: None,
                table: SharedTable::Projects,
                key: "project-one".into(),
                data: serde_json::json!({
                    "id":"project-one",
                    "name":"Project one",
                    "revision":1,
                    "model":null,
                    "effort":null,
                    "permission":"workspace-write",
                    "hidden":false
                }),
            }])
            .unwrap();
        let access = store.memory_access_for_user(None, None).await.unwrap();
        let first = store
            .record_source(&access, "source-first", 1, "first evidence")
            .await
            .unwrap();
        let second = store
            .record_source(&access, "source-second", 1, "second evidence")
            .await
            .unwrap();
        let memory = store
            .publish_memory(
                &access,
                brr_agent_core::MemoryScope::User,
                "decision",
                "the reviewed decision",
                &["source-first@1".into(), "source-second@1".into()],
            )
            .await
            .unwrap();
        store
            .revise_memory(
                &access,
                &memory.id,
                memory.revision,
                "the decision with one current source",
                &["source-first@1".into()],
            )
            .await
            .unwrap();

        let records = catalog.repos.lock().unwrap().writer.list_records().unwrap();
        let removed_link = records.iter().find(|record| {
            record.deleted
                && decode_record(record).is_ok_and(|envelope| {
                    envelope.table == SharedTable::MemoryRecordSources
                        && envelope.data["sourceId"] == second.source_id
                })
        });
        assert!(
            removed_link.is_some(),
            "removed evidence link must be tombstoned in Git"
        );
        assert!(records.iter().any(|record| {
            !record.deleted
                && decode_record(record).is_ok_and(|envelope| {
                    envelope.table == SharedTable::MemoryRecordSources
                        && envelope.data["sourceId"] == first.source_id
                })
        }));
        assert!(records.iter().any(|record| {
            !record.deleted
                && decode_record(record).is_ok_and(|envelope| {
                    envelope.table == SharedTable::Projects && envelope.key == "project-one"
                })
        }));

        catalog.recover().await.unwrap();
        let memories = store.list_memory(&access).await.unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].source_ids, vec!["source-first"]);
    }

    fn history_export(threads: Vec<Value>) -> HistoryExport {
        HistoryExport {
            schema_version: 1,
            owner_origin: "local-runtime".into(),
            complete: true,
            threads,
            blobs: Vec::new(),
            failures: Vec::new(),
        }
    }

    #[tokio::test]
    async fn saved_history_attachment_and_blob_are_committed_together_as_git_records() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("primary");
        let store = AgentStore::open(data.join("engine/agents")).await.unwrap();
        let primary = Catalog::init_primary(&data, "primary", store)
            .await
            .unwrap();
        let bytes = b"hello";
        let sha256 = hex(&Sha256::digest(bytes));
        let source = std::fs::canonicalize(directory.path())
            .unwrap()
            .join("accepted-attachment.bin");
        std::fs::write(&source, bytes).unwrap();
        let attachment = HistoryAttachment {
            id: "attachment-1".into(),
            name: "hello.txt".into(),
            mime: "text/plain".into(),
            kind: "text".into(),
            size_bytes: bytes.len() as u64,
            sha256: sha256.clone(),
            submission_id: "submission-1".into(),
            turn_id: Some("turn-1".into()),
            item_id: Some("item-1".into()),
        };
        let mut export = history_export(vec![serde_json::json!({
            "thread":{"id":"history-with-file","title":"Attached"},
            "turns":[{"id":"turn-1","items":[]}],
            "origin":"local-runtime",
            "attachments":[attachment],
        })]);
        export.blobs.push(
            HistoryBlob::from_verified_path(&source, sha256.clone(), bytes.len() as u64).unwrap(),
        );

        let published = primary.publish_history(&export).unwrap();
        assert_eq!(published.count, 1);
        let records = primary.records().unwrap();
        let manifest = records
            .iter()
            .find(|record| {
                serde_json::to_value(&record.kind).unwrap() == "transcript"
                    && record.content["thread"]["id"] == "history-with-file"
            })
            .unwrap();
        let blob = records
            .iter()
            .find(|record| {
                serde_json::to_value(&record.kind).unwrap() == "blob" && record.id == sha256
            })
            .unwrap();
        assert_eq!(manifest.origin_node_id, blob.origin_node_id);
        assert_eq!(manifest.content["attachments"][0]["sha256"], blob.id);
        assert_eq!(blob.content["sizeBytes"], bytes.len() as u64);
        assert!(blob.content.get("dataBase64").is_none());
        let view = primary
            .historical_thread("primary", "history-with-file")
            .unwrap()
            .unwrap();
        assert_eq!(view["attachments"][0]["id"], "attachment-1");
        assert!(view["attachments"][0].get("dataBase64").is_none());
        let (metadata, saved_blob) = primary
            .historical_attachment("primary", "history-with-file", &sha256)
            .unwrap()
            .unwrap();
        assert_eq!(metadata.id, "attachment-1");
        assert_eq!(saved_blob.read_verified_bytes().unwrap(), bytes);
        assert!(primary
            .historical_attachment("other-node", "history-with-file", &sha256)
            .unwrap()
            .is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let raw_path = primary
                .repos
                .lock()
                .unwrap()
                .writer
                .repo_path()
                .join("blobs")
                .join(&sha256);
            let outside = directory.path().join("outside-blob.bin");
            std::fs::write(&outside, bytes).unwrap();
            std::fs::remove_file(&raw_path).unwrap();
            symlink(&outside, &raw_path).unwrap();
            assert!(primary
                .historical_attachment("primary", "history-with-file", &sha256)
                .is_err());
        }
    }

    #[tokio::test]
    async fn history_import_is_committed_readonly_and_survives_shared_agent_edits_on_real_git() {
        let directory = tempfile::tempdir().unwrap();
        let primary_data = directory.path().join("primary");
        let primary_store = AgentStore::open(primary_data.join("engine/agents"))
            .await
            .unwrap();
        let primary = Catalog::init_primary(&primary_data, "primary", primary_store.clone())
            .await
            .unwrap();
        let pool = primary_store.pool();
        sqlx::query("INSERT INTO tasks(id,agent_id,title,instructions,acceptance_criteria,dependencies,status,created_at,updated_at) VALUES('queued-preserved','agent','Queued','Do not run','[]','[]','queued',1,1)")
            .execute(pool).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS agent_learning_jobs(id TEXT PRIMARY KEY,source_thread_id TEXT NOT NULL,source_turn_id TEXT NOT NULL,source_refs TEXT NOT NULL,agent_id TEXT,project_id TEXT,cwd TEXT,state TEXT NOT NULL,learning_thread_id TEXT,reason TEXT,turn_id TEXT,completion_turn_id TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,continuation_count INTEGER NOT NULL DEFAULT 0,UNIQUE(source_thread_id,source_turn_id))")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO agent_learning_jobs(id,source_thread_id,source_turn_id,source_refs,state,created_at,updated_at) VALUES('learning-preserved','thread','turn','[]','queued',1,1)")
            .execute(pool).await.unwrap();
        let before: (String, String) = sqlx::query_as("SELECT (SELECT status FROM tasks WHERE id='queued-preserved'),(SELECT state FROM agent_learning_jobs WHERE id='learning-preserved')")
            .fetch_one(pool).await.unwrap();

        let export = history_export(vec![
            serde_json::json!({"thread":{"id":"history-a","title":"First"},"turns":[{"id":"turn-a","items":[{"text":"kept"}]}],"origin":"local-runtime"}),
            serde_json::json!({"thread":{"id":"history-b","title":"Second"},"turns":[],"origin":"local-runtime"}),
        ]);
        let publication = primary.publish_history(&export).unwrap();
        assert_eq!(publication.count, 2);
        assert_eq!(publication.sha256.len(), 64);
        assert_eq!(publication.revision, primary.status().unwrap().revision);
        let view = primary
            .historical_thread("primary", "history-a")
            .unwrap()
            .unwrap();
        assert_eq!(view["ownerNodeId"], "primary");
        assert_eq!(view["readOnly"], true);
        assert_eq!(view["thread"]["id"], "history-a");
        assert_eq!(view["turns"][0]["items"][0]["text"], "kept");
        assert_eq!(primary.historical_threads().unwrap().len(), 2);
        let (_, shared) = primary.committed_shared().unwrap();
        assert!(shared
            .rows
            .iter()
            .all(|row| row.table != SharedTable::TaskDefinitions));
        assert!(shared
            .rows
            .iter()
            .all(|row| row.key != "history-a" && row.key != "history-b"));
        primary.synchronize().await.unwrap();

        let node_data = directory.path().join("node");
        let node_store = AgentStore::open(node_data.join("engine/agents"))
            .await
            .unwrap();
        let node = Catalog::init_node(
            &node_data,
            "worker",
            &primary.origin().unwrap(),
            None,
            node_store.clone(),
        )
        .await
        .unwrap();
        let remote_view = node
            .historical_thread("primary", "history-a")
            .unwrap()
            .unwrap();
        assert_eq!(remote_view["readOnly"], true);
        assert_eq!(remote_view["ownerNodeId"], "primary");

        let draft = node_store
            .propose_profile("node-interview", None, candidate("Node edit"))
            .await
            .unwrap();
        node_store
            .approve_profile(&draft.id, &draft.hash, "node-review")
            .await
            .unwrap();
        node_store
            .apply_profile(&draft.id, &draft.hash, "node-apply")
            .await
            .unwrap();
        assert_eq!(
            node.historical_threads().unwrap().len(),
            2,
            "a shared AgentStore write must preserve transcript records"
        );
        node.synchronize().await.unwrap();
        primary.synchronize().await.unwrap();
        assert_eq!(primary.historical_threads().unwrap().len(), 2);
        let after: (String, String) = sqlx::query_as("SELECT (SELECT status FROM tasks WHERE id='queued-preserved'),(SELECT state FROM agent_learning_jobs WHERE id='learning-preserved')")
            .fetch_one(pool).await.unwrap();
        assert_eq!(
            after, before,
            "history publication must not enqueue, replay, or settle task/learning work"
        );
    }

    #[tokio::test]
    async fn history_publication_rejects_incomplete_duplicate_and_open_envelopes_without_git_changes(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("data");
        let store = AgentStore::open(data_dir.join("engine/agents"))
            .await
            .unwrap();
        let catalog = Catalog::init_primary(&data_dir, "primary", store)
            .await
            .unwrap();
        let before = catalog.status().unwrap().revision;

        let mut incomplete = history_export(Vec::new());
        incomplete.complete = false;
        assert!(catalog
            .publish_history(&incomplete)
            .unwrap_err()
            .contains("incomplete"));

        let repeated =
            serde_json::json!({"thread":{"id":"same"},"turns":[],"origin":"local-runtime"});
        let duplicate = history_export(vec![repeated.clone(), repeated]);
        assert!(catalog
            .publish_history(&duplicate)
            .unwrap_err()
            .contains("repeats thread ID"));

        let open = history_export(vec![
            serde_json::json!({"thread":{"id":"open"},"turns":[],"origin":"local-runtime","unexpected":"must reject"}),
        ]);
        assert!(catalog.publish_history(&open).is_err());
        assert_eq!(catalog.status().unwrap().revision, before);
        assert!(catalog.records().unwrap().is_empty());
    }

    #[tokio::test]
    async fn history_deletion_is_owner_scoped_idempotent_and_propagates_as_git_tombstone() {
        let directory = tempfile::tempdir().unwrap();
        let primary_data = directory.path().join("primary");
        let primary_store = AgentStore::open(primary_data.join("engine/agents"))
            .await
            .unwrap();
        let primary = Catalog::init_primary(&primary_data, "primary", primary_store)
            .await
            .unwrap();
        let export = history_export(vec![serde_json::json!({
            "thread":{"id":"owned-thread","title":"Historical"},
            "turns":[],
            "origin":"local-runtime"
        })]);
        primary.publish_history(&export).unwrap();
        primary.synchronize().await.unwrap();

        let node_data = directory.path().join("node");
        let node_store = AgentStore::open(node_data.join("engine/agents"))
            .await
            .unwrap();
        let node = Catalog::init_node(
            &node_data,
            "node",
            &primary.origin().unwrap(),
            None,
            node_store,
        )
        .await
        .unwrap();
        assert!(node
            .historical_thread("primary", "owned-thread")
            .unwrap()
            .is_some());
        assert_eq!(node.tombstone_history("owned-thread").unwrap(), None);
        assert!(node
            .historical_thread("primary", "owned-thread")
            .unwrap()
            .is_some());

        primary.tombstone_history("owned-thread").unwrap().unwrap();
        assert!(primary
            .historical_thread("primary", "owned-thread")
            .unwrap()
            .is_none());
        assert_eq!(primary.tombstone_history("owned-thread").unwrap(), None);
        assert!(primary.records().unwrap().iter().any(|record| {
            record.kind == RecordKind::Transcript
                && record.origin_node_id == "primary"
                && record.deleted
        }));

        primary.synchronize().await.unwrap();
        node.synchronize().await.unwrap();
        assert!(node
            .historical_thread("primary", "owned-thread")
            .unwrap()
            .is_none());
    }
}
