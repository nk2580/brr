//! Private, verified backup preparation before fleet activation.

use brr_host::{
    export::{HistoryExport, Inventory, RuntimeFileInventory, StoreSnapshot, TableInventory},
    Host,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    AssertSqlSafe,
};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportBackup {
    pub directory: PathBuf,
    pub manifest: BackupManifest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub schema_version: u32,
    pub node_id: String,
    pub history_complete: bool,
    pub verified_empty_history: bool,
    pub history_thread_count: usize,
    pub history_sha256: String,
    pub store_complete: bool,
    pub inventory_complete: bool,
    pub store_shared: std::collections::BTreeMap<String, TableInventory>,
    pub store_owner_local: std::collections::BTreeMap<String, TableInventory>,
    pub inventory_tables: std::collections::BTreeMap<String, TableInventory>,
    pub databases: Vec<DatabaseManifest>,
    pub files: Vec<BackupFile>,
    pub warnings: Vec<String>,
    pub created_at_unix: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseManifest {
    pub name: String,
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub table_counts: Vec<TableCount>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TableCount {
    pub name: String,
    pub classification: String,
    pub count: u64,
    pub shared_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub relative_path: String,
    pub classification: String,
    pub size_bytes: u64,
    pub sha256: String,
}

const SESSIONS_ABSENT_WARNING: &str =
    "runtime history directory is absent; history file inventory may be incomplete";
const AUTH_EXCLUDED_WARNING: &str =
    "authentication material is deliberately excluded from fleet inventory";

pub async fn prepare(
    host: &Host,
    node_id: &str,
    history: &HistoryExport,
) -> Result<ImportBackup, String> {
    validate_node_id(node_id)?;
    if host.is_connected().await {
        return Err("migration backup requires a quiesced, disconnected runtime".into());
    }
    validate_history(history)?;
    let store = host.store_snapshot().await?;
    if store.schema_version != 1 || !store.complete {
        return Err("local store inventory is incomplete; import backup was not prepared".into());
    }
    let inventory = host.inventory().await?;
    let runtime_history = verify_rollout_coverage(&inventory, history)?;
    let verified_empty_history = history.threads.is_empty() && runtime_history == 0;
    validate_inventory(&inventory, &store, verified_empty_history)?;

    let data_dir = host.data_dir();
    let node_dir = data_dir.join("node");
    ensure_private_dir(&node_dir)?;
    let backup_root = node_dir.join("import-backups");
    ensure_private_dir(&backup_root)?;
    let backup_dir = backup_root.join(Uuid::new_v4().to_string());
    create_private_dir(&backup_dir)?;

    let result = async {
        let mut databases = Vec::new();
        let sources = [
            (
                "agents",
                "agent",
                data_dir.join("engine/agents/agents.sqlite3"),
                true,
            ),
            (
                "projects",
                "projects",
                data_dir.join("projects.sqlite"),
                true,
            ),
            (
                "chat",
                "chat",
                data_dir
                    .join(brr_host::export::CHAT_STORE_RELATIVE_PATH)
                    .join("chat.sqlite3"),
                false,
            ),
        ]
        .into_iter()
        .chain(
            brr_host::export::OWNED_RUNTIME_DATABASES
                .iter()
                .map(|(role, file)| (*role, *role, data_dir.join("engine").join(file), false)),
        );
        for (name, prefix, source, required) in sources {
            if !exists_regular_nonsymlink(&source)? {
                if required {
                    return Err(format!("required local {name} database is missing"));
                }
                continue;
            }
            verify_source_path(&source)?;
            check_sidecar(&PathBuf::from(format!("{}-wal", source.display())))?;
            check_sidecar(&PathBuf::from(format!("{}-shm", source.display())))?;
            let table_counts = source_table_counts(&source, &inventory, prefix).await?;
            let destination_relative = format!("databases/{}.sqlite3", name);
            let destination = backup_dir.join(&destination_relative);
            ensure_private_dir(destination.parent().unwrap())?;
            vacuum_into(&source, &destination).await?;
            let database =
                database_manifest(name, destination_relative, &destination, table_counts).await?;
            databases.push(database);
        }

        let history_payload = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": history.schema_version,
            "ownerOrigin": history.owner_origin,
            "threads": history.threads,
        }))
        .map_err(|error| format!("could not encode verified runtime history: {error}"))?;
        let history_relative = "files/history/history.json";
        let history_path = backup_dir.join(history_relative);
        ensure_parent_private(&history_path)?;
        write_private_new(&history_path, &history_payload)?;
        let mut files = vec![file_manifest(
            history_relative,
            "history-export",
            &history_path,
        )?];
        for entry in &inventory.runtime_files {
            files.push(copy_inventory_file(data_dir, &backup_dir, entry)?);
        }
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        databases.sort_by(|left, right| left.name.cmp(&right.name));

        let mut warnings = store.warnings.clone();
        warnings.extend(inventory.warnings.clone());
        let manifest = BackupManifest {
            schema_version: 1,
            node_id: node_id.into(),
            history_complete: history.complete,
            verified_empty_history,
            history_thread_count: history.threads.len(),
            history_sha256: digest(&history_payload),
            store_complete: store.complete,
            inventory_complete: inventory.complete,
            store_shared: store.shared_inventory.clone(),
            store_owner_local: store.owner_local.clone(),
            inventory_tables: inventory.tables.clone(),
            databases,
            files,
            warnings,
            created_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_secs() as i64,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| format!("could not encode import backup manifest: {error}"))?;
        write_private_new(&backup_dir.join("manifest.json"), &manifest_bytes)?;
        let backup = ImportBackup {
            directory: backup_dir.clone(),
            manifest,
        };
        backup.verify().await?;
        Ok(backup)
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_dir_all(&backup_dir);
    }
    result
}

