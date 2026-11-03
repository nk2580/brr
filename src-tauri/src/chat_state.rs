use serde_json::{json, Value};
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
        let root = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
        let blobs = root.join("blobs");
        std::fs::create_dir_all(&blobs).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let options = SqliteConnectOptions::new()
            .filename(root.join("chat.sqlite3"))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
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
            "CREATE TABLE IF NOT EXISTS chat_submissions(id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, payload TEXT NOT NULL, status TEXT NOT NULL, revision INTEGER NOT NULL, created_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_queue_state(thread_id TEXT PRIMARY KEY, paused INTEGER NOT NULL DEFAULT 0, reason TEXT)",
            "CREATE TABLE IF NOT EXISTS chat_branch_lineage(thread_id TEXT PRIMARY KEY,lineage TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_assignment_context(operation_id TEXT PRIMARY KEY,assignment_owner TEXT NOT NULL,payload TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_operations(id TEXT PRIMARY KEY, payload TEXT NOT NULL, revision INTEGER NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE IF NOT EXISTS chat_recovery_map(source_kind TEXT NOT NULL, source_id TEXT NOT NULL, operation_id TEXT NOT NULL UNIQUE, draft_id TEXT NOT NULL, source_thread_id TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(source_kind,source_id))",
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
            "attachments.import" => self.attachments_import(&params).await,
            "attachments.importBytes" => self.attachments_import_bytes(&params).await,
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
        let mut tx = self.pool.begin().await.map_err(db)?;
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
        let mut tx = self.pool.begin().await.map_err(db)?;
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
        let mut tx = self.pool.begin().await.map_err(db)?;
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
        let r = sqlx::query("DELETE FROM chat_drafts WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        for blob in blobs {
            let _ = std::fs::remove_file(self.root.join("blobs").join(blob));
        }
        Ok(json!({"deleted":r.rows_affected()>0}))
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
        let usage: (i64, i64) = sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(a.size),0) FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?")
            .bind(owner).fetch_one(&self.pool).await.map_err(db)?;
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
        let mut tx = self.pool.begin().await.map_err(|e| {
            let _ = std::fs::remove_file(&final_path);
            e.to_string()
        })?;
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
            let _ = std::fs::remove_file(&final_path);
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
        let usage: (i64,i64) = sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(a.size),0) FROM chat_attachment_grants g JOIN chat_attachments a ON a.id=g.attachment_id WHERE g.owner_id=?").bind(owner).fetch_one(&self.pool).await.map_err(db)?;
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
        let mut tx = self.pool.begin().await.map_err(db)?;
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
            let _ = std::fs::remove_file(&final_path);
            return Err(db(e));
        }
        Ok(json!({"id":id,"name":name,"mime":mime,"size":bytes.len(),"kind":kind,"status":"ready"}))
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
        let mut tx = self.pool.begin().await.map_err(db)?;
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
        let mut tx = self.pool.begin().await.map_err(db)?;
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
