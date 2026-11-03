use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, Manager};

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
}

impl ProjectStore {
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
        sqlx::query("CREATE TABLE IF NOT EXISTS project_records (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, folders TEXT NOT NULL, primary_folder TEXT, revision INTEGER NOT NULL, model TEXT, effort TEXT, permission TEXT CHECK(permission IS NULL OR permission IN ('read-only','workspace-write')), hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0,1)))").execute(&pool).await.map_err(|e| e.to_string())?;
        sqlx::query("CREATE TABLE IF NOT EXISTS chat_project_memberships (thread_id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL REFERENCES project_records(id) ON DELETE CASCADE)").execute(&pool).await.map_err(|e| e.to_string())?;
        Ok(Self { pool })
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
        sqlx::query("INSERT INTO project_records(id,name,folders,primary_folder,revision,model,effort,permission,hidden) VALUES(?,?,?,?,1,?,?,?,0)").bind(&record.id).bind(&record.name).bind(serde_json::to_string(&record.folders).map_err(|e| e.to_string())?).bind(&record.primary_folder).bind(&record.model).bind(&record.effort).bind(record.permission.as_ref().map(ProjectPermission::as_str)).execute(&self.pool).await.map_err(|e| e.to_string())?;
        Ok(record)
    }
    pub async fn save(&self, mut project: ProjectRecord) -> Result<ProjectRecord, String> {
        normalize_record(&mut project)?;
        let result = sqlx::query("UPDATE project_records SET name=?,folders=?,primary_folder=?,revision=revision+1,model=?,effort=?,permission=?,hidden=0 WHERE id=? AND revision=? AND hidden=0")
            .bind(&project.name).bind(serde_json::to_string(&project.folders).map_err(|e| e.to_string())?).bind(&project.primary_folder).bind(&project.model).bind(&project.effort).bind(project.permission.as_ref().map(ProjectPermission::as_str)).bind(&project.id).bind(project.revision).execute(&self.pool).await.map_err(|e| e.to_string())?;
        if result.rows_affected() != 1 {
            return Err("Project was changed or deleted; reload before saving".into());
        }
        project.revision += 1;
        project.hidden = false;
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
        tx.commit().await.map_err(|e| e.to_string())
    }
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
fn database_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("projects.sqlite"))
}
#[tauri::command]
pub async fn list_projects(app: AppHandle) -> Result<Value, String> {
    Ok(json!({"data":ProjectStore::open(database_path(&app)?).await?.list().await?}))
}
#[tauri::command]
pub async fn create_project(
    app: AppHandle,
    project: ProjectInput,
) -> Result<ProjectRecord, String> {
    ProjectStore::open(database_path(&app)?)
        .await?
        .create(project)
        .await
}
#[tauri::command]
pub async fn save_project(app: AppHandle, project: ProjectRecord) -> Result<ProjectRecord, String> {
    ProjectStore::open(database_path(&app)?)
        .await?
        .save(project)
        .await
}
#[tauri::command]
pub async fn move_chat_to_project(
    app: AppHandle,
    thread_id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    ProjectStore::open(database_path(&app)?)
        .await?
        .set_membership(&thread_id, project_id.as_deref())
        .await
}
#[tauri::command]
pub async fn delete_project(app: AppHandle, project_id: String) -> Result<(), String> {
    ProjectStore::open(database_path(&app)?)
        .await?
        .delete(&project_id)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