impl ImportBackup {
    /// Reconstruct only verified backup-owned history; never deserialize paths.
    pub async fn history_export(&self) -> Result<HistoryExport, String> {
        self.verify().await?;
        let payload: serde_json::Value = serde_json::from_slice(
            &fs::read(self.directory.join("files/history/history.json")).map_err(io_error)?,
        )
        .map_err(|e| e.to_string())?;
        let mut history = HistoryExport {
            schema_version: payload["schemaVersion"]
                .as_u64()
                .filter(|v| *v == 1)
                .ok_or("unsupported backup history schema")? as u32,
            owner_origin: payload["ownerOrigin"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or("backup history has no origin")?
                .into(),
            complete: true,
            threads: payload["threads"]
                .as_array()
                .cloned()
                .ok_or("backup history has no thread inventory")?,
            blobs: vec![],
            failures: vec![],
        };
        let mut referenced = std::collections::BTreeMap::<String, u64>::new();
        for thread in &history.threads {
            let attachments: Vec<brr_host::export::HistoryAttachment> = serde_json::from_value(
                thread
                    .get("attachments")
                    .cloned()
                    .unwrap_or(serde_json::json!([])),
            )
            .map_err(|e| e.to_string())?;
            for attachment in attachments {
                if referenced
                    .insert(attachment.sha256, attachment.size_bytes)
                    .is_some_and(|old| old != attachment.size_bytes)
                {
                    return Err("backup attachment digest has inconsistent sizes".into());
                }
            }
        }
        for (sha256, size_bytes) in referenced {
            let file = self
                .manifest
                .files
                .iter()
                .find(|file| {
                    file.classification == "app-attachment-blob"
                        && file.sha256 == sha256
                        && file.size_bytes == size_bytes
                        && file
                            .relative_path
                            .strip_prefix("files/app-attachment-blob/")
                            .is_some_and(|id| {
                                Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
                            })
                })
                .ok_or("backup history references an attachment with no verified content file")?;
            verify_entry_under(&self.directory, Path::new(&file.relative_path), false)?;
            history
                .blobs
                .push(brr_host::export::HistoryBlob::from_verified_path(
                    self.directory.join(&file.relative_path),
                    sha256,
                    size_bytes,
                )?);
        }
        Ok(history)
    }

    pub async fn verify(&self) -> Result<(), String> {
        verify_private_dir(&self.directory)?;
        let manifest_path = self.directory.join("manifest.json");
        verify_entry_under(&self.directory, Path::new("manifest.json"), false)?;
        let persisted: BackupManifest =
            serde_json::from_slice(&fs::read(&manifest_path).map_err(io_error)?)
                .map_err(|error| format!("import backup manifest is invalid: {error}"))?;
        let persisted_value =
            serde_json::to_value(&persisted).map_err(|error| error.to_string())?;
        let expected_value =
            serde_json::to_value(&self.manifest).map_err(|error| error.to_string())?;
        if persisted_value != expected_value || persisted.schema_version != 1 {
            return Err(
                "persisted import backup manifest does not match its verified contents".into(),
            );
        }
        if !persisted.store_complete || !persisted.history_complete {
            return Err("import backup is marked incomplete".into());
        }
        for database in &persisted.databases {
            let expected_relative = match database.name.as_str() {
                "agents" | "projects" | "chat" => format!("databases/{}.sqlite3", database.name),
                role if brr_host::export::OWNED_RUNTIME_DATABASES
                    .iter()
                    .any(|(name, _)| *name == role) =>
                {
                    format!("databases/{role}.sqlite3")
                }
                _ => return Err("import backup has an unknown database role".into()),
            };
            if database.relative_path != expected_relative {
                return Err("import backup database path is not an approved fixed path".into());
            }
            let path = self.directory.join(&expected_relative);
            verify_entry_under(&self.directory, Path::new(&expected_relative), false)?;
            let metadata = fs::metadata(&path).map_err(io_error)?;
            if metadata.len() != database.size_bytes || hash_file(&path)? != database.sha256 {
                return Err(format!(
                    "backup database `{}` hash or size mismatch",
                    database.name
                ));
            }
            verify_sqlite(&path, &database.table_counts).await?;
            let shared_role = match database.name.as_str() {
                "agents" => Some("agent"),
                "projects" => Some("projects"),
                "chat" => Some("chat"),
                _ => None,
            };
            if let Some(role) = shared_role {
                brr_host::export::verify_backup_store(&path, role, &persisted.inventory_tables)
                    .await?;
            }
        }
        for file in &persisted.files {
            if !matches!(
                file.classification.as_str(),
                "history-export"
                    | "runtime-history"
                    | "runtime-archived-history"
                    | "app-attachment-blob"
                    | "app-attachment-upload"
            ) {
                return Err("import backup has an unknown file classification".into());
            }
            let relative = safe_relative(&file.relative_path)?;
            if !relative.starts_with("files") {
                return Err(
                    "import backup contains a file outside the fixed files directory".into(),
                );
            }
            let path = self.directory.join(&relative);
            verify_entry_under(&self.directory, &relative, false)?;
            let metadata = fs::metadata(&path).map_err(io_error)?;
            if metadata.len() != file.size_bytes || hash_file(&path)? != file.sha256 {
                return Err(format!(
                    "backup file `{}` hash or size mismatch",
                    file.relative_path
                ));
            }
        }
        let history = persisted
            .files
            .iter()
            .find(|file| file.relative_path == "files/history/history.json")
            .ok_or("import backup history archive is missing")?;
        if history.sha256 != persisted.history_sha256 {
            return Err("import backup history hash does not match the manifest".into());
        }
        Ok(())
    }
}

fn validate_node_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("node id must be 1-128 ASCII letters, digits, '-' or '_'".into());
    }
    Ok(())
}

