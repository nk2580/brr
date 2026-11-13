//! Read-only inventory and portable store snapshots for fleet migration.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, Sqlite, SqlitePool, Transaction,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

// These filenames are pinned by the owned runtime's state/sqlite.rs. They
// are backed up locally; only the typed history export becomes shared Git.
pub const OWNED_RUNTIME_DATABASES: &[(&str, &str)] = &[
    ("runtime-state", "state_5.sqlite"),
    ("runtime-thread-history", "thread_history_1.sqlite"),
    ("runtime-goals", "goals_1.sqlite"),
    ("runtime-queue", "queue_1.sqlite"),
    ("runtime-memories", "memories_1.sqlite"),
    ("runtime-memories-v2", "memories_v2_1.sqlite"),
    ("runtime-logs", "logs_2.sqlite"),
];

/// Live ChatStore location under the owned runtime data directory.
pub const CHAT_STORE_RELATIVE_PATH: &str = "engine/agents/chat";
const HISTORY_ATTACHMENT_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const HISTORY_ATTACHMENT_MAX_OWNER_BYTES: u64 = 200 * 1024 * 1024;
const HISTORY_ATTACHMENT_MAX_OWNER_FILES: usize = 20;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSnapshot {
    pub schema_version: u32,
    pub complete: bool,
    pub shared: BTreeMap<String, Vec<Value>>,
    pub shared_inventory: BTreeMap<String, TableInventory>,
    pub owner_local: BTreeMap<String, TableInventory>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInventory {
    pub classification: String,
    pub record_count: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub schema_version: u32,
    pub complete: bool,
    pub tables: BTreeMap<String, TableInventory>,
    pub runtime_files: Vec<RuntimeFileInventory>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFileInventory {
    pub relative_path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub classification: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryExport {
    pub schema_version: u32,
    pub owner_origin: String,
    pub complete: bool,
    pub threads: Vec<Value>,
    #[serde(default)]
    pub blobs: Vec<HistoryBlob>,
    pub failures: Vec<HistoryFailure>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAttachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub kind: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub submission_id: String,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBlob {
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(skip)]
    source_path: Option<PathBuf>,
}

impl HistoryBlob {
    fn from_local_path(path: &Path, size_bytes: u64) -> Result<Self, String> {
        let (path, bytes, sha256) = read_checked_blob(path, size_bytes, None)?;
        drop(bytes);
        Ok(Self {
            sha256,
            size_bytes,
            source_path: Some(path),
        })
    }

    /// Construct a local source for content already addressed by its SHA256.
    /// The file is fully checked before the source is accepted.
    pub fn from_verified_path(
        path: impl AsRef<Path>,
        sha256: impl Into<String>,
        size_bytes: u64,
    ) -> Result<Self, String> {
        let sha256 = sha256.into();
        if !is_sha256(&sha256) {
            return Err("Attachment SHA256 must be 64 lowercase hexadecimal characters".into());
        }
        let (path, bytes, actual_sha256) =
            read_checked_blob(path.as_ref(), size_bytes, Some(&sha256))?;
        drop(bytes);
        if actual_sha256 != sha256 {
            return Err("Attachment file does not match its declared SHA256".into());
        }
        Ok(Self {
            sha256,
            size_bytes,
            source_path: Some(path),
        })
    }

    /// A verified app-owned source path for streaming into the Git object store.
    /// The consumer must still re-open without following links and re-hash while copying.
    pub fn verified_source_path(&self) -> Result<&Path, String> {
        self.source_path
            .as_deref()
            .ok_or_else(|| "Attachment blob source is unavailable".into())
    }

    /// Read one bounded blob and re-check its exact size and digest.
    pub fn read_verified_bytes(&self) -> Result<Vec<u8>, String> {
        let (path, bytes, sha256) = read_checked_blob(
            self.verified_source_path()?,
            self.size_bytes,
            Some(&self.sha256),
        )?;
        let _ = path;
        if sha256 != self.sha256 {
            return Err("Attachment file does not match its declared SHA256".into());
        }
        Ok(bytes)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFailure {
    pub thread_id: Option<String>,
    pub message: String,
}

impl crate::Host {
    pub async fn store_snapshot(&self) -> Result<StoreSnapshot, String> {
        let mut shared = BTreeMap::new();
        let mut shared_inventory = BTreeMap::new();
        let mut owner_local = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut complete = true;
        if legacy_chat_store_has_data(&self.data_dir)? {
            complete = false;
            warnings.push(
                "legacy chat store at engine/chat contains data; reconcile it before fleet activation".into(),
            );
        }

        let mut agent_tx = self.stores.0.pool().begin().await.map_err(db)?;
        inspect_tables(
            &mut agent_tx,
            "agent",
            &agent_specs(),
            &mut warnings,
            &mut complete,
        )
        .await?;
        for spec in agent_specs() {
            if !table_exists(&mut agent_tx, spec.name).await? {
                if spec.classification == "shared" {
                    shared.insert(spec.name.into(), Vec::new());
                }
                owner_local.insert(
                    format!("agent/{}", spec.name),
                    TableInventory {
                        classification: spec.classification.into(),
                        record_count: Some(0),
                        sha256: None,
                    },
                );
            } else if spec.classification == "shared" {
                let rows = read_table(&mut agent_tx, spec).await?;
                shared_inventory.insert(
                    format!("agent/{}", spec.name),
                    TableInventory {
                        classification: "shared".into(),
                        record_count: Some(rows.len() as u64),
                        sha256: Some(hash_rows(&rows)),
                    },
                );
                shared.insert(spec.name.into(), rows);
            } else {
                let count = count_table(&mut agent_tx, spec.name).await?;
                owner_local.insert(
                    format!("agent/{}", spec.name),
                    TableInventory {
                        classification: spec.classification.into(),
                        record_count: Some(count),
                        sha256: None,
                    },
                );
            }
        }
        agent_tx.commit().await.map_err(db)?;

        let mut project_tx = self.stores.1.pool().begin().await.map_err(db)?;
        inspect_tables(
            &mut project_tx,
            "projects",
            &project_specs(),
            &mut warnings,
            &mut complete,
        )
        .await?;
        for spec in project_specs() {
            if !table_exists(&mut project_tx, spec.name).await? {
                if spec.classification == "shared" {
                    shared.insert(spec.name.into(), Vec::new());
                }
                owner_local.insert(
                    format!("projects/{}", spec.name),
                    TableInventory {
                        classification: spec.classification.into(),
                        record_count: Some(0),
                        sha256: None,
                    },
                );
            } else if spec.classification == "shared" {
                let rows = read_table(&mut project_tx, spec).await?;
                shared_inventory.insert(
                    format!("projects/{}", spec.name),
                    TableInventory {
                        classification: "shared".into(),
                        record_count: Some(rows.len() as u64),
                        sha256: Some(hash_rows(&rows)),
                    },
                );
                shared.insert(spec.name.into(), rows);
            } else {
                let count = count_table(&mut project_tx, spec.name).await?;
                owner_local.insert(
                    format!("projects/{}", spec.name),
                    TableInventory {
                        classification: spec.classification.into(),
                        record_count: Some(count),
                        sha256: None,
                    },
                );
            }
        }
        project_tx.commit().await.map_err(db)?;

        let chat_db = self
            .data_dir
            .join(CHAT_STORE_RELATIVE_PATH)
            .join("chat.sqlite3");
        if chat_db.exists() {
            let pool = readonly_pool(&chat_db).await?;
            let mut tx = pool.begin().await.map_err(db)?;
            inspect_tables(&mut tx, "chat", &chat_specs(), &mut warnings, &mut complete).await?;
            for spec in chat_specs() {
                if !table_exists(&mut tx, spec.name).await? {
                    owner_local.insert(
                        format!("chat/{}", spec.name),
                        TableInventory {
                            classification: spec.classification.into(),
                            record_count: Some(0),
                            sha256: None,
                        },
                    );
                    if spec.classification == "attachment-manifest" {
                        shared.insert(spec.name.into(), Vec::new());
                    }
                    continue;
                }
                let count = count_table(&mut tx, spec.name).await?;
                owner_local.insert(
                    format!("chat/{}", spec.name),
                    TableInventory {
                        classification: spec.classification.into(),
                        record_count: Some(count),
                        sha256: None,
                    },
                );
                if spec.classification == "attachment-manifest" {
                    let mut rows = read_table(&mut tx, spec).await?;
                    if spec.name == "chat_blobs" {
                        for blob in &mut rows {
                            if let Some(id) = blob.get("id").and_then(Value::as_str) {
                                match content_hash_for_blob(&self.data_dir, id) {
                                    Ok(Some(hash)) => blob["sha256"] = json!(hash),
                                    Ok(None) => {
                                        complete = false;
                                        warnings.push(format!("attachment blob `{id}` is missing or not a regular managed blob"));
                                    }
                                    Err(error) => {
                                        complete = false;
                                        warnings.push(format!(
                                            "could not hash managed attachment blob `{id}`: {error}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    shared_inventory.insert(
                        format!("chat/{}", spec.name),
                        TableInventory {
                            classification: spec.classification.into(),
                            record_count: Some(rows.len() as u64),
                            sha256: Some(hash_rows(&rows)),
                        },
                    );
                    shared.insert(spec.name.into(), rows);
                }
            }
            tx.commit().await.map_err(db)?;
            pool.close().await;
        } else {
            for spec in chat_specs() {
                owner_local.insert(
                    format!("chat/{}", spec.name),
                    TableInventory {
                        classification: spec.classification.into(),
                        record_count: Some(0),
                        sha256: None,
                    },
                );
                if spec.classification == "attachment-manifest" {
                    shared.insert(spec.name.into(), Vec::new());
                    shared_inventory.insert(
                        format!("chat/{}", spec.name),
                        TableInventory {
                            classification: spec.classification.into(),
                            record_count: Some(0),
                            sha256: Some(hash_rows(&[])),
                        },
                    );
                }
            }
        }

        Ok(StoreSnapshot {
            schema_version: 1,
            complete,
            shared,
            shared_inventory,
            owner_local,
            warnings,
        })
    }

    pub async fn inventory(&self) -> Result<Inventory, String> {
        let mut tables = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut complete = true;
        let mut blob_ids = Vec::new();
        if legacy_chat_store_has_data(&self.data_dir)? {
            complete = false;
            warnings.push(
                "legacy chat store at engine/chat contains data; reconcile it before fleet activation".into(),
            );
        }
        inventory_db(
            self.stores.0.pool(),
            "agent",
            &agent_specs(),
            &mut tables,
            &mut warnings,
            &mut complete,
        )
        .await?;
        inventory_db(
            self.stores.1.pool(),
            "projects",
            &project_specs(),
            &mut tables,
            &mut warnings,
            &mut complete,
        )
        .await?;
        let chat_db = self
            .data_dir
            .join(CHAT_STORE_RELATIVE_PATH)
            .join("chat.sqlite3");
        if chat_db.exists() {
            let pool = readonly_pool(&chat_db).await?;
            inventory_db(
                &pool,
                "chat",
                &chat_specs(),
                &mut tables,
                &mut warnings,
                &mut complete,
            )
            .await?;
            let has_blobs: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='chat_blobs'",
            )
            .fetch_one(&pool)
            .await
            .map_err(db)?;
            if has_blobs > 0 {
                blob_ids = sqlx::query_scalar::<_, String>("SELECT id FROM chat_blobs ORDER BY id")
                    .fetch_all(&pool)
                    .await
                    .map_err(db)?;
            }
            pool.close().await;
        } else {
            for spec in chat_specs() {
                tables.insert(
                    format!("chat/{}", spec.name),
                    TableInventory {
                        classification: spec.classification.into(),
                        record_count: Some(0),
                        sha256: None,
                    },
                );
            }
        }
        inventory_owned_runtime_databases(&self.data_dir.join("engine"), &mut tables).await?;
        let (runtime_files, files_complete, file_warnings) =
            inventory_runtime_files(&self.data_dir, &blob_ids)?;
        complete &= files_complete;
        warnings.extend(file_warnings);
        Ok(Inventory {
            schema_version: 1,
            complete,
            tables,
            runtime_files,
            warnings,
        })
    }

    pub async fn export_history(&self) -> HistoryExport {
        let mut result = HistoryExport {
            schema_version: 1,
            owner_origin: "local-runtime".into(),
            complete: true,
            threads: Vec::new(),
            blobs: Vec::new(),
            failures: Vec::new(),
        };
        let engine = match self.engine().await {
            Ok(engine) => engine,
            Err(error) => {
                result.complete = false;
                result.failures.push(HistoryFailure {
                    thread_id: None,
                    message: error,
                });
                return result;
            }
        };

        export_history_with_engine(&engine, &self.data_dir).await
    }

    /// Refresh one already persisted conversation without resuming a turn or
    /// changing its execution binding.
    pub async fn export_history_thread(
        &self,
        thread_id: &str,
        archived: Option<bool>,
    ) -> Result<Value, String> {
        Ok(self
            .export_history_thread_with_blobs(thread_id, archived)
            .await?
            .0)
    }

    /// Typed companion for Git/history publishers that retain verified blob sources.
    pub async fn export_history_thread_with_blobs(
        &self,
        thread_id: &str,
        archived: Option<bool>,
    ) -> Result<(Value, Vec<HistoryBlob>), String> {
        let engine = self.engine().await?;
        let result = engine
            .request(
                "thread/read",
                json!({"threadId":thread_id,"includeTurns":false}),
            )
            .await?;
        let mut thread = result
            .get("thread")
            .cloned()
            .ok_or("Runtime did not return a thread summary")?;
        if thread["id"].as_str() != Some(thread_id) {
            return Err("Runtime returned a different history owner thread".into());
        }
        if let Some(object) = thread.as_object_mut() {
            object.remove("turns");
            if let Some(archived) = archived {
                object.insert("archived".into(), json!(archived));
            }
        }
        let turns = export_thread_turns(&engine, thread_id).await?;
        let (attachments, blobs) =
            export_thread_attachments(&self.data_dir, &engine.agents.chat, thread_id, &turns)
                .await?;
        let blob_metadata = serde_json::to_value(&blobs).map_err(db)?;
        Ok((
            json!({"thread":thread,"turns":turns,"origin":"local-runtime","attachments":attachments,"blobs":blob_metadata}),
            blobs,
        ))
    }

    /// Read one content-addressed attachment only when an accepted submission
    /// links it to a saved user item in this runtime thread.
    pub async fn history_attachment(
        &self,
        thread_id: &str,
        sha256: &str,
    ) -> Result<Option<(HistoryAttachment, HistoryBlob)>, String> {
        if !is_sha256(sha256) {
            return Err("Attachment SHA256 must be 64 lowercase hexadecimal characters".into());
        }
        let engine = self.engine().await?;
        let turns = export_thread_turns(&engine, thread_id).await?;
        let (attachments, blobs) =
            export_thread_attachments(&self.data_dir, &engine.agents.chat, thread_id, &turns)
                .await?;
        let Some(attachment) = attachments
            .into_iter()
            .find(|attachment| attachment.sha256 == sha256)
        else {
            return Ok(None);
        };
        let blob = blobs
            .into_iter()
            .find(|blob| blob.sha256 == sha256)
            .ok_or("Accepted attachment content was not exported")?;
        Ok(Some((attachment, blob)))
    }

    /// Exports history using the explicitly configured owned runtime with all
    /// agent workers disabled and only history/account reads permitted.
    pub async fn export_quiesced_history(&self) -> Result<HistoryExport, String> {
        let _guard = self.connect_guard.lock().await;
        if self.engine.lock().await.is_some() {
            return Err("Disconnect the coding runtime before exporting quiesced history".into());
        }
        let home = self.data_dir.join("engine");
        std::fs::create_dir_all(&home).map_err(db)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).map_err(db)?;
        }
        let mut command = tokio::process::Command::new(&self.runtime_path);
        crate::configure_runtime_environment(&mut command, &home, &self.data_dir);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let engine = crate::bridge::Engine::connect_quiesced_history_with_stores(
            command,
            self.events.clone(),
            self.stores.0.clone(),
            self.stores.1.clone(),
        )
        .await?;
        if let Err(error) = engine.validate_history_account().await {
            let _ = engine.shutdown_and_drain().await;
            return Err(error);
        }
        let exported = export_history_with_engine(&engine, &self.data_dir).await;
        engine.shutdown_and_drain().await?;
        Ok(exported)
    }
}

async fn export_history_with_engine(
    engine: &crate::bridge::Engine,
    data_dir: &Path,
) -> HistoryExport {
    let mut result = HistoryExport {
        schema_version: 1,
        owner_origin: "local-runtime".into(),
        complete: true,
        threads: Vec::new(),
        blobs: Vec::new(),
        failures: Vec::new(),
    };
    let mut summaries = BTreeMap::<String, Value>::new();
    for archived in [false, true] {
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let mut params = json!({"limit":100,"sortKey":"updated_at","sortDirection":"desc","archived":archived});
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            let page = match engine.request("thread/list", params).await {
                Ok(page) => page,
                Err(error) => {
                    result.complete = false;
                    result.failures.push(HistoryFailure {
                        thread_id: None,
                        message: error,
                    });
                    break;
                }
            };
            let Some(items) = page.get("data").and_then(Value::as_array) else {
                result.complete = false;
                result.failures.push(HistoryFailure {
                    thread_id: None,
                    message: "thread/list returned malformed data".into(),
                });
                break;
            };
            for item in items {
                match item
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    Some(id) => {
                        let mut summary = item.clone();
                        if let Some(object) = summary.as_object_mut() {
                            object.remove("turns");
                            object.insert("archived".into(), json!(archived));
                        }
                        summaries.insert(id.to_owned(), summary);
                    }
                    None => {
                        result.complete = false;
                        result.failures.push(HistoryFailure {
                            thread_id: None,
                            message: "thread/list returned an entry without a thread id".into(),
                        });
                    }
                }
            }
            match page.get("nextCursor") {
                Some(Value::Null) => break,
                Some(Value::String(next)) if !next.is_empty() && seen.insert(next.clone()) => {
                    cursor = Some(next.clone())
                }
                _ => {
                    result.complete = false;
                    result.failures.push(HistoryFailure {
                        thread_id: None,
                        message: "thread/list returned invalid or repeated pagination cursor"
                            .into(),
                    });
                    break;
                }
            }
        }
    }

    let mut blobs = BTreeMap::new();
    for (thread_id, summary) in summaries {
        match export_thread_turns(&engine, &thread_id).await {
            Ok(turns) => {
                match export_thread_attachments(data_dir, &engine.agents.chat, &thread_id, &turns)
                    .await
                {
                    Ok((attachments, thread_blobs)) => {
                        for blob in thread_blobs {
                            blobs.entry(blob.sha256.clone()).or_insert(blob);
                        }
                        result.threads.push(json!({
                            "thread":summary,
                            "turns":turns,
                            "origin":"local-runtime",
                            "attachments":attachments,
                        }));
                    }
                    Err(message) => {
                        result.complete = false;
                        result.failures.push(HistoryFailure {
                            thread_id: Some(thread_id.clone()),
                            message: message.clone(),
                        });
                        result.threads.push(json!({
                            "thread":summary,
                            "turns":turns,
                            "origin":"local-runtime",
                            "attachments":[],
                            "attachmentWarning":message,
                        }));
                    }
                }
            }
            Err(message) => {
                result.complete = false;
                result.failures.push(HistoryFailure {
                    thread_id: Some(thread_id),
                    message,
                });
            }
        }
    }
    result.blobs = blobs.into_values().collect();
    result
}

#[derive(Clone, Copy)]
struct TableSpec {
    name: &'static str,
    columns: &'static str,
    classification: &'static str,
}

fn agent_specs() -> Vec<TableSpec> {
    vec![
        shared(
            "agent_profiles",
            "id,revision,candidate,created_at,updated_at",
        ),
        shared(
            "profile_revisions",
            "agent_id,revision,candidate,created_at,updated_at,draft_id,hash",
        ),
        shared(
            "profile_drafts",
            "id,thread_id,agent_id,candidate,hash,status,base_revision,created_at",
        ),
        shared(
            "tasks",
            "id,agent_id,title,instructions,acceptance_criteria,context,project_id,model,effort,dependencies,status,reason,parent_task_id,delegation_id,delegation_index,workflow_id,occurrence_id,created_at,updated_at,workspace_permission,ticket_id,ticket_status_revision,ticket_status_started",
        ),
        shared(
            "workflows",
            "id,name,agent_id,task,nodes,expression,timezone,paused,next_run_at,last_run_at,last_status,created_at,updated_at",
        ),
        shared(
            "memory_sources",
            "source_id,revision,thread_id,agent_id,project_id,content,active,updated_at,event_id",
        ),
        shared(
            "memory_source_revisions",
            "source_id,revision,content,updated_at,event_id",
        ),
        shared(
            "memory_records",
            "id,scope,scope_id,kind,content,revision,active,created_at,updated_at,event_id",
        ),
        shared(
            "memory_record_sources",
            "memory_id,source_id,source_revision",
        ),
        shared(
            "learned_skills",
            "id,name,description,content,revision,scope,scope_id,active,created_at,updated_at,event_id",
        ),
        shared("skill_sources", "skill_id,source_id,source_revision"),
        local("memory_fts", "owner-local-derived-index"),
        local("memory_fts_data", "owner-local-derived-index"),
        local("memory_fts_idx", "owner-local-derived-index"),
        local("memory_fts_content", "owner-local-derived-index"),
        local("memory_fts_docsize", "owner-local-derived-index"),
        local("memory_fts_config", "owner-local-derived-index"),
        local("shared_projection_state", "owner-local-derived-projection"),
        local("fleet_shared_projection", "owner-local-derived-projection"),
        local("fleet_shared_identity", "owner-local-derived-projection"),
        local("fleet_shared_state", "owner-local-derived-projection"),
        local("fleet_shared_tombstones", "owner-local-derived-projection"),
        local("events", "owner-local"),
        local("operation_results", "owner-local"),
        local("session_bindings", "owner-local"),
        local("workflow_occurrences", "owner-local"),
        local("task_runs", "owner-local-secret-excluded"),
        local("delegation_proposals", "owner-local"),
        local("task_settings", "owner-local"),
        local("question_requests", "owner-local"),
        local("memory_undo_results", "owner-local"),
        local("ticket_counters", "owner-local"),
        local("tickets", "owner-local"),
        local("ticket_comments", "owner-local"),
        local("ticket_activity", "owner-local"),
        local("ticket_run_proposals", "owner-local"),
        local("host_learning_evidence", "owner-local"),
        local("host_task_source_workspaces", "owner-local"),
        local("agent_task_turns", "owner-local"),
        local("agent_task_progress", "owner-local"),
        local("agent_task_submissions", "owner-local"),
        local("agent_learning_jobs", "owner-local"),
        local("question_observed_turns", "owner-local"),
        local("intelligent_ui_state", "owner-local"),
        local("intelligent_ui_reviews", "owner-local"),
    ]
}

fn project_specs() -> Vec<TableSpec> {
    vec![
        shared(
            "project_records",
            "id,name,revision,model,effort,permission,hidden",
        ),
        local("chat_project_memberships", "owner-local-project-mapping"),
        local("fleet_project_identity", "owner-local-derived-projection"),
        local("fleet_project_state", "owner-local-derived-projection"),
        local("fleet_project_metadata", "owner-local-derived-projection"),
        local("fleet_project_tombstones", "owner-local-derived-projection"),
    ]
}

fn chat_specs() -> Vec<TableSpec> {
    vec![
        local("chat_drafts", "owner-local"),
        local("chat_submissions", "owner-local"),
        local("chat_queue_state", "owner-local"),
        local("chat_branch_lineage", "owner-local"),
        local("chat_assignment_context", "owner-local"),
        local("chat_operations", "owner-local"),
        local("chat_recovery_map", "owner-local"),
        local("chat_deleted_threads", "owner-local"),
        local("chat_attachment_owner_tombstones", "owner-local-upload-state"),
        local("chat_attachment_uploads", "owner-local-upload-state"),
        local("chat_attachment_grants", "owner-local"),
        TableSpec {
            name: "chat_blobs",
            columns: "id,size",
            classification: "attachment-manifest",
        },
        TableSpec {
            name: "chat_attachments",
            columns: "id,blob_id,name,mime,size,kind",
            classification: "attachment-manifest",
        },
    ]
}

fn shared(name: &'static str, columns: &'static str) -> TableSpec {
    TableSpec {
        name,
        columns,
        classification: "shared",
    }
}

fn local(name: &'static str, classification: &'static str) -> TableSpec {
    TableSpec {
        name,
        columns: "",
        classification,
    }
}

async fn read_table(
    tx: &mut Transaction<'_, Sqlite>,
    spec: TableSpec,
) -> Result<Vec<Value>, String> {
    let query = format!("SELECT {} FROM {}", spec.columns, spec.name);
    // Both identifiers come exclusively from the compile-time table/column allowlists above.
    let rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .fetch_all(&mut **tx)
        .await
        .map_err(db)?;
    let mut values = rows.iter().map(row_json).collect::<Result<Vec<_>, _>>()?;
    values.sort_by_key(|value| serde_json::to_string(value).unwrap_or_default());
    Ok(values)
}

async fn count_table(tx: &mut Transaction<'_, Sqlite>, name: &'static str) -> Result<u64, String> {
    let query = format!("SELECT COUNT(*) FROM {name}");
    // The table name is always one of the fixed TableSpec constants.
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?;
    Ok(count.max(0) as u64)
}

async fn table_exists(
    tx: &mut Transaction<'_, Sqlite>,
    name: &'static str,
) -> Result<bool, String> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?")
            .bind(name)
            .fetch_one(&mut **tx)
            .await
            .map_err(db)?;
    Ok(count > 0)
}

fn row_json(row: &sqlx::sqlite::SqliteRow) -> Result<Value, String> {
    use sqlx::{Column, TypeInfo, ValueRef};
    let mut map = serde_json::Map::new();
    for (index, column) in row.columns().iter().enumerate() {
        let raw = row.try_get_raw(index).map_err(db)?;
        let value = if raw.is_null() {
            Value::Null
        } else {
            match raw.type_info().name() {
                "INTEGER" => json!(row.try_get::<i64, _>(index).map_err(db)?),
                "REAL" => json!(row.try_get::<f64, _>(index).map_err(db)?),
                "BLOB" => {
                    let bytes = row.try_get::<Vec<u8>, _>(index).map_err(db)?;
                    json!({"base16": hex(&bytes)})
                }
                _ => json!(row.try_get::<String, _>(index).map_err(db)?),
            }
        };
        map.insert(snake_to_camel(column.name()), value);
    }
    Ok(Value::Object(map))
}

fn snake_to_camel(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut uppercase = false;
    for ch in value.chars() {
        if ch == '_' {
            uppercase = true;
        } else if uppercase {
            out.extend(ch.to_uppercase());
            uppercase = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn hash_rows(rows: &[Value]) -> String {
    let mut digest = Sha256::new();
    for row in rows {
        let bytes = serde_json::to_vec(row).unwrap_or_default();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

async fn inspect_tables(
    tx: &mut Transaction<'_, Sqlite>,
    store: &str,
    specs: &[TableSpec],
    warnings: &mut Vec<String>,
    complete: &mut bool,
) -> Result<(), String> {
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;
    let known = specs.iter().map(|spec| spec.name).collect::<HashSet<_>>();
    for name in existing {
        if !known.contains(name.as_str()) {
            *complete = false;
            warnings.push(format!("{store} store contains unclassified table `{name}`; full import inventory is unavailable"));
        }
    }
    for spec in specs {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?")
                .bind(spec.name)
                .fetch_one(&mut **tx)
                .await
                .map_err(db)?;
        if exists == 0 && spec.classification == "shared" {
            *complete = false;
            warnings.push(format!(
                "{store} store is missing expected table `{}`",
                spec.name
            ));
        }
    }
    Ok(())
}

async fn inventory_db(
    pool: &SqlitePool,
    store: &str,
    specs: &[TableSpec],
    output: &mut BTreeMap<String, TableInventory>,
    warnings: &mut Vec<String>,
    complete: &mut bool,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(db)?;
    inspect_tables(&mut tx, store, specs, warnings, complete).await?;
    for spec in specs {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?")
                .bind(spec.name)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        if exists == 0 {
            output.insert(
                format!("{store}/{}", spec.name),
                TableInventory {
                    classification: spec.classification.into(),
                    record_count: Some(0),
                    sha256: None,
                },
            );
            continue;
        }
        let count = count_table(&mut tx, spec.name).await?;
        let sha256 = if spec.classification == "shared" {
            Some(hash_rows(&read_table(&mut tx, *spec).await?))
        } else {
            None
        };
        output.insert(
            format!("{store}/{}", spec.name),
            TableInventory {
                classification: spec.classification.into(),
                record_count: Some(count),
                sha256,
            },
        );
    }
    tx.commit().await.map_err(db)
}

async fn readonly_pool(path: &Path) -> Result<SqlitePool, String> {
    let options = SqliteConnectOptions::new().filename(path).read_only(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(db)
}

pub async fn verify_backup_store(
    path: &Path,
    role: &str,
    expected: &BTreeMap<String, TableInventory>,
) -> Result<(), String> {
    let specs = match role {
        "agent" => agent_specs(),
        "projects" => project_specs(),
        "chat" => chat_specs(),
        _ => return Err("Unknown backup store role".into()),
    };
    let pool = readonly_pool(path).await?;
    let mut actual = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut complete = true;
    inventory_db(
        &pool,
        role,
        &specs,
        &mut actual,
        &mut warnings,
        &mut complete,
    )
    .await?;
    pool.close().await;
    if !complete {
        return Err(format!("Backup store {role} contains unclassified tables"));
    }
    for (key, info) in actual {
        let source = expected
            .get(&key)
            .ok_or_else(|| format!("Source inventory has no record of {key}"))?;
        if info.record_count != source.record_count
            || info.sha256 != source.sha256
            || info.classification != source.classification
        {
            return Err(format!(
                "Backup store {key} does not match the inventoried source counts and hashes"
            ));
        }
    }
    Ok(())
}

async fn inventory_owned_runtime_databases(
    engine: &Path,
    tables: &mut BTreeMap<String, TableInventory>,
) -> Result<(), String> {
    for (role, name) in OWNED_RUNTIME_DATABASES {
        let path = engine.join(name);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(db(error)),
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(format!(
                "Owned runtime database {name} is not a regular file"
            ));
        }
        let pool = readonly_pool(&path).await?;
        let names=sqlx::query_scalar::<_,String>("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&pool).await.map_err(db)?;
        for table in names {
            if table.is_empty()
                || !table
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err("Owned runtime database has an unsupported table name".into());
            }
            let sql = sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM \"{table}\""));
            let count = sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .map_err(db)?;
            tables.insert(
                format!("{role}/{table}"),
                TableInventory {
                    classification: "owner-local-runtime-state".into(),
                    record_count: Some(count as u64),
                    sha256: None,
                },
            );
        }
        pool.close().await;
    }
    Ok(())
}

fn inventory_runtime_files(
    data_dir: &Path,
    blob_ids: &[String],
) -> Result<(Vec<RuntimeFileInventory>, bool, Vec<String>), String> {
    let mut output = Vec::new();
    let mut warnings = Vec::new();
    let mut complete = true;
    let engine = data_dir.join("engine");
    if engine.exists() {
        for entry in std::fs::read_dir(&engine).map_err(db)? {
            let entry = entry.map_err(db)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry.file_type().map_err(db)?;
            // Pinned runtime: core-plugins/rollout staging uses .tmp, arg0
            // helpers use tmp, and core owns the local installation UUID.
            // These never enter either the migration copy or shared Git.
            if (matches!(name.as_str(), ".tmp" | "tmp") && kind.is_dir())
                || (name == "installation_id" && kind.is_file())
            {
                continue;
            }
            if name == "auth.json" {
                warnings.push(
                    "authentication material is deliberately excluded from fleet inventory".into(),
                );
            } else if !matches!(
                name.as_str(),
                "agents"
                    | "chat"
                    | "sessions"
                    | "archived_sessions"
                    | "config.toml"
                    | "version.json"
                    | "models_cache"
                    | "log"
                    | "logs"
                    | "models_cache.json"
                    | "session_index.jsonl"
                    | "worktrees"
                    | "worktree-sessions"
                    | "session-workspaces"
                    | "shell_snapshots"
                    | "skills"
                    | "plugins"
            ) && !OWNED_RUNTIME_DATABASES.iter().any(|(_, file)| {
                name == *file
                    || name.strip_suffix("-wal") == Some(*file)
                    || name.strip_suffix("-shm") == Some(*file)
            }) {
                warnings.push(format!(
                    "unclassified entry in runtime data directory: {name}"
                ));
                complete = false;
            }
        }
    }
    let mut history_directory = false;
    for (directory, classification) in [
        ("sessions", "runtime-history"),
        ("archived_sessions", "runtime-archived-history"),
    ] {
        let path = engine.join(directory);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                history_directory = true;
                walk_history_files(
                    &path,
                    &path,
                    classification,
                    &mut output,
                    &mut warnings,
                    &mut complete,
                )?;
            }
            Ok(_) => {
                complete = false;
                warnings.push(format!("Unsafe runtime history directory: {directory}"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(db(error)),
        }
    }
    if !history_directory {
        warnings.push(
            "runtime history directory is absent; history file inventory may be incomplete".into(),
        );
        complete = false;
    }
    for blob_id in blob_ids {
        match content_hash_for_blob(data_dir, blob_id) {
            Ok(Some(sha256)) => {
                let path = data_dir
                    .join(CHAT_STORE_RELATIVE_PATH)
                    .join("blobs")
                    .join(blob_id);
                let size_bytes = std::fs::symlink_metadata(&path).map_err(db)?.len();
                output.push(RuntimeFileInventory {
                    relative_path: format!("app-attachment-blob/{blob_id}"),
                    size_bytes,
                    sha256,
                    classification: "app-attachment-blob".into(),
                });
            }
            Ok(None) => {
                complete = false;
                warnings.push(format!(
                    "referenced attachment blob `{blob_id}` is missing or unsafe"
                ));
            }
            Err(error) => {
                complete = false;
                warnings.push(format!(
                    "could not inventory referenced attachment blob `{blob_id}`: {error}"
                ));
            }
        }
    }
    let upload_dir = data_dir
        .join(CHAT_STORE_RELATIVE_PATH)
        .join("uploads");
    let upload_directory = match std::fs::symlink_metadata(&upload_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            complete = false;
            warnings.push("attachment upload staging is not a real directory".into());
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            complete = false;
            warnings.push(format!("could not inspect attachment upload staging: {error}"));
            false
        }
    };
    if upload_directory {
    match std::fs::read_dir(&upload_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        complete = false;
                        warnings.push(format!("could not read staged attachment inventory: {error}"));
                        continue;
                    }
                };
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(upload_id) = name.strip_suffix(".part") else {
                    complete = false;
                    warnings.push("upload staging contains an unclassified file".into());
                    continue;
                };
                if !canonical_uuid(upload_id) {
                    complete = false;
                    warnings.push("upload staging contains an invalid upload ID".into());
                    continue;
                }
                match hash_local_file(&path, HISTORY_ATTACHMENT_MAX_FILE_BYTES) {
                    Ok((size_bytes, sha256)) => output.push(RuntimeFileInventory {
                        relative_path: format!("app-attachment-upload/{upload_id}.part"),
                        size_bytes,
                        sha256,
                        classification: "app-attachment-upload".into(),
                    }),
                    Err(error) => {
                        complete = false;
                        warnings.push(format!("staged upload `{upload_id}` is unsafe or unreadable: {error}"));
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            complete = false;
            warnings.push(format!("could not inspect attachment upload staging: {error}"));
        }
    }
    }
    Ok((output, complete, warnings))
}

fn content_hash_for_blob(data_dir: &Path, blob_id: &str) -> Result<Option<String>, String> {
    if uuid::Uuid::parse_str(blob_id).is_err() {
        return Ok(None);
    }
    let base = data_dir.join(CHAT_STORE_RELATIVE_PATH).join("blobs");
    let base_metadata = match std::fs::symlink_metadata(&base) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let _ = base_metadata;
    let path = base.join(blob_id);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let _ = metadata;
    let bytes = std::fs::read(path).map_err(db)?;
    Ok(Some(format!("{:x}", Sha256::digest(bytes))))
}

fn legacy_chat_store_has_data(data_dir: &Path) -> Result<bool, String> {
    let path = data_dir.join("engine/chat");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Ok(std::fs::read_dir(path).map_err(db)?.next().is_some())
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(db(error)),
    }
}

fn walk_history_files(
    base: &Path,
    directory: &Path,
    classification: &str,
    output: &mut Vec<RuntimeFileInventory>,
    warnings: &mut Vec<String>,
    complete: &mut bool,
) -> Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(db)? {
        let entry = entry.map_err(db)?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(db)?;
        if metadata.file_type().is_symlink() {
            warnings.push(format!(
                "skipped symlink in known data directory: {}",
                path.strip_prefix(base).unwrap_or(&path).display()
            ));
            *complete = false;
            continue;
        }
        if metadata.is_dir() {
            walk_history_files(base, &path, classification, output, warnings, complete)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(base).map_err(db)?;
            let name = relative
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if matches!(
                classification,
                "runtime-history" | "runtime-archived-history"
            ) && !name.ends_with(".jsonl")
                && !name.ends_with(".jsonl.zst")
            {
                warnings.push(format!(
                    "unclassified file in runtime history directory: {}",
                    relative.display()
                ));
                *complete = false;
                continue;
            }
            let bytes = std::fs::read(&path).map_err(db)?;
            let digest = Sha256::digest(&bytes);
            output.push(RuntimeFileInventory {
                relative_path: format!("{}/{}", classification, relative.to_string_lossy()),
                size_bytes: metadata.len(),
                sha256: format!("{:x}", digest),
                classification: classification.into(),
            });
        }
    }
    Ok(())
}

async fn export_thread_turns(
    engine: &crate::bridge::Engine,
    thread_id: &str,
) -> Result<Vec<Value>, String> {
    let mut turns = Vec::new();
    let mut cursor = None;
    let mut seen = HashSet::new();
    loop {
        let mut params =
            json!({"threadId":thread_id,"limit":100,"sortDirection":"asc","itemsView":"full"});
        if let Some(cursor) = &cursor {
            params["cursor"] = json!(cursor);
        }
        let page = engine.request("thread/turns/list", params).await?;
        let items = page
            .get("data")
            .and_then(Value::as_array)
            .ok_or("thread/turns/list returned malformed data")?;
        turns.extend(items.iter().cloned());
        match page.get("nextCursor") {
            Some(Value::Null) => break,
            Some(Value::String(next)) if !next.is_empty() && seen.insert(next.clone()) => {
                cursor = Some(next.clone())
            }
            _ => {
                return Err(
                    "thread/turns/list returned invalid or repeated pagination cursor".into(),
                );
            }
        }
    }
    Ok(turns)
}

async fn export_thread_attachments(
    data_dir: &Path,
    chat: &crate::chat_state::ChatStore,
    thread_id: &str,
    turns: &[Value],
) -> Result<(Vec<HistoryAttachment>, Vec<HistoryBlob>), String> {
    let blob_dir = checked_chat_blob_directory(data_dir)?;
    let submissions = sqlx::query(
        "SELECT id,payload FROM chat_submissions WHERE thread_id=? AND status='accepted' ORDER BY created_at,rowid",
    )
    .bind(thread_id)
    .fetch_all(&chat.pool)
    .await
    .map_err(db)?;

    let mut attachments = Vec::new();
    let mut blobs = BTreeMap::<String, HistoryBlob>::new();
    let mut blob_hashes = HashMap::<String, String>::new();
    for row in submissions {
        let submission_id: String = row.try_get("id").map_err(db)?;
        let payload_text: String = row.try_get("payload").map_err(db)?;
        let payload: Value = serde_json::from_str(&payload_text).map_err(|e| e.to_string())?;
        if payload["id"].as_str() != Some(&submission_id)
            || payload["threadId"].as_str() != Some(thread_id)
            || payload["status"].as_str() != Some("accepted")
        {
            return Err(format!(
                "Accepted attachment submission {submission_id} has inconsistent owner metadata"
            ));
        }
        let Some(ids) = payload.get("attachmentIds").and_then(Value::as_array) else {
            if payload.get("attachmentIds").is_none() || payload["attachmentIds"].is_null() {
                continue;
            }
            return Err(format!(
                "Accepted attachment submission {submission_id} has malformed attachment IDs"
            ));
        };
        if ids.is_empty() {
            continue;
        }
        let stored_turn_id = match payload.get("turnId") {
            None | Some(Value::Null) => None,
            Some(Value::String(turn_id)) if !turn_id.is_empty() => Some(turn_id.as_str()),
            _ => {
                return Err(format!(
                    "Accepted attachment submission {submission_id} has an invalid stored turn ID"
                ));
            }
        };
        let (turn_id, item_id) = correlate_submission_item(&submission_id, stored_turn_id, turns)?;

        let mut submission_ids = HashSet::new();
        let mut submission_bytes = 0_u64;
        if ids.len() > HISTORY_ATTACHMENT_MAX_OWNER_FILES {
            return Err(format!(
                "Accepted submission {submission_id} exceeds the 20-file limit"
            ));
        }
        for id in ids {
            let attachment_id = id.as_str().filter(|id| canonical_uuid(id)).ok_or_else(|| {
                format!(
                    "Accepted attachment submission {submission_id} has an unsafe attachment ID"
                )
            })?;
            if !submission_ids.insert(attachment_id.to_owned()) {
                return Err(format!(
                    "Accepted attachment submission {submission_id} repeats an attachment ID"
                ));
            }
            let attachment = sqlx::query(
                "SELECT a.id,a.blob_id,a.name,a.mime,a.size,a.kind,b.size AS blob_size FROM chat_attachments a JOIN chat_blobs b ON b.id=a.blob_id JOIN chat_attachment_grants g ON g.attachment_id=a.id AND g.owner_id=? WHERE a.id=?",
            )
            .bind(&submission_id)
            .bind(attachment_id)
            .fetch_optional(&chat.pool)
            .await
            .map_err(db)?
            .ok_or_else(|| format!("Accepted submission {submission_id} lost an attachment grant"))?;
            let blob_id: String = attachment.try_get("blob_id").map_err(db)?;
            if !canonical_uuid(&blob_id) {
                return Err(format!("Attachment {attachment_id} has an unsafe blob ID"));
            }
            let size: i64 = attachment.try_get("size").map_err(db)?;
            let blob_size: i64 = attachment.try_get("blob_size").map_err(db)?;
            if size < 0 || blob_size != size || size as u64 > HISTORY_ATTACHMENT_MAX_FILE_BYTES {
                return Err(format!(
                    "Attachment {attachment_id} has an invalid stored size"
                ));
            }
            let size_bytes = size as u64;
            submission_bytes = submission_bytes.saturating_add(size_bytes);
            if submission_bytes > HISTORY_ATTACHMENT_MAX_OWNER_BYTES {
                return Err(format!(
                    "Accepted submission {submission_id} exceeds the 200 MiB owner limit"
                ));
            }
            let path = blob_dir.join(&blob_id);
            let metadata = std::fs::symlink_metadata(&path).map_err(db)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != size_bytes
            {
                return Err(format!(
                    "Attachment blob {blob_id} is missing, unsafe, or has a size mismatch"
                ));
            }
            let blob = if let Some(sha256) = blob_hashes.get(&blob_id) {
                blobs
                    .get(sha256)
                    .cloned()
                    .ok_or("Cached attachment blob source is missing")?
            } else {
                let blob = HistoryBlob::from_local_path(&path, size_bytes)?;
                blob_hashes.insert(blob_id, blob.sha256.clone());
                blobs
                    .entry(blob.sha256.clone())
                    .or_insert_with(|| blob.clone());
                blob
            };
            let sha256 = blob.sha256.clone();
            let name: String = attachment.try_get("name").map_err(db)?;
            let mime: String = attachment.try_get("mime").map_err(db)?;
            let kind: String = attachment.try_get("kind").map_err(db)?;
            attachments.push(HistoryAttachment {
                id: attachment_id.to_owned(),
                name,
                mime,
                kind,
                size_bytes,
                sha256: sha256.clone(),
                submission_id: submission_id.clone(),
                turn_id: turn_id.clone(),
                item_id: item_id.clone(),
            });
            blobs.entry(sha256).or_insert(blob);
        }
    }
    Ok((attachments, blobs.into_values().collect()))
}

fn correlate_submission_item(
    submission_id: &str,
    stored_turn_id: Option<&str>,
    turns: &[Value],
) -> Result<(Option<String>, Option<String>), String> {
    let matching_client_items = turns
        .iter()
        .filter_map(|turn| {
            let turn_id = turn["id"].as_str()?;
            let items = turn["items"].as_array()?;
            Some(items.iter().filter_map(move |item| {
                let client_id = item["clientId"]
                    .as_str()
                    .or_else(|| item["clientUserMessageId"].as_str());
                (item["type"] == "userMessage" && client_id == Some(submission_id))
                    .then_some((turn_id, item))
            }))
        })
        .flatten()
        .collect::<Vec<_>>();

    if let Some(turn_id) = stored_turn_id.filter(|turn_id| !turn_id.is_empty()) {
        if matching_client_items
            .iter()
            .any(|(matching_turn_id, _)| *matching_turn_id != turn_id)
        {
            return Err(format!(
                "Accepted attachment submission {submission_id} client ID conflicts with its stored turn"
            ));
        }
        let turn = turns
            .iter()
            .find(|turn| turn["id"].as_str() == Some(turn_id))
            .ok_or_else(|| {
                format!("Accepted attachment submission {submission_id} turn is not in history")
            })?;
        let items = turn["items"].as_array().ok_or_else(|| {
            format!("Accepted attachment submission {submission_id} turn has no item list")
        })?;
        let direct = items
            .iter()
            .filter(|item| {
                item["type"] == "userMessage"
                    && (item["clientId"].as_str() == Some(submission_id)
                        || item["clientUserMessageId"].as_str() == Some(submission_id))
            })
            .collect::<Vec<_>>();
        if direct.len() == 1 {
            return Ok((
                Some(turn_id.to_owned()),
                direct[0]["id"].as_str().map(str::to_owned),
            ));
        }
        if direct.len() > 1 {
            return Ok((Some(turn_id.to_owned()), None));
        }
        let user_items = items
            .iter()
            .filter(|item| item["type"] == "userMessage")
            .collect::<Vec<_>>();
        if user_items.is_empty()
            || user_items.iter().any(|item| {
                item.get("clientId").is_some_and(|v| !v.is_null())
                    || item
                        .get("clientUserMessageId")
                        .is_some_and(|v| !v.is_null())
            })
        {
            return Err(format!(
                "Accepted attachment submission {submission_id} has no proven user item in its saved turn"
            ));
        }
        let item_id = if user_items.len() == 1 {
            user_items[0]["id"].as_str().map(str::to_owned)
        } else {
            None
        };
        return Ok((Some(turn_id.to_owned()), item_id));
    }

    if matching_client_items.len() == 1 {
        let (turn_id, item) = matching_client_items[0];
        let item_id = item["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        return Ok((Some(turn_id.to_owned()), item_id));
    }
    // The accepted receipt and exact submission-owned grants prove conversation
    // ownership. Without a runtime link, keep it at conversation scope.
    Ok((None, None))
}

fn checked_chat_blob_directory(data_dir: &Path) -> Result<std::path::PathBuf, String> {
    let mut path = std::fs::canonicalize(data_dir).map_err(db)?;
    for component in ["engine", "agents", "chat", "blobs"] {
        path.push(component);
        let metadata = std::fs::symlink_metadata(&path).map_err(db)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!("Unsafe attachment storage directory: {component}"));
        }
    }
    let database = path
        .parent()
        .ok_or("Attachment database path is invalid")?
        .join("chat.sqlite3");
    let metadata = std::fs::symlink_metadata(&database).map_err(db)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Unsafe attachment registry database".into());
    }
    Ok(path)
}

fn read_checked_blob(
    path: &Path,
    size_bytes: u64,
    expected_sha256: Option<&str>,
) -> Result<(PathBuf, Vec<u8>, String), String> {
    if size_bytes > HISTORY_ATTACHMENT_MAX_FILE_BYTES {
        return Err("Attachment exceeds the 50 MiB per-file limit".into());
    }
    if !path.is_absolute() {
        return Err("Attachment source path is not absolute".into());
    }
    let canonical = fs::canonicalize(path).map_err(db)?;
    if canonical != path {
        return Err(
            "Attachment source path traverses a symbolic link or non-canonical component".into(),
        );
    }
    let metadata = fs::symlink_metadata(&canonical).map_err(db)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != size_bytes {
        return Err("Attachment source is not a regular file of the recorded size".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&canonical).map_err(db)?;
    let opened_metadata = file.metadata().map_err(db)?;
    if !opened_metadata.is_file() || opened_metadata.len() != size_bytes {
        return Err("Attachment source changed before it could be read".into());
    }
    let mut bytes = Vec::with_capacity(size_bytes as usize);
    file.take(size_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(db)?;
    if bytes.len() as u64 != size_bytes {
        return Err("Attachment source changed while it was read".into());
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if expected_sha256.is_some_and(|expected| expected != sha256) {
        return Err("Attachment source does not match its recorded SHA256".into());
    }
    Ok((canonical, bytes, sha256))
}

fn hash_local_file(path: &Path, max_bytes: u64) -> Result<(u64, String), String> {
    let metadata = fs::symlink_metadata(path).map_err(db)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes {
        return Err("managed file is not a bounded regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(db)?;
    let opened = file.metadata().map_err(db)?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err("managed file changed while opening".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.uid() != unsafe { libc::geteuid() } || opened.mode() & 0o077 != 0 {
            return Err("managed upload file is not private to this user".into());
        }
    }
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(db)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > max_bytes || size > metadata.len() {
            return Err("managed file changed while being hashed".into());
        }
        hasher.update(&buffer[..count]);
    }
    if size != metadata.len() {
        return Err("managed file changed while being hashed".into());
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|uuid| uuid.hyphenated().to_string() == value)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn db(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventSink, Host, HostConfig};
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn host() -> (TempDir, Host) {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(
            HostConfig {
                data_dir: dir.path().join("data"),
                runtime_path: dir.path().join("runtime-that-is-not-started"),
            },
            Arc::new(|_: Value| {}) as EventSink,
        )
        .await
        .unwrap();
        (dir, host)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quiesced_history_uses_only_owned_read_rpc_and_does_not_run_workers() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("owned-runtime");
        let calls = dir.path().join("rpc-calls.jsonl");
        std::fs::write(&runtime, format!(r#"#!/usr/bin/env python3
import json,sys,time
log={log}
for line in sys.stdin:
    m=json.loads(line)
    if 'method' not in m: continue
    with open(log,'a') as f: f.write(json.dumps(m)+'\n')
    if m['method']=='initialized':
        print(json.dumps({{'id':900,'method':'item/tool/call','params':{{'name':'brr_task_complete'}}}}),flush=True)
        continue
    method=m['method']
    result={{'account':None}} if method=='account/read' else {{'data':[],'nextCursor':None}}
    print(json.dumps({{'id':m['id'],'result':result}}),flush=True)
"#, log = serde_json::to_string(calls.to_str().unwrap()).unwrap())).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let data_dir = dir.path().join("data");
        let host = Host::open(
            HostConfig {
                data_dir,
                runtime_path: runtime,
            },
            Arc::new(|_: Value| {}),
        )
        .await
        .unwrap();
        let store = host.stores.0.pool();
        sqlx::query("INSERT INTO tasks(id,agent_id,title,instructions,acceptance_criteria,dependencies,status,created_at,updated_at) VALUES('task-preserved','agent','Queued','Do nothing','[]','[]','queued',1,1)")
            .execute(store).await.unwrap();
        sqlx::query("INSERT INTO workflows(id,name,agent_id,task,expression,timezone,paused,created_at,updated_at) VALUES('workflow-preserved','Paused','agent','{}','0 0 * * *','UTC',1,1,1)")
            .execute(store).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS agent_learning_jobs(id TEXT PRIMARY KEY,source_thread_id TEXT NOT NULL,source_turn_id TEXT NOT NULL,source_refs TEXT NOT NULL,agent_id TEXT,project_id TEXT,cwd TEXT,state TEXT NOT NULL,learning_thread_id TEXT,reason TEXT,turn_id TEXT,completion_turn_id TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,continuation_count INTEGER NOT NULL DEFAULT 0,UNIQUE(source_thread_id,source_turn_id))")
            .execute(store).await.unwrap();
        sqlx::query("INSERT INTO agent_learning_jobs(id,source_thread_id,source_turn_id,source_refs,state,created_at,updated_at) VALUES('learning-preserved','thread','turn','[]','queued',1,1)")
            .execute(store).await.unwrap();
        let before: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM tasks WHERE status='queued'),(SELECT COUNT(*) FROM workflows WHERE paused=1),(SELECT COUNT(*) FROM agent_learning_jobs WHERE state='queued')")
            .fetch_one(store).await.unwrap();

        let exported = host.export_quiesced_history().await.unwrap();
        assert!(exported.complete, "{:?}", exported.failures);
        assert!(exported.threads.is_empty());
        assert!(!host.is_connected().await);
        let after: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM tasks WHERE status='queued'),(SELECT COUNT(*) FROM workflows WHERE paused=1),(SELECT COUNT(*) FROM agent_learning_jobs WHERE state='queued')")
            .fetch_one(store).await.unwrap();
        assert_eq!(after, before);
        let calls = std::fs::read_to_string(calls).unwrap();
        let methods = calls
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|message| message.get("id").is_some())
            .filter_map(|message| message["method"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(methods.contains(&"initialize".into()));
        assert!(methods.contains(&"account/read".into()));
        assert_eq!(
            methods
                .iter()
                .filter(|m| m.as_str() == "thread/list")
                .count(),
            2
        );
        assert!(methods
            .iter()
            .all(|m| matches!(m.as_str(), "initialize" | "account/read" | "thread/list")));
    }

    #[tokio::test]
    async fn snapshot_preserves_profile_memory_and_provenance_without_local_paths_or_secrets() {
        let (_dir, host) = host().await;
        let pool = host.stores.0.pool();
        sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES(?,?,?,?,?)")
            .bind("agent-1").bind(3_i64).bind(r#"{"name":"Planner","instructions":"keep evidence"}"#).bind(1_i64).bind(2_i64)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO profile_revisions(agent_id,revision,candidate,created_at,updated_at,draft_id,hash) VALUES(?,?,?,?,?,?,?)")
            .bind("agent-1").bind(3_i64).bind(r#"{"name":"Planner","instructions":"keep evidence"}"#).bind(1_i64).bind(2_i64).bind("draft-raw-id").bind("candidate-hash")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO profile_drafts(id,thread_id,agent_id,candidate,hash,status,base_revision,created_at) VALUES(?,?,?,?,?,?,?,?)")
            .bind("draft-review-id").bind("thread-local").bind("agent-1").bind("candidate-json").bind("draft-hash").bind("approved").bind(2_i64).bind(3_i64)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO memory_sources(source_id,revision,thread_id,agent_id,project_id,content,active,updated_at,event_id) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind("source-id").bind(2_i64).bind("thread-local").bind("agent-1").bind(Option::<String>::None).bind("source evidence").bind(1_i64).bind(4_i64).bind(7_i64)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO memory_source_revisions(source_id,revision,content,updated_at,event_id) VALUES(?,?,?,?,?)")
            .bind("source-id").bind(1_i64).bind("old evidence").bind(2_i64).bind(6_i64).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO memory_records(id,scope,scope_id,kind,content,revision,active,created_at,updated_at,event_id) VALUES(?,?,?,?,?,?,?,?,?,?)")
            .bind("memory-id").bind("agent").bind("agent-1").bind("fact").bind("remembered fact").bind(1_i64).bind(0_i64).bind(1_i64).bind(4_i64).bind(8_i64)
            .execute(pool).await.unwrap();
        sqlx::query(
            "INSERT INTO memory_record_sources(memory_id,source_id,source_revision) VALUES(?,?,?)",
        )
        .bind("memory-id")
        .bind("source-id")
        .bind(2_i64)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO learned_skills(id,name,description,content,revision,scope,scope_id,active,created_at,updated_at,event_id) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
            .bind("skill-id").bind("Review").bind("procedure").bind("steps").bind(2_i64).bind("agent").bind("agent-1").bind(1_i64).bind(1_i64).bind(4_i64).bind(9_i64)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO skill_sources(skill_id,source_id,source_revision) VALUES(?,?,?)")
            .bind("skill-id")
            .bind("source-id")
            .bind(1_i64)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE export_test_owner_secret(token TEXT NOT NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO export_test_owner_secret(token) VALUES(?)")
            .bind("DO_NOT_EXPORT_AUTH_TOKEN")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tasks(id,agent_id,title,instructions,acceptance_criteria,dependencies,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind("task-secret").bind("agent-1").bind("secret test task").bind("instructions").bind("criteria").bind("[]").bind("running").bind(1_i64).bind(1_i64)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO task_runs(id,task_id,agent_id,worker_id,lease_token,state,created_at) VALUES(?,?,?,?,?,?,?)")
            .bind("run-secret").bind("task-secret").bind("agent-1").bind("worker").bind("DO_NOT_EXPORT_LEASE").bind("running").bind(1_i64)
            .execute(pool).await.unwrap();

        host.project_request("create", json!({"name":"Fleet demo","folders":["/owner/private/project"],"primaryFolder":"/owner/private/project"})).await.unwrap();

        let snapshot = host.store_snapshot().await.unwrap();
        assert_eq!(snapshot.schema_version, 1);
        assert!(snapshot.shared["agent_profiles"]
            .iter()
            .any(|r| r["id"] == "agent-1"));
        assert!(snapshot.shared["profile_revisions"]
            .iter()
            .any(|r| r["draftId"] == "draft-raw-id"));
        assert!(snapshot.shared["profile_drafts"]
            .iter()
            .any(|r| r["id"] == "draft-review-id"));
        assert!(snapshot.shared["memory_sources"]
            .iter()
            .any(|r| r["active"] == 1));
        assert!(snapshot.shared["memory_source_revisions"]
            .iter()
            .any(|r| r["revision"] == 1));
        assert!(snapshot.shared["memory_records"]
            .iter()
            .any(|r| r["active"] == 0));
        assert!(snapshot.shared["memory_record_sources"]
            .iter()
            .any(|r| r["sourceRevision"] == 2));
        assert!(snapshot.shared["skill_sources"]
            .iter()
            .any(|r| r["sourceRevision"] == 1));
        assert!(snapshot.shared["project_records"]
            .iter()
            .any(|r| r["name"] == "Fleet demo"));
        let text = serde_json::to_string(&snapshot).unwrap();
        assert!(!text.contains("/owner/private/project"));
        assert!(!text.contains("DO_NOT_EXPORT_AUTH_TOKEN"));
        assert!(!text.contains("DO_NOT_EXPORT_LEASE"));
        assert!(text.contains("unclassified table `export_test_owner_secret`"));
        assert!(!snapshot.warnings.is_empty());
    }

    #[tokio::test]
    async fn history_without_runtime_is_explicitly_incomplete_without_connecting_or_inferring() {
        let (_dir, host) = host().await;
        let exported = host.export_history().await;
        assert!(!exported.complete);
        assert_eq!(exported.owner_origin, "local-runtime");
        assert!(!exported.failures.is_empty());
        assert!(exported.threads.is_empty());
        assert!(!host.is_connected().await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn history_export_includes_only_accepted_saved_message_attachments_and_blob_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("owned-runtime");
        let legacy_mode = dir.path().join("legacy-mode");
        let runtime_script = r#"#!/usr/bin/env python3
import json,sys,os
legacy_mode=MODEFILE
for line in sys.stdin:
    message=json.loads(line)
    if 'method' not in message: continue
    method=message['method']
    params=message.get('params',{})
    if method=='initialized': continue
    if method=='account/read':
        result={'account':{'type':'chatgpt'}}
    elif method=='thread/list':
        result={'data':([] if params.get('archived') else [{'id':'thread-a','title':'Saved thread'}]),'nextCursor':None}
    elif method=='thread/read':
        result={'thread':{'id':params.get('threadId'),'title':'Saved thread','turns':[]}}
    elif method=='turn/start':
        result={'turn':{'id':'turn-a','status':'completed','items':[]}}
    elif method=='thread/turns/list':
        item={'id':'user-item-a','type':'userMessage','clientId':'accepted-submission'}
        if os.path.exists(legacy_mode): item.pop('clientId',None)
        result={'data':[{'id':'turn-a','items':[item]}],'nextCursor':None}
    else:
        result={}
    if 'id' in message: print(json.dumps({'id':message['id'],'result':result}),flush=True)
"#
        .replace(
            "MODEFILE",
            &serde_json::to_string(legacy_mode.to_str().unwrap()).unwrap(),
        );
        std::fs::write(&runtime, runtime_script).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let data_dir = dir.path().join("data");
        let host = Host::open(
            HostConfig {
                data_dir,
                runtime_path: runtime,
            },
            Arc::new(|_: Value| {}),
        )
        .await
        .unwrap();
        host.connect().await.unwrap();

        let accepted_file = host.chat_request("attachments.importBytes", json!({
            "ownerId":"thread-a",
            "files":[{"name":"accepted.txt","mime":"text/plain","data":"YWNjZXB0ZWQgYnl0ZXM="}]
        })).await.unwrap();
        let queued_file = host
            .chat_request(
                "attachments.importBytes",
                json!({
                    "ownerId":"thread-a",
                    "files":[{"name":"queued.txt","mime":"text/plain","data":"cXVldWVkIGJ5dGVz"}]
                }),
            )
            .await
            .unwrap();
        let _draft_file = host.chat_request("attachments.importBytes", json!({
            "ownerId":"thread-a",
            "files":[{"name":"unused-draft.txt","mime":"text/plain","data":"ZHJhZnQgYnl0ZXM="}]
        })).await.unwrap();
        host.chat_request(
            "foreground.submit",
            json!({
                "id":"accepted-submission","threadId":"thread-a",
                "document":{"version":1,"text":"sent","references":[],"quotes":[]},
                "attachmentIds":[accepted_file[0]["id"]],
                "settings":{"model":"_runtime_default","mode":"default","effort":null}
            }),
        )
        .await
        .unwrap();
        host.chat_request(
            "queue.pause",
            json!({"threadId":"thread-a","reason":"fixture"}),
        )
        .await
        .unwrap();
        host.chat_request(
            "foreground.submit",
            json!({
                "id":"queued-submission","threadId":"thread-a",
                "document":{"version":1,"text":"not sent","references":[],"quotes":[]},
                "attachmentIds":[queued_file[0]["id"]],
                "settings":{"model":"_runtime_default","mode":"default","effort":null}
            }),
        )
        .await
        .unwrap();
        host.shutdown().await;

        let exported = host.export_quiesced_history().await.unwrap();
        assert!(exported.complete, "{:?}", exported.failures);
        let encoded = serde_json::to_value(&exported).unwrap();
        assert_eq!(encoded["threads"].as_array().unwrap().len(), 1);
        assert_eq!(encoded["threads"][0]["thread"]["id"], "thread-a");
        assert_eq!(
            encoded["threads"][0]["attachments"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let manifest = &encoded["threads"][0]["attachments"][0];
        assert_eq!(manifest["id"], accepted_file[0]["id"]);
        assert_eq!(manifest["submissionId"], "accepted-submission");
        assert_eq!(manifest["turnId"], "turn-a");
        assert_eq!(manifest["itemId"], "user-item-a");
        assert_eq!(encoded["blobs"].as_array().unwrap().len(), 1);
        assert_eq!(
            exported.blobs[0].read_verified_bytes().unwrap(),
            b"accepted bytes"
        );
        assert!(encoded["blobs"][0].get("dataBase64").is_none());
        assert!(encoded["blobs"][0].get("sourcePath").is_none());
        let serialized = serde_json::to_string(&encoded).unwrap();
        assert!(!serialized.contains("queued bytes"));
        assert!(!serialized.contains("draft bytes"));
        assert!(!serialized.contains("not in history"));

        host.connect().await.unwrap();
        let (one, one_blobs) = host
            .export_history_thread_with_blobs("thread-a", None)
            .await
            .unwrap();
        assert_eq!(one["attachments"].as_array().unwrap().len(), 1);
        assert_eq!(one["blobs"].as_array().unwrap().len(), 1);
        assert_eq!(
            one_blobs[0].read_verified_bytes().unwrap(),
            b"accepted bytes"
        );
        assert!(HistoryBlob::from_verified_path(
            one_blobs[0].verified_source_path().unwrap(),
            "0".repeat(64),
            one_blobs[0].size_bytes,
        )
        .is_err());
        let sha256 = one["attachments"][0]["sha256"].as_str().unwrap();
        let (attachment, blob) = host
            .history_attachment("thread-a", sha256)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attachment.submission_id, "accepted-submission");
        assert_eq!(blob.read_verified_bytes().unwrap(), b"accepted bytes");
        assert!(host
            .history_attachment("thread-a", &"f".repeat(64))
            .await
            .unwrap()
            .is_none());
        assert!(host
            .history_attachment("thread-a", "invalid")
            .await
            .is_err());
        let engine = host.engine().await.unwrap();
        let payload_text: String = sqlx::query_scalar(
            "SELECT payload FROM chat_submissions WHERE id='accepted-submission'",
        )
        .fetch_one(&engine.agents.chat.pool)
        .await
        .unwrap();
        let mut payload: Value = serde_json::from_str(&payload_text).unwrap();
        payload["turnId"] = Value::Null;
        sqlx::query("UPDATE chat_submissions SET payload=? WHERE id='accepted-submission'")
            .bind(payload.to_string())
            .execute(&engine.agents.chat.pool)
            .await
            .unwrap();
        drop(engine);
        std::fs::write(
            &legacy_mode,
            "legacy accepted receipt without runtime item IDs",
        )
        .unwrap();
        host.shutdown().await;
        let legacy_export = host.export_quiesced_history().await.unwrap();
        assert!(legacy_export.complete, "{:?}", legacy_export.failures);
        let legacy = &legacy_export.threads[0]["attachments"][0];
        assert!(legacy["turnId"].is_null());
        assert!(legacy["itemId"].is_null());

        host.connect().await.unwrap();
        let engine = host.engine().await.unwrap();
        let payload_text: String = sqlx::query_scalar(
            "SELECT payload FROM chat_submissions WHERE id='accepted-submission'",
        )
        .fetch_one(&engine.agents.chat.pool)
        .await
        .unwrap();
        let mut payload: Value = serde_json::from_str(&payload_text).unwrap();
        payload["turnId"] = json!("missing-turn");
        sqlx::query("UPDATE chat_submissions SET payload=? WHERE id='accepted-submission'")
            .bind(payload.to_string())
            .execute(&engine.agents.chat.pool)
            .await
            .unwrap();
        drop(engine);
        host.shutdown().await;
        let incomplete = host.export_quiesced_history().await.unwrap();
        assert!(!incomplete.complete);
        assert_eq!(incomplete.threads.len(), 1);
        assert!(incomplete.threads[0]["turns"].is_array());
        assert!(incomplete.threads[0]["attachments"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(incomplete
            .failures
            .iter()
            .any(|failure| failure.message.contains("turn is not in history")));

        host.connect().await.unwrap();
        let engine = host.engine().await.unwrap();
        let payload_text: String = sqlx::query_scalar(
            "SELECT payload FROM chat_submissions WHERE id='accepted-submission'",
        )
        .fetch_one(&engine.agents.chat.pool)
        .await
        .unwrap();
        let mut payload: Value = serde_json::from_str(&payload_text).unwrap();
        payload["turnId"] = json!("");
        sqlx::query("UPDATE chat_submissions SET payload=? WHERE id='accepted-submission'")
            .bind(payload.to_string())
            .execute(&engine.agents.chat.pool)
            .await
            .unwrap();
        drop(engine);
        host.shutdown().await;
        let malformed_turn = host.export_quiesced_history().await.unwrap();
        assert!(!malformed_turn.complete);
        assert!(malformed_turn
            .failures
            .iter()
            .any(|failure| failure.message.contains("invalid stored turn ID")));

        host.connect().await.unwrap();
        let engine = host.engine().await.unwrap();
        let payload_text: String = sqlx::query_scalar(
            "SELECT payload FROM chat_submissions WHERE id='accepted-submission'",
        )
        .fetch_one(&engine.agents.chat.pool)
        .await
        .unwrap();
        let mut payload: Value = serde_json::from_str(&payload_text).unwrap();
        payload["turnId"] = Value::Null;
        sqlx::query("UPDATE chat_submissions SET payload=? WHERE id='accepted-submission'")
            .bind(payload.to_string())
            .execute(&engine.agents.chat.pool)
            .await
            .unwrap();
        let blob_id: String = sqlx::query_scalar("SELECT blob_id FROM chat_attachments WHERE id=?")
            .bind(&attachment.id)
            .fetch_one(&engine.agents.chat.pool)
            .await
            .unwrap();
        drop(engine);
        let blob_path = host
            .data_dir()
            .join(CHAT_STORE_RELATIVE_PATH)
            .join("blobs")
            .join(blob_id);
        let original_blob = blob_path.with_extension("original");
        std::fs::rename(&blob_path, &original_blob).unwrap();
        std::os::unix::fs::symlink(&original_blob, &blob_path).unwrap();
        assert!(host.history_attachment("thread-a", sha256).await.is_err());
        host.shutdown().await;
    }

    #[test]
    fn old_history_export_without_blobs_deserializes_as_empty() {
        let old = json!({
            "schemaVersion":1,"ownerOrigin":"local-runtime","complete":true,
            "threads":[{"thread":{"id":"old"},"turns":[],"origin":"local-runtime"}],
            "failures":[]
        });
        let parsed: HistoryExport = serde_json::from_value(old).unwrap();
        assert!(parsed.blobs.is_empty());
    }

    #[test]
    fn legacy_attachment_without_turn_or_client_link_is_conversation_scoped() {
        let legacy_turns = json!([{
            "id":"turn-legacy",
            "items":[{"id":"legacy-user-item","type":"userMessage"}]
        }]);
        assert_eq!(
            correlate_submission_item("accepted-id", None, legacy_turns.as_array().unwrap())
                .unwrap(),
            (None, None)
        );
    }

    #[test]
    fn stored_turn_rejects_submission_client_id_on_a_different_turn() {
        let turns = json!([{
            "id":"some-turn",
            "items":[{"id":"legacy-user-item","type":"userMessage"}]
        }]);
        let contradictory = json!([
            {"id":"stored-turn","items":[{"id":"legacy-user-item","type":"userMessage"}]},
            {"id":"other-turn","items":[{"id":"linked-item","type":"userMessage","clientId":"accepted-id"}]}
        ]);
        assert!(correlate_submission_item(
            "accepted-id",
            Some("stored-turn"),
            contradictory.as_array().unwrap(),
        )
        .is_err());
        assert!(correlate_submission_item(
            "accepted-id",
            Some("missing-turn"),
            turns.as_array().unwrap(),
        )
        .is_err());
    }

    #[tokio::test]
    async fn attachment_limits_are_per_accepted_submission_not_thread_lifetime() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let chat_root = data_dir.join(CHAT_STORE_RELATIVE_PATH);
        let chat = crate::chat_state::ChatStore::open(chat_root).await.unwrap();
        let first_files = (0..HISTORY_ATTACHMENT_MAX_OWNER_FILES)
            .map(|index| {
                json!({
                    "name":format!("file-{index}.txt"),
                    "mime":"text/plain",
                    "data":"eA=="
                })
            })
            .collect::<Vec<_>>();
        let first_import = chat
            .dispatch_storage(
                "attachments.importBytes",
                json!({"ownerId":"thread-a","files":first_files}),
            )
            .await
            .unwrap();
        let first_ids = first_import
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["id"].clone())
            .collect::<Vec<_>>();
        let first = chat
            .dispatch_storage(
                "submissions.put",
                json!({
                    "id":"accepted-first","threadId":"thread-a","document":{"text":"first"},
                    "attachmentIds":first_ids,"settings":{},"expectedRevision":0
                }),
            )
            .await
            .unwrap();
        let dispatching = chat
            .dispatch_storage(
                "submissions.update",
                json!({"id":"accepted-first","expectedRevision":first["revision"],"status":"dispatching"}),
            )
            .await
            .unwrap();
        chat.dispatch_storage(
            "submissions.update",
            json!({"id":"accepted-first","expectedRevision":dispatching["revision"],"status":"accepted","turnId":"turn-first"}),
        )
        .await
        .unwrap();
        for id in first_import
            .as_array()
            .unwrap()
            .iter()
            .map(|file| &file["id"])
        {
            chat.dispatch_storage("attachments.remove", json!({"ownerId":"thread-a","id":id}))
                .await
                .unwrap();
        }

        let second_import = chat
            .dispatch_storage(
                "attachments.importBytes",
                json!({
                    "ownerId":"thread-a",
                    "files":[{"name":"later.txt","mime":"text/plain","data":"eA=="}]
                }),
            )
            .await
            .unwrap();
        let second = chat
            .dispatch_storage(
                "submissions.put",
                json!({
                    "id":"accepted-second","threadId":"thread-a","document":{"text":"second"},
                    "attachmentIds":[second_import[0]["id"]],"settings":{},"expectedRevision":0
                }),
            )
            .await
            .unwrap();
        let dispatching = chat
            .dispatch_storage(
                "submissions.update",
                json!({"id":"accepted-second","expectedRevision":second["revision"],"status":"dispatching"}),
            )
            .await
            .unwrap();
        chat.dispatch_storage(
            "submissions.update",
            json!({"id":"accepted-second","expectedRevision":dispatching["revision"],"status":"accepted","turnId":"turn-second"}),
        )
        .await
        .unwrap();

        let turns = json!([
            {"id":"turn-first","items":[{"id":"item-first","type":"userMessage","clientId":"accepted-first"}]},
            {"id":"turn-second","items":[{"id":"item-second","type":"userMessage","clientId":"accepted-second"}]}
        ]);
        let (attachments, blobs) =
            export_thread_attachments(&data_dir, &chat, "thread-a", turns.as_array().unwrap())
                .await
                .unwrap();
        assert_eq!(attachments.len(), 21);
        assert_eq!(blobs.len(), 1);
        assert_eq!(
            attachments
                .iter()
                .filter(|row| row.submission_id == "accepted-first")
                .count(),
            20
        );
        assert_eq!(
            attachments
                .iter()
                .filter(|row| row.submission_id == "accepted-second")
                .count(),
            1
        );
        chat.pool.close().await;
    }

    #[tokio::test]
    async fn inventory_counts_safe_local_rows_hashes_shared_rows_and_flags_unknown_tables() {
        let (dir, host) = host().await;
        let sessions = dir.path().join("data/engine/sessions/2026/11");
        std::fs::create_dir_all(&sessions).unwrap();
        let runtime_body = b"runtime transcript body must not be returned";
        std::fs::write(sessions.join("thread.jsonl"), runtime_body).unwrap();
        std::fs::write(
            dir.path().join("data/engine/auth.json"),
            "CREDENTIAL_MUST_NOT_LEAK",
        )
        .unwrap();
        let pool = host.stores.0.pool();
        sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES(?,?,?,?,?)")
            .bind("profile-id").bind(5_i64).bind("profile data").bind(1_i64).bind(2_i64)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO tasks(id,agent_id,title,instructions,acceptance_criteria,dependencies,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind("task-id").bind("agent-1").bind("task").bind("instructions").bind("criteria").bind("[]").bind("queued").bind(1_i64).bind(1_i64)
            .execute(pool).await.unwrap();
        sqlx::query("INSERT INTO task_runs(id,task_id,agent_id,worker_id,lease_token,state,created_at) VALUES(?,?,?,?,?,?,?)")
            .bind("run-id").bind("task-id").bind("agent-1").bind("worker").bind("INVENTORY_SECRET_LEASE").bind("running").bind(1_i64)
            .execute(pool).await.unwrap();
        sqlx::query("CREATE TABLE export_test_unknown (payload TEXT NOT NULL)")
            .execute(pool)
            .await
            .unwrap();

        let inventory = host.inventory().await.unwrap();
        let profile = &inventory.tables["agent/agent_profiles"];
        assert_eq!(profile.record_count, Some(1));
        assert!(profile
            .sha256
            .as_ref()
            .is_some_and(|digest| digest.len() == 64));
        let runs = &inventory.tables["agent/task_runs"];
        assert_eq!(runs.record_count, Some(1));
        assert_eq!(runs.classification, "owner-local-secret-excluded");
        assert!(runs.sha256.is_none());
        assert!(!inventory.complete);
        assert!(inventory
            .warnings
            .iter()
            .any(|warning| warning.contains("unclassified table `export_test_unknown`")));
        let history = inventory
            .runtime_files
            .iter()
            .find(|file| file.relative_path == "runtime-history/2026/11/thread.jsonl")
            .unwrap();
        assert_eq!(
            history.sha256,
            format!("{:x}", Sha256::digest(runtime_body))
        );
        let serialized = serde_json::to_string(&inventory).unwrap();
        assert!(!serialized.contains("INVENTORY_SECRET_LEASE"));
        assert!(!serialized.contains("CREDENTIAL_MUST_NOT_LEAK"));
        assert!(!serialized.contains("runtime transcript body must not be returned"));
        assert!(!serialized.contains("auth.json"));
    }

    #[tokio::test]
    async fn known_auth_file_exclusion_does_not_make_inventory_incomplete() {
        let (dir, host) = host().await;
        std::fs::create_dir_all(dir.path().join("data/engine/sessions")).unwrap();
        std::fs::write(
            dir.path().join("data/engine/auth.json"),
            "LOCAL_CREDENTIAL_MUST_NOT_LEAVE_THIS_NODE",
        )
        .unwrap();

        let inventory = host.inventory().await.unwrap();
        assert!(
            inventory.complete,
            "known local credentials are excluded by design: {:?}",
            inventory.warnings
        );
        let serialized = serde_json::to_string(&inventory).unwrap();
        assert!(!serialized.contains("LOCAL_CREDENTIAL_MUST_NOT_LEAVE_THIS_NODE"));
        assert!(!inventory
            .runtime_files
            .iter()
            .any(|file| file.relative_path.ends_with("auth.json")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_upload_metadata_and_stage_are_backed_up_as_owner_local_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, host) = host().await;
        let upload_id = "9fd8c4bd-6020-44a8-84d4-85a763a1dfd7";
        let uploads = host
            .data_dir()
            .join(CHAT_STORE_RELATIVE_PATH)
            .join("uploads");
        let stage = uploads.join(format!("{upload_id}.part"));
        std::fs::write(&stage, b"pending upload bytes").unwrap();
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = json!({
            "ownerId":"draft-owner",
            "uploadId":upload_id,
            "name":"pending.txt",
            "mime":"text/plain",
            "sizeBytes":20,
            "sha256":format!("{:x}", Sha256::digest(b"pending upload bytes")),
        });
        let host = host;
        let pool = &host.chat_store.pool;
        sqlx::query("INSERT INTO chat_attachment_uploads(upload_id,owner_id,metadata,attachment_id,blob_id,state,receipt,created_at,updated_at) VALUES(?,?,?,'attachment-reserved','blob-reserved','pending',NULL,'1','1')")
            .bind(upload_id)
            .bind("draft-owner")
            .bind(metadata.to_string())
            .execute(pool)
            .await
            .unwrap();

        let inventory = host.inventory().await.unwrap();
        let stage_inventory = inventory
            .runtime_files
            .iter()
            .find(|file| file.relative_path == format!("app-attachment-upload/{upload_id}.part"))
            .unwrap();
        assert_eq!(stage_inventory.classification, "app-attachment-upload");
        assert_eq!(stage_inventory.sha256, format!("{:x}", Sha256::digest(b"pending upload bytes")));
        assert_eq!(inventory.tables["chat/chat_attachment_uploads"].classification, "owner-local-upload-state");
        assert_eq!(inventory.tables["chat/chat_attachment_owner_tombstones"].classification, "owner-local-upload-state");

        let snapshot = host.store_snapshot().await.unwrap();
        assert!(snapshot.shared.get("chat_attachment_uploads").is_none());
        assert_eq!(snapshot.owner_local["chat/chat_attachment_uploads"].classification, "owner-local-upload-state");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attachment_snapshot_reads_the_live_chat_store_created_through_host_api() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("owned-runtime");
        std::fs::write(
            &runtime,
            "#!/usr/bin/env python3\nimport json,sys\nfor line in sys.stdin:\n m=json.loads(line)\n if 'id' in m: print(json.dumps({'id':m['id'],'result':{'account':None,'data':[],'nextCursor':None,'capabilities':{}}}),flush=True)\n",
        )
        .unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let data_dir = dir.path().join("data");
        let host = Host::open(
            HostConfig {
                data_dir: data_dir.clone(),
                runtime_path: runtime,
            },
            Arc::new(|_: Value| {}),
        )
        .await
        .unwrap();
        host.connect().await.unwrap();
        let bytes = b"payload";
        let imported = host
            .chat_request(
                "attachments.importBytes",
                json!({
                    "ownerId":"thread-imported-qa",
                    "files":[{"name":"notes.txt","mime":"text/plain","data":"cGF5bG9hZA=="}]
                }),
            )
            .await
            .unwrap();
        let attachment_id = imported[0]["id"].as_str().unwrap();
        let live_store = data_dir.join(CHAT_STORE_RELATIVE_PATH);
        assert!(live_store.join("chat.sqlite3").is_file());
        let chat_pool = readonly_pool(&live_store.join("chat.sqlite3"))
            .await
            .unwrap();
        let blob_id: String = sqlx::query_scalar("SELECT blob_id FROM chat_attachments WHERE id=?")
            .bind(attachment_id)
            .fetch_one(&chat_pool)
            .await
            .unwrap();
        chat_pool.close().await;
        let snapshot = host.store_snapshot().await.unwrap();
        let blob = snapshot.shared["chat_blobs"]
            .iter()
            .find(|row| row["id"] == blob_id)
            .unwrap();
        assert_eq!(blob["sha256"], format!("{:x}", Sha256::digest(bytes)));
        assert!(snapshot.shared["chat_attachments"]
            .iter()
            .any(|row| row["id"] == attachment_id));
        let inventory = host.inventory().await.unwrap();
        let file = inventory
            .runtime_files
            .iter()
            .find(|file| file.relative_path == format!("app-attachment-blob/{blob_id}"))
            .unwrap();
        assert_eq!(file.sha256, format!("{:x}", Sha256::digest(bytes)));
        assert_eq!(
            std::fs::read(live_store.join("blobs").join(blob_id)).unwrap(),
            bytes
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("payload"));
        host.shutdown().await;
    }

    #[tokio::test]
    async fn populated_legacy_chat_path_makes_export_and_inventory_incomplete() {
        let (dir, host) = host().await;
        let legacy = dir.path().join("data/engine/chat");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("chat.sqlite3"), b"legacy chat database").unwrap();
        let snapshot = host.store_snapshot().await.unwrap();
        let inventory = host.inventory().await.unwrap();
        assert!(!snapshot.complete);
        assert!(!inventory.complete);
        assert!(snapshot
            .warnings
            .iter()
            .chain(inventory.warnings.iter())
            .any(|warning| warning.contains("legacy chat store")));
    }
}
