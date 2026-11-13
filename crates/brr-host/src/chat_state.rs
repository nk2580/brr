use crate::attachment_upload::{
    validate_metadata as validate_upload_metadata, validate_staged_path, AttachmentUploadMetadata,
    AttachmentUploadRecord, AttachmentUploadState,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_FILE: u64 = 50 * 1024 * 1024;
const MAX_OWNER: i64 = 200 * 1024 * 1024;
const MAX_FILES: i64 = 20;
const MAX_PREVIEW: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ChatStore {
    pub(crate) pool: SqlitePool,
    root: PathBuf,
}

impl ChatStore {
    pub(crate) async fn open(root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        ensure_real_directory(&root)?;
        let root = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
        let blobs = root.join("blobs");
        std::fs::create_dir_all(&blobs).map_err(|e| e.to_string())?;
        ensure_real_directory(&blobs)?;
        let uploads = root.join("uploads");
        std::fs::create_dir_all(&uploads).map_err(|e| e.to_string())?;
        ensure_real_directory(&uploads)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&uploads, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let options = SqliteConnectOptions::new()
            .filename(root.join("chat.sqlite3"))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| e.to_string())?;
        for query in [
            "CREATE TABLE IF NOT EXISTS chat_drafts(id TEXT PRIMARY KEY, thread_id TEXT, project_id TEXT, agent_id TEXT, document TEXT NOT NULL, settings TEXT NOT NULL, revision INTEGER NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_blobs(id TEXT PRIMARY KEY, size INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_attachments(id TEXT PRIMARY KEY, blob_id TEXT NOT NULL REFERENCES chat_blobs(id), name TEXT NOT NULL, mime TEXT NOT NULL, size INTEGER NOT NULL, kind TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_attachment_grants(owner_id TEXT NOT NULL, attachment_id TEXT NOT NULL REFERENCES chat_attachments(id) ON DELETE CASCADE, PRIMARY KEY(owner_id,attachment_id))",
            "CREATE TABLE IF NOT EXISTS chat_attachment_uploads(upload_id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,metadata TEXT NOT NULL,attachment_id TEXT NOT NULL,blob_id TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','committed','cancelled')),receipt TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_submissions(id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, payload TEXT NOT NULL, status TEXT NOT NULL, revision INTEGER NOT NULL, created_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_queue_state(thread_id TEXT PRIMARY KEY, paused INTEGER NOT NULL DEFAULT 0, reason TEXT)",
            "CREATE TABLE IF NOT EXISTS chat_branch_lineage(thread_id TEXT PRIMARY KEY,lineage TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_assignment_context(operation_id TEXT PRIMARY KEY,assignment_owner TEXT NOT NULL,payload TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_operations(id TEXT PRIMARY KEY, payload TEXT NOT NULL, revision INTEGER NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_recovery_map(source_kind TEXT NOT NULL, source_id TEXT NOT NULL, operation_id TEXT NOT NULL UNIQUE, draft_id TEXT NOT NULL, source_thread_id TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(source_kind,source_id))",
            "CREATE TABLE IF NOT EXISTS chat_deleted_threads(thread_id TEXT PRIMARY KEY NOT NULL,deleted_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_attachment_owner_tombstones(owner_id TEXT PRIMARY KEY NOT NULL,closed_at TEXT NOT NULL)",
        ] { sqlx::query(query).execute(&pool).await.map_err(|e| e.to_string())?; }
        // A process restart makes any in-flight send outcome unknowable. Keep it durable and
        // stop the thread queue until the host can reconcile it with the runtime.
        sqlx::query("UPDATE chat_submissions SET status='uncertain', revision=revision+1, payload=json_set(payload,'$.status','uncertain','$.revision',revision+1,'$.error','Application restarted during dispatch') WHERE status='dispatching'").execute(&pool).await.map_err(db)?;
        sqlx::query("INSERT INTO chat_queue_state(thread_id,paused,reason) SELECT DISTINCT thread_id,1,'Application restarted with pending submissions' FROM chat_submissions WHERE status IN ('queued','uncertain') ON CONFLICT(thread_id) DO UPDATE SET paused=1,reason='Application restarted with pending submissions'").execute(&pool).await.map_err(db)?;
        Ok(Self { pool, root })
    }

    pub(crate) async fn dispatch_storage(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match method {
            "drafts.list" => self.drafts_list().await,
            "drafts.get" => self.draft_get(required(&params, "id")?).await,
            "drafts.save" => self.draft_save(params).await,
            "drafts.promote" => self.draft_promote(params).await,
            "drafts.delete" => self.draft_delete(required(&params, "id")?).await,
            "thread.deleted" => self.thread_deleted(required(&params, "threadId")?).await,
            "attachments.import" => self.attachments_import(&params).await,
            "attachments.importBytes" => self.attachments_import_bytes(&params).await,
            "attachments.upload.reserve" => {
                let metadata: AttachmentUploadMetadata = serde_json::from_value(params)
                    .map_err(|_| "Invalid attachment upload metadata")?;
                serde_json::to_value(self.attachment_upload_begin(metadata).await?)
                    .map_err(db)
            }
            "attachments.upload.status" => {
                let owner = required(&params, "ownerId")?;
                let upload = required(&params, "uploadId")?;
                match self.attachment_upload_status(owner, upload).await? {
                    Some(record) => serde_json::to_value(record).map_err(db),
                    None => Err("Attachment upload not found".into()),
                }
            }
            "attachments.upload.commit" => {
                let owner = required(&params, "ownerId")?;
                let upload = required(&params, "uploadId")?;
                let path = PathBuf::from(required(&params, "sourcePath")?);
                serde_json::to_value(self.attachment_upload_commit(owner, upload, &path).await?)
                    .map_err(db)
            }
            "attachments.upload.cancel" => {
                let owner = required(&params, "ownerId")?;
                let upload = required(&params, "uploadId")?;
                serde_json::to_value(self.attachment_upload_cancel(owner, upload).await?)
                    .map_err(db)
            }
            "attachments.list" => self.attachments_list(required(&params, "ownerId")?).await,
            "attachments.preview" => self.attachment_preview(&params).await,
            "attachments.remove" => self.attachment_remove(&params).await,
            "submissions.put" => self.submission_put(params).await,
            "submissions.get" => self.submission_get(required(&params, "id")?).await,
            "submissions.list" => self.submission_list(params["threadId"].as_str()).await,
            "submissions.update" => self.submission_update(params).await,
            "recovery.restoreDraft" => self.recovery_restore_draft(params).await,
            "recovery.cancelQueued" => self.recovery_cancel_queued(params).await,
            "queue.state" => self.queue_state(required(&params, "threadId")?).await,
            "queue.pause" => self.queue_set(&params, true).await,
            "queue.resume" => self.queue_set(&params, false).await,
            "operations.put" => self.operation_put(params).await,
            "operations.get" => self.operation_get(required(&params, "id")?).await,
            "operations.update" => self.operation_update(params).await,
            "operations.list" => self.operation_list().await,
            _ => Err(format!("Unknown chat storage method: {method}")),
        }
    }

    async fn drafts_list(&self) -> Result<Value, String> {
        let rows = sqlx::query("SELECT id,thread_id,project_id,agent_id,document,settings,revision,updated_at FROM chat_drafts WHERE thread_id IS NULL ORDER BY updated_at DESC").fetch_all(&self.pool).await.map_err(db)?;
        let mut out = Vec::new();
        for row in rows {
            let v = draft_json(&row)?;
            if !is_empty_document(&v["document"]) {
                out.push(v);
            }
        }
        Ok(json!(out))
    }
    async fn draft_get(&self, id: &str) -> Result<Value, String> {
        validate_id(id)?;
        let row = sqlx::query("SELECT id,thread_id,project_id,agent_id,document,settings,revision,updated_at FROM chat_drafts WHERE id=?").bind(id).fetch_optional(&self.pool).await.map_err(db)?.ok_or("Draft not found")?;
        draft_json(&row)
    }
    async fn draft_save(&self, mut p: Value) -> Result<Value, String> {
        let id = p["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_id(&id)?;
        let expected = p["expectedRevision"]
            .as_i64()
            .ok_or("expectedRevision is required")?;
        let document = p.get("document").cloned().ok_or("document is required")?;
        let settings = p.get("settings").cloned().unwrap_or_else(|| json!({}));
        let now = now();
        let doc = document.to_string();
        let settings = settings.to_string();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        if let Some(thread_id) = p["threadId"].as_str().filter(|id| !id.is_empty()) {
            let deleted: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM chat_deleted_threads WHERE thread_id=?)",
            )
            .bind(thread_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            if deleted {
                return Err("This chat was deleted and cannot accept saved work".into());
            }
        }
        if expected == 0 {
            sqlx::query("INSERT INTO chat_drafts(id,thread_id,project_id,agent_id,document,settings,revision,updated_at) VALUES(?,?,?,?,?,?,1,?)")
                .bind(&id).bind(p["threadId"].as_str()).bind(p["projectId"].as_str()).bind(p["agentId"].as_str()).bind(doc).bind(settings).bind(&now).execute(&mut *tx).await.map_err(|e| if e.to_string().contains("UNIQUE") { "Draft revision conflict".to_string() } else { db(e) })?;
        } else {
            let result = sqlx::query("UPDATE chat_drafts SET thread_id=?,project_id=?,agent_id=?,document=?,settings=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?")
                .bind(p["threadId"].as_str()).bind(p["projectId"].as_str()).bind(p["agentId"].as_str()).bind(doc).bind(settings).bind(&now).bind(&id).bind(expected).execute(&mut *tx).await.map_err(db)?;
            if result.rows_affected() != 1 {
                return Err("Draft revision conflict".into());
            }
        }
        tx.commit().await.map_err(db)?;
        p["id"] = json!(id);
        p["revision"] = json!(expected + 1);
        p["updatedAt"] = json!(now);
        p.as_object_mut().unwrap().remove("expectedRevision");
        Ok(p)
    }
    async fn draft_promote(&self, p: Value) -> Result<Value, String> {
        let id = required(&p, "id")?;
        let thread = required(&p, "threadId")?;
        validate_id(id)?;
        validate_id(thread)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        let pending_uploads: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM chat_attachment_uploads WHERE owner_id=? AND state='pending'",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if pending_uploads > 0 {
            return Err("Wait for this draft's attachment uploads to finish or cancel them before promoting it".into());
        }
        let deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_deleted_threads WHERE thread_id=?)",
        )
        .bind(thread)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if deleted {
            return Err("This chat was deleted and cannot receive saved work".into());
        }
        let result = sqlx::query(
            "UPDATE chat_drafts SET id=?,thread_id=?,revision=revision+1,updated_at=? WHERE id=?",
        )
        .bind(thread)
        .bind(thread)
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            return Err("Draft not found".into());
        }
        sqlx::query("INSERT OR IGNORE INTO chat_attachment_owner_tombstones(owner_id,closed_at) VALUES(?,?)")
            .bind(id).bind(now()).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("UPDATE chat_attachment_uploads SET state='cancelled',receipt=NULL,updated_at=? WHERE owner_id=? AND state='committed'")
            .bind(now()).bind(id).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT OR IGNORE INTO chat_attachment_grants(owner_id,attachment_id) SELECT ?,attachment_id FROM chat_attachment_grants WHERE owner_id=?").bind(thread).bind(id).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("DELETE FROM chat_attachment_grants WHERE owner_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("UPDATE chat_recovery_map SET draft_id=?,updated_at=? WHERE draft_id=?")
            .bind(thread)
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        self.draft_get(thread).await
    }
    async fn draft_delete(&self, id: &str) -> Result<Value, String> {
        validate_id(id)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        sqlx::query("INSERT OR IGNORE INTO chat_attachment_owner_tombstones(owner_id,closed_at) VALUES(?,?)")
            .bind(id).bind(now()).execute(&mut *tx).await.map_err(db)?;
        let upload_ids: Vec<String> = sqlx::query_scalar(
            "SELECT upload_id FROM chat_attachment_uploads WHERE owner_id=? AND state='pending'",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let blobs: Vec<String> = sqlx::query_scalar("SELECT a.blob_id FROM chat_attachments a JOIN chat_attachment_grants g ON a.id=g.attachment_id WHERE g.owner_id=? AND (SELECT COUNT(*) FROM chat_attachment_grants other WHERE other.attachment_id=a.id)=1")
            .bind(id).fetch_all(&mut *tx).await.map_err(db)?;
        sqlx::query("DELETE FROM chat_attachments WHERE id IN (SELECT attachment_id FROM chat_attachment_grants WHERE owner_id=?) AND (SELECT COUNT(*) FROM chat_attachment_grants other WHERE other.attachment_id=chat_attachments.id)=1")
            .bind(id).execute(&mut *tx).await.map_err(db)?;
        for blob in &blobs {
            sqlx::query("DELETE FROM chat_blobs WHERE id=?")
                .bind(blob)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        sqlx::query("DELETE FROM chat_attachment_grants WHERE owner_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("UPDATE chat_attachment_uploads SET state='cancelled',receipt=NULL,updated_at=? WHERE owner_id=? AND state IN ('pending','committed')")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let r = sqlx::query("DELETE FROM chat_drafts WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        self.remove_upload_stages(&upload_ids);
        for blob in blobs {
            let _ = std::fs::remove_file(self.root.join("blobs").join(blob));
        }
        Ok(json!({"deleted":r.rows_affected()>0}))
    }

    async fn thread_deleted(&self, thread_id: &str) -> Result<Value, String> {
        validate_id(thread_id)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        sqlx::query("INSERT OR IGNORE INTO chat_deleted_threads(thread_id,deleted_at) VALUES(?,?)")
            .bind(thread_id)
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let mut draft_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM chat_drafts WHERE thread_id=?")
                .bind(thread_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(db)?;
        let recovery_drafts: Vec<String> =
            sqlx::query_scalar("SELECT d.id FROM chat_recovery_map m JOIN chat_drafts d ON d.id=m.draft_id WHERE m.source_thread_id=? AND (d.thread_id IS NULL OR d.thread_id=?)")
                .bind(thread_id)
                .bind(thread_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(db)?;
        draft_ids.extend(recovery_drafts);
        draft_ids.sort();
        draft_ids.dedup();
        let submission_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM chat_submissions WHERE thread_id=?")
                .bind(thread_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(db)?;
        let mut operation_ids: Vec<String> = sqlx::query_scalar(
            "SELECT operation_id FROM chat_recovery_map WHERE source_thread_id=?",
        )
        .bind(thread_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        operation_ids.extend(
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM chat_operations WHERE json_extract(payload,'$.threadId')=? OR json_extract(payload,'$.result.thread.id')=? OR json_extract(payload,'$.forkResult.thread.id')=?",
            )
            .bind(thread_id)
            .bind(thread_id)
            .bind(thread_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(db)?,
        );
        operation_ids.sort();
        operation_ids.dedup();

        let mut owners = vec![thread_id.to_owned()];
        owners.extend(draft_ids.iter().cloned());
        owners.extend(submission_ids.iter().cloned());
        for owner in &owners {
            sqlx::query("INSERT OR IGNORE INTO chat_attachment_owner_tombstones(owner_id,closed_at) VALUES(?,?)")
                .bind(owner).bind(now()).execute(&mut *tx).await.map_err(db)?;
        }
        let mut upload_ids = Vec::new();
        for owner in &owners {
            let mut owner_uploads: Vec<String> = sqlx::query_scalar(
                "SELECT upload_id FROM chat_attachment_uploads WHERE owner_id=? AND state IN ('pending','committed')",
            )
            .bind(owner)
            .fetch_all(&mut *tx)
            .await
            .map_err(db)?;
            upload_ids.append(&mut owner_uploads);
            sqlx::query("UPDATE chat_attachment_uploads SET state='cancelled',receipt=NULL,updated_at=? WHERE owner_id=? AND state IN ('pending','committed')")
                .bind(now())
                .bind(owner)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        let mut blobs = Vec::new();
        for owner in &owners {
            let owner_blobs: Vec<String> = sqlx::query_scalar(
                "SELECT a.blob_id FROM chat_attachments a JOIN chat_attachment_grants g ON a.id=g.attachment_id WHERE g.owner_id=? AND (SELECT COUNT(*) FROM chat_attachment_grants other WHERE other.attachment_id=a.id)=1",
            )
            .bind(owner)
            .fetch_all(&mut *tx)
            .await
            .map_err(db)?;
            blobs.extend(owner_blobs);
            sqlx::query("DELETE FROM chat_attachments WHERE id IN (SELECT attachment_id FROM chat_attachment_grants WHERE owner_id=?) AND (SELECT COUNT(*) FROM chat_attachment_grants other WHERE other.attachment_id=chat_attachments.id)=1")
                .bind(owner).execute(&mut *tx).await.map_err(db)?;
            sqlx::query("DELETE FROM chat_attachment_grants WHERE owner_id=?")
                .bind(owner)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        blobs.sort();
        blobs.dedup();
        for blob in &blobs {
            sqlx::query("DELETE FROM chat_blobs WHERE id=?")
                .bind(blob)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        for draft_id in &draft_ids {
            sqlx::query("DELETE FROM chat_drafts WHERE id=?")
                .bind(draft_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        sqlx::query("DELETE FROM chat_submissions WHERE thread_id=?")
            .bind(thread_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM chat_queue_state WHERE thread_id=?")
            .bind(thread_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM chat_branch_lineage WHERE thread_id=?")
            .bind(thread_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM chat_recovery_map WHERE source_thread_id=?")
            .bind(thread_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        for operation_id in operation_ids {
            sqlx::query("DELETE FROM chat_assignment_context WHERE operation_id=?")
                .bind(&operation_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            sqlx::query("DELETE FROM chat_operations WHERE id=?")
                .bind(operation_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        self.remove_upload_stages(&upload_ids);
        for blob in blobs {
            let _ = std::fs::remove_file(self.root.join("blobs").join(blob));
        }
        Ok(json!({"deleted":true}))
    }

    fn remove_upload_stages(&self, upload_ids: &[String]) {
        let directory = self.root.join("uploads");
        let Ok(metadata) = std::fs::symlink_metadata(&directory) else {
            return;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return;
        }
        let mut changed = false;
        for upload_id in upload_ids {
            let Ok(uuid) = uuid::Uuid::parse_str(upload_id) else {
                continue;
            };
            if uuid.hyphenated().to_string() != *upload_id {
                continue;
            }
            let path = directory.join(format!("{upload_id}.part"));
            let Ok(file_metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if (file_metadata.is_file() || file_metadata.file_type().is_symlink())
                && std::fs::remove_file(path).is_ok()
            {
                changed = true;
            }
        }
        if changed {
            let _ = sync_directory(&directory);
        }
    }

    async fn attachments_import(&self, p: &Value) -> Result<Value, String> {
        let owner = required(p, "ownerId")?;
        validate_id(owner)?;
        let paths = p["paths"].as_array().ok_or("paths must be an array")?;
        if paths.is_empty() || paths.len() > MAX_FILES as usize {
            return Err("Import must contain 1 to 20 files".into());
        }
        let mut result = Vec::new();
        for path in paths {
            let src = PathBuf::from(path.as_str().ok_or("path must be text")?);
            result.push(self.import_path(owner, &src).await?);
        }
        Ok(json!(result))
    }
    async fn attachments_import_bytes(&self, p: &Value) -> Result<Value, String> {
        let owner = required(p, "ownerId")?;
        validate_id(owner)?;
        let files = p["files"].as_array().ok_or("files must be an array")?;
        if files.is_empty() || files.len() > MAX_FILES as usize {
            return Err("Import must contain 1 to 20 files".into());
        }
        let mut out = Vec::new();
        for f in files {
            let name = safe_name(required(f, "name")?)?;
            let encoded = required(f, "data")?;
            let bytes = decode_base64(encoded)?;
            if bytes.len() as u64 > MAX_FILE {
                return Err("Attachment exceeds 50 MiB".into());
            }
            let kind = sniff_kind(&bytes);
            let mime = if kind == "image" {
                sniff_mime(&bytes).to_owned()
            } else {
                safe_mime(f["mime"].as_str().unwrap_or("application/octet-stream"))?
            };
            out.push(self.import_bytes(owner, &name, &mime, &bytes, kind).await?);
        }
        Ok(json!(out))
    }
    async fn import_path(&self, owner: &str, path: &Path) -> Result<Value, String> {
        let md = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !md.file_type().is_file() {
            return Err("Only regular files can be imported".into());
        }
        if md.len() > MAX_FILE {
            return Err("Attachment exceeds 50 MiB".into());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000);
        }
        let mut input = options.open(path).map_err(|e| e.to_string())?;
        if !input.metadata().map_err(db)?.is_file() {
            return Err("Choose a regular local file".into());
        }
        let name = safe_name(
            path.file_name()
                .and_then(|n| n.to_str())
                .ok_or("Invalid file name")?,
        )?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        let closed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_attachment_owner_tombstones WHERE owner_id=?) OR EXISTS(SELECT 1 FROM chat_deleted_threads WHERE thread_id=?)",
        ).bind(owner).bind(owner).fetch_one(&mut *tx).await.map_err(db)?;
        if closed { return Err("This attachment owner was deleted or promoted and cannot accept new imports".into()); }
        let usage: (i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?)+(SELECT COUNT(*) FROM chat_attachment_uploads WHERE owner_id=? AND state='pending'),(SELECT COALESCE(SUM(a.size),0) FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?)+(SELECT COALESCE(SUM(json_extract(metadata,'$.sizeBytes')),0) FROM chat_attachment_uploads WHERE owner_id=? AND state='pending')")
            .bind(owner).bind(owner).bind(owner).bind(owner).fetch_one(&mut *tx).await.map_err(db)?;
        if usage.0 >= MAX_FILES {
            return Err("Owner already has 20 attachments".into());
        }
        if usage.1 + md.len() as i64 > MAX_OWNER {
            return Err("Owner attachment total exceeds 200 MiB".into());
        }
        let blob = uuid::Uuid::new_v4().to_string();
        let id = uuid::Uuid::new_v4().to_string();
        let blobs = self.root.join("blobs");
        let temp = blobs.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let final_path = blobs.join(&blob);
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        let size = match std::io::copy(
            &mut std::io::Read::by_ref(&mut input).take(MAX_FILE + 1),
            &mut output,
        ) {
            Ok(size) if size <= MAX_FILE => size,
            Ok(_) => {
                let _ = std::fs::remove_file(&temp);
                return Err("Attachment exceeds 50 MiB".into());
            }
            Err(e) => {
                let _ = std::fs::remove_file(&temp);
                return Err(e.to_string());
            }
        };
        if let Err(e) = output.sync_all() {
            let _ = std::fs::remove_file(&temp);
            return Err(e.to_string());
        }
        drop(output);
        let mut header = [0u8; 16];
        let header_len = std::fs::File::open(&temp)
            .and_then(|mut f| f.read(&mut header))
            .map_err(|e| e.to_string())?;
        let kind = sniff_kind(&header[..header_len]);
        let mime = if kind == "image" {
            sniff_mime(&header[..header_len]).to_owned()
        } else {
            "application/octet-stream".into()
        };
        if usage.1 + size as i64 > MAX_OWNER {
            let _ = std::fs::remove_file(&temp);
            return Err("Owner attachment total exceeds 200 MiB".into());
        }
        if let Err(e) = std::fs::rename(&temp, &final_path) {
            let _ = std::fs::remove_file(&temp);
            return Err(e.to_string());
        }
        let save = async {
            sqlx::query("INSERT INTO chat_blobs(id,size) VALUES(?,?)")
                .bind(&blob)
                .bind(size as i64)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO chat_attachments(id,blob_id,name,mime,size,kind) VALUES(?,?,?,?,?,?)",
            )
            .bind(&id)
            .bind(&blob)
            .bind(&name)
            .bind(&mime)
            .bind(size as i64)
            .bind(kind)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO chat_attachment_grants(owner_id,attachment_id) VALUES(?,?)")
                .bind(owner)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            Ok::<(), sqlx::Error>(())
        }
        .await;
        if let Err(e) = save {
            let _ = std::fs::remove_file(&final_path);
            return Err(e.to_string());
        }
        if let Err(e) = tx.commit().await {
            // Commit errors are ambiguous: SQLite may have made the grant
            // durable even if the client lost the acknowledgement. Preserve
            // the content so a committed attachment never points at missing
            // bytes; an orphan can be collected after registry reconciliation.
            return Err(db(e));
        }
        Ok(json!({"id":id,"name":name,"mime":mime,"size":size,"kind":kind,"status":"ready"}))
    }
    async fn import_bytes(
        &self,
        owner: &str,
        name: &str,
        mime: &str,
        bytes: &[u8],
        kind: &str,
    ) -> Result<Value, String> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        let closed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_attachment_owner_tombstones WHERE owner_id=?) OR EXISTS(SELECT 1 FROM chat_deleted_threads WHERE thread_id=?)",
        ).bind(owner).bind(owner).fetch_one(&mut *tx).await.map_err(db)?;
        if closed { return Err("This attachment owner was deleted or promoted and cannot accept new imports".into()); }
        let usage: (i64,i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?)+(SELECT COUNT(*) FROM chat_attachment_uploads WHERE owner_id=? AND state='pending'),(SELECT COALESCE(SUM(a.size),0) FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?)+(SELECT COALESCE(SUM(json_extract(metadata,'$.sizeBytes')),0) FROM chat_attachment_uploads WHERE owner_id=? AND state='pending')").bind(owner).bind(owner).bind(owner).bind(owner).fetch_one(&mut *tx).await.map_err(db)?;
        if usage.0 >= MAX_FILES {
            return Err("Owner already has 20 attachments".into());
        }
        if usage.1 + bytes.len() as i64 > MAX_OWNER {
            return Err("Owner attachment total exceeds 200 MiB".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let blob = uuid::Uuid::new_v4().to_string();
        let final_path = self.root.join("blobs").join(&blob);
        let temp = self
            .root
            .join("blobs")
            .join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
            let _ = std::fs::remove_file(&temp);
            return Err(e.to_string());
        }
        drop(f);
        if let Err(e) = std::fs::rename(&temp, &final_path) {
            let _ = std::fs::remove_file(&temp);
            return Err(e.to_string());
        }
        let save = async {
            sqlx::query("INSERT INTO chat_blobs(id,size) VALUES(?,?)")
                .bind(&blob)
                .bind(bytes.len() as i64)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO chat_attachments(id,blob_id,name,mime,size,kind) VALUES(?,?,?,?,?,?)",
            )
            .bind(&id)
            .bind(&blob)
            .bind(name)
            .bind(mime)
            .bind(bytes.len() as i64)
            .bind(kind)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO chat_attachment_grants(owner_id,attachment_id) VALUES(?,?)")
                .bind(owner)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            Ok::<(), sqlx::Error>(())
        }
        .await;
        if let Err(e) = save {
            let _ = std::fs::remove_file(final_path);
            return Err(e.to_string());
        }
        if let Err(e) = tx.commit().await {
            // As above, never unlink content after an ambiguous commit result.
            return Err(db(e));
        }
        Ok(json!({"id":id,"name":name,"mime":mime,"size":bytes.len(),"kind":kind,"status":"ready"}))
    }
    pub(crate) async fn attachment_upload_begin(
        &self,
        metadata: AttachmentUploadMetadata,
    ) -> Result<AttachmentUploadRecord, String> {
        validate_upload_metadata(&metadata, MAX_FILE)?;
        safe_name(&metadata.name)?;
        safe_mime(&metadata.mime)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        if let Some(row) = sqlx::query("SELECT owner_id,metadata,state,receipt FROM chat_attachment_uploads WHERE upload_id=?")
            .bind(&metadata.upload_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?
        {
            let owner: String = row.try_get("owner_id").map_err(db)?;
            let encoded: String = row.try_get("metadata").map_err(db)?;
            let existing: AttachmentUploadMetadata = serde_json::from_str(&encoded).map_err(db)?;
            if owner != metadata.owner_id || existing != metadata {
                return Err("Upload ID is already reserved for different attachment metadata".into());
            }
            let record = upload_record_from_row(row)?;
            tx.commit().await.map_err(db)?;
            return Ok(record);
        }
        let closed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_attachment_owner_tombstones WHERE owner_id=?) OR EXISTS(SELECT 1 FROM chat_deleted_threads WHERE thread_id=?)",
        )
        .bind(&metadata.owner_id)
        .bind(&metadata.owner_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if closed {
            return Err("This attachment owner was deleted or promoted and cannot accept new uploads".into());
        }
        let usage: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*),COALESCE(SUM(a.size),0),(SELECT COUNT(*) FROM chat_attachment_uploads WHERE owner_id=? AND state='pending'),(SELECT COALESCE(SUM(json_extract(metadata,'$.sizeBytes')),0) FROM chat_attachment_uploads WHERE owner_id=? AND state='pending') FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?",
        )
        .bind(&metadata.owner_id).bind(&metadata.owner_id).bind(&metadata.owner_id)
        .fetch_one(&mut *tx).await.map_err(db)?;
        if usage.0 + usage.2 >= MAX_FILES {
            return Err("Owner already has 20 attachments or reserved uploads".into());
        }
        if usage.1 + usage.3 + metadata.size_bytes as i64 > MAX_OWNER {
            return Err("Owner attachment total exceeds 200 MiB".into());
        }
        let record = AttachmentUploadRecord {
            metadata: metadata.clone(),
            state: AttachmentUploadState::Pending,
            attachments: Vec::new(),
        };
        let now = now();
        sqlx::query("INSERT INTO chat_attachment_uploads(upload_id,owner_id,metadata,attachment_id,blob_id,state,receipt,created_at,updated_at) VALUES(?,?,?, ?,?,'pending',NULL,?,?)")
            .bind(&metadata.upload_id).bind(&metadata.owner_id)
            .bind(serde_json::to_string(&metadata).map_err(db)?)
            .bind(uuid::Uuid::new_v4().to_string()).bind(uuid::Uuid::new_v4().to_string())
            .bind(&now).bind(&now).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(record)
    }

    pub(crate) async fn attachment_upload_status(
        &self,
        owner: &str,
        upload_id: &str,
    ) -> Result<Option<AttachmentUploadRecord>, String> {
        validate_upload_owner(owner, upload_id)?;
        let row = sqlx::query("SELECT owner_id,metadata,state,receipt FROM chat_attachment_uploads WHERE upload_id=? AND owner_id=?")
            .bind(upload_id).bind(owner).fetch_optional(&self.pool).await.map_err(db)?;
        row.map(upload_record_from_row).transpose()
    }

    pub(crate) async fn attachment_upload_cancel(
        &self,
        owner: &str,
        upload_id: &str,
    ) -> Result<AttachmentUploadRecord, String> {
        validate_upload_owner(owner, upload_id)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        let row = sqlx::query("SELECT owner_id,metadata,state,receipt FROM chat_attachment_uploads WHERE upload_id=? AND owner_id=?")
            .bind(upload_id).bind(owner).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or("Attachment upload not found")?;
        let mut record = upload_record_from_row(row)?;
        match record.state {
            AttachmentUploadState::Committed => return Err("A committed attachment upload cannot be cancelled".into()),
            AttachmentUploadState::Cancelled => { tx.commit().await.map_err(db)?; return Ok(record); }
            AttachmentUploadState::Pending => {}
        }
        sqlx::query("UPDATE chat_attachment_uploads SET state='cancelled',updated_at=? WHERE upload_id=? AND owner_id=? AND state='pending'")
            .bind(now()).bind(upload_id).bind(owner).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        record.state = AttachmentUploadState::Cancelled;
        Ok(record)
    }

    pub(crate) async fn attachment_upload_commit(
        &self,
        owner: &str,
        upload_id: &str,
        staged_path: &Path,
    ) -> Result<AttachmentUploadRecord, String> {
        validate_upload_owner(owner, upload_id)?;
        validate_staged_path(&self.root, upload_id, staged_path)?;
        let row = sqlx::query("SELECT metadata,attachment_id,blob_id,state,receipt FROM chat_attachment_uploads WHERE upload_id=? AND owner_id=?")
            .bind(upload_id).bind(owner).fetch_optional(&self.pool).await.map_err(db)?
            .ok_or("Attachment upload not found")?;
        let metadata: AttachmentUploadMetadata = serde_json::from_str(&row.try_get::<String, _>("metadata").map_err(db)?).map_err(db)?;
        let initial_blob_id: String = row.try_get("blob_id").map_err(db)?;
        match parse_upload_state(&row.try_get::<String, _>("state").map_err(db)?)? {
            AttachmentUploadState::Committed => return upload_record_from_row(row),
            AttachmentUploadState::Cancelled => {
                let registered: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_blobs WHERE id=?)")
                    .bind(&initial_blob_id).fetch_one(&self.pool).await.map_err(db)?;
                if !registered {
                    let _ = std::fs::remove_file(self.root.join("blobs").join(&initial_blob_id));
                    let _ = std::fs::remove_file(self.root.join("blobs").join(format!(".{initial_blob_id}.upload-tmp")));
                }
                return Err("Cancelled attachment uploads cannot be committed".into());
            }
            AttachmentUploadState::Pending => {}
        }
        let attachment_id: String = row.try_get("attachment_id").map_err(db)?;
        let blob_id: String = row.try_get("blob_id").map_err(db)?;
        let (size, sha256, kind, mime) = read_verified_upload(staged_path, &metadata)?;
        let final_path = self.root.join("blobs").join(&blob_id);
        let temp_path = self.root.join("blobs").join(format!(".{blob_id}.upload-tmp"));
        publish_verified_upload_blob(staged_path, &temp_path, &final_path, size, &sha256)?;
        // Only after the content file and directory are durable do we enter the
        // transaction which publishes its registry, grant, and receipt together.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        let current = sqlx::query("SELECT metadata,attachment_id,blob_id,state,receipt FROM chat_attachment_uploads WHERE upload_id=? AND owner_id=?")
            .bind(upload_id).bind(owner).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or("Attachment upload not found")?;
        let current_metadata: AttachmentUploadMetadata = serde_json::from_str(&current.try_get::<String, _>("metadata").map_err(db)?).map_err(db)?;
        match parse_upload_state(&current.try_get::<String, _>("state").map_err(db)?)? {
            AttachmentUploadState::Committed => { let record = upload_record_from_row(current)?; tx.commit().await.map_err(db)?; return Ok(record); }
            AttachmentUploadState::Cancelled => {
                let registered: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_blobs WHERE id=?)")
                    .bind(&blob_id).fetch_one(&mut *tx).await.map_err(db)?;
                if !registered {
                    let _ = std::fs::remove_file(&final_path);
                    let _ = std::fs::remove_file(&temp_path);
                }
                return Err("Cancelled attachment uploads cannot be committed".into());
            }
            AttachmentUploadState::Pending => {}
        }
        if current_metadata != metadata
            || current.try_get::<String, _>("attachment_id").map_err(db)? != attachment_id
            || current.try_get::<String, _>("blob_id").map_err(db)? != blob_id
        {
            return Err("Attachment upload reservation changed during commit".into());
        }
        let attachment = json!({"id":attachment_id,"name":metadata.name,"mime":mime,"size":size,"kind":kind,"status":"ready"});
        sqlx::query("INSERT OR IGNORE INTO chat_blobs(id,size) VALUES(?,?)")
            .bind(&blob_id).bind(size as i64).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT OR IGNORE INTO chat_attachments(id,blob_id,name,mime,size,kind) VALUES(?,?,?,?,?,?)")
            .bind(&attachment_id).bind(&blob_id).bind(&metadata.name).bind(&mime).bind(size as i64).bind(kind)
            .execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT OR IGNORE INTO chat_attachment_grants(owner_id,attachment_id) VALUES(?,?)")
            .bind(owner).bind(&attachment_id).execute(&mut *tx).await.map_err(db)?;
        let receipt = vec![attachment.clone()];
        sqlx::query("UPDATE chat_attachment_uploads SET state='committed',receipt=?,updated_at=? WHERE upload_id=? AND owner_id=? AND state='pending'")
            .bind(serde_json::to_string(&receipt).map_err(db)?).bind(now()).bind(upload_id).bind(owner)
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(AttachmentUploadRecord { metadata, state: AttachmentUploadState::Committed, attachments: receipt })
    }

    async fn attachments_list(&self, owner: &str) -> Result<Value, String> {
        validate_id(owner)?;
        let rows=sqlx::query("SELECT a.id,a.name,a.mime,a.size,a.kind FROM chat_attachments a JOIN chat_attachment_grants g ON a.id=g.attachment_id WHERE g.owner_id=? ORDER BY a.rowid").bind(owner).fetch_all(&self.pool).await.map_err(db)?;
        rows.iter()
            .map(attachment_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::from)
    }
    async fn attachment_preview(&self, p: &Value) -> Result<Value, String> {
        let owner = required(p, "ownerId")?;
        let id = required(p, "id")?;
        validate_id(owner)?;
        validate_id(id)?;
        let row=sqlx::query("SELECT a.id,a.blob_id,a.name,a.mime,a.size,a.kind FROM chat_attachments a JOIN chat_attachment_grants g ON a.id=g.attachment_id WHERE a.id=? AND g.owner_id=?").bind(id).bind(owner).fetch_optional(&self.pool).await.map_err(db)?.ok_or("Attachment not found for owner")?;
        let meta = attachment_json(&row)?;
        if meta["size"].as_u64().unwrap_or(u64::MAX) > MAX_PREVIEW as u64 {
            return Ok(json!({"attachment":meta,"preview":null,"reason":"preview-too-large"}));
        }
        let blob: String = row.try_get("blob_id").map_err(db)?;
        let bytes = std::fs::read(self.root.join("blobs").join(blob)).map_err(db)?;
        if meta["kind"] == "image" {
            return Ok(
                json!({"attachment":meta,"preview":format!("data:{};base64,{}",meta["mime"].as_str().unwrap_or("image/png"),encode_base64(&bytes))}),
            );
        }
        match String::from_utf8(bytes) {
            Ok(text)
                if text.chars().all(|character| {
                    !character.is_control() || matches!(character, '\n' | '\r' | '\t' | '\u{000c}')
                }) =>
            {
                Ok(json!({"attachment":meta,"preview":text}))
            }
            _ => Ok(json!({"attachment":meta,"preview":null,"reason":"preview-binary"})),
        }
    }
    async fn attachment_remove(&self, p: &Value) -> Result<Value, String> {
        let owner = required(p, "ownerId")?;
        let id = required(p, "id")?;
        validate_id(owner)?;
        validate_id(id)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        sqlx::query("UPDATE chat_attachment_uploads SET state='cancelled',receipt=NULL,updated_at=? WHERE owner_id=? AND attachment_id=? AND state='committed'")
            .bind(now()).bind(owner).bind(id).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("DELETE FROM chat_attachment_grants WHERE owner_id=? AND attachment_id=?")
            .bind(owner)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let blob:Option<String>=sqlx::query_scalar("SELECT blob_id FROM chat_attachments WHERE id=? AND NOT EXISTS(SELECT 1 FROM chat_attachment_grants WHERE attachment_id=?)").bind(id).bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
        if blob.is_some() {
            sqlx::query("DELETE FROM chat_attachments WHERE id=?")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            sqlx::query("DELETE FROM chat_blobs WHERE id=?")
                .bind(blob.as_deref().unwrap())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        if let Some(b) = blob {
            let _ = std::fs::remove_file(self.root.join("blobs").join(b));
        }
        Ok(json!({"removed":true}))
    }
    pub(crate) async fn attachment_paths(
        &self,
        owner: &str,
        ids: &[String],
    ) -> Result<Vec<PathBuf>, String> {
        validate_id(owner)?;
        let mut out = Vec::new();
        for id in ids {
            validate_id(id)?;
            let blob:Option<String>=sqlx::query_scalar("SELECT a.blob_id FROM chat_attachments a JOIN chat_attachment_grants g ON a.id=g.attachment_id WHERE a.id=? AND g.owner_id=?").bind(id).bind(owner).fetch_optional(&self.pool).await.map_err(db)?;
            let blob = blob.ok_or("Attachment grant is missing")?;
            out.push(self.root.join("blobs").join(blob));
        }
        Ok(out)
    }

    /// Materialize exactly the selected grants into a private, read-only directory for a
    /// runtime request. The caller receives no original source paths and can only name IDs
    /// granted to this owner.
    pub(crate) async fn attachment_directory(
        &self,
        owner: &str,
        ids: &[String],
    ) -> Result<PathBuf, String> {
        self.stage_attachments(owner, ids)
            .await
            .map(|(directory, _)| directory)
    }

    /// Return the private directory plus an explicit ID-to-staged-path map for runtime input
    /// construction. Filenames use the opaque attachment ID prefix and safe imported name.
    pub(crate) async fn stage_attachments(
        &self,
        owner: &str,
        ids: &[String],
    ) -> Result<(PathBuf, Vec<(String, PathBuf)>), String> {
        validate_id(owner)?;
        let directory = self.root.join("runtime").join(owner);
        let staged = self.stage_into(owner, ids, &directory).await?;
        Ok((directory, staged))
    }
    pub(crate) async fn stage_for_thread(
        &self,
        thread: &str,
        owner: &str,
        ids: &[String],
    ) -> Result<Vec<(String, PathBuf)>, String> {
        validate_id(thread)?;
        validate_id(owner)?;
        self.stage_into(
            owner,
            ids,
            &self.root.join("runtime").join(thread).join(owner),
        )
        .await
    }
    async fn stage_into(
        &self,
        owner: &str,
        ids: &[String],
        directory: &std::path::Path,
    ) -> Result<Vec<(String, PathBuf)>, String> {
        let paths = self.attachment_paths(owner, ids).await?;
        std::fs::create_dir_all(directory).map_err(db)?;
        let resolved = std::fs::canonicalize(directory).map_err(db)?;
        let runtime = std::fs::canonicalize(self.root.join("runtime")).map_err(db)?;
        if !resolved.starts_with(&runtime) || resolved != directory {
            return Err("Attachment staging location changed".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                .map_err(db)?;
        }
        let mut result = Vec::new();
        for (id, source) in ids.iter().zip(paths.iter()) {
            let name: String = sqlx::query_scalar("SELECT name FROM chat_attachments WHERE id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?
                .ok_or("Attachment not found")?;
            let target = directory.join(format!("{}-{}", id, name));
            match std::fs::symlink_metadata(&target) {
                Ok(metadata) => {
                    if !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || std::fs::read(&target).map_err(db)?
                            != std::fs::read(source).map_err(db)?
                    {
                        return Err("A retained attachment changed. Restore the original before continuing.".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Write a fresh inode and publish it atomically. Never overwrite a
                    // read-only original used by an earlier accepted message.
                    let mut temp = tempfile::NamedTempFile::new_in(directory).map_err(db)?;
                    std::io::copy(&mut std::fs::File::open(source).map_err(db)?, &mut temp)
                        .map_err(db)?;
                    temp.as_file().sync_all().map_err(db)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        temp.as_file()
                            .set_permissions(std::fs::Permissions::from_mode(0o400))
                            .map_err(db)?;
                    }
                    temp.persist_noclobber(&target).map_err(db)?;
                }
                Err(e) => return Err(e.to_string()),
            }
            result.push((id.clone(), target));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500))
                .map_err(db)?;
        }
        Ok(result)
    }

    pub(crate) fn staged_context(&self, thread: &str) -> Result<Option<String>, String> {
        validate_id(thread)?;
        let root = self.root.join("runtime").join(thread);
        if !root.exists() {
            return Ok(None);
        }
        let mut paths = Vec::new();
        for owner in std::fs::read_dir(&root).map_err(db)? {
            let owner = owner.map_err(db)?;
            if !owner.file_type().map_err(db)?.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(owner.path()).map_err(db)? {
                let file = file.map_err(db)?;
                if file.file_type().map_err(db)?.is_file() {
                    paths.push(file.path());
                }
            }
        }
        paths.sort();
        if paths.is_empty() {
            return Ok(None);
        }
        let shown = paths
            .iter()
            .take(100)
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        Ok(Some(format!("Retained originals for this conversation are read-only under {}. Earlier history may name another chat's attachment paths; use this conversation's paths instead. Available files: {}",root.display(),serde_json::to_string(&shown).map_err(db)?)))
    }

    pub(crate) async fn copy_attachment_grants(
        &self,
        from: &str,
        to: &str,
        ids: &[String],
    ) -> Result<(), String> {
        validate_id(from)?;
        validate_id(to)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
        for id in ids {
            validate_id(id)?;
            let r=sqlx::query("INSERT OR IGNORE INTO chat_attachment_grants(owner_id,attachment_id) SELECT ?,attachment_id FROM chat_attachment_grants WHERE owner_id=? AND attachment_id=?").bind(to).bind(from).bind(id).execute(&mut *tx).await.map_err(db)?;
            if r.rows_affected() == 0 {
                let n:i64=sqlx::query_scalar("SELECT COUNT(*) FROM chat_attachment_grants WHERE owner_id=? AND attachment_id=?").bind(to).bind(id).fetch_one(&mut *tx).await.map_err(db)?;
                if n == 0 {
                    return Err("Attachment grant is missing".into());
                }
            }
        }
        let usage: (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*),COALESCE(SUM(a.size),0) FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?",
        )
        .bind(to)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if usage.0 > MAX_FILES || usage.1 > MAX_OWNER {
            return Err("Copied attachments exceed the owner's 20-file or 200 MiB limit".into());
        }
        tx.commit().await.map_err(db)
    }

    async fn submission_put(&self, mut p: Value) -> Result<Value, String> {
        let id = p["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_id(&id)?;
        let thread = required(&p, "threadId")?.to_owned();
        validate_id(&thread)?;
        let expected = p["expectedRevision"].as_i64().unwrap_or(0);
        let status = p["status"].as_str().unwrap_or("queued").to_owned();
        if status != "queued" {
            return Err("New submissions must be persisted as queued".into());
        }
        let now = now();
        p["id"] = json!(id);
        p["revision"] = json!(1);
        p["createdAt"] = json!(now);
        p["status"] = json!(status);
        let payload = p.clone();
        let mut tx = self.pool.begin().await.map_err(db)?;
        let deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_deleted_threads WHERE thread_id=?)",
        )
        .bind(&thread)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if deleted {
            return Err("This chat was deleted and cannot accept queued work".into());
        }
        if expected != 0 {
            return Err("Submission payload is immutable".into());
        }
        sqlx::query("INSERT INTO chat_submissions(id,thread_id,payload,status,revision,created_at) VALUES(?,?,?,?,1,?)").bind(&id).bind(&thread).bind(payload.to_string()).bind(status).bind(&now).execute(&mut *tx).await.map_err(db)?;
        let ids = if payload.get("attachmentIds").is_some() {
            string_ids(&payload["attachmentIds"])?
        } else {
            Vec::new()
        };
        for attachment_id in ids {
            validate_id(&attachment_id)?;
            let grant = sqlx::query("INSERT OR IGNORE INTO chat_attachment_grants(owner_id,attachment_id) SELECT ?,attachment_id FROM chat_attachment_grants WHERE owner_id=? AND attachment_id=?")
                .bind(&id).bind(&thread).bind(&attachment_id).execute(&mut *tx).await.map_err(db)?;
            if grant.rows_affected() == 0 {
                return Err("Submission attachment grant is missing".into());
            }
        }
        tx.commit().await.map_err(db)?;
        Ok(payload)
    }
    async fn submission_get(&self, id: &str) -> Result<Value, String> {
        validate_id(id)?;
        let s: Option<String> =
            sqlx::query_scalar("SELECT payload FROM chat_submissions WHERE id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        s.map(|x| serde_json::from_str(&x).map_err(|e| e.to_string()))
            .unwrap_or_else(|| Err("Submission not found".into()))
    }
    async fn submission_list(&self, thread: Option<&str>) -> Result<Value, String> {
        let rows = if let Some(t) = thread {
            sqlx::query(
                "SELECT payload FROM chat_submissions WHERE thread_id=? ORDER BY created_at,rowid",
            )
            .bind(t)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?
        } else {
            sqlx::query("SELECT payload FROM chat_submissions ORDER BY created_at,rowid")
                .fetch_all(&self.pool)
                .await
                .map_err(db)?
        };
        let mut out = Vec::new();
        for row in rows {
            let s: String = row.try_get("payload").map_err(db)?;
            out.push(serde_json::from_str::<Value>(&s).map_err(|e| e.to_string())?);
        }
        Ok(json!(out))
    }
    async fn submission_update(&self, p: Value) -> Result<Value, String> {
        let id = required(&p, "id")?;
        validate_id(id)?;
        let status = required(&p, "status")?;
        if !valid_status(status) {
            return Err("Invalid submission status".into());
        }
        let expected = p["expectedRevision"]
            .as_i64()
            .ok_or("expectedRevision is required")?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let s: Option<String> =
            sqlx::query_scalar("SELECT payload FROM chat_submissions WHERE id=? AND revision=?")
                .bind(id)
                .bind(expected)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let mut payload: Value = serde_json::from_str(&s.ok_or("Submission revision conflict")?)
            .map_err(|e| e.to_string())?;
        let current = payload["status"].as_str().unwrap_or_default();
        if !valid_transition(current, status) {
            return Err(format!(
                "Invalid submission transition: {current} -> {status}"
            ));
        }
        if payload["status"] == "dispatching" && p.get("payload").is_some() {
            return Err("Submission payload is immutable once dispatch starts".into());
        }
        payload["status"] = json!(status);
        for k in ["turnId", "error"] {
            if p.get(k).is_some() {
                payload[k] = p[k].clone();
            }
        }
        payload["revision"] = json!(expected + 1);
        let r=sqlx::query("UPDATE chat_submissions SET payload=?,status=?,revision=revision+1 WHERE id=? AND revision=?").bind(payload.to_string()).bind(status).bind(id).bind(expected).execute(&mut *tx).await.map_err(db)?;
        if r.rows_affected() != 1 {
            return Err("Submission revision conflict".into());
        }
        tx.commit().await.map_err(db)?;
        Ok(payload)
    }
    async fn queue_state(&self, thread: &str) -> Result<Value, String> {
        validate_id(thread)?;
        let row = sqlx::query("SELECT paused,reason FROM chat_queue_state WHERE thread_id=?")
            .bind(thread)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        if let Some(r) = row {
            Ok(
                json!({"threadId":thread,"paused":r.try_get::<i64,_>("paused").map_err(db)?!=0,"reason":r.try_get::<Option<String>,_>("reason").map_err(db)?}),
            )
        } else {
            Ok(json!({"threadId":thread,"paused":false,"reason":null}))
        }
    }
    async fn queue_set(&self, p: &Value, paused: bool) -> Result<Value, String> {
        let thread = required(p, "threadId")?;
        validate_id(thread)?;
        let reason = if paused { p["reason"].as_str() } else { None };
        sqlx::query("INSERT INTO chat_queue_state(thread_id,paused,reason) VALUES(?,?,?) ON CONFLICT(thread_id) DO UPDATE SET paused=excluded.paused,reason=excluded.reason").bind(thread).bind(paused as i64).bind(reason).execute(&self.pool).await.map_err(db)?;
        self.queue_state(thread).await
    }
    async fn operation_put(&self, mut p: Value) -> Result<Value, String> {
        let id = p["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_id(&id)?;
        let expected = p["expectedRevision"].as_i64().unwrap_or(0);
        if expected != 0 {
            return Err("Operation payload is immutable on creation".into());
        }
        let now = now();
        p["id"] = json!(id);
        p["revision"] = json!(1);
        p["updatedAt"] = json!(now);
        sqlx::query("INSERT INTO chat_operations(id,payload,revision,updated_at) VALUES(?,?,1,?)")
            .bind(&id)
            .bind(p.to_string())
            .bind(&now)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(p)
    }
    async fn operation_get(&self, id: &str) -> Result<Value, String> {
        validate_id(id)?;
        let s: Option<String> =
            sqlx::query_scalar("SELECT payload FROM chat_operations WHERE id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        s.map(|x| serde_json::from_str(&x).map_err(|e| e.to_string()))
            .unwrap_or_else(|| Err("Operation not found".into()))
    }
    async fn operation_update(&self, p: Value) -> Result<Value, String> {
        let id = required(&p, "id")?;
        let expected = p["expectedRevision"]
            .as_i64()
            .ok_or("expectedRevision is required")?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let s: Option<String> =
            sqlx::query_scalar("SELECT payload FROM chat_operations WHERE id=? AND revision=?")
                .bind(id)
                .bind(expected)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let mut old: Value = serde_json::from_str(&s.ok_or("Operation revision conflict")?)
            .map_err(|e| e.to_string())?;
        for (k, v) in p.as_object().ok_or("operation must be an object")? {
            if !matches!(
                k.as_str(),
                "expectedRevision" | "id" | "revision" | "updatedAt"
            ) {
                old[k] = v.clone();
            }
        }
        old["revision"] = json!(expected + 1);
        old["updatedAt"] = json!(now());
        let r=sqlx::query("UPDATE chat_operations SET payload=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?").bind(old.to_string()).bind(old["updatedAt"].as_str()).bind(id).bind(expected).execute(&mut *tx).await.map_err(db)?;
        if r.rows_affected() != 1 {
            return Err("Operation revision conflict".into());
        }
        tx.commit().await.map_err(db)?;
        Ok(old)
    }
    async fn operation_list(&self) -> Result<Value, String> {
        let rows = sqlx::query("SELECT payload FROM chat_operations ORDER BY updated_at")
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        let mut out = Vec::new();
        for r in rows {
            let s: String = r.try_get("payload").map_err(db)?;
            out.push(serde_json::from_str::<Value>(&s).map_err(|e| e.to_string())?);
        }
        Ok(json!(out))
    }

    async fn recovery_restore_draft(&self, p: Value) -> Result<Value, String> {
        let source_id = required(&p, "sourceId")?;
        let source_kind = required(&p, "sourceKind")?;
        let thread_id = required(&p, "threadId")?;
        let operation_id = required(&p, "operationId")?;
        validate_id(source_id)?;
        validate_id(thread_id)?;
        validate_id(operation_id)?;
        if !matches!(source_kind, "submission" | "draft") {
            return Err("Invalid recovery source kind".into());
        }

        let mut tx = self.pool.begin().await.map_err(db)?;
        let deleted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_deleted_threads WHERE thread_id=?)",
        )
        .bind(thread_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if deleted {
            return Err("This chat was deleted and its work cannot be restored".into());
        }
        if let Some((prior_operation, draft_id)) = sqlx::query_as::<_, (String, String)>(
            "SELECT operation_id,draft_id FROM chat_recovery_map WHERE source_kind=? AND source_id=?",
        )
        .bind(source_kind)
        .bind(source_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        {
            if prior_operation != operation_id {
                // The source itself is the idempotency key: a repeated user action
                // cannot mint another draft even if the client lost its operation ID.
                tx.commit().await.map_err(db)?;
                return Ok(json!({"draftId":draft_id,"replayed":true}));
            }
            tx.commit().await.map_err(db)?;
            return Ok(json!({"draftId":draft_id,"replayed":true}));
        }
        let op_owner: Option<(String, String)> = sqlx::query_as(
            "SELECT source_kind,source_id FROM chat_recovery_map WHERE operation_id=?",
        )
        .bind(operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if op_owner.is_some() {
            return Err("Recovery operation ID is already used for another message".into());
        }

        let (mut document, settings, attachment_ids, created_at) = match source_kind {
            "submission" => {
                let row = sqlx::query("SELECT payload,status,created_at FROM chat_submissions WHERE id=? AND thread_id=?")
                    .bind(source_id).bind(thread_id).fetch_optional(&mut *tx).await.map_err(db)?
                    .ok_or("Recovery submission was not found for this chat")?;
                let status: String = row.try_get("status").map_err(db)?;
                if !matches!(status.as_str(), "queued" | "failed") {
                    return Err(
                        "Only queued or definitively failed messages can be restored".into(),
                    );
                }
                let payload: String = row.try_get("payload").map_err(db)?;
                let payload: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
                let attachment_ids = string_ids(&payload["attachmentIds"])?;
                (
                    payload
                        .get("document")
                        .cloned()
                        .ok_or("Submission document is missing")?,
                    payload
                        .get("settings")
                        .cloned()
                        .ok_or("Submission settings are missing")?,
                    attachment_ids,
                    row.try_get::<String, _>("created_at").map_err(db)?,
                )
            }
            "draft" => {
                let row = sqlx::query("SELECT id,thread_id,project_id,agent_id,document,settings,revision,updated_at FROM chat_drafts WHERE id=? AND thread_id=?")
                    .bind(source_id).bind(thread_id).fetch_optional(&mut *tx).await.map_err(db)?
                    .ok_or("Promoted draft was not found for this chat")?;
                let draft = draft_json(&row)?;
                let attachment_ids = draft["document"]["attachmentIds"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .map(|value| {
                                value
                                    .as_str()
                                    .map(str::to_owned)
                                    .ok_or("Invalid draft attachment ID".to_string())
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                (
                    draft["document"].clone(),
                    draft["settings"].clone(),
                    attachment_ids,
                    draft["updatedAt"].as_str().unwrap_or_default().to_owned(),
                )
            }
            _ => unreachable!(),
        };
        if let Some(rebased_document) = p.get("document") {
            if !rebased_document.is_object() {
                return Err("Restored document is malformed".into());
            }
            document = rebased_document.clone();
        }
        if is_empty_document(&document) {
            return Err("There is no unsent content to restore".into());
        }
        if !document.is_object() || !settings.is_object() {
            return Err("Saved message data is malformed and cannot be restored".into());
        }

        let draft_id = uuid::Uuid::new_v4().to_string();
        let now = now();
        let project_id = p["projectId"].as_str();
        let agent_id = p["agentId"].as_str();
        sqlx::query("INSERT INTO chat_drafts(id,thread_id,project_id,agent_id,document,settings,revision,updated_at) VALUES(?,NULL,?,?,?,?,1,?)")
            .bind(&draft_id)
            .bind(project_id)
            .bind(agent_id)
            .bind(document.to_string())
            .bind(settings.to_string())
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(db)?;

        let attachment_owner = if source_kind == "submission" {
            source_id
        } else {
            thread_id
        };
        let mut total = 0_i64;
        for attachment_id in &attachment_ids {
            validate_id(attachment_id)?;
            let row = sqlx::query("SELECT a.size FROM chat_attachments a JOIN chat_attachment_grants g ON g.attachment_id=a.id WHERE g.owner_id=? AND a.id=?")
                .bind(attachment_owner).bind(attachment_id).fetch_optional(&mut *tx).await.map_err(db)?
                .ok_or_else(|| format!("Selected attachment {attachment_id} is no longer available"))?;
            total += row.try_get::<i64, _>("size").map_err(db)?;
        }
        if attachment_ids.len() > MAX_FILES as usize || total > MAX_OWNER {
            return Err("Selected attachments exceed the recovery draft limits".into());
        }
        for attachment_id in &attachment_ids {
            let result = sqlx::query(
                "INSERT INTO chat_attachment_grants(owner_id,attachment_id) VALUES(?,?)",
            )
            .bind(&draft_id)
            .bind(attachment_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            if result.rows_affected() != 1 {
                return Err("Selected attachment grant could not be restored".into());
            }
        }

        if source_kind == "submission" {
            let payload: String = sqlx::query_scalar("SELECT payload FROM chat_submissions WHERE id=? AND thread_id=? AND status='queued'")
                .bind(source_id).bind(thread_id).fetch_optional(&mut *tx).await.map_err(db)?
                .unwrap_or_default();
            if !payload.is_empty() {
                let mut queued: Value =
                    serde_json::from_str(&payload).map_err(|e| e.to_string())?;
                queued["status"] = json!("cancelled");
                queued["revision"] = json!(queued["revision"].as_i64().unwrap_or(0) + 1);
                queued["error"] = json!("Restored as a draft");
                let result = sqlx::query("UPDATE chat_submissions SET payload=?,status='cancelled',revision=revision+1 WHERE id=? AND thread_id=? AND status='queued' AND revision=?")
                    .bind(queued.to_string()).bind(source_id).bind(thread_id).bind(queued["revision"].as_i64().unwrap_or(1) - 1)
                    .execute(&mut *tx).await.map_err(db)?;
                if result.rows_affected() != 1 {
                    return Err("Queued message changed before it could be restored".into());
                }
            }
        }
        sqlx::query("INSERT INTO chat_recovery_map(source_kind,source_id,operation_id,draft_id,source_thread_id,updated_at) VALUES(?,?,?,?,?,?)")
            .bind(source_kind).bind(source_id).bind(operation_id).bind(&draft_id).bind(thread_id).bind(&now)
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        let draft = self.draft_get(&draft_id).await?;
        Ok(json!({"draftId":draft_id,"draft":draft,"createdAt":created_at,"replayed":false}))
    }

    async fn recovery_cancel_queued(&self, p: Value) -> Result<Value, String> {
        let id = required(&p, "submissionId")?;
        validate_id(id)?;
        let expected = p["expectedRevision"]
            .as_i64()
            .ok_or("expectedRevision is required")?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM chat_submissions WHERE id=? AND revision=? AND status='queued'",
        )
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or("Only a queued message can be cancelled; refresh recovery items")?;
        let mut value: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
        value["status"] = json!("cancelled");
        value["revision"] = json!(expected + 1);
        value["error"] = json!("Cancelled during recovery");
        let result = sqlx::query("UPDATE chat_submissions SET payload=?,status='cancelled',revision=revision+1 WHERE id=? AND revision=? AND status='queued'")
            .bind(value.to_string()).bind(id).bind(expected).execute(&mut *tx).await.map_err(db)?;
        if result.rows_affected() != 1 {
            return Err("Queued message changed before it could be cancelled".into());
        }
        tx.commit().await.map_err(db)?;
        Ok(value)
    }
}

fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{k} is required"))
}
fn validate_id(s: &str) -> Result<(), String> {
    if s.len() > 128
        || s.is_empty()
        || matches!(s, "." | "..")
        || s.chars().any(|c| {
            c.is_control() || !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        })
    {
        Err("Invalid identifier".into())
    } else {
        Ok(())
    }
}
fn safe_name(s: &str) -> Result<String, String> {
    if s.is_empty() || s.len() > 255 || s.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
        Err("Invalid attachment name".into())
    } else {
        Ok(s.to_owned())
    }
}
fn safe_mime(s: &str) -> Result<String, String> {
    let valid_token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    let mut parts = s.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(kind), Some(subtype), None)
            if s.len() <= 128 && valid_token(kind) && valid_token(subtype) =>
        {
            Ok(s.to_ascii_lowercase())
        }
        _ => Err("Invalid attachment content type".into()),
    }
}
fn now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}
fn db(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn validate_upload_owner(owner: &str, upload_id: &str) -> Result<(), String> {
    crate::attachment_upload::validate_id(owner)?;
    let parsed = uuid::Uuid::parse_str(upload_id).map_err(|_| "Invalid upload ID")?;
    if parsed.hyphenated().to_string() != upload_id {
        return Err("Upload ID must be a canonical UUID".into());
    }
    Ok(())
}
fn ensure_real_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(db)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Attachment storage directories must be real directories".into());
    }
    Ok(())
}
fn parse_upload_state(value: &str) -> Result<AttachmentUploadState, String> {
    match value {
        "pending" => Ok(AttachmentUploadState::Pending),
        "committed" => Ok(AttachmentUploadState::Committed),
        "cancelled" => Ok(AttachmentUploadState::Cancelled),
        _ => Err("Stored attachment upload state is invalid".into()),
    }
}
fn upload_record_from_row(row: sqlx::sqlite::SqliteRow) -> Result<AttachmentUploadRecord, String> {
    let metadata: AttachmentUploadMetadata =
        serde_json::from_str(&row.try_get::<String, _>("metadata").map_err(db)?)
            .map_err(db)?;
    let state = parse_upload_state(&row.try_get::<String, _>("state").map_err(db)?)?;
    let receipt: Option<String> = row.try_get("receipt").map_err(db)?;
    let attachments = match (state, receipt) {
        (AttachmentUploadState::Committed, Some(receipt)) => {
            serde_json::from_str(&receipt).map_err(db)?
        }
        (AttachmentUploadState::Committed, None) => {
            return Err("Committed attachment upload has no receipt".into());
        }
        (_, None) => Vec::new(),
        (_, Some(_)) => return Err("Uncommitted attachment upload has a receipt".into()),
    };
    Ok(AttachmentUploadRecord {
        metadata,
        state,
        attachments,
    })
}
fn open_regular_nofollow(path: &Path) -> Result<std::fs::File, String> {
    let before = std::fs::symlink_metadata(path).map_err(db)?;
    if !before.file_type().is_file() || before.file_type().is_symlink() {
        return Err("Staged attachment must be a regular file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options.open(path).map_err(db)?;
    let after = file.metadata().map_err(db)?;
    if !after.is_file() || after.len() != before.len() {
        return Err("Staged attachment changed while opening".into());
    }
    Ok(file)
}
fn read_verified_upload(
    path: &Path,
    metadata: &AttachmentUploadMetadata,
) -> Result<(u64, String, &'static str, String), String> {
    let mut file = open_regular_nofollow(path)?;
    let file_metadata = file.metadata().map_err(db)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if file_metadata.uid() != unsafe { libc::geteuid() } || file_metadata.mode() & 0o077 != 0 {
            return Err("Staged attachment is not private to this user".into());
        }
    }
    let size = file_metadata.len();
    if size != metadata.size_bytes || size > MAX_FILE {
        return Err("Staged attachment size does not match its reservation".into());
    }
    let mut hasher = Sha256::new();
    let mut prefix = Vec::with_capacity(16);
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(db)?;
        if count == 0 {
            break;
        }
        if prefix.len() < 16 {
            prefix.extend_from_slice(&buffer[..count.min(16 - prefix.len())]);
        }
        total += count as u64;
        if total > MAX_FILE || total > metadata.size_bytes {
            return Err("Staged attachment grew beyond its reservation".into());
        }
        hasher.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if total != metadata.size_bytes || digest != metadata.sha256 {
        return Err("Staged attachment SHA-256 does not match its reservation".into());
    }
    let kind = sniff_kind(&prefix);
    let mime = if kind == "image" {
        sniff_mime(&prefix).to_owned()
    } else {
        safe_mime(&metadata.mime)?
    };
    Ok((total, digest, kind, mime))
}
fn hash_regular_file(path: &Path, limit: u64) -> Result<(u64, String), String> {
    let mut file = open_regular_nofollow(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(db)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > limit {
            return Err("Attachment blob exceeds its allowed size".into());
        }
        hasher.update(&buffer[..count]);
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}
fn publish_verified_upload_blob(
    staged: &Path,
    temporary: &Path,
    final_path: &Path,
    expected_size: u64,
    expected_sha: &str,
) -> Result<(), String> {
    let matches = |path: &Path| -> Result<bool, String> {
        match hash_regular_file(path, MAX_FILE) {
            Ok((size, sha)) => Ok(size == expected_size && sha == expected_sha),
            Err(_error) if std::fs::symlink_metadata(path).is_err() => Ok(false),
            Err(error) => Err(error),
        }
    };
    if std::fs::symlink_metadata(final_path).is_ok() {
        if !matches(final_path)? {
            return Err("Existing attachment blob does not match its upload reservation".into());
        }
        open_regular_nofollow(final_path)?.sync_all().map_err(db)?;
        sync_directory(final_path.parent().ok_or("Attachment blob directory is missing")?)?;
        return Ok(());
    }
    if std::fs::symlink_metadata(temporary).is_ok() {
        if matches(temporary)? {
            open_regular_nofollow(temporary)?.sync_all().map_err(db)?;
            if let Err(error) = std::fs::rename(temporary, final_path) {
                if std::fs::symlink_metadata(final_path).is_err() || !matches(final_path)? {
                    return Err(db(error));
                }
                open_regular_nofollow(final_path)?.sync_all().map_err(db)?;
                let _ = std::fs::remove_file(temporary);
            }
            sync_directory(final_path.parent().ok_or("Attachment blob directory is missing")?)?;
            return Ok(());
        }
        // The path is generated from the durable reservation ID inside the
        // private blob directory. An invalid crash residue is discarded only
        // after the actual staged source has already been hash-verified.
        std::fs::remove_file(temporary).map_err(db)?;
    }
    let mut source = open_regular_nofollow(staged)?;
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .map_err(db)?;
    if let Err(error) = std::io::copy(&mut source, &mut target) {
        let _ = std::fs::remove_file(temporary);
        return Err(db(error));
    }
    if let Err(error) = target.sync_all() {
        let _ = std::fs::remove_file(temporary);
        return Err(db(error));
    }
    drop(target);
    if !matches(temporary)? {
        let _ = std::fs::remove_file(temporary);
        return Err("Staged attachment changed while publishing its blob".into());
    }
    if let Err(error) = std::fs::rename(temporary, final_path) {
        if std::fs::symlink_metadata(final_path).is_err() || !matches(final_path)? {
            let _ = std::fs::remove_file(temporary);
            return Err(db(error));
        }
        open_regular_nofollow(final_path)?.sync_all().map_err(db)?;
        let _ = std::fs::remove_file(temporary);
    }
    sync_directory(final_path.parent().ok_or("Attachment blob directory is missing")?)
}
fn sync_directory(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(db)
}
fn is_empty_document(v: &Value) -> bool {
    if v.is_null() {
        return true;
    }
    if let Some(text) = v.as_str() {
        return text.trim().is_empty();
    }
    v["text"].as_str().unwrap_or("").trim().is_empty()
        && v["references"].as_array().is_none_or(Vec::is_empty)
        && v["quotes"].as_array().is_none_or(Vec::is_empty)
        && v["attachmentIds"].as_array().is_none_or(Vec::is_empty)
}

fn draft_json(r: &sqlx::sqlite::SqliteRow) -> Result<Value, String> {
    let parse = |k: &str| -> Result<Value, String> {
        let s: String = r.try_get(k).map_err(db)?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
    };
    Ok(
        json!({"id":r.try_get::<String,_>("id").map_err(db)?,"threadId":r.try_get::<Option<String>,_>("thread_id").map_err(db)?,"projectId":r.try_get::<Option<String>,_>("project_id").map_err(db)?,"agentId":r.try_get::<Option<String>,_>("agent_id").map_err(db)?,"document":parse("document")?,"settings":parse("settings")?,"revision":r.try_get::<i64,_>("revision").map_err(db)?,"updatedAt":r.try_get::<String,_>("updated_at").map_err(db)?}),
    )
}
fn attachment_json(r: &sqlx::sqlite::SqliteRow) -> Result<Value, String> {
    Ok(
        json!({"id":r.try_get::<String,_>("id").map_err(db)?,"name":r.try_get::<String,_>("name").map_err(db)?,"mime":r.try_get::<String,_>("mime").map_err(db)?,"size":r.try_get::<i64,_>("size").map_err(db)?,"kind":r.try_get::<String,_>("kind").map_err(db)?,"status":"ready"}),
    )
}
fn valid_status(s: &str) -> bool {
    matches!(
        s,
        "queued" | "dispatching" | "accepted" | "uncertain" | "failed" | "cancelled"
    )
}
fn valid_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("queued", "dispatching" | "failed" | "cancelled")
            | ("dispatching", "accepted" | "uncertain" | "failed")
            | ("uncertain", "accepted" | "failed")
    )
}
fn string_ids(v: &Value) -> Result<Vec<String>, String> {
    let a = v.as_array().ok_or("attachmentIds must be an array")?;
    a.iter()
        .map(|x| {
            x.as_str()
                .map(str::to_owned)
                .ok_or("invalid attachment id".into())
        })
        .collect()
}
fn sniff_kind(b: &[u8]) -> &'static str {
    if sniff_mime_opt(b).is_some() {
        "image"
    } else {
        "file"
    }
}
fn sniff_mime(b: &[u8]) -> &'static str {
    sniff_mime_opt(b).unwrap_or("application/octet-stream")
}
fn sniff_mime_opt(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    if s.len() > ((MAX_FILE as usize + 2) / 3) * 4 || s.len() % 4 != 0 {
        return Err("Attachment exceeds 50 MiB".into());
    }
    let padding = s.bytes().rev().take_while(|b| *b == b'=').count();
    if padding > 2 || s[..s.len().saturating_sub(padding)].contains('=') {
        return Err("Invalid base64 attachment data".into());
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let n = B64
            .iter()
            .position(|x| *x == c)
            .ok_or("Invalid base64 attachment data")? as u32;
        acc = (acc << 6) | n;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}
fn encode_base64(b: &[u8]) -> String {
    let mut s = String::with_capacity((b.len() + 2) / 3 * 4);
    for ch in b.chunks(3) {
        let n = ((ch[0] as u32) << 16)
            | (((*ch.get(1).unwrap_or(&0)) as u32) << 8)
            | (*ch.get(2).unwrap_or(&0) as u32);
        for i in 0..4 {
            let idx = ((n >> (18 - 6 * i)) & 63) as usize;
            if i == 2 && ch.len() < 2 {
                s.push('=')
            } else if i == 3 && ch.len() < 3 {
                s.push('=')
            } else {
                s.push(B64[idx] as char)
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opaque_ids_cannot_be_directory_components() {
        for id in [".", "..", "../another", "bad/name", "bad\\name"] {
            assert!(validate_id(id).is_err(), "{id}");
        }
        assert!(validate_id("thread-a_123").is_ok());
        assert!(is_empty_document(
            &json!({"version":1,"text":"","references":[],"quotes":[],"selection":{"anchor":0,"focus":0}})
        ));
        assert!(!is_empty_document(
            &json!({"version":1,"text":"","attachmentIds":["selected"]})
        ));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn upload_store_rejects_symlink_before_chmodding_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("chat");
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        let external = dir.path().join("outside");
        std::fs::create_dir(&external).unwrap();
        std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&external, root.join("uploads")).unwrap();
        assert!(ChatStore::open(root).await.is_err());
        assert_eq!(
            std::fs::metadata(&external).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    #[tokio::test]
    async fn immutable_originals_can_be_staged_again_for_the_same_submission_and_branch() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let imported = store.dispatch_storage("attachments.importBytes", json!({"ownerId":"submission-a","files":[{"name":"notes.txt","mime":"text/plain","data":encode_base64(b"original bytes")}]})).await.unwrap();
        let ids = vec![imported[0]["id"].as_str().unwrap().to_owned()];
        let first = store
            .stage_for_thread("source-chat", "submission-a", &ids)
            .await
            .unwrap();
        let second = store
            .stage_for_thread("source-chat", "submission-a", &ids)
            .await
            .unwrap();
        let fork = store
            .stage_for_thread("fork-chat", "submission-a", &ids)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read(&first[0].1).unwrap(), b"original bytes");
        assert_eq!(std::fs::read(&fork[0].1).unwrap(), b"original bytes");
    }

    #[tokio::test]
    async fn staged_upload_commit_and_reopen_replay_the_same_attachment_receipt() {
        use sha2::{Digest, Sha256};

        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let upload_dir = root.join("uploads");
        std::fs::create_dir(&upload_dir).unwrap();
        let staged = upload_dir.join("9fd8c4bd-6020-44a8-84d4-85a763a1dfd7.part");
        let bytes = b"accepted local attachment bytes";
        std::fs::write(&staged, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let sha = format!("{:x}", Sha256::digest(bytes));
        let metadata = json!({
            "ownerId":"draft-owner",
            "uploadId":"9fd8c4bd-6020-44a8-84d4-85a763a1dfd7",
            "name":"notes.txt",
            "mime":"text/plain",
            "sizeBytes":bytes.len(),
            "sha256":sha,
        });

        let store = ChatStore::open(root.clone()).await.unwrap();
        let begun = store
            .dispatch_storage("attachments.upload.reserve", metadata.clone())
            .await
            .unwrap();
        assert_eq!(begun["state"], "pending");
        let blob_id: String = sqlx::query_scalar("SELECT blob_id FROM chat_attachment_uploads WHERE upload_id=?")
            .bind(metadata["uploadId"].as_str().unwrap())
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let orphan_temp = root.join("blobs").join(format!(".{blob_id}.upload-tmp"));
        let mut orphan = std::fs::File::create(&orphan_temp).unwrap();
        orphan.write_all(bytes).unwrap();
        orphan.sync_all().unwrap();
        drop(orphan);
        let receipt = store
            .dispatch_storage(
                "attachments.upload.commit",
                json!({"ownerId":"draft-owner","uploadId":metadata["uploadId"],"sourcePath":staged}),
            )
            .await
            .unwrap();
        assert_eq!(receipt["state"], "committed");
        assert_eq!(receipt["attachments"].as_array().unwrap().len(), 1);
        assert!(!orphan_temp.exists(), "matching fsynced crash residue should be promoted");
        drop(store);

        let reopened = ChatStore::open(root).await.unwrap();
        let replay = reopened
            .dispatch_storage("attachments.upload.reserve", metadata.clone())
            .await
            .unwrap();
        assert_eq!(replay["state"], "committed");
        assert_eq!(replay["attachments"], receipt["attachments"]);
        let listed = reopened
            .dispatch_storage("attachments.list", json!({"ownerId":"draft-owner"}))
            .await
            .unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);

        let mut mismatched = metadata;
        mismatched["name"] = json!("different-name.txt");
        assert!(reopened
            .dispatch_storage("attachments.upload.reserve", mismatched)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cancellation_releases_reservation_but_never_reverses_a_commit() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let store = ChatStore::open(root.clone()).await.unwrap();
        let cancelled_id = "a305fb97-c5b3-4d1a-8174-ad76c580f147";
        let empty_sha = format!("{:x}", Sha256::digest(b""));
        let cancelled_metadata = json!({
            "ownerId":"cancel-owner","uploadId":cancelled_id,"name":"first.txt",
            "mime":"text/plain","sizeBytes":0,"sha256":empty_sha
        });
        store
            .dispatch_storage("attachments.upload.reserve", cancelled_metadata.clone())
            .await
            .unwrap();
        let cancelled = store
            .dispatch_storage("attachments.upload.cancel", json!({"ownerId":"cancel-owner","uploadId":cancelled_id}))
            .await
            .unwrap();
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(
            store
                .dispatch_storage("attachments.upload.reserve", cancelled_metadata)
                .await
                .unwrap()["state"],
            "cancelled"
        );

        let committed_id = "0fb96fc9-1845-4e70-8f2f-bf1b07a0946e";
        let committed_metadata = json!({
            "ownerId":"cancel-owner","uploadId":committed_id,"name":"saved.txt",
            "mime":"text/plain","sizeBytes":0,"sha256":empty_sha
        });
        store
            .dispatch_storage("attachments.upload.reserve", committed_metadata)
            .await
            .unwrap();
        let stage_dir = root.join("uploads");
        let stage_path = stage_dir.join(format!("{committed_id}.part"));
        std::fs::File::create(&stage_path).unwrap().sync_all().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stage_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        store
            .dispatch_storage(
                "attachments.upload.commit",
                json!({"ownerId":"cancel-owner","uploadId":committed_id,"sourcePath":stage_path}),
            )
            .await
            .unwrap();
        assert!(store
            .dispatch_storage("attachments.upload.cancel", json!({"ownerId":"cancel-owner","uploadId":committed_id}))
            .await
            .is_err());
        let attachments = store
            .dispatch_storage("attachments.list", json!({"ownerId":"cancel-owner"}))
            .await
            .unwrap();
        assert_eq!(attachments.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deleting_or_promoting_an_owner_cannot_leave_an_upload_on_a_dead_owner() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let store = ChatStore::open(root.clone()).await.unwrap();
        store
            .dispatch_storage(
                "drafts.save",
                json!({"id":"draft-owner","document":{"text":"draft"},"settings":{},"expectedRevision":0}),
            )
            .await
            .unwrap();
        let bytes = b"draft attachment";
        let draft_upload = "2fe8e27a-2d4a-485f-9ac8-541df7d86756";
        let draft_metadata = json!({
            "ownerId":"draft-owner","uploadId":draft_upload,"name":"draft.txt",
            "mime":"text/plain","sizeBytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))
        });
        store
            .dispatch_storage("attachments.upload.reserve", draft_metadata)
            .await
            .unwrap();
        assert!(store
            .dispatch_storage("drafts.promote", json!({"id":"draft-owner","threadId":"new-thread"}))
            .await
            .unwrap_err()
            .contains("uploads to finish"));
        let draft_stage = root.join("uploads").join(format!("{draft_upload}.part"));
        std::fs::write(&draft_stage, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&draft_stage, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        store
            .dispatch_storage("drafts.delete", json!({"id":"draft-owner"}))
            .await
            .unwrap();
        assert!(!draft_stage.exists());
        let draft_status = store
            .attachment_upload_status("draft-owner", draft_upload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(draft_status.state, AttachmentUploadState::Cancelled);
        let late_draft_upload = json!({
            "ownerId":"draft-owner",
            "uploadId":"47ea5f36-fd4b-4bd3-8a7d-7b7c03a70aaf",
            "name":"late.txt","mime":"text/plain","sizeBytes":0,
            "sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        });
        assert!(store.dispatch_storage("attachments.upload.reserve", late_draft_upload).await.unwrap_err().contains("cannot accept new uploads"));

        store.dispatch_storage("drafts.save", json!({"id":"promoted-owner","document":{"text":"promoted"},"settings":{},"expectedRevision":0})).await.unwrap();
        store.dispatch_storage("drafts.promote", json!({"id":"promoted-owner","threadId":"promoted-thread"})).await.unwrap();
        let late_promoted_upload = json!({
            "ownerId":"promoted-owner",
            "uploadId":"ee409098-bb62-48a3-83e4-1e838ab2521a",
            "name":"late.txt","mime":"text/plain","sizeBytes":0,
            "sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        });
        assert!(store.dispatch_storage("attachments.upload.reserve", late_promoted_upload).await.unwrap_err().contains("cannot accept new uploads"));

        let thread_upload = "d0a8c2f4-e6c1-4aac-8e09-d844d93ee1c3";
        let thread_bytes = b"committed then deleted";
        let thread_metadata = json!({
            "ownerId":"deleted-thread","uploadId":thread_upload,"name":"saved.txt",
            "mime":"text/plain","sizeBytes":thread_bytes.len(),"sha256":format!("{:x}",Sha256::digest(thread_bytes))
        });
        store
            .dispatch_storage("attachments.upload.reserve", thread_metadata)
            .await
            .unwrap();
        let thread_stage = root.join("uploads").join(format!("{thread_upload}.part"));
        std::fs::write(&thread_stage, thread_bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&thread_stage, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        store
            .dispatch_storage(
                "attachments.upload.commit",
                json!({"ownerId":"deleted-thread","uploadId":thread_upload,"sourcePath":thread_stage}),
            )
            .await
            .unwrap();
        store
            .dispatch_storage("thread.deleted", json!({"threadId":"deleted-thread"}))
            .await
            .unwrap();
        let status = store
            .attachment_upload_status("deleted-thread", thread_upload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.state, AttachmentUploadState::Cancelled);
        assert!(status.attachments.is_empty());
        assert!(store
            .dispatch_storage("attachments.list", json!({"ownerId":"deleted-thread"}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!thread_stage.exists());
        let late_thread_upload = json!({
            "ownerId":"deleted-thread",
            "uploadId":"73c4eadf-b2ac-4c33-9300-59773fd7036d",
            "name":"late.txt","mime":"text/plain","sizeBytes":0,
            "sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        });
        assert!(store.dispatch_storage("attachments.upload.reserve", late_thread_upload).await.unwrap_err().contains("cannot accept new uploads"));
        let late_bytes_path = root.join("late-import.txt");
        std::fs::write(&late_bytes_path, b"late").unwrap();
        assert!(store.dispatch_storage("attachments.import", json!({"ownerId":"deleted-thread","paths":[late_bytes_path]})).await.unwrap_err().contains("cannot accept new imports"));
        assert!(store.dispatch_storage("attachments.importBytes", json!({"ownerId":"deleted-thread","files":[{"name":"late.txt","mime":"text/plain","data":encode_base64(b"late")}]})).await.unwrap_err().contains("cannot accept new imports"));
    }

    #[tokio::test]
    async fn promote_and_remove_invalidate_old_committed_upload_receipts_without_losing_transferred_grants() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let store = ChatStore::open(root.clone()).await.unwrap();
        store.dispatch_storage("drafts.save", json!({"id":"source-draft","document":{"text":"draft"},"settings":{},"expectedRevision":0})).await.unwrap();
        let bytes = b"retained across promotion";
        let upload_id = "9fdf1616-47a2-4f06-a289-15f61f8ec1ea";
        let metadata = json!({"ownerId":"source-draft","uploadId":upload_id,"name":"retained.txt","mime":"text/plain","sizeBytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))});
        store.dispatch_storage("attachments.upload.reserve", metadata.clone()).await.unwrap();
        let staged = root.join("uploads").join(format!("{upload_id}.part"));
        std::fs::write(&staged, bytes).unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&staged,std::fs::Permissions::from_mode(0o600)).unwrap(); }
        let receipt = store.dispatch_storage("attachments.upload.commit",json!({"ownerId":"source-draft","uploadId":upload_id,"sourcePath":staged})).await.unwrap();
        let attachment_id = receipt["attachments"][0]["id"].as_str().unwrap();
        store.dispatch_storage("drafts.promote",json!({"id":"source-draft","threadId":"target-thread"})).await.unwrap();
        let stale = store.attachment_upload_status("source-draft",upload_id).await.unwrap().unwrap();
        assert_eq!(stale.state,AttachmentUploadState::Cancelled);
        assert!(stale.attachments.is_empty());
        assert_eq!(store.dispatch_storage("attachments.list",json!({"ownerId":"target-thread"})).await.unwrap()[0]["id"],attachment_id);

        store.dispatch_storage("attachments.remove",json!({"ownerId":"target-thread","id":attachment_id})).await.unwrap();
        let removed = store.attachment_upload_status("target-thread",upload_id).await.unwrap();
        assert!(removed.is_none(), "the upload receipt remains scoped to its original owner");
        let original_status = store.attachment_upload_status("source-draft",upload_id).await.unwrap().unwrap();
        assert_eq!(original_status.state,AttachmentUploadState::Cancelled);
        assert!(original_status.attachments.is_empty());

        let removable_bytes=b"remove receipt";
        let removable_id="8ec97b29-5904-438a-a302-230819e095c1";
        let removable_metadata=json!({"ownerId":"target-thread","uploadId":removable_id,"name":"remove.txt","mime":"text/plain","sizeBytes":removable_bytes.len(),"sha256":format!("{:x}",Sha256::digest(removable_bytes))});
        store.dispatch_storage("attachments.upload.reserve",removable_metadata).await.unwrap();
        let removable_stage=root.join("uploads").join(format!("{removable_id}.part"));
        std::fs::write(&removable_stage,removable_bytes).unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&removable_stage,std::fs::Permissions::from_mode(0o600)).unwrap(); }
        let removable_receipt=store.dispatch_storage("attachments.upload.commit",json!({"ownerId":"target-thread","uploadId":removable_id,"sourcePath":removable_stage})).await.unwrap();
        let removable_attachment=removable_receipt["attachments"][0]["id"].as_str().unwrap();
        store.dispatch_storage("attachments.remove",json!({"ownerId":"target-thread","id":removable_attachment})).await.unwrap();
        let removed_status=store.attachment_upload_status("target-thread",removable_id).await.unwrap().unwrap();
        assert_eq!(removed_status.state,AttachmentUploadState::Cancelled);
        assert!(removed_status.attachments.is_empty());
    }

    #[tokio::test]
    async fn cancelled_upload_cleans_a_previously_published_unregistered_blob() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let store = ChatStore::open(root.clone()).await.unwrap();
        let bytes = b"published before cancellation";
        let upload_id = "02dc2a1a-025e-4f4b-b682-d4360327a321";
        let metadata = json!({"ownerId":"race-owner","uploadId":upload_id,"name":"race.txt","mime":"text/plain","sizeBytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))});
        store.dispatch_storage("attachments.upload.reserve",metadata.clone()).await.unwrap();
        let blob_id: String = sqlx::query_scalar("SELECT blob_id FROM chat_attachment_uploads WHERE upload_id=?").bind(upload_id).fetch_one(&store.pool).await.unwrap();
        let staged = root.join("uploads").join(format!("{upload_id}.part"));
        std::fs::write(&staged,bytes).unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&staged,std::fs::Permissions::from_mode(0o600)).unwrap(); }
        let sha = format!("{:x}",Sha256::digest(bytes));
        let final_path=root.join("blobs").join(&blob_id);
        let temp_path=root.join("blobs").join(format!(".{blob_id}.upload-tmp"));
        publish_verified_upload_blob(&staged,&temp_path,&final_path,bytes.len() as u64,&sha).unwrap();
        assert!(final_path.exists());
        store.dispatch_storage("attachments.upload.cancel",json!({"ownerId":"race-owner","uploadId":upload_id})).await.unwrap();
        assert!(store.attachment_upload_commit("race-owner",upload_id,&staged).await.is_err());
        assert!(!final_path.exists());
        assert!(!temp_path.exists());
    }

    #[tokio::test]
    async fn legacy_import_cannot_exceed_quota_reserved_by_a_staged_upload() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        for _ in 0..MAX_FILES {
            let metadata = json!({
                "ownerId":"quota-owner",
                "uploadId":uuid::Uuid::new_v4().to_string(),
                "name":"reserved.txt",
                "mime":"text/plain",
                "sizeBytes":0,
                "sha256":format!("{:064x}", 0),
            });
            store
                .dispatch_storage("attachments.upload.reserve", metadata)
                .await
                .unwrap();
        }
        let error = store
            .dispatch_storage(
                "attachments.importBytes",
                json!({"ownerId":"quota-owner","files":[{"name":"extra.txt","mime":"text/plain","data":"YQ=="}]}),
            )
            .await
            .unwrap_err();
        assert!(error.contains("20 attachments"));
        let files = store
            .dispatch_storage("attachments.list", json!({"ownerId":"quota-owner"}))
            .await
            .unwrap();
        assert_eq!(files.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn two_chat_store_pools_serialize_upload_and_legacy_import_owner_quotas() {
        use serde_json::Value;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let first = ChatStore::open(root.clone()).await.unwrap();
        let second = ChatStore::open(root).await.unwrap();
        let first_for_fill = first.clone();
        let fill_owner = move |owner: &'static str| {
            let store = first_for_fill.clone();
            async move {
                let files = (0..19)
                    .map(|index| json!({
                        "name":format!("reserved-{index}.txt"),
                        "mime":"text/plain",
                        "data":"YQ=="
                    }))
                    .collect::<Vec<Value>>();
                store
                    .dispatch_storage("attachments.importBytes", json!({"ownerId":owner,"files":files}))
                    .await
                    .unwrap();
            }
        };
        fill_owner("begin-race").await;
        let make_metadata = |owner: &str| AttachmentUploadMetadata {
            owner_id: owner.to_owned(),
            upload_id: uuid::Uuid::new_v4().to_string(),
            name: "reserved.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 0,
            sha256: format!("{:064x}", 0),
        };
        let (one, two) = tokio::join!(
            first.attachment_upload_begin(make_metadata("begin-race")),
            second.attachment_upload_begin(make_metadata("begin-race")),
        );
        assert_eq!(one.is_ok() as usize + two.is_ok() as usize, 1);

        fill_owner("import-race").await;
        let (reservation, legacy_import) = tokio::join!(
            second.attachment_upload_begin(make_metadata("import-race")),
            first.dispatch_storage(
                "attachments.importBytes",
                json!({"ownerId":"import-race","files":[{"name":"extra.txt","mime":"text/plain","data":"YQ=="}]}),
            ),
        );
        assert_eq!(reservation.is_ok() as usize + legacy_import.is_ok() as usize, 1);
    }

    #[tokio::test]
    async fn concurrent_grant_copies_cannot_exceed_the_target_owner_quota() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let first = ChatStore::open(root.clone()).await.unwrap();
        let second = ChatStore::open(root).await.unwrap();
        let files = (0..19)
            .map(|index| json!({"name":format!("target-{index}.txt"),"mime":"text/plain","data":"YQ=="}))
            .collect::<Vec<_>>();
        first
            .dispatch_storage("attachments.importBytes", json!({"ownerId":"target-owner","files":files}))
            .await
            .unwrap();
        let source_a = first
            .dispatch_storage("attachments.importBytes", json!({"ownerId":"source-a","files":[{"name":"a.txt","mime":"text/plain","data":"YQ=="}]}))
            .await
            .unwrap()[0]["id"].as_str().unwrap().to_owned();
        let source_b = second
            .dispatch_storage("attachments.importBytes", json!({"ownerId":"source-b","files":[{"name":"b.txt","mime":"text/plain","data":"YQ=="}]}))
            .await
            .unwrap()[0]["id"].as_str().unwrap().to_owned();
        let source_ids_a = [source_a];
        let source_ids_b = [source_b];
        let (copy_a, copy_b) = tokio::join!(
            first.copy_attachment_grants("source-a", "target-owner", &source_ids_a),
            second.copy_attachment_grants("source-b", "target-owner", &source_ids_b),
        );
        assert_eq!(copy_a.is_ok() as usize + copy_b.is_ok() as usize, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_attachment_grants WHERE owner_id='target-owner'")
            .fetch_one(&first.pool).await.unwrap();
        assert_eq!(count, MAX_FILES);
    }

    #[tokio::test]
    async fn drafts_persist_and_reject_stale_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let draft = store.dispatch_storage("drafts.save", json!({"id":"draft-a","document":{"text":"hello"},"settings":{"model":"m"},"expectedRevision":0})).await.unwrap();
        assert_eq!(draft["revision"], 1);
        assert!(store
            .dispatch_storage(
                "drafts.save",
                json!({"id":"draft-a","document":{"text":"stale"},"expectedRevision":0})
            )
            .await
            .unwrap_err()
            .contains("conflict"));
        drop(store);
        let reopened = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let loaded = reopened
            .dispatch_storage("drafts.get", json!({"id":"draft-a"}))
            .await
            .unwrap();
        assert_eq!(loaded["document"]["text"], "hello");
    }

    #[tokio::test]
    async fn confirmed_thread_deletion_purges_recovery_and_blocks_late_saved_work() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        store.dispatch_storage("drafts.save", json!({"id":"deleted-chat","threadId":"deleted-chat","document":{"text":"unsent"},"expectedRevision":0})).await.unwrap();
        store
            .dispatch_storage(
                "drafts.save",
                json!({"id":"standalone-draft","document":{"text":"keep"},"expectedRevision":0}),
            )
            .await
            .unwrap();
        let imported = store.dispatch_storage("attachments.importBytes", json!({"ownerId":"deleted-chat","files":[{"name":"note.txt","mime":"text/plain","data":encode_base64(b"private note")}]})).await.unwrap();
        let attachment_id = imported[0]["id"].as_str().unwrap();
        store
            .copy_attachment_grants(
                "deleted-chat",
                "surviving-chat",
                &[attachment_id.to_owned()],
            )
            .await
            .unwrap();
        store.dispatch_storage("operations.put", json!({"id":"branch-op","kind":"branch","sourceThreadId":"deleted-chat","phase":"preparing"})).await.unwrap();
        store.dispatch_storage("operations.put", json!({"id":"target-op","kind":"branch","sourceThreadId":"other-chat","threadId":"deleted-chat","phase":"ready"})).await.unwrap();
        store
            .dispatch_storage(
                "queue.pause",
                json!({"threadId":"deleted-chat","reason":"pending"}),
            )
            .await
            .unwrap();
        store.dispatch_storage("submissions.put", json!({"id":"queued-message","threadId":"deleted-chat","document":{"text":"queued"},"attachmentIds":[],"settings":{},"expectedRevision":0})).await.unwrap();
        store.dispatch_storage("recovery.restoreDraft", json!({"sourceKind":"submission","sourceId":"queued-message","threadId":"deleted-chat","operationId":"restore-op"})).await.unwrap();

        store
            .dispatch_storage("thread.deleted", json!({"threadId":"deleted-chat"}))
            .await
            .unwrap();
        store
            .dispatch_storage("thread.deleted", json!({"threadId":"deleted-chat"}))
            .await
            .unwrap();

        assert!(store
            .dispatch_storage("drafts.get", json!({"id":"deleted-chat"}))
            .await
            .is_err());
        let drafts = store
            .dispatch_storage("drafts.list", json!({}))
            .await
            .unwrap();
        assert_eq!(drafts.as_array().unwrap().len(), 1);
        assert_eq!(drafts[0]["id"], "standalone-draft");
        assert_eq!(
            store
                .dispatch_storage("submissions.list", json!({"threadId":"deleted-chat"}))
                .await
                .unwrap(),
            json!([])
        );
        assert_eq!(
            store
                .dispatch_storage("attachments.list", json!({"ownerId":"deleted-chat"}))
                .await
                .unwrap(),
            json!([])
        );
        assert_eq!(
            store
                .dispatch_storage("attachments.list", json!({"ownerId":"surviving-chat"}))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .dispatch_storage("operations.get", json!({"id":"branch-op"}))
                .await
                .unwrap()["phase"],
            "preparing"
        );
        assert!(store
            .dispatch_storage("operations.get", json!({"id":"target-op"}))
            .await
            .is_err());
        let queue: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM chat_queue_state WHERE thread_id='deleted-chat'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(queue, 0);
        assert!(store.dispatch_storage("drafts.save", json!({"id":"deleted-chat","threadId":"deleted-chat","document":{"text":"late autosave"},"expectedRevision":0})).await.unwrap_err().contains("deleted"));
        assert!(store.dispatch_storage("submissions.put", json!({"threadId":"deleted-chat","document":{"text":"late queued message"},"attachmentIds":[],"settings":{},"expectedRevision":0})).await.unwrap_err().contains("deleted"));
        assert!(store.dispatch_storage("recovery.restoreDraft", json!({"sourceKind":"submission","sourceId":"queued-message","threadId":"deleted-chat","operationId":"restore-again"})).await.unwrap_err().contains("deleted"));
        assert!(store
            .dispatch_storage(
                "drafts.promote",
                json!({"id":"standalone-draft","threadId":"deleted-chat"})
            )
            .await
            .unwrap_err()
            .contains("deleted"));
        let mappings: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM chat_recovery_map WHERE source_thread_id='deleted-chat'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(mappings, 0);
    }

    #[tokio::test]
    async fn deleting_a_draft_keeps_accepted_outbox_receipts_and_their_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        store.dispatch_storage("drafts.save", json!({"id":"chat-a","threadId":"chat-a","document":{"text":"draft"},"expectedRevision":0})).await.unwrap();
        let imported = store.dispatch_storage("attachments.importBytes", json!({"ownerId":"chat-a","files":[{"name":"note.txt","mime":"text/plain","data":encode_base64(b"retained receipt file")}]})).await.unwrap();
        let attachment_id = imported[0]["id"].as_str().unwrap();
        let queued = store.dispatch_storage("submissions.put", json!({"id":"submission-a","threadId":"chat-a","document":{"text":"send"},"attachmentIds":[attachment_id],"settings":{},"expectedRevision":0})).await.unwrap();
        let dispatching = store.dispatch_storage("submissions.update", json!({"id":"submission-a","expectedRevision":queued["revision"],"status":"dispatching"})).await.unwrap();
        store.dispatch_storage("submissions.update", json!({"id":"submission-a","expectedRevision":dispatching["revision"],"status":"accepted","turnId":"turn-a"})).await.unwrap();

        store
            .dispatch_storage("drafts.delete", json!({"id":"chat-a"}))
            .await
            .unwrap();

        let receipt = store
            .dispatch_storage("submissions.get", json!({"id":"submission-a"}))
            .await
            .unwrap();
        assert_eq!(receipt["status"], "accepted");
        assert_eq!(
            store
                .dispatch_storage("attachments.list", json!({"ownerId":"submission-a"}))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn deleting_a_recovery_source_preserves_its_draft_promoted_to_another_chat() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let imported = store.dispatch_storage("attachments.importBytes", json!({"ownerId":"source-chat","files":[{"name":"note.txt","mime":"text/plain","data":encode_base64(b"keep for destination")}]})).await.unwrap();
        let attachment_id = imported[0]["id"].as_str().unwrap();
        store.dispatch_storage("submissions.put", json!({"id":"failed-send","threadId":"source-chat","document":{"text":"recover me"},"attachmentIds":[attachment_id],"settings":{},"expectedRevision":0})).await.unwrap();
        let dispatching = store
            .dispatch_storage(
                "submissions.update",
                json!({"id":"failed-send","expectedRevision":1,"status":"dispatching"}),
            )
            .await
            .unwrap();
        store.dispatch_storage("submissions.update", json!({"id":"failed-send","expectedRevision":dispatching["revision"],"status":"failed","error":"definitive rejection"})).await.unwrap();
        let restored = store.dispatch_storage("recovery.restoreDraft", json!({"sourceKind":"submission","sourceId":"failed-send","threadId":"source-chat","operationId":"restore-failed-send"})).await.unwrap();
        store
            .dispatch_storage(
                "drafts.promote",
                json!({"id":restored["draftId"],"threadId":"destination-chat"}),
            )
            .await
            .unwrap();

        store
            .dispatch_storage("thread.deleted", json!({"threadId":"source-chat"}))
            .await
            .unwrap();

        let destination = store
            .dispatch_storage("drafts.get", json!({"id":"destination-chat"}))
            .await
            .unwrap();
        assert_eq!(destination["threadId"], "destination-chat");
        assert_eq!(destination["document"]["text"], "recover me");
        assert_eq!(
            store
                .dispatch_storage("attachments.list", json!({"ownerId":"destination-chat"}))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn attachment_grants_gate_preview_and_survive_owner_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let png = b"\x89PNG\r\n\x1a\nrest";
        let imported=store.dispatch_storage("attachments.importBytes",json!({"ownerId":"draft-a","files":[{"name":"tiny.png","mime":"text/plain","data":encode_base64(png)}]})).await.unwrap();
        let a = &imported[0];
        assert_eq!(a["kind"], "image");
        assert_eq!(a["mime"], "image/png");
        assert!(a.get("path").is_none());
        let staged = store
            .attachment_directory("draft-a", &[a["id"].as_str().unwrap().into()])
            .await
            .unwrap();
        assert!(staged.is_dir());
        assert_eq!(std::fs::read_dir(&staged).unwrap().count(), 1);
        assert!(store
            .dispatch_storage(
                "attachments.preview",
                json!({"ownerId":"other","id":a["id"]})
            )
            .await
            .is_err());
        store
            .copy_attachment_grants("draft-a", "thread-a", &[a["id"].as_str().unwrap().into()])
            .await
            .unwrap();
        let preview = store
            .dispatch_storage(
                "attachments.preview",
                json!({"ownerId":"thread-a","id":a["id"]}),
            )
            .await
            .unwrap();
        assert!(preview["preview"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        store
            .dispatch_storage(
                "attachments.remove",
                json!({"ownerId":"draft-a","id":a["id"]}),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .attachment_paths("thread-a", &[a["id"].as_str().unwrap().into()])
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn restart_marks_inflight_submission_uncertain_and_pauses_thread() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let store = ChatStore::open(root.clone()).await.unwrap();
        let sub=store.dispatch_storage("submissions.put",json!({"threadId":"thread-a","document":{"text":"send"},"attachmentIds":[],"settings":{},"status":"queued","expectedTurnId":null})).await.unwrap();
        store
            .dispatch_storage(
                "submissions.update",
                json!({"id":sub["id"],"status":"dispatching","expectedRevision":1}),
            )
            .await
            .unwrap();
        drop(store);
        let reopened = ChatStore::open(root).await.unwrap();
        let got = reopened
            .dispatch_storage("submissions.get", json!({"id":sub["id"]}))
            .await
            .unwrap();
        assert_eq!(got["status"], "uncertain");
        let state = reopened
            .dispatch_storage("queue.state", json!({"threadId":"thread-a"}))
            .await
            .unwrap();
        assert_eq!(state["paused"], true);
    }

    #[tokio::test]
    async fn imports_reject_symlinks_directories_and_oversize_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert!(store
            .dispatch_storage(
                "attachments.import",
                json!({"ownerId":"draft-a","paths":[folder]})
            )
            .await
            .is_err());
        assert!(store
            .dispatch_storage(
                "attachments.importBytes",
                json!({"ownerId":"draft-a","files":[{"name":"bad","data":"?"}]})
            )
            .await
            .is_err());
        assert!(store.dispatch_storage("attachments.importBytes", json!({"ownerId":"draft-a","files":[{"name":"bad","mime":"text/plain;evil","data":"aGVsbG8="}]})).await.is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = dir.path().join("source");
            std::fs::write(&target, b"hello").unwrap();
            let link = dir.path().join("link");
            symlink(target, &link).unwrap();
            assert!(store
                .dispatch_storage(
                    "attachments.import",
                    json!({"ownerId":"draft-a","paths":[link]})
                )
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn text_preview_rejects_binary_and_attachment_list_preserves_import_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let first = store.dispatch_storage("attachments.importBytes", json!({"ownerId":"thread-a","files":[{"name":"z-last-name.txt","mime":"text/plain","data":encode_base64(b"literal text\nsecond line")}]})).await.unwrap();
        let text = store
            .dispatch_storage(
                "attachments.preview",
                json!({"ownerId":"thread-a","id":first[0]["id"]}),
            )
            .await
            .unwrap();
        assert_eq!(text["preview"], "literal text\nsecond line");
        store.dispatch_storage("attachments.importBytes", json!({"ownerId":"thread-a","files":[{"name":"a-first-name.txt","mime":"text/plain","data":encode_base64(b"second import")}]})).await.unwrap();
        let list = store
            .dispatch_storage("attachments.list", json!({"ownerId":"thread-a"}))
            .await
            .unwrap();
        assert_eq!(list[0]["name"], "z-last-name.txt");
        assert_eq!(list[1]["name"], "a-first-name.txt");
        let binary = store.dispatch_storage("attachments.importBytes", json!({"ownerId":"thread-a","files":[{"name":"binary.dat","mime":"application/octet-stream","data":encode_base64(&[0,1,2,255])}]})).await.unwrap();
        let rejected = store
            .dispatch_storage(
                "attachments.preview",
                json!({"ownerId":"thread-a","id":binary[0]["id"]}),
            )
            .await
            .unwrap();
        assert!(rejected["preview"].is_null());
        assert_eq!(rejected["reason"], "preview-binary");
    }

    #[tokio::test]
    async fn oversized_text_is_not_previewed() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::open(dir.path().to_path_buf()).await.unwrap();
        let payload = vec![b'x'; MAX_PREVIEW + 1];
        let file = store.dispatch_storage("attachments.importBytes", json!({"ownerId":"thread-a","files":[{"name":"large.txt","mime":"text/plain","data":encode_base64(&payload)}]})).await.unwrap();
        let preview = store
            .dispatch_storage(
                "attachments.preview",
                json!({"ownerId":"thread-a","id":file[0]["id"]}),
            )
            .await
            .unwrap();
        assert!(preview["preview"].is_null());
        assert_eq!(preview["reason"], "preview-too-large");
    }
}