fn validate_history(history: &HistoryExport) -> Result<(), String> {
    if history.schema_version != 1
        || history.owner_origin.trim().is_empty()
        || !history.complete
        || !history.failures.is_empty()
    {
        return Err("runtime history export is incomplete or unsupported".into());
    }
    Ok(())
}

fn verify_rollout_coverage(
    inventory: &Inventory,
    history: &HistoryExport,
) -> Result<usize, String> {
    let exported = history
        .threads
        .iter()
        .filter_map(|row| row["thread"]["id"].as_str().or_else(|| row["id"].as_str()))
        .collect::<HashSet<_>>();
    let mut recognized = HashSet::new();
    let mut legacy = HashSet::new();
    for file in inventory.runtime_files.iter().filter(|file| {
        matches!(
            file.classification.as_str(),
            "runtime-history" | "runtime-archived-history"
        )
    }) {
        let path = file
            .relative_path
            .strip_suffix(".zst")
            .unwrap_or(&file.relative_path);
        // Keep both original files in the backup, but the runtime treats a
        // plain/compressed pair or multiple revert rollouts as one thread.
        let id = path
            .rsplit('/')
            .next()
            .and_then(|name| name.strip_prefix("rollout-")?.strip_suffix(".jsonl"))
            .and_then(|core| core.get(20..).filter(|_| core.get(19..20) == Some("-")))
            .and_then(|ids| Uuid::parse_str(ids.split('_').next()?).ok())
            .map(|uuid| uuid.to_string());
        if let Some(id) = id {
            if !exported.contains(id.as_str()) {
                return Err(format!(
                    "An owned runtime rollout has no exported thread ({id})"
                ));
            }
            recognized.insert(id);
        } else {
            // Preserve earlier rollout naming layouts too; they still need
            // enough distinct exported conversations to account for the files.
            legacy.insert(path);
        }
    }
    let inventoried = recognized.len() + legacy.len();
    if inventoried > exported.len() {
        return Err(format!("The runtime exported {} conversations but inventory found {inventoried} rollout histories",exported.len()));
    }
    Ok(inventoried)
}

