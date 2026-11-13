use serde::{Deserialize, Serialize};
#[path = "project_shared.rs"]
mod shared;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

const MAX_PATH_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 256;
const MAX_MODEL_BYTES: usize = 256;
const ALLOWED_EFFORTS: &[&str] = &["low", "minimal", "medium", "high", "xhigh", "none"];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub folders: Vec<String>,
    pub primary_folder: Option<String>,
    pub revision: i64,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission: Option<ProjectPermission>,
    pub hidden: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInput {
    pub name: String,
    pub folders: Vec<String>,
    #[serde(default)]
    pub primary_folder: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission: Option<ProjectPermission>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectPermission {
    ReadOnly,
    WorkspaceWrite,
}
impl ProjectPermission {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
    fn parse(value: String) -> Result<Self, String> {
        match value.as_str() {
            "read-only" => Ok(Self::ReadOnly),
            "workspace-write" => Ok(Self::WorkspaceWrite),
            _ => Err(format!("Unknown stored project permission: {value}")),
        }
    }
}

#[derive(Clone)]
pub struct ProjectStore {
    pool: SqlitePool,
    shared_hook: Arc<RwLock<Option<(String, ProjectCommitHook)>>>,
}
type ProjectCommitHook =
    Arc<dyn Fn(&[brr_agent_core::SharedRow]) -> Result<String, String> + Send + Sync>;

impl ProjectStore {
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn open(database_path: impl AsRef<Path>) -> Result<Self, String> {
        let path = database_path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
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
        initialize_schema(&pool).await?;
        shared::init(&pool).await?;
        Ok(Self {
            pool,
            shared_hook: Arc::new(RwLock::new(None)),
        })
    }
    pub fn set_shared_commit_hook<F>(&self, node_id: &str, hook: F)
    where
        F: Fn(&[brr_agent_core::SharedRow]) -> Result<String, String> + Send + Sync + 'static,
    {
        *self.shared_hook.write().expect("project shared hook") =
            Some((node_id.into(), Arc::new(hook)));
    }
    pub async fn list(&self) -> Result<Vec<ProjectRecord>, String> {
        let rows = sqlx::query("SELECT id,name,folders,primary_folder,revision,model,effort,permission,hidden FROM project_records ORDER BY name COLLATE NOCASE,id").fetch_all(&self.pool).await.map_err(|e| e.to_string())?;
        rows.into_iter().map(project_from_row).collect()
    }
    pub async fn read(&self, id: &str) -> Result<Option<ProjectRecord>, String> {
        let row = sqlx::query("SELECT id,name,folders,primary_folder,revision,model,effort,permission,hidden FROM project_records WHERE id=?").bind(id).fetch_optional(&self.pool).await.map_err(|e| e.to_string())?;
        row.map(project_from_row).transpose()
    }
    pub async fn create(&self, input: ProjectInput) -> Result<ProjectRecord, String> {
        let mut record = ProjectRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: input.name,
            folders: input.folders,
            primary_folder: input.primary_folder,
            revision: 1,
            model: input.model,
            effort: input.effort,
            permission: input.permission,
            hidden: false,
        };
        normalize_record(&mut record)?;
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO project_records(id,name,folders,primary_folder,revision,model,effort,permission,hidden) VALUES(?,?,?,?,1,?,?,?,0)").bind(&record.id).bind(&record.name).bind(serde_json::to_string(&record.folders).map_err(|e| e.to_string())?).bind(&record.primary_folder).bind(&record.model).bind(&record.effort).bind(record.permission.as_ref().map(ProjectPermission::as_str)).execute(&mut *tx).await.map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO fleet_project_metadata(local_id,shared_revision) VALUES(?,1)")
            .bind(&record.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        self.before_shared_commit(&mut tx).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(record)
    }
    pub async fn save(&self, mut project: ProjectRecord) -> Result<ProjectRecord, String> {
        normalize_record(&mut project)?;
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let prior = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>, i64)>(
            "SELECT name,model,effort,permission,hidden FROM project_records WHERE id=? AND revision=? AND hidden=0",
        )
        .bind(&project.id)
        .bind(project.revision)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Project was changed or deleted; reload before saving".to_owned())?;
        let shared_metadata_changed = prior.0 != project.name
            || prior.1 != project.model
            || prior.2 != project.effort
            || prior.3
                != project
                    .permission
                    .as_ref()
                    .map(ProjectPermission::as_str)
                    .map(str::to_owned)
            || prior.4 != 0;
        let result = sqlx::query("UPDATE project_records SET name=?,folders=?,primary_folder=?,revision=revision+1,model=?,effort=?,permission=?,hidden=0 WHERE id=? AND revision=? AND hidden=0")
            .bind(&project.name).bind(serde_json::to_string(&project.folders).map_err(|e| e.to_string())?).bind(&project.primary_folder).bind(&project.model).bind(&project.effort).bind(project.permission.as_ref().map(ProjectPermission::as_str)).bind(&project.id).bind(project.revision).execute(&mut *tx).await.map_err(|e| e.to_string())?;
        if result.rows_affected() != 1 {
            return Err("Project was changed or deleted; reload before saving".into());
        }
        if shared_metadata_changed {
            sqlx::query("UPDATE fleet_project_metadata SET shared_revision=shared_revision+1 WHERE local_id=?")
                .bind(&project.id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        project.revision += 1;
        project.hidden = false;
        self.before_shared_commit(&mut tx).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(project)
    }

    /// Update only portable project metadata while preserving node-local folder
    /// mappings and chat membership. The revision check and write share one
    /// transaction, so a stale editor cannot overwrite newer metadata.
    pub async fn save_metadata(
        &self,
        id: &str,
        expected_revision: i64,
        name: String,
        model: Option<String>,
        effort: Option<String>,
        permission: Option<ProjectPermission>,
    ) -> Result<ProjectRecord, String> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| e.to_string())?;
        let row = sqlx::query("SELECT id,name,folders,primary_folder,revision,model,effort,permission,hidden FROM project_records WHERE id=? AND revision=? AND hidden=0")
            .bind(id)
            .bind(expected_revision)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Project was changed or deleted; reload before saving".to_owned())?;
        let mut project = project_from_row(row)?;
        let previous = (
            project.name.clone(),
            project.model.clone(),
            project.effort.clone(),
            project.permission.clone(),
        );
        let local_folders = project.folders.clone();
        let local_primary_folder = project.primary_folder.clone();
        project.name = name;
        project.model = model;
        project.effort = effort;
        project.permission = permission;
        normalize_record(&mut project)?;
        project.folders = local_folders;
        project.primary_folder = local_primary_folder;
        let changed = previous
            != (
                project.name.clone(),
                project.model.clone(),
                project.effort.clone(),
                project.permission.clone(),
            );
        let result = sqlx::query("UPDATE project_records SET name=?,revision=revision+1,model=?,effort=?,permission=? WHERE id=? AND revision=? AND hidden=0")
            .bind(&project.name)
            .bind(&project.model)
            .bind(&project.effort)
            .bind(project.permission.as_ref().map(ProjectPermission::as_str))
            .bind(id)
            .bind(expected_revision)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        if result.rows_affected() != 1 {
            return Err("Project was changed or deleted; reload before saving".into());
        }
        if changed {
            sqlx::query("UPDATE fleet_project_metadata SET shared_revision=shared_revision+1 WHERE local_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        project.revision += 1;
        self.before_shared_commit(&mut tx).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(project)
    }
    pub async fn set_membership(
        &self,
        thread_id: &str,
        project_id: Option<&str>,
    ) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        if let Some(id) = project_id {
            let exists: i64 =
                sqlx::query("SELECT COUNT(*) FROM project_records WHERE id=? AND hidden=0")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?
                    .try_get(0)
                    .map_err(|e| e.to_string())?;
            if exists == 0 {
                return Err("Project does not exist".into());
            }
            sqlx::query("INSERT INTO chat_project_memberships(thread_id,project_id) VALUES(?,?) ON CONFLICT(thread_id) DO UPDATE SET project_id=excluded.project_id").bind(thread_id).bind(id).execute(&mut *tx).await.map_err(|e| e.to_string())?;
        } else {
            sqlx::query("DELETE FROM chat_project_memberships WHERE thread_id=?")
                .bind(thread_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())
    }
    pub async fn membership(&self, thread_id: &str) -> Result<Option<String>, String> {
        sqlx::query("SELECT project_id FROM chat_project_memberships WHERE thread_id=?")
            .bind(thread_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| e.to_string())?
            .map(|r| r.try_get(0).map_err(|e| e.to_string()))
            .transpose()
    }
    pub async fn members(&self, project_id: &str) -> Result<Vec<String>, String> {
        sqlx::query(
            "SELECT thread_id FROM chat_project_memberships WHERE project_id=? ORDER BY thread_id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| r.try_get(0).map_err(|e| e.to_string()))
        .collect()
    }
    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM chat_project_memberships WHERE project_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let result = sqlx::query(
            "UPDATE project_records SET hidden=1,revision=revision+1 WHERE id=? AND hidden=0",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        if result.rows_affected() != 1 {
            return Err("Project does not exist".into());
        }
        sqlx::query(
            "UPDATE fleet_project_metadata SET shared_revision=shared_revision+1 WHERE local_id=?",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        self.before_shared_commit(&mut tx).await?;
        tx.commit().await.map_err(|e| e.to_string())
    }

    /// Delete with an outer, caller-supplied revision check. Hiding the project
    /// happens before memberships are removed, inside the same transaction.
    pub async fn delete_expected_revision(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> Result<(), String> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| e.to_string())?;
        let changed = sqlx::query("UPDATE project_records SET hidden=1,revision=revision+1 WHERE id=? AND revision=? AND hidden=0")
            .bind(id)
            .bind(expected_revision)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        if changed.rows_affected() != 1 {
            return Err("Project was changed or deleted; reload before deleting".into());
        }
        sqlx::query("DELETE FROM chat_project_memberships WHERE project_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query(
            "UPDATE fleet_project_metadata SET shared_revision=shared_revision+1 WHERE local_id=?",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        self.before_shared_commit(&mut tx).await?;
        tx.commit().await.map_err(|e| e.to_string())
    }
}

async fn initialize_schema(pool: &SqlitePool) -> Result<(), String> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| e.to_string())?;
    let project_columns = table_columns(&mut tx, "project_records").await?;
    let membership_columns = table_columns(&mut tx, "chat_project_memberships").await?;
    let current_columns: HashSet<&str> = [
        "id",
        "name",
        "folders",
        "primary_folder",
        "revision",
        "model",
        "effort",
        "permission",
        "hidden",
    ]
    .into_iter()
    .collect();
    let legacy_columns: HashSet<&str> = ["path", "name", "model", "effort", "permission", "hidden"]
        .into_iter()
        .collect();
    let membership_schema: HashSet<&str> = ["thread_id", "project_id"].into_iter().collect();

    if project_columns.is_empty() {
        if !membership_columns.is_empty() {
            return Err(
                "Cannot initialize project storage: memberships exist without project records"
                    .into(),
            );
        }
        create_project_table(&mut tx).await?;
        create_membership_table(&mut tx).await?;
    } else if project_columns == current_columns {
        if !membership_columns.is_empty() && membership_columns != membership_schema {
            return Err(
                "Cannot initialize project storage: unrecognized chat membership schema".into(),
            );
        }
        if membership_columns.is_empty() {
            create_membership_table(&mut tx).await?;
        }
    } else if project_columns == legacy_columns {
        if !membership_columns.is_empty() && membership_columns != membership_schema {
            return Err(
                "Cannot migrate project storage: unrecognized chat membership schema".into(),
            );
        }
        migrate_legacy_schema(&mut tx, !membership_columns.is_empty()).await?;
    } else {
        return Err(format!(
            "Cannot initialize project storage: unrecognized project_records columns ({})",
            project_columns.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    tx.commit().await.map_err(|e| e.to_string())
}

async fn table_columns(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &str,
) -> Result<HashSet<&'static str>, String> {
    let pragma = match table {
        "project_records" => "PRAGMA table_info(project_records)",
        "chat_project_memberships" => "PRAGMA table_info(chat_project_memberships)",
        _ => return Err("Cannot inspect an unknown project storage table".into()),
    };
    let rows = sqlx::query(pragma)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    rows.into_iter()
        .map(|row| {
            let name: String = row.try_get("name").map_err(|e| e.to_string())?;
            match name.as_str() {
                "id" => Ok("id"),
                "path" => Ok("path"),
                "name" => Ok("name"),
                "folders" => Ok("folders"),
                "primary_folder" => Ok("primary_folder"),
                "revision" => Ok("revision"),
                "model" => Ok("model"),
                "effort" => Ok("effort"),
                "permission" => Ok("permission"),
                "hidden" => Ok("hidden"),
                "thread_id" => Ok("thread_id"),
                "project_id" => Ok("project_id"),
                _ => Err(format!(
                    "Cannot initialize project storage: unknown column {name} in {table}"
                )),
            }
        })
        .collect()
}

async fn create_project_table(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<(), String> {
    sqlx::query("CREATE TABLE project_records (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, folders TEXT NOT NULL, primary_folder TEXT, revision INTEGER NOT NULL, model TEXT, effort TEXT, permission TEXT CHECK(permission IS NULL OR permission IN ('read-only','workspace-write')), hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0,1)))")
        .execute(&mut **tx).await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn create_membership_table(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), String> {
    sqlx::query("CREATE TABLE chat_project_memberships (thread_id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL REFERENCES project_records(id) ON DELETE CASCADE)")
        .execute(&mut **tx).await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn migrate_legacy_schema(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    has_memberships: bool,
) -> Result<(), String> {
    let records =
        sqlx::query("SELECT path,name,model,effort,permission,hidden FROM project_records")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
    let memberships = if has_memberships {
        sqlx::query("SELECT thread_id,project_id FROM chat_project_memberships")
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };

    if has_memberships {
        sqlx::query("DROP TABLE chat_project_memberships")
            .execute(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    sqlx::query("DROP TABLE project_records")
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    create_project_table(tx).await?;
    for row in records {
        let path: String = row.try_get("path").map_err(|e| e.to_string())?;
        let name: String = row.try_get("name").map_err(|e| e.to_string())?;
        let model: Option<String> = row.try_get("model").map_err(|e| e.to_string())?;
        let effort: Option<String> = row.try_get("effort").map_err(|e| e.to_string())?;
        let permission: Option<String> = row.try_get("permission").map_err(|e| e.to_string())?;
        let hidden: i64 = row.try_get("hidden").map_err(|e| e.to_string())?;
        let folders = serde_json::to_string(&vec![path.clone()]).map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO project_records(id,name,folders,primary_folder,revision,model,effort,permission,hidden) VALUES(?,?,?,?,1,?,?,?,?)")
            .bind(&path).bind(name).bind(folders).bind(&path).bind(model).bind(effort)
            .bind(permission).bind(hidden).execute(&mut **tx).await.map_err(|e| e.to_string())?;
    }
    create_membership_table(tx).await?;
    for row in memberships {
        let thread_id: String = row.try_get("thread_id").map_err(|e| e.to_string())?;
        let project_id: String = row.try_get("project_id").map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO chat_project_memberships(thread_id,project_id) VALUES(?,?)")
            .bind(thread_id)
            .bind(project_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn project_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ProjectRecord, String> {
    let permission: Option<String> = row.try_get("permission").map_err(|e| e.to_string())?;
    Ok(ProjectRecord {
        id: row.try_get("id").map_err(|e| e.to_string())?,
        name: row.try_get("name").map_err(|e| e.to_string())?,
        folders: serde_json::from_str(
            &row.try_get::<String, _>("folders")
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?,
        primary_folder: row.try_get("primary_folder").map_err(|e| e.to_string())?,
        revision: row.try_get("revision").map_err(|e| e.to_string())?,
        model: row.try_get("model").map_err(|e| e.to_string())?,
        effort: row.try_get("effort").map_err(|e| e.to_string())?,
        permission: permission.map(ProjectPermission::parse).transpose()?,
        hidden: row.try_get::<i64, _>("hidden").map_err(|e| e.to_string())? != 0,
    })
}
fn normalize_record(p: &mut ProjectRecord) -> Result<(), String> {
    p.name = p.name.trim().to_owned();
    validate_text(&p.name, "name", MAX_NAME_BYTES)?;
    if let Some(m) = &p.model {
        validate_text(m, "model", MAX_MODEL_BYTES)?;
    }
    if let Some(e) = &p.effort {
        if !ALLOWED_EFFORTS.contains(&e.as_str()) {
            return Err("Choose a supported project reasoning effort".into());
        }
    }
    let mut seen = HashSet::new();
    let mut folders = Vec::new();
    for folder in &p.folders {
        let canonical = normalize_project_path(folder)?;
        if !seen.insert(canonical.clone()) {
            return Err("Project folders must be unique".into());
        }
        folders.push(canonical);
    }
    p.primary_folder = match p.primary_folder.as_deref() {
        Some(path) => Some(normalize_project_path(path)?),
        None => folders.first().cloned(),
    };
    if p.primary_folder
        .as_ref()
        .is_some_and(|primary| !folders.contains(primary))
    {
        return Err("Primary folder must belong to the project".into());
    }
    p.folders = folders;
    Ok(())
}
fn validate_text(v: &str, field: &str, max: usize) -> Result<(), String> {
    if v.trim().is_empty() || v.chars().any(char::is_control) {
        return Err(format!("Choose a valid project {field}"));
    }
    if v.len() > max {
        return Err(format!("Project {field} is too long"));
    }
    Ok(())
}
pub(crate) fn normalize_project_path(path: &str) -> Result<String, String> {
    validate_text(path, "path", MAX_PATH_BYTES)?;
    if !Path::new(path).is_absolute() {
        return Err("Project paths must be absolute".into());
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("Cannot resolve project directory: {e}")),
        Ok(_) => Err("The project path must be a directory".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut out = PathBuf::new();
            for c in Path::new(path).components() {
                match c {
                    std::path::Component::Prefix(x) => out.push(x.as_os_str()),
                    std::path::Component::RootDir => out.push(c.as_os_str()),
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    std::path::Component::Normal(x) => out.push(x),
                }
            }
            Ok(out.to_string_lossy().into_owned())
        }
        Err(e) => Err(format!("Cannot inspect project path: {e}")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnection;
    use sqlx::Connection;

    #[tokio::test]
    async fn opens_legacy_path_keyed_projects_and_allows_no_project_membership() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("legacy.sqlite");
        let options = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .foreign_keys(false);
        let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
        sqlx::query("CREATE TABLE project_records (path TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,model TEXT,effort TEXT,permission TEXT CHECK(permission IS NULL OR permission IN ('read-only', 'workspace-write')),hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0,1)))")
            .execute(&mut connection).await.unwrap();
        sqlx::query("CREATE TABLE chat_project_memberships (thread_id TEXT PRIMARY KEY NOT NULL,project_id TEXT NOT NULL REFERENCES project_records(id) ON DELETE CASCADE)")
            .execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO project_records(path,name,model,effort,permission,hidden) VALUES(?,?,?,?,?,?),(?,?,?,?,?,?)")
            .bind("/legacy/active").bind("Active").bind(Some("gpt-5"))
            .bind(Some("high")).bind(Some("workspace-write")).bind(0_i64)
            .bind("/legacy/hidden").bind("Hidden").bind(Option::<String>::None)
            .bind(Option::<String>::None).bind(Option::<String>::None).bind(1_i64)
            .execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO chat_project_memberships(thread_id,project_id) VALUES(?,?)")
            .bind("previously-grouped-chat")
            .bind("/legacy/active")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let (store, concurrent_store) =
            tokio::join!(ProjectStore::open(&db), ProjectStore::open(&db));
        let store = store.unwrap();
        let concurrent_store = concurrent_store.unwrap();
        let records = store.list().await.unwrap();
        assert_eq!(concurrent_store.list().await.unwrap(), records);
        assert_eq!(records.len(), 2);
        let active = records
            .iter()
            .find(|record| record.id == "/legacy/active")
            .unwrap();
        assert_eq!(active.name, "Active");
        assert_eq!(active.folders, vec!["/legacy/active"]);
        assert_eq!(active.primary_folder.as_deref(), Some("/legacy/active"));
        assert_eq!(active.model.as_deref(), Some("gpt-5"));
        assert_eq!(active.effort.as_deref(), Some("high"));
        assert_eq!(active.permission, Some(ProjectPermission::WorkspaceWrite));
        assert!(!active.hidden);
        assert!(records
            .iter()
            .any(|record| record.id == "/legacy/hidden" && record.hidden));
        assert_eq!(
            store.membership("previously-grouped-chat").await.unwrap(),
            Some("/legacy/active".into())
        );

        store.set_membership("ordinary-chat", None).await.unwrap();
        assert_eq!(store.membership("ordinary-chat").await.unwrap(), None);
        let reopened = ProjectStore::open(&db).await.unwrap();
        assert_eq!(reopened.list().await.unwrap(), records);
        assert_eq!(
            reopened
                .membership("previously-grouped-chat")
                .await
                .unwrap(),
            Some("/legacy/active".into())
        );
        let fresh = reopened
            .create(ProjectInput {
                name: "Fresh project".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        assert_ne!(fresh.id, "/legacy/active");
        reopened
            .set_membership("new-chat", Some(&fresh.id))
            .await
            .unwrap();
        assert_eq!(
            reopened.membership("new-chat").await.unwrap(),
            Some(fresh.id)
        );
    }

    #[tokio::test]
    async fn failed_legacy_migration_rolls_back_without_dropping_existing_rows() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("legacy.sqlite");
        let options = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .foreign_keys(false);
        let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
        sqlx::query("CREATE TABLE project_records (path TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,model TEXT,effort TEXT,permission TEXT CHECK(permission IS NULL OR permission IN ('read-only', 'workspace-write')),hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0,1)))")
            .execute(&mut connection).await.unwrap();
        sqlx::query("CREATE TABLE chat_project_memberships (thread_id TEXT PRIMARY KEY NOT NULL,project_id TEXT NOT NULL REFERENCES project_records(id) ON DELETE CASCADE)")
            .execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO project_records(path,name) VALUES(?,?)")
            .bind("/legacy/preserved")
            .bind("Preserved")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO chat_project_memberships(thread_id,project_id) VALUES(?,?)")
            .bind("orphaned-chat")
            .bind("/missing/project")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let error = match ProjectStore::open(&db).await {
            Ok(_) => panic!("malformed legacy memberships unexpectedly migrated"),
            Err(error) => error,
        };
        assert!(
            error.contains("FOREIGN KEY"),
            "unexpected migration error: {error}"
        );

        let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
        let project: (String, String) = sqlx::query_as("SELECT path,name FROM project_records")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(project, ("/legacy/preserved".into(), "Preserved".into()));
        let membership: (String, String) =
            sqlx::query_as("SELECT thread_id,project_id FROM chat_project_memberships")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(
            membership,
            ("orphaned-chat".into(), "/missing/project".into())
        );
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn create_save_membership_and_delete_are_durable_and_revision_checked() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("p.sqlite");
        let s = ProjectStore::open(&db).await.unwrap();
        let folder = d.path().join("work");
        std::fs::create_dir(&folder).unwrap();
        let input = ProjectInput {
            name: " Test ".into(),
            folders: vec![folder.to_string_lossy().into_owned()],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        };
        let p = s.create(input).await.unwrap();
        assert_eq!(p.name, "Test");
        assert_eq!(p.revision, 1);
        s.set_membership("thread", Some(&p.id)).await.unwrap();
        assert_eq!(s.membership("thread").await.unwrap(), Some(p.id.clone()));
        let mut changed = p.clone();
        changed.name = "Renamed".into();
        let saved = s.save(changed).await.unwrap();
        assert_eq!(saved.revision, 2);
        assert!(s.save(p).await.unwrap_err().contains("changed"));
        s.delete(&saved.id).await.unwrap();
        assert_eq!(s.membership("thread").await.unwrap(), None);
        assert!(s.list().await.unwrap()[0].hidden);
    }
    #[tokio::test]
    async fn folder_duplicates_and_invalid_primary_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        let s = ProjectStore::open(d.path().join("p.sqlite")).await.unwrap();
        let f = d.path().join("missing").to_string_lossy().to_string();
        let i = ProjectInput {
            name: "x".into(),
            folders: vec![f.clone(), f.clone()],
            primary_folder: None,
            model: None,
            effort: None,
            permission: None,
        };
        assert!(s.create(i).await.unwrap_err().contains("unique"));
    }

    #[tokio::test]
    async fn canonical_folder_aliases_are_unique_within_a_project_and_shared_between_projects() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projects.sqlite");
        let one = temp.path().join("one");
        let two = temp.path().join("two");
        std::fs::create_dir(&one).unwrap();
        std::fs::create_dir(&two).unwrap();
        let canonical = one.canonicalize().unwrap().to_string_lossy().into_owned();
        let input = ProjectInput {
            name: "Shared".into(),
            folders: vec![
                one.to_string_lossy().into_owned(),
                two.to_string_lossy().into_owned(),
            ],
            primary_folder: Some(two.to_string_lossy().into_owned()),
            model: None,
            effort: None,
            permission: None,
        };
        let store = ProjectStore::open(&db).await.unwrap();
        let first = store.create(input.clone()).await.unwrap();
        let second = store.create(input.clone()).await.unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(first.folders[0], canonical);
        let mut duplicate = input.clone();
        duplicate.folders = vec![canonical, one.join(".").to_string_lossy().into_owned()];
        duplicate.primary_folder = None;
        assert!(store
            .create(duplicate)
            .await
            .unwrap_err()
            .contains("unique"));
        let mut invalid = input;
        invalid.primary_folder = Some(temp.path().to_string_lossy().into_owned());
        assert!(store.create(invalid).await.is_err());
        let restarted = ProjectStore::open(&db).await.unwrap();
        assert_eq!(restarted.read(&first.id).await.unwrap(), Some(first));
        assert_eq!(restarted.read(&second.id).await.unwrap(), Some(second));
    }

    #[tokio::test]
    async fn folderless_projects_persist_without_a_primary_folder() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("p.sqlite");
        let store = ProjectStore::open(&db).await.unwrap();
        let created = store
            .create(ProjectInput {
                name: "Scratch project".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        assert_eq!(created.folders, Vec::<String>::new());
        assert_eq!(created.primary_folder, None);
        assert_eq!(
            ProjectStore::open(&db)
                .await
                .unwrap()
                .read(&created.id)
                .await
                .unwrap(),
            Some(created)
        );
    }

    #[tokio::test]
    async fn rejected_shared_project_write_rolls_back_the_local_project() {
        let d = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(d.path().join("p.sqlite")).await.unwrap();
        store.set_shared_commit_hook("local-node", |_| Err("Git rejected project write".into()));
        let result = store
            .create(ProjectInput {
                name: "Shared project".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await;
        assert!(
            result.is_err(),
            "Project metadata must commit through shared authority"
        );
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn shared_project_metadata_preserves_local_folder_and_saved_chat_membership() {
        let d = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(d.path().join("p.sqlite")).await.unwrap();
        let folder = d.path().to_string_lossy().into_owned();
        let original = store
            .create(ProjectInput {
                name: "Before".into(),
                folders: vec![folder.clone()],
                primary_folder: Some(folder),
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        store
            .set_membership("existing-thread", Some(&original.id))
            .await
            .unwrap();
        let mut rows = store.shared_snapshot("local-node").await.unwrap();
        assert!(rows[0].data.get("folders").is_none());
        rows[0].data["name"] = serde_json::json!("After");
        rows[0].data["revision"] = serde_json::json!(2);
        store
            .materialize_shared_snapshot("local-node", &rows, "git-update")
            .await
            .unwrap();
        let updated = store.read(&original.id).await.unwrap().unwrap();
        assert_eq!(updated.name, "After");
        assert_eq!(updated.folders, original.folders);
        assert_eq!(updated.primary_folder, original.primary_folder);
        assert_eq!(
            store
                .membership("existing-thread")
                .await
                .unwrap()
                .as_deref(),
            Some(original.id.as_str())
        );
        let mut foreign = rows[0].clone();
        foreign.origin_node_id = Some("other-node".into());
        let mut merged_rows = rows.clone();
        merged_rows.push(foreign);
        store
            .materialize_shared_snapshot("local-node", &merged_rows, "git-foreign")
            .await
            .unwrap();
        let imported = store
            .read(&brr_agent_core::shared_local_id(
                "other-node",
                brr_agent_core::SharedTable::Projects,
                &original.id,
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(imported.folders.is_empty());
        assert!(imported.primary_folder.is_none());
        assert_eq!(
            store
                .shared_snapshot("local-node")
                .await
                .unwrap()
                .iter()
                .filter(|row| row.key == original.id)
                .count(),
            2,
            "Equal raw IDs on different origins remain independent"
        );
        store.set_shared_commit_hook("local-node", |_| Err("Git rejected deletion".into()));
        assert!(store.delete(&original.id).await.is_err());
        assert!(!store.read(&original.id).await.unwrap().unwrap().hidden);
        assert_eq!(
            store
                .membership("existing-thread")
                .await
                .unwrap()
                .as_deref(),
            Some(original.id.as_str())
        );
    }

    #[tokio::test]
    async fn scoped_metadata_and_delete_cas_preserve_local_membership_on_stale_revision() {
        let d = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(d.path().join("scoped-projects.sqlite"))
            .await
            .unwrap();
        let folder = d.path().to_string_lossy().into_owned();
        let original = store
            .create(ProjectInput {
                name: "Before".into(),
                folders: vec![folder.clone()],
                primary_folder: Some(folder),
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        store
            .set_membership("local-thread", Some(&original.id))
            .await
            .unwrap();
        let updated = store
            .save_metadata(
                &original.id,
                1,
                "After".into(),
                Some("fixture-model".into()),
                Some("high".into()),
                Some(ProjectPermission::ReadOnly),
            )
            .await
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.folders, original.folders);
        assert_eq!(updated.primary_folder, original.primary_folder);
        assert_eq!(
            store.membership("local-thread").await.unwrap().as_deref(),
            Some(original.id.as_str())
        );
        assert!(store
            .save_metadata(&original.id, 1, "Stale".into(), None, None, None)
            .await
            .is_err());
        assert!(store
            .delete_expected_revision(&original.id, 1)
            .await
            .is_err());
        assert!(!store.read(&original.id).await.unwrap().unwrap().hidden);
        assert_eq!(
            store.membership("local-thread").await.unwrap().as_deref(),
            Some(original.id.as_str()),
            "a stale delete must fail before it removes membership"
        );

        store
            .delete_expected_revision(&original.id, 2)
            .await
            .unwrap();
        assert!(store.read(&original.id).await.unwrap().unwrap().hidden);
        assert_eq!(store.membership("local-thread").await.unwrap(), None);
    }

    #[tokio::test]
    async fn independent_folder_edits_do_not_change_shared_project_metadata() {
        let base = tempfile::tempdir().unwrap();
        let first_db = base.path().join("first.sqlite");
        let second_db = base.path().join("second.sqlite");
        let first = ProjectStore::open(&first_db).await.unwrap();
        let second = ProjectStore::open(&second_db).await.unwrap();
        let created = first
            .create(ProjectInput {
                name: "Shared project".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let baseline = first.shared_snapshot("first-node").await.unwrap();
        second
            .materialize_shared_snapshot("second-node", &baseline, "git-head-1")
            .await
            .unwrap();

        let one = base.path().join("one");
        let two = base.path().join("two");
        let three = base.path().join("three");
        std::fs::create_dir(&one).unwrap();
        std::fs::create_dir(&two).unwrap();
        std::fs::create_dir(&three).unwrap();
        let mut first_project = first.read(&created.id).await.unwrap().unwrap();
        first_project.folders = vec![one.to_string_lossy().into_owned()];
        first_project.primary_folder = first_project.folders.first().cloned();
        first.save(first_project).await.unwrap();

        let imported_id = brr_agent_core::shared_local_id(
            "first-node",
            brr_agent_core::SharedTable::Projects,
            &created.id,
        );
        let mut second_project = second.read(&imported_id).await.unwrap().unwrap();
        second_project.folders = vec![two.to_string_lossy().into_owned()];
        second_project.primary_folder = second_project.folders.first().cloned();
        second_project = second.save(second_project).await.unwrap();
        second_project
            .folders
            .push(three.to_string_lossy().into_owned());
        second_project.primary_folder = second_project.folders.first().cloned();
        second.save(second_project).await.unwrap();

        let first_snapshot = first.shared_snapshot("first-node").await.unwrap();
        let second_snapshot = second.shared_snapshot("second-node").await.unwrap();
        assert_eq!(first_snapshot, baseline);
        assert_eq!(second_snapshot, baseline);
    }

    #[tokio::test]
    async fn materializing_changed_metadata_advances_local_cas_even_when_shared_revision_matches() {
        let d = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(d.path().join("p.sqlite")).await.unwrap();
        let stale = store
            .create(ProjectInput {
                name: "Before".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let mut incoming = store.shared_snapshot("local-node").await.unwrap();
        incoming[0].data["name"] = serde_json::json!("After from Git");
        // Deliberately keep the numeric shared revision equal to the local
        // CAS revision. Content comparison must still invalidate stale saves.
        incoming[0].data["revision"] = serde_json::json!(stale.revision);
        store
            .materialize_shared_snapshot("local-node", &incoming, "git-new-head")
            .await
            .unwrap();
        assert!(store
            .save(ProjectRecord {
                name: "Stale edit".into(),
                ..stale
            })
            .await
            .unwrap_err()
            .contains("changed or deleted"));
        assert_eq!(
            store.read(&incoming[0].key).await.unwrap().unwrap().name,
            "After from Git"
        );
    }

    #[tokio::test]
    async fn git_project_tombstone_hides_but_preserves_local_project_state_and_stays_suppressed() {
        let d = tempfile::tempdir().unwrap();
        let store = ProjectStore::open(d.path().join("p.sqlite")).await.unwrap();
        store.set_shared_commit_hook("local-node", |_| Ok("git-project-head".into()));
        let folder = d.path().join("workspace");
        std::fs::create_dir(&folder).unwrap();
        let folder = folder.to_string_lossy().into_owned();
        let project = store
            .create(ProjectInput {
                name: "Shared project".into(),
                folders: vec![folder.clone()],
                primary_folder: Some(folder.clone()),
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        store
            .set_membership("saved-thread", Some(&project.id))
            .await
            .unwrap();

        store
            .materialize_shared_snapshot("local-node", &[], "git-project-tombstone")
            .await
            .unwrap();
        let hidden = store.read(&project.id).await.unwrap().unwrap();
        assert!(hidden.hidden);
        assert_eq!(hidden.folders, project.folders);
        assert_eq!(hidden.primary_folder, project.primary_folder);
        assert_eq!(
            store.membership("saved-thread").await.unwrap(),
            Some(project.id.clone())
        );
        assert!(store
            .shared_snapshot("local-node")
            .await
            .unwrap()
            .is_empty());

        store
            .create(ProjectInput {
                name: "Unrelated project".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let projects = store.list().await.unwrap();
        let unrelated_id = projects
            .iter()
            .find(|project| project.name == "Unrelated project")
            .unwrap()
            .id
            .clone();
        let rows = store.shared_snapshot("local-node").await.unwrap();
        assert_eq!(
            rows.iter().map(|row| row.key.clone()).collect::<Vec<_>>(),
            vec![unrelated_id]
        );
        store.delete(&rows[0].key).await.unwrap();
        let logical_delete = store.shared_snapshot("local-node").await.unwrap();
        assert_eq!(logical_delete.len(), 1);
        assert_eq!(logical_delete[0].key, rows[0].key);
        assert_eq!(logical_delete[0].data["hidden"], serde_json::json!(true));
    }
}
