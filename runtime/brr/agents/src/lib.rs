mod memory;
mod profiles;
mod questions;
mod schedule;
mod shared;
mod shared_validate;
mod tasks;
mod tickets;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use uuid::Uuid;

pub use memory::{LearnedSkill, MemoryAccess, MemoryRecord, MemoryScope, MemorySource};
pub use profiles::{AgentProfile, ProfileCandidate, ProfileDraft, SessionBinding};
pub use questions::{
    NewQuestionRequest, Question, QuestionAnswers, QuestionOption, QuestionRequest,
};
pub use schedule::{Workflow, WorkflowInput};
pub use shared::{SharedRow, SharedSnapshot, SharedTable, shared_entity_id, shared_local_id};
pub use tasks::{
    DelegationNode, DelegationProposal, Task, TaskAction, TaskInput, TaskRun, TaskSettings,
    WorkspacePermission,
};
pub use tickets::{
    AcceptanceCriterion, NewAcceptanceCriterion, Ticket, TicketActivity, TicketActor,
    TicketActorKind, TicketComment, TicketDetail, TicketFilter, TicketInput, TicketMutation,
    TicketPage, TicketPriority, TicketRunProposal, TicketRunSummary, TicketStatus, TicketUpdate,
};

#[derive(Clone)]
pub struct AgentStore {
    pub(crate) pool: SqlitePool,
    pub(crate) root: Arc<PathBuf>,
    shared_commit_hook: Arc<RwLock<Option<SharedCommitHook>>>,
}

pub type SharedCommitHook = Arc<dyn Fn(&SharedSnapshot) -> Result<String, String> + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEvent {
    pub seq: i64,
    pub kind: String,
    pub entity_id: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

impl AgentStore {
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let path = root.join("agents.sqlite3");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, entity_id TEXT NOT NULL, payload TEXT NOT NULL, created_at INTEGER NOT NULL)")
            .execute(&pool).await.map_err(|e| e.to_string())?;
        sqlx::query("CREATE TABLE IF NOT EXISTS operation_results (operation_id TEXT PRIMARY KEY, kind TEXT NOT NULL, result TEXT NOT NULL)")
            .execute(&pool).await.map_err(|e| e.to_string())?;
        profiles::init(&pool).await?;
        memory::init(&pool).await?;
        tasks::init(&pool).await?;
        tickets::init(&pool).await?;
        questions::init(&pool).await?;
        schedule::init(&pool).await?;
        shared::init(&pool).await?;
        Ok(Self {
            pool,
            root: Arc::new(root),
            shared_commit_hook: Arc::new(RwLock::new(None)),
        })
    }

    /// Installs the synchronous authority hook used before shared SQL changes
    /// commit. The hook receives a snapshot built from the active transaction.
    pub fn set_shared_commit_hook<F>(&self, hook: F)
    where
        F: Fn(&SharedSnapshot) -> Result<String, String> + Send + Sync + 'static,
    {
        *self.shared_commit_hook.write().expect("shared hook lock") = Some(Arc::new(hook));
    }

    pub fn clear_shared_commit_hook(&self) {
        *self.shared_commit_hook.write().expect("shared hook lock") = None;
    }

    /// Returns the local shared-data candidate for Git publication. Its rows
    /// have no origin until fleet activation establishes the local identity.
    pub async fn shared_snapshot(&self) -> Result<SharedSnapshot, String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let snapshot = shared::snapshot_tx(&mut tx).await?;
        tx.rollback().await.map_err(|e| e.to_string())?;
        Ok(snapshot)
    }

    /// Replaces only the validated shared SQL projection. Execution tables,
    /// session bindings, schedules, and pending operations are left untouched.
    pub async fn materialize_shared_snapshot(
        &self,
        local_node_id: &str,
        snapshot: &SharedSnapshot,
        revision: &str,
    ) -> Result<(), String> {
        shared::materialize(&self.pool, local_node_id, snapshot, revision).await
    }

    /// Reads authority inside the SQL writer transaction. Git commits use the
    /// same SQL-then-Git lock order, so recovery cannot overwrite a newer write.
    pub async fn materialize_shared_from<F>(
        &self,
        local_node_id: &str,
        read: F,
    ) -> Result<String, String>
    where
        F: FnOnce() -> Result<(String, SharedSnapshot), String> + Send,
    {
        shared::materialize_from(&self.pool, local_node_id, read).await
    }

    pub async fn materialized_shared_snapshot(
        &self,
    ) -> Result<Option<(String, SharedSnapshot)>, String> {
        shared::materialized(&self.pool).await
    }

    /// Resolves an imported local SQL ID to its immutable Git origin identity.
    pub async fn shared_origin(
        &self,
        table: SharedTable,
        local_id: &str,
    ) -> Result<Option<(String, String)>, String> {
        shared::origin(&self.pool, table, local_id).await
    }

    pub(crate) async fn before_shared_commit(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<(), String> {
        let hook = self
            .shared_commit_hook
            .read()
            .expect("shared hook lock")
            .clone();
        let Some(hook) = hook else {
            return Ok(());
        };
        let snapshot = shared::snapshot_tx(tx).await?;
        let revision = hook(&snapshot)?;
        shared::persist_projection(tx, &snapshot, &revision).await?;
        Ok(())
    }

    pub async fn events(&self, after: i64, limit: i64) -> Result<Vec<AgentEvent>, String> {
        let rows = sqlx::query_as::<_, (i64, String, String, String, i64)>(
            "SELECT seq,kind,entity_id,payload,created_at FROM events WHERE seq > ? ORDER BY seq LIMIT ?",
        ).bind(after).bind(limit.clamp(1, 1000)).fetch_all(&self.pool).await.map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|(seq, kind, entity_id, payload, created_at)| {
                Ok(AgentEvent {
                    seq,
                    kind,
                    entity_id,
                    payload: serde_json::from_str(&payload).map_err(|e| e.to_string())?,
                    created_at,
                })
            })
            .collect()
    }
}

pub(crate) fn id() -> String {
    Uuid::new_v4().to_string()
}
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub(crate) fn digest<T: Serialize>(value: &T) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