fn validate_inventory(
    inventory: &Inventory,
    store: &StoreSnapshot,
    verified_empty: bool,
) -> Result<(), String> {
    if inventory.schema_version != 1 {
        return Err("local inventory schema is unsupported".into());
    }
    let only_missing_sessions = !inventory.complete
        && verified_empty
        && inventory
            .warnings
            .iter()
            .filter(|w| *w != SESSIONS_ABSENT_WARNING && *w != AUTH_EXCLUDED_WARNING)
            .count()
            == 0
        && inventory
            .warnings
            .iter()
            .any(|w| w == SESSIONS_ABSENT_WARNING);
    if !inventory.complete && !only_missing_sessions {
        return Err("local inventory contains unclassified or unsafe data".into());
    }
    if !store.complete {
        return Err("local store snapshot is incomplete".into());
    }
    for file in &inventory.runtime_files {
        match file.classification.as_str() {
            kind @ ("runtime-history" | "runtime-archived-history")
                if file.relative_path.starts_with(&format!("{kind}/"))
                    && (file.relative_path.ends_with(".jsonl")
                        || file.relative_path.ends_with(".jsonl.zst")) =>
            {
                if verified_empty {
                    return Err(
                        "empty history conflicts with inventoried runtime history files".into(),
                    );
                }
            }
            "app-attachment-blob" => {
                let id = file
                    .relative_path
                    .strip_prefix("app-attachment-blob/")
                    .unwrap_or_default();
                if uuid::Uuid::parse_str(id).is_err() || id.contains('/') {
                    return Err("attachment inventory contains an invalid managed blob path".into());
                }
            }
            "app-attachment-upload" => {
                let id = file
                    .relative_path
                    .strip_prefix("app-attachment-upload/")
                    .and_then(|name| name.strip_suffix(".part"))
                    .unwrap_or_default();
                if Uuid::parse_str(id).is_err()
                    || Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() != id)
                {
                    return Err("attachment upload inventory contains an invalid staging path".into());
                }
            }
            _ => {
                return Err(
                    "inventory contains an unapproved runtime file path or classification".into(),
                );
            }
        }
    }
    Ok(())
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(io_error)?;
    }
    let meta = fs::symlink_metadata(path).map_err(io_error)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("backup directory is not a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err("backup directory is not owned by the current user".into());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        if fs::metadata(path).map_err(io_error)?.permissions().mode() & 0o777 != 0o700 {
            return Err("backup directory permissions are not private".into());
        }
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir(path).map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    ensure_private_dir(path)
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    verify_private_file(path)
}

fn verify_private_dir(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(io_error)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("backup path is not a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o777 != 0o700 {
            return Err(format!(
                "backup directory ownership or permissions are unsafe at {} uid={} mode={:o}",
                path.display(),
                meta.uid(),
                meta.permissions().mode() & 0o777
            ));
        }
    }
    Ok(())
}

fn verify_private_file(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(io_error)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("backup entry is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o777 != 0o600 {
            return Err("backup file ownership or permissions are unsafe".into());
        }
    }
    Ok(())
}

fn verify_entry_under(root: &Path, relative: &Path, is_directory: bool) -> Result<(), String> {
    let safe = safe_relative(&relative.to_string_lossy())?;
    let mut current = root.to_path_buf();
    let parts: Vec<_> = safe.components().collect();
    for (index, component) in parts.iter().enumerate() {
        current.push(component.as_os_str());
        let final_component = index + 1 == parts.len();
        if final_component && !is_directory {
            verify_private_file(&current)?;
        } else {
            verify_private_dir(&current)?;
        }
    }
    Ok(())
}

fn exists_regular_nonsymlink(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("database source is not a regular file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(e)),
    }
}

fn check_sidecar(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("SQLite sidecar is not a regular file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}

async fn source_table_counts(
    source: &Path,
    inventory: &Inventory,
    prefix: &str,
) -> Result<Vec<TableCount>, String> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(source))
        .await
        .map_err(db_error)?;
    let mut counts = Vec::new();
    for (key, info) in inventory
        .tables
        .iter()
        .filter(|(key, _)| key.starts_with(&format!("{prefix}/")))
    {
        let table = key.strip_prefix(&format!("{prefix}/")).unwrap();
        if table.is_empty()
            || !table
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err("inventory table name is invalid".into());
        }
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?")
                .bind(table)
                .fetch_one(&pool)
                .await
                .map_err(db_error)?;
        let count = if exists == 0 {
            0
        } else {
            let sql = AssertSqlSafe(format!("SELECT COUNT(*) FROM \"{table}\""));
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .map_err(db_error)? as u64
        };
        if info.record_count.is_some_and(|expected| expected != count) {
            return Err(format!(
                "Source table {prefix}/{table} changed after inventory"
            ));
        }
        counts.push(TableCount {
            name: table.into(),
            classification: info.classification.clone(),
            count,
            shared_sha256: info.sha256.clone(),
        });
    }
    pool.close().await;
    counts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(counts)
}

