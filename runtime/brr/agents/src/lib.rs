mod memory;
mod profiles;
mod questions;
mod schedule;
mod tasks;
mod tickets;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

pub use memory::{LearnedSkill, MemoryAccess, MemoryRecord, MemoryScope, MemorySource};
pub use profiles::{AgentProfile, ProfileCandidate, ProfileDraft, SessionBinding};
pub use questions::{
    NewQuestionRequest, Question, QuestionAnswers, QuestionOption, QuestionRequest,
};
pub use schedule::{Workflow, WorkflowInput};
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
}

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
        Ok(Self {
            pool,
            root: Arc::new(root),
        })
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