async fn vacuum_into(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        return Err("backup database destination already exists".into());
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(source))
        .await
        .map_err(db_error)?;
    sqlx::query("VACUUM INTO ?")
        .bind(destination.to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .map_err(db_error)?;
    pool.close().await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
    }
    verify_private_file(destination)
}

async fn database_manifest(
    name: &str,
    relative: String,
    path: &Path,
    table_counts: Vec<TableCount>,
) -> Result<DatabaseManifest, String> {
    verify_sqlite(path, &table_counts).await?;
    let meta = fs::metadata(path).map_err(io_error)?;
    Ok(DatabaseManifest {
        name: name.into(),
        relative_path: relative,
        sha256: hash_file(path)?,
        size_bytes: meta.len(),
        table_counts,
    })
}

async fn verify_sqlite(path: &Path, expected: &[TableCount]) -> Result<(), String> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .map_err(db_error)?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .map_err(db_error)?;
    if integrity != "ok" {
        pool.close().await;
        return Err("backup SQLite integrity check failed".into());
    }
    for table in expected {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?")
                .bind(&table.name)
                .fetch_one(&pool)
                .await
                .map_err(db_error)?;
        let actual = if exists == 0 {
            0
        } else {
            let sql = AssertSqlSafe(format!("SELECT COUNT(*) FROM \"{}\"", table.name));
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .map_err(db_error)? as u64
        };
        if actual != table.count {
            pool.close().await;
            return Err(format!(
                "backup SQLite row count mismatch for {}",
                table.name
            ));
        }
    }
    pool.close().await;
    Ok(())
}

fn copy_inventory_file(
    data_dir: &Path,
    backup_dir: &Path,
    item: &RuntimeFileInventory,
) -> Result<BackupFile, String> {
    let rel = safe_relative(&item.relative_path)?;
    let (source, destination_relative) = match item.classification.as_str() {
        kind @ ("runtime-history" | "runtime-archived-history")
            if item.relative_path.starts_with(&format!("{kind}/")) =>
        {
            let tail = rel.strip_prefix(kind).map_err(|_| "invalid history path")?;
            let directory = if kind == "runtime-history" {
                "sessions"
            } else {
                "archived_sessions"
            };
            (
                data_dir.join("engine").join(directory).join(tail),
                format!("files/{}", item.relative_path),
            )
        }
        "app-attachment-blob" if item.relative_path.starts_with("app-attachment-blob/") => {
            let id = item
                .relative_path
                .strip_prefix("app-attachment-blob/")
                .ok_or("invalid blob path")?;
            (
                data_dir
                    .join(brr_host::export::CHAT_STORE_RELATIVE_PATH)
                    .join("blobs")
                    .join(id),
                format!("files/{}", item.relative_path),
            )
        }
        "app-attachment-upload" if item.relative_path.starts_with("app-attachment-upload/") => {
            let name = item
                .relative_path
                .strip_prefix("app-attachment-upload/")
                .ok_or("invalid staged upload path")?;
            let id = name.strip_suffix(".part").ok_or("invalid staged upload path")?;
            if Uuid::parse_str(id).is_err()
                || Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() != id)
            {
                return Err("invalid staged upload ID".into());
            }
            (
                data_dir
                    .join(brr_host::export::CHAT_STORE_RELATIVE_PATH)
                    .join("uploads")
                    .join(name),
                format!("files/{}", item.relative_path),
            )
        }
        _ => return Err("runtime file is not on an approved path".into()),
    };
    verify_source_path(&source)?;
    let bytes = fs::read(&source).map_err(io_error)?;
    if bytes.len() as u64 != item.size_bytes || digest(&bytes) != item.sha256 {
        return Err("runtime source file changed after inventory".into());
    }
    let destination = backup_dir.join(&destination_relative);
    ensure_parent_private(&destination)?;
    write_private_new(&destination, &bytes)?;
    file_manifest(&destination_relative, &item.classification, &destination)
}
fn ensure_parent_private(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("backup file has no parent")?;
    let mut missing = Vec::new();
    let mut at = parent;
    while !at.exists() {
        missing.push(at.to_path_buf());
        at = at.parent().ok_or("invalid backup parent")?;
    }
    for dir in missing.into_iter().rev() {
        create_private_dir(&dir)?;
    }
    ensure_private_dir(parent)
}

fn verify_source_path(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Normal(_)) {
            let meta = fs::symlink_metadata(&current).map_err(io_error)?;
            if meta.file_type().is_symlink() {
                return Err("source file path contains a symlink".into());
            }
        }
    }
    let meta = fs::symlink_metadata(path).map_err(io_error)?;
    if !meta.is_file() {
        return Err("source file is not regular".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err("source file is not owned by the current user".into());
        }
    }
    Ok(())
}

fn safe_relative(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("path is not a safe relative path".into());
    }
    Ok(path.to_path_buf())
}

fn file_manifest(
    relative_path: &str,
    classification: &str,
    path: &Path,
) -> Result<BackupFile, String> {
    verify_private_file(path)?;
    let meta = fs::metadata(path).map_err(io_error)?;
    Ok(BackupFile {
        relative_path: relative_path.into(),
        classification: classification.into(),
        size_bytes: meta.len(),
        sha256: hash_file(path)?,
    })
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn io_error(error: std::io::Error) -> String {
    error.to_string()
}
fn db_error(error: sqlx::Error) -> String {
    format!("SQLite backup operation failed: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use brr_host::{export::HistoryExport, HostConfig};
    use serde_json::{json, Value};
    use sqlx::{
        sqlite::{SqliteConnectOptions, SqliteConnection},
        Connection,
    };
    use std::{io::Write, sync::Arc};
    use tempfile::TempDir;

    #[cfg(unix)]
    #[tokio::test]
    async fn backup_captures_chat_attachment_imported_through_live_host_api() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("owned-runtime");
        std::fs::write(
            &runtime,
            "#!/usr/bin/env python3\nimport json,sys\nfor line in sys.stdin:\n m=json.loads(line)\n if 'id' in m: print(json.dumps({'id':m['id'],'result':{'account':None,'data':[],'nextCursor':None,'capabilities':{}}}),flush=True)\n",
        ).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let data_dir = directory.path().join("data");
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
        let live_chat = data_dir
            .join(brr_host::export::CHAT_STORE_RELATIVE_PATH)
            .join("chat.sqlite3");
        let chat_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&live_chat)
                    .read_only(true),
            )
            .await
            .unwrap();
        let blob_id: String = sqlx::query_scalar("SELECT blob_id FROM chat_attachments WHERE id=?")
            .bind(attachment_id)
            .fetch_one(&chat_pool)
            .await
            .unwrap();
        chat_pool.close().await;
        host.shutdown().await;
        let history_file = data_dir.join("engine/sessions/2026/11/thread-imported-qa.jsonl");
        fs::create_dir_all(history_file.parent().unwrap()).unwrap();
        fs::write(&history_file, b"synthetic history evidence\n").unwrap();
        let sha = digest(bytes);
        let mut exported = history(vec![
            json!({"thread":{"id":"thread-imported-qa"},"turns":[],"origin":"local-runtime","attachments":[{
                "id":attachment_id,"name":"notes.txt","mime":"text/plain","kind":"text","sizeBytes":bytes.len(),"sha256":sha,"submissionId":"accepted-recovery-fixture","turnId":null,"itemId":null
            }]}),
        ]);
        exported.blobs.push(
            brr_host::export::HistoryBlob::from_verified_path(
                std::fs::canonicalize(
                    data_dir
                        .join(brr_host::export::CHAT_STORE_RELATIVE_PATH)
                        .join("blobs")
                        .join(&blob_id),
                )
                .unwrap(),
                sha.clone(),
                bytes.len() as u64,
            )
            .unwrap(),
        );
        let backup = prepare(&host, "local-node-1", &exported).await.unwrap();
        let chat_database = backup
            .manifest
            .databases
            .iter()
            .find(|db| db.name == "chat")
            .unwrap();
        assert_eq!(chat_database.relative_path, "databases/chat.sqlite3");
        let attachment = backup
            .manifest
            .files
            .iter()
            .find(|file| file.relative_path == format!("files/app-attachment-blob/{blob_id}"))
            .unwrap();
        assert_eq!(attachment.sha256, digest(bytes));
        assert_eq!(
            std::fs::read(backup.directory.join(&attachment.relative_path)).unwrap(),
            bytes
        );
        backup.verify().await.unwrap();
        let recovered = backup.history_export().await.unwrap();
        assert_eq!(
            recovered.blobs.len(),
            1,
            "activation retry must restore verified file-backed attachment sources"
        );
        assert_eq!(recovered.blobs[0].read_verified_bytes().unwrap(), bytes);
        assert!(recovered.blobs[0]
            .verified_source_path()
            .unwrap()
            .starts_with(&backup.directory));
    }

    #[tokio::test]
    async fn populated_legacy_chat_directory_refuses_activation_backup() {
        let (_directory, host) = host().await;
        let legacy = host.data_dir().join("engine/chat");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("chat.sqlite3"),
            b"legacy data must not be skipped",
        )
        .unwrap();
        let error = prepare(
            &host,
            "local-node-1",
            &history(vec![json!({"id":"thread-1"})]),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            "local store inventory is incomplete; import backup was not prepared"
        );
        assert_eq!(
            fs::read(legacy.join("chat.sqlite3")).unwrap(),
            b"legacy data must not be skipped"
        );
        assert!(!host.data_dir().join("node/import-backups").exists());
    }

    #[tokio::test]
    async fn backup_includes_the_owned_runtime_database_and_archived_rollouts() {
        let (_directory, host) = host().await;
        let archived = host.data_dir().join("engine/archived_sessions");
        std::fs::create_dir_all(&archived).unwrap();
        std::fs::write(archived.join("prior.jsonl"), b"archived rollout fixture\n").unwrap();
        let db = host.data_dir().join("engine/state_5.sqlite");
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&db)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::query("CREATE TABLE threads(id TEXT PRIMARY KEY)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO threads VALUES('active'),('archived')")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();
        let backup = prepare(
            &host,
            "node-test",
            &history(vec![
                json!({"thread":{"id":"active"},"turns":[],"origin":"local-runtime"}),
                json!({"thread":{"id":"archived"},"turns":[],"origin":"local-runtime"}),
            ]),
        )
        .await
        .unwrap();
        assert!(backup
            .directory
            .join("files/runtime-archived-history/prior.jsonl")
            .is_file());
        let state = backup
            .manifest
            .databases
            .iter()
            .find(|db| db.name == "runtime-state")
            .unwrap();
        assert_eq!(
            state
                .table_counts
                .iter()
                .find(|table| table.name == "threads")
                .unwrap()
                .count,
            2
        );
        backup.verify().await.unwrap();
    }

    async fn host() -> (TempDir, Host) {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("data");
        let host = Host::open(
            HostConfig {
                data_dir,
                runtime_path: directory.path().join("brr-runtime"),
            },
            Arc::new(|_: Value| {}),
        )
        .await
        .unwrap();
        std::fs::create_dir_all(host.data_dir().join("engine/sessions/2026/11")).unwrap();
        std::fs::write(
            host.data_dir().join("engine/sessions/2026/11/thread.jsonl"),
            b"private rollout fixture\n",
        )
        .unwrap();
        (directory, host)
    }

    fn history(threads: Vec<Value>) -> HistoryExport {
        HistoryExport {
            schema_version: 1,
            owner_origin: "local-runtime".into(),
            complete: true,
            threads,
            blobs: vec![],
            failures: vec![],
        }
    }

    #[tokio::test]
    async fn prepare_uses_wal_consistent_database_snapshots_and_excludes_credentials() {
        let (_directory, host) = host().await;
        let chat = host
            .data_dir()
            .join(brr_host::export::CHAT_STORE_RELATIVE_PATH)
            .join("chat.sqlite3");
        std::fs::create_dir_all(chat.parent().unwrap()).unwrap();
        let options = SqliteConnectOptions::new()
            .filename(&chat)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
        sqlx::query("INSERT INTO chat_drafts(id,thread_id,project_id,agent_id,document,settings,revision,updated_at) VALUES('qa-wal-row',NULL,NULL,NULL,'committed-in-wal','{}',1,'qa')")
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(chat.with_file_name("chat.sqlite3-wal").exists());
        std::fs::write(
            host.data_dir().join("engine/auth.json"),
            "CREDENTIAL_MUST_NOT_BE_BACKED_UP_OR_LISTED",
        )
        .unwrap();

        let backup = prepare(
            &host,
            "local-node-1",
            &history(vec![json!({"id":"thread-1","turns":[{"text":"history"}]})]),
        )
        .await
        .unwrap();
        connection.close().await.unwrap();
        backup.verify().await.unwrap();
        assert!(backup.directory.join("databases/chat.sqlite3").exists());
        assert!(backup.directory.join("files/history/history.json").exists());
        let manifest = serde_json::to_string(&backup.manifest).unwrap();
        assert!(!manifest.contains("CREDENTIAL_MUST_NOT_BE_BACKED_UP_OR_LISTED"));
        assert!(!manifest.contains("auth.json"));

        let backup_chat = backup.directory.join("databases/chat.sqlite3");
        let mut backup_connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(backup_chat)
                .read_only(true),
        )
        .await
        .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chat_drafts WHERE document='committed-in-wal'")
                .fetch_one(&mut backup_connection)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn verification_rejects_changed_backup_hash_and_row_counts() {
        let (_directory, host) = host().await;
        let backup = prepare(
            &host,
            "local-node-1",
            &history(vec![
                json!({"thread":{"id":"thread"},"turns":[],"origin":"local-runtime"}),
            ]),
        )
        .await
        .unwrap();
        let mut changed_counts = backup.clone();
        changed_counts.manifest.databases[0].table_counts[0].count += 1;
        assert!(changed_counts.verify().await.is_err());

        let file = backup
            .directory
            .join(&backup.manifest.databases[0].relative_path);
        std::fs::OpenOptions::new()
            .append(true)
            .open(file)
            .unwrap()
            .write_all(b"tamper")
            .unwrap();
        assert!(backup.verify().await.is_err());
    }

    #[tokio::test]
    async fn incomplete_history_or_inventory_fails_closed() {
        let (_directory, host) = host().await;
        let mut incomplete = history(vec![]);
        incomplete.complete = false;
        incomplete.failures.push(brr_host::export::HistoryFailure {
            thread_id: None,
            message: "runtime unavailable".into(),
        });
        assert!(prepare(&host, "local-node-1", &incomplete).await.is_err());
        std::fs::remove_dir_all(host.data_dir().join("engine/sessions")).unwrap();
        let verified_empty = prepare(&host, "local-node-1", &history(vec![]))
            .await
            .unwrap();
        assert!(verified_empty.manifest.verified_empty_history);
        assert!(!verified_empty.manifest.inventory_complete);
        assert!(verified_empty
            .manifest
            .warnings
            .iter()
            .any(|w| w == SESSIONS_ABSENT_WARNING));

        std::fs::create_dir(host.data_dir().join("engine/unclassified-private")).unwrap();
        assert!(prepare(&host, "local-node-1", &history(vec![]))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn fresh_owned_runtime_scratch_and_installation_identity_do_not_block_activation() {
        let (_directory, host) = host().await;
        let engine = host.data_dir().join("engine");
        std::fs::remove_dir_all(engine.join("sessions")).unwrap();
        std::fs::create_dir(engine.join(".tmp")).unwrap();
        std::fs::create_dir(engine.join("tmp")).unwrap();
        std::fs::write(engine.join("installation_id"), "OWNER_INSTALLATION_ID").unwrap();
        std::fs::write(engine.join("tmp/scratch"), "OWNER_TEMPORARY_CONTENT").unwrap();
        let backup = prepare(&host, "local-node-1", &history(vec![]))
            .await
            .unwrap();
        assert!(backup.manifest.verified_empty_history);
        let manifest = serde_json::to_string(&backup.manifest).unwrap();
        assert!(!manifest.contains("installation_id") && !manifest.contains("scratch"));
        backup.verify().await.unwrap();
    }

    #[tokio::test]
    async fn compressed_runtime_rollouts_require_their_exact_exported_thread() {
        let (_directory, host) = host().await;
        let id = "b5064008-1941-4ae1-9b4f-5b530782d0c5";
        let base = format!("rollout-2026-11-05T03-04-05-{id}.jsonl");
        let engine = host.data_dir().join("engine");
        std::fs::remove_dir_all(engine.join("sessions")).unwrap();
        std::fs::create_dir(engine.join("sessions")).unwrap();
        // The app server decodes compressed rollout bytes. The migration backup
        // retains the original bytes, including a plain/compressed sibling pair.
        std::fs::write(engine.join("sessions").join(&base), b"original rollout").unwrap();
        std::fs::write(
            engine.join("sessions").join(format!("{base}.zst")),
            b"compressed rollout",
        )
        .unwrap();
        let missing = prepare(
            &host,
            "local-node-1",
            &history(vec![json!({"id":"different"})]),
        )
        .await
        .unwrap_err();
        assert!(missing.contains("has no exported thread"), "{missing}");
        let backup = prepare(&host, "local-node-1", &history(vec![json!({"id":id})]))
            .await
            .unwrap();
        assert_eq!(backup.manifest.history_thread_count, 1);
        assert_eq!(
            backup
                .manifest
                .files
                .iter()
                .filter(|file| file.classification == "runtime-history")
                .count(),
            2
        );
        backup.verify().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn verification_rejects_symlinked_backup_parent() {
        use std::os::unix::fs::symlink;
        let (_directory, host) = host().await;
        let backup = prepare(&host, "local-node-1", &history(vec![json!({"id":"x"})]))
            .await
            .unwrap();
        let files_dir = backup.directory.join("files");
        let displaced = backup.directory.join("files.original");
        std::fs::rename(&files_dir, &displaced).unwrap();
        symlink(&displaced, &files_dir).unwrap();
        assert!(backup.verify().await.is_err());
    }
}
