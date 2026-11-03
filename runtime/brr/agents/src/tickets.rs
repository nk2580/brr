use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::{AgentStore, Task, TaskInput, digest, id, now, tasks};

const PAGE_SIZE: i64 = 50;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceCriterion {
    pub id: String,
    pub text: String,
    pub checked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewAcceptanceCriterion {
    #[serde(default)]
    pub id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub checked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TicketPriority {
    None,
    Low,
    Medium,
    High,
    Urgent,
}
impl Default for TicketPriority {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    Todo,
    InProgress,
    Review,
    Complete,
}
impl Default for TicketStatus {
    fn default() -> Self {
        Self::Todo
    }
}
impl TicketStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Review => "review",
            Self::Complete => "complete",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TicketActorKind {
    Human,
    Agent,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketActor {
    pub kind: TicketActorKind,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
}
impl TicketActor {
    pub fn human() -> Self {
        Self {
            kind: TicketActorKind::Human,
            agent_id: None,
            thread_id: None,
            task_id: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketMutation {
    pub operation_id: String,
    pub actor: TicketActor,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketInput {
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<NewAcceptanceCriterion>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub priority: TicketPriority,
    #[serde(default)]
    pub status: TicketStatus,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<Vec<NewAcceptanceCriterion>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_agent"
    )]
    pub agent_id: Option<Option<String>>,
    pub priority: Option<TicketPriority>,
    pub status: Option<TicketStatus>,
    pub archived: Option<bool>,
}

fn optional_agent<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub id: String,
    pub number: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub agent_id: Option<String>,
    pub priority: TicketPriority,
    pub status: TicketStatus,
    pub position: i64,
    pub revision: i64,
    pub status_revision: i64,
    pub archived: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketFilter {
    pub status: Option<TicketStatus>,
    pub archived: Option<bool>,
    pub agent_id: Option<String>,
    pub priority: Option<TicketPriority>,
    pub query: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketPage {
    pub data: Vec<Ticket>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketComment {
    pub id: String,
    pub sequence: i64,
    pub ticket_id: String,
    pub body: String,
    pub actor: TicketActor,
    pub created_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketActivity {
    pub id: i64,
    pub ticket_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub actor: TicketActor,
    pub created_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketRunSummary {
    pub task_id: String,
    pub status: String,
    pub result: Option<String>,
    pub evidence: Option<Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketDetail {
    pub ticket: Ticket,
    pub comments: TicketPageComments,
    pub activity: TicketPageActivity,
    pub runs: Vec<TicketRunSummary>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketPageComments {
    pub data: Vec<TicketComment>,
    pub next_cursor: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketPageActivity {
    pub data: Vec<TicketActivity>,
    pub next_cursor: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TicketRunProposal {
    pub id: String,
    pub ticket_id: String,
    pub ticket_revision: i64,
    pub hash: String,
    pub status: String,
    pub snapshot: TaskInput,
    pub created_at: i64,
}

pub(crate) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("CREATE TABLE IF NOT EXISTS ticket_counters (id INTEGER PRIMARY KEY CHECK(id=1), next_number INTEGER NOT NULL)").execute(pool).await.map_err(err)?;
    sqlx::query("INSERT OR IGNORE INTO ticket_counters(id,next_number) VALUES(1,1)")
        .execute(pool)
        .await
        .map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS tickets (id TEXT PRIMARY KEY,number TEXT NOT NULL UNIQUE,project_id TEXT NOT NULL,title TEXT NOT NULL,description TEXT NOT NULL,acceptance_criteria TEXT NOT NULL,agent_id TEXT,priority TEXT NOT NULL,status TEXT NOT NULL,position INTEGER NOT NULL,revision INTEGER NOT NULL,status_revision INTEGER NOT NULL,archived INTEGER NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL)").execute(pool).await.map_err(err)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS tickets_project_order ON tickets(project_id,status,archived,position,id)").execute(pool).await.map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS ticket_comments (sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,ticket_id TEXT NOT NULL REFERENCES tickets(id),body TEXT NOT NULL,actor TEXT NOT NULL,created_at INTEGER NOT NULL)").execute(pool).await.map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS ticket_activity (id INTEGER PRIMARY KEY AUTOINCREMENT,ticket_id TEXT NOT NULL REFERENCES tickets(id),kind TEXT NOT NULL,payload TEXT NOT NULL,actor TEXT NOT NULL,created_at INTEGER NOT NULL)").execute(pool).await.map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS ticket_run_proposals (id TEXT PRIMARY KEY,ticket_id TEXT NOT NULL REFERENCES tickets(id),ticket_revision INTEGER NOT NULL,hash TEXT NOT NULL,status TEXT NOT NULL,snapshot TEXT NOT NULL,task_id TEXT REFERENCES tasks(id),created_at INTEGER NOT NULL)").execute(pool).await.map_err(err)?;
    Ok(())
}

impl AgentStore {
    pub async fn create_ticket(
        &self,
        input: TicketInput,
        mutation: TicketMutation,
    ) -> Result<Ticket, String> {
        validate_mutation(&mutation)?;
        validate_ticket_input(&input)?;
        if let Some(agent) = input.agent_id.as_deref() {
            tasks::validate_agent(&self.pool, agent).await?;
        }
        let input_hash = digest(&(&input, &mutation.actor))?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(existing) = operation_result(
            &mut tx,
            &mutation.operation_id,
            "ticket.create",
            &input_hash,
        )
        .await?
        {
            tx.rollback().await.map_err(err)?;
            return decode_field(existing, "ticket");
        }
        let seq: i64 = sqlx::query_scalar("SELECT next_number FROM ticket_counters WHERE id=1")
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
        sqlx::query("UPDATE ticket_counters SET next_number=? WHERE id=1")
            .bind(seq + 1)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        let position = next_position(&mut tx, &input.project_id, input.status.as_str()).await?;
        let stamp = now();
        let ticket = Ticket {
            id: id(),
            number: format!("BRR-{seq:06}"),
            project_id: input.project_id,
            title: input.title,
            description: input.description,
            acceptance_criteria: criteria(input.acceptance_criteria)?,
            agent_id: input.agent_id,
            priority: input.priority,
            status: input.status,
            position,
            revision: 1,
            status_revision: 1,
            archived: false,
            created_at: stamp,
            updated_at: stamp,
        };
        insert_ticket(&mut tx, &ticket).await?;
        record_activity(
            &mut tx,
            &ticket.id,
            "ticket.created",
            &json!({"ticket":ticket}),
            &mutation.actor,
            stamp,
        )
        .await?;
        save_operation(
            &mut tx,
            &mutation,
            "ticket.create",
            &input_hash,
            &json!({"ticket":ticket}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(ticket)
    }

    pub async fn update_ticket(
        &self,
        ticket_id: &str,
        expected_revision: i64,
        update: TicketUpdate,
        mutation: TicketMutation,
    ) -> Result<Ticket, String> {
        validate_mutation(&mutation)?;
        let hash = digest(&(ticket_id, expected_revision, &update, &mutation.actor))?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(v) =
            operation_result(&mut tx, &mutation.operation_id, "ticket.update", &hash).await?
        {
            tx.rollback().await.map_err(err)?;
            return decode_field(v, "ticket");
        }
        let mut ticket = get_ticket_tx(&mut tx, ticket_id).await?;
        ensure_revision(&ticket, expected_revision)?;
        if let Some(v) = update.title {
            if v.trim().is_empty() {
                return Err("ticket title is required".into());
            }
            ticket.title = v;
        }
        if let Some(v) = update.description {
            ticket.description = v;
        }
        if let Some(v) = update.acceptance_criteria {
            ticket.acceptance_criteria = criteria(v)?;
        }
        if let Some(v) = update.agent_id {
            if let Some(agent) = v.as_deref() {
                tasks::validate_agent_tx(&mut tx, agent).await?;
            }
            ticket.agent_id = v;
        }
        if let Some(v) = update.priority {
            ticket.priority = v;
        }
        let status_written = update.status.is_some();
        if let Some(v) = update.status {
            ticket.status = v;
        }
        if let Some(v) = update.archived {
            ticket.archived = v;
        }
        let stamp = now();
        ticket.revision += 1;
        if status_written {
            ticket.status_revision += 1;
        }
        ticket.updated_at = stamp;
        persist_ticket(&mut tx, &ticket).await?;
        record_activity(
            &mut tx,
            &ticket.id,
            "ticket.updated",
            &json!({"ticket":ticket}),
            &mutation.actor,
            stamp,
        )
        .await?;
        save_operation(
            &mut tx,
            &mutation,
            "ticket.update",
            &hash,
            &json!({"ticket":ticket}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(ticket)
    }

    pub async fn move_ticket(
        &self,
        ticket_id: &str,
        expected_revision: i64,
        status: TicketStatus,
        before_id: Option<&str>,
        mutation: TicketMutation,
    ) -> Result<Ticket, String> {
        validate_mutation(&mutation)?;
        let hash = digest(&(
            ticket_id,
            expected_revision,
            &status,
            before_id,
            &mutation.actor,
        ))?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(v) =
            operation_result(&mut tx, &mutation.operation_id, "ticket.move", &hash).await?
        {
            tx.rollback().await.map_err(err)?;
            return decode_field(v, "ticket");
        }
        let mut ticket = get_ticket_tx(&mut tx, ticket_id).await?;
        ensure_revision(&ticket, expected_revision)?;
        if let Some(before) = before_id {
            if before == ticket_id {
                return Err("ticket cannot be moved before itself".into());
            }
            let dest = get_ticket_tx(&mut tx, before).await?;
            if dest.project_id != ticket.project_id || dest.status != status {
                return Err("move target must be in the same project and status".into());
            }
        }
        let mut ordered=sqlx::query("SELECT id FROM tickets WHERE project_id=? AND status=? AND archived=0 AND id<>? ORDER BY position,id").bind(&ticket.project_id).bind(status.as_str()).bind(ticket_id).fetch_all(&mut *tx).await.map_err(err)?.into_iter().map(|r|r.try_get::<String,_>("id").map_err(err)).collect::<Result<Vec<_>,_>>()?;
        let at = before_id
            .and_then(|before| ordered.iter().position(|id| id == before))
            .unwrap_or(ordered.len());
        ordered.insert(at, ticket_id.to_string());
        let old_status = ticket.status.clone();
        ticket.status = status;
        ticket.status_revision += 1;
        ticket.revision += 1;
        ticket.updated_at = now();
        for (index, id) in ordered.iter().enumerate() {
            if id == ticket_id {
                ticket.position = (index as i64 + 1) * 1000;
            } else {
                sqlx::query("UPDATE tickets SET position=? WHERE id=?")
                    .bind((index as i64 + 1) * 1000)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
            }
        }
        persist_ticket(&mut tx, &ticket).await?;
        record_activity(
            &mut tx,
            &ticket.id,
            "ticket.moved",
            &json!({"from":old_status,"to":ticket.status,"position":ticket.position}),
            &mutation.actor,
            ticket.updated_at,
        )
        .await?;
        save_operation(
            &mut tx,
            &mutation,
            "ticket.move",
            &hash,
            &json!({"ticket":ticket}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(ticket)
    }

    pub async fn list_tickets(
        &self,
        project_id: &str,
        cursor: Option<&str>,
        limit: i64,
        filter: Option<TicketFilter>,
    ) -> Result<TicketPage, String> {
        let filter = filter.unwrap_or_default();
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT * FROM tickets WHERE project_id=");
        qb.push_bind(project_id);
        if let Some(status) = filter.status {
            qb.push(" AND status=").push_bind(status.as_str());
        }
        if let Some(archived) = filter.archived {
            qb.push(" AND archived=").push_bind(archived as i64);
        }
        if let Some(agent) = filter.agent_id {
            qb.push(" AND agent_id=").push_bind(agent);
        }
        if let Some(priority) = filter.priority {
            qb.push(" AND priority=").push_bind(priority_str(&priority));
        }
        if let Some(query) = filter.query.filter(|query| !query.trim().is_empty()) {
            let literal = query
                .trim()
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_");
            let pattern = format!("%{literal}%");
            qb.push(" AND (LOWER(title) LIKE LOWER(")
                .push_bind(pattern.clone())
                .push(" ) ESCAPE '!' OR LOWER(description) LIKE LOWER(")
                .push_bind(pattern)
                .push(" ) ESCAPE '!')");
        }
        if let Some(cursor) = cursor {
            let cursor_project: Option<String> =
                sqlx::query_scalar("SELECT project_id FROM tickets WHERE id=?")
                    .bind(cursor)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(err)?;
            if cursor_project.as_deref() != Some(project_id) {
                return Err("ticket cursor does not belong to this project".into());
            }
            qb.push(" AND (position,id)>(SELECT position,id FROM tickets WHERE id=")
                .push_bind(cursor)
                .push(")");
        }
        let limit = limit.clamp(1, 200);
        qb.push(" ORDER BY position,id LIMIT ").push_bind(limit + 1);
        let rows = qb.build().fetch_all(&self.pool).await.map_err(err)?;
        let mut data = rows
            .into_iter()
            .map(row_ticket)
            .collect::<Result<Vec<_>, _>>()?;
        let more = data.len() as i64 > limit;
        if more {
            data.pop();
        }
        let next_cursor = if more {
            data.last().map(|t| t.id.clone())
        } else {
            None
        };
        Ok(TicketPage { data, next_cursor })
    }

    pub async fn read_ticket(
        &self,
        ticket_id: &str,
        comments_cursor: Option<i64>,
        activity_cursor: Option<i64>,
    ) -> Result<TicketDetail, String> {
        let ticket = self.get_ticket(ticket_id).await?;
        let cc = comments_cursor.unwrap_or(0).max(0);
        let ac = activity_cursor.unwrap_or(0).max(0);
        let cr=sqlx::query("SELECT * FROM ticket_comments WHERE ticket_id=? AND sequence>? ORDER BY sequence LIMIT ?").bind(ticket_id).bind(cc).bind(PAGE_SIZE+1).fetch_all(&self.pool).await.map_err(err)?;
        let mut comments = cr
            .into_iter()
            .map(row_comment)
            .collect::<Result<Vec<_>, _>>()?;
        let cm = comments.len() > PAGE_SIZE as usize;
        if cm {
            comments.pop();
        }
        let next_comments = cm
            .then(|| comments.last().map(|comment| comment.sequence))
            .flatten();
        let ar = sqlx::query(
            "SELECT * FROM ticket_activity WHERE ticket_id=? AND id>? ORDER BY id LIMIT ?",
        )
        .bind(ticket_id)
        .bind(ac)
        .bind(PAGE_SIZE + 1)
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;
        let mut activity = ar
            .into_iter()
            .map(row_activity)
            .collect::<Result<Vec<_>, _>>()?;
        let am = activity.len() > PAGE_SIZE as usize;
        if am {
            activity.pop();
        }
        let next_activity = am.then(|| activity.last().map(|item| item.id)).flatten();
        let rows = sqlx::query(
            "SELECT id,status,result,evidence FROM tasks WHERE ticket_id=? ORDER BY created_at,id",
        )
        .bind(ticket_id)
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;
        let runs = rows
            .into_iter()
            .map(|r| {
                Ok(TicketRunSummary {
                    task_id: r.try_get("id").map_err(err)?,
                    status: r.try_get("status").map_err(err)?,
                    result: r.try_get("result").map_err(err)?,
                    evidence: r
                        .try_get::<Option<String>, _>("evidence")
                        .map_err(err)?
                        .map(|v| serde_json::from_str(&v))
                        .transpose()
                        .map_err(err)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(TicketDetail {
            ticket,
            comments: TicketPageComments {
                data: comments,
                next_cursor: next_comments,
            },
            activity: TicketPageActivity {
                data: activity,
                next_cursor: next_activity,
            },
            runs,
        })
    }

    pub async fn get_ticket(&self, ticket_id: &str) -> Result<Ticket, String> {
        let row = sqlx::query("SELECT * FROM tickets WHERE id=?")
            .bind(ticket_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(err)?
            .ok_or_else(|| "ticket not found".to_string())?;
        row_ticket(row)
    }

    pub async fn comment_ticket(
        &self,
        ticket_id: &str,
        body: &str,
        mutation: TicketMutation,
    ) -> Result<TicketComment, String> {
        validate_mutation(&mutation)?;
        if body.trim().is_empty() {
            return Err("comment body is required".into());
        }
        let hash = digest(&(ticket_id, body, &mutation.actor))?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(v) =
            operation_result(&mut tx, &mutation.operation_id, "ticket.comment", &hash).await?
        {
            tx.rollback().await.map_err(err)?;
            return decode_field(v, "comment");
        }
        get_ticket_tx(&mut tx, ticket_id).await?;
        let mut comment = TicketComment {
            id: id(),
            sequence: 0,
            ticket_id: ticket_id.into(),
            body: body.into(),
            actor: mutation.actor.clone(),
            created_at: now(),
        };
        sqlx::query(
            "INSERT INTO ticket_comments(id,ticket_id,body,actor,created_at) VALUES(?,?,?,?,?)",
        )
        .bind(&comment.id)
        .bind(ticket_id)
        .bind(body)
        .bind(encode(&comment.actor)?)
        .bind(comment.created_at)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        comment.sequence = sqlx::query("SELECT sequence FROM ticket_comments WHERE id=?")
            .bind(&comment.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?
            .try_get("sequence")
            .map_err(err)?;
        record_activity(
            &mut tx,
            ticket_id,
            "ticket.comment",
            &json!({"commentId":comment.id}),
            &mutation.actor,
            comment.created_at,
        )
        .await?;
        save_operation(
            &mut tx,
            &mutation,
            "ticket.comment",
            &hash,
            &json!({"comment":comment}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(comment)
    }

    pub async fn propose_ticket_run(
        &self,
        ticket_id: &str,
        expected_revision: i64,
        snapshot: TaskInput,
        mutation: TicketMutation,
    ) -> Result<TicketRunProposal, String> {
        validate_mutation(&mutation)?;
        tasks::validate_input(&snapshot)?;
        let hash = digest(&(ticket_id, expected_revision, &snapshot, &mutation.actor))?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(v) =
            operation_result(&mut tx, &mutation.operation_id, "ticket.proposeRun", &hash).await?
        {
            tx.rollback().await.map_err(err)?;
            return decode_field(v, "proposal");
        }
        let ticket = get_ticket_tx(&mut tx, ticket_id).await?;
        ensure_revision(&ticket, expected_revision)?;
        validate_snapshot_for_ticket(&ticket, &snapshot)?;
        tasks::validate_agent_tx(&mut tx, &snapshot.agent_id).await?;
        let proposal_id = id();
        let snapshot_hash = digest(&(ticket.revision, &snapshot, &proposal_id))?;
        let proposal = TicketRunProposal {
            id: proposal_id,
            ticket_id: ticket_id.into(),
            ticket_revision: ticket.revision,
            hash: snapshot_hash,
            status: "pending".into(),
            snapshot,
            created_at: now(),
        };
        sqlx::query("INSERT INTO ticket_run_proposals(id,ticket_id,ticket_revision,hash,status,snapshot,created_at) VALUES(?,?,?,?,'pending',?,?)").bind(&proposal.id).bind(ticket_id).bind(proposal.ticket_revision).bind(&proposal.hash).bind(encode(&proposal.snapshot)?).bind(proposal.created_at).execute(&mut *tx).await.map_err(err)?;
        record_activity(
            &mut tx,
            ticket_id,
            "ticket.run_proposed",
            &json!({"proposalId":proposal.id,"hash":proposal.hash}),
            &mutation.actor,
            proposal.created_at,
        )
        .await?;
        save_operation(
            &mut tx,
            &mutation,
            "ticket.proposeRun",
            &hash,
            &json!({"proposal":proposal}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(proposal)
    }

    pub async fn list_ticket_run_proposals(
        &self,
        ticket_id: &str,
    ) -> Result<Vec<TicketRunProposal>, String> {
        let rows = sqlx::query(
            "SELECT * FROM ticket_run_proposals WHERE ticket_id=? ORDER BY created_at,id",
        )
        .bind(ticket_id)
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;
        rows.into_iter().map(row_proposal).collect()
    }

    pub async fn approve_ticket_run(
        &self,
        ticket_id: &str,
        proposal_hash: &str,
        mutation: TicketMutation,
    ) -> Result<Task, String> {
        validate_mutation(&mutation)?;
        let hash = digest(&(ticket_id, proposal_hash, &mutation.actor))?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(v) =
            operation_result(&mut tx, &mutation.operation_id, "ticket.approveRun", &hash).await?
        {
            tx.rollback().await.map_err(err)?;
            return decode_field(v, "task");
        }
        let row = sqlx::query("SELECT * FROM ticket_run_proposals WHERE ticket_id=? AND hash=?")
            .bind(ticket_id)
            .bind(proposal_hash)
            .fetch_optional(&mut *tx)
            .await
            .map_err(err)?
            .ok_or_else(|| "ticket run proposal not found".to_string())?;
        let linked_task_id: Option<String> = row.try_get("task_id").map_err(err)?;
        let mut proposal = row_proposal(row)?;
        if proposal.status == "approved" {
            let task_id = linked_task_id
                .ok_or_else(|| "approved proposal is missing its task link".to_string())?;
            let task = tasks::get_task_tx(&mut tx, &task_id).await?;
            save_operation(
                &mut tx,
                &mutation,
                "ticket.approveRun",
                &hash,
                &json!({"task":task,"proposal":proposal}),
            )
            .await?;
            tx.commit().await.map_err(err)?;
            return Ok(task);
        }
        if proposal.status != "pending" {
            return Err("ticket run proposal is no longer pending".into());
        }
        let ticket = get_ticket_tx(&mut tx, ticket_id).await?;
        if ticket.revision != proposal.ticket_revision {
            return Err(conflict(&ticket));
        }
        tasks::validate_input(&proposal.snapshot)?;
        tasks::validate_agent_tx(&mut tx, &proposal.snapshot.agent_id).await?;
        validate_snapshot_for_ticket(&ticket, &proposal.snapshot)?;
        let (current_model, current_effort): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT json_extract(candidate,'$.model'),json_extract(candidate,'$.effort') FROM agent_profiles WHERE id=?",
        )
        .bind(&proposal.snapshot.agent_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(err)?;
        if proposal.snapshot.model.as_deref() != current_model.as_deref()
            || proposal.snapshot.effort.as_deref() != current_effort.as_deref()
        {
            return Err("ticket run agent model or effort changed; propose the run again".into());
        }
        let existing:i64=sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE ticket_id=? AND status NOT IN ('succeeded','failed','cancelled')").bind(ticket_id).fetch_one(&mut *tx).await.map_err(err)?;
        if existing > 0 {
            return Err("ticket already has an unfinished task".into());
        }
        let stamp = now();
        let task = tasks::task_from_input(
            id(),
            proposal.snapshot.clone(),
            "queued",
            None,
            None,
            None,
            None,
            stamp,
        );
        tasks::insert_task(&mut tx, &task, None, None).await?;
        sqlx::query("UPDATE tasks SET ticket_id=?,ticket_status_revision=?,ticket_status_started=0 WHERE id=?")
            .bind(ticket_id)
            .bind(ticket.status_revision)
            .bind(&task.id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        sqlx::query(
            "UPDATE ticket_run_proposals SET status='approved',task_id=? WHERE id=? AND status='pending'",
        )
        .bind(&task.id)
        .bind(&proposal.id)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        proposal.status = "approved".into();
        crate::tasks::event(&mut tx, "task.created", &task.id, &task).await?;
        record_activity(
            &mut tx,
            ticket_id,
            "ticket.run_approved",
            &json!({"proposalId":proposal.id,"taskId":task.id}),
            &mutation.actor,
            stamp,
        )
        .await?;
        save_operation(
            &mut tx,
            &mutation,
            "ticket.approveRun",
            &hash,
            &json!({"task":task,"proposal":proposal}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    pub async fn reject_ticket_run(
        &self,
        ticket_id: &str,
        proposal_hash: &str,
        mutation: TicketMutation,
    ) -> Result<TicketRunProposal, String> {
        validate_mutation(&mutation)?;
        let hash = digest(&(ticket_id, proposal_hash, &mutation.actor))?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(v) =
            operation_result(&mut tx, &mutation.operation_id, "ticket.rejectRun", &hash).await?
        {
            tx.rollback().await.map_err(err)?;
            return decode_field(v, "proposal");
        }
        let row = sqlx::query("SELECT * FROM ticket_run_proposals WHERE ticket_id=? AND hash=?")
            .bind(ticket_id)
            .bind(proposal_hash)
            .fetch_optional(&mut *tx)
            .await
            .map_err(err)?
            .ok_or_else(|| "ticket run proposal not found".to_string())?;
        let mut proposal = row_proposal(row)?;
        if proposal.status != "pending" {
            return Err("ticket run proposal is no longer pending".into());
        }
        proposal.status = "rejected".into();
        sqlx::query(
            "UPDATE ticket_run_proposals SET status='rejected' WHERE id=? AND status='pending'",
        )
        .bind(&proposal.id)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        record_activity(
            &mut tx,
            ticket_id,
            "ticket.run_rejected",
            &json!({"proposalId":proposal.id}),
            &mutation.actor,
            now(),
        )
        .await?;
        save_operation(
            &mut tx,
            &mutation,
            "ticket.rejectRun",
            &hash,
            &json!({"proposal":proposal}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(proposal)
    }
}

pub(crate) async fn auto_lifecycle_status(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    task_id: &str,
    status: TicketStatus,
    stamp: i64,
) -> Result<(), String> {
    let relation = sqlx::query(
        "SELECT ticket_id,ticket_status_revision,ticket_status_started FROM tasks WHERE id=?",
    )
    .bind(task_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(err)?;
    let Some(relation) = relation else {
        return Ok(());
    };
    let Some(ticket_id) = relation
        .try_get::<Option<String>, _>("ticket_id")
        .map_err(err)?
    else {
        return Ok(());
    };
    let expected: Option<i64> = relation.try_get("ticket_status_revision").map_err(err)?;
    let Some(expected) = expected else {
        return Ok(());
    };
    let is_start = status == TicketStatus::InProgress;
    if is_start
        && relation
            .try_get::<i64, _>("ticket_status_started")
            .map_err(err)?
            != 0
    {
        return Ok(());
    }
    let changed = sqlx::query("UPDATE tickets SET status=?,revision=revision+1,status_revision=status_revision+1,updated_at=? WHERE id=? AND status_revision=?")
        .bind(status.as_str()).bind(stamp).bind(&ticket_id).bind(expected)
        .execute(&mut **tx).await.map_err(err)?;
    if changed.rows_affected() == 1 {
        sqlx::query("UPDATE tasks SET ticket_status_revision=ticket_status_revision+1, ticket_status_started=CASE WHEN ? THEN 1 ELSE ticket_status_started END WHERE id=? AND ticket_id=?")
            .bind(is_start as i64)
            .bind(task_id).bind(&ticket_id).execute(&mut **tx).await.map_err(err)?;
        let actor = TicketActor {
            kind: TicketActorKind::System,
            agent_id: None,
            thread_id: None,
            task_id: Some(task_id.into()),
        };
        record_activity(
            tx,
            &ticket_id,
            "ticket.status_auto_changed",
            &json!({"status":status}),
            &actor,
            stamp,
        )
        .await?;
    } else if is_start {
        // Record the first authoritative attach even when a user's intervening status write
        // won the revision race; later continuation events must not retry this transition.
        sqlx::query("UPDATE tasks SET ticket_status_started=1 WHERE id=? AND ticket_id=?")
            .bind(task_id)
            .bind(&ticket_id)
            .execute(&mut **tx)
            .await
            .map_err(err)?;
    }
    Ok(())
}

fn validate_mutation(m: &TicketMutation) -> Result<(), String> {
    if m.operation_id.trim().is_empty() {
        Err("operation id is required".into())
    } else {
        Ok(())
    }
}
fn validate_ticket_input(i: &TicketInput) -> Result<(), String> {
    if i.project_id.trim().is_empty() || i.title.trim().is_empty() {
        return Err("project and ticket title are required".into());
    }
    for c in &i.acceptance_criteria {
        if c.text.trim().is_empty() {
            return Err("acceptance criteria text is required".into());
        }
    }
    Ok(())
}
fn validate_snapshot_for_ticket(ticket: &Ticket, snapshot: &TaskInput) -> Result<(), String> {
    if snapshot.project_id.as_deref() != Some(ticket.project_id.as_str()) {
        return Err("task project must match ticket project".into());
    }
    if snapshot.title != ticket.title || snapshot.instructions != ticket.description {
        return Err("task title and instructions must match ticket text".into());
    }
    let expected: Vec<&str> = ticket
        .acceptance_criteria
        .iter()
        .map(|item| item.text.as_str())
        .collect();
    let actual: Vec<&str> = snapshot
        .acceptance_criteria
        .iter()
        .map(String::as_str)
        .collect();
    if actual != expected {
        return Err("task acceptance criteria must match ticket criteria".into());
    }
    if ticket
        .agent_id
        .as_deref()
        .is_some_and(|agent| agent != snapshot.agent_id)
    {
        return Err("task agent must match ticket assignment".into());
    }
    Ok(())
}
fn criteria(input: Vec<NewAcceptanceCriterion>) -> Result<Vec<AcceptanceCriterion>, String> {
    let mut ids = std::collections::HashSet::new();
    input
        .into_iter()
        .map(|c| {
            if c.text.trim().is_empty() {
                return Err("acceptance criteria text is required".into());
            }
            let id = c.id.filter(|s| !s.trim().is_empty()).unwrap_or_else(id);
            if !ids.insert(id.clone()) {
                return Err("acceptance criteria ids must be unique".into());
            }
            Ok(AcceptanceCriterion {
                id,
                text: c.text,
                checked: c.checked,
            })
        })
        .collect()
}
fn priority_str(p: &TicketPriority) -> &'static str {
    match p {
        TicketPriority::None => "none",
        TicketPriority::Low => "low",
        TicketPriority::Medium => "medium",
        TicketPriority::High => "high",
        TicketPriority::Urgent => "urgent",
    }
}
fn parse_priority(p: &str) -> Result<TicketPriority, String> {
    match p {
        "none" => Ok(TicketPriority::None),
        "low" => Ok(TicketPriority::Low),
        "medium" => Ok(TicketPriority::Medium),
        "high" => Ok(TicketPriority::High),
        "urgent" => Ok(TicketPriority::Urgent),
        _ => Err("invalid ticket priority".into()),
    }
}
fn parse_status(s: &str) -> Result<TicketStatus, String> {
    match s {
        "todo" => Ok(TicketStatus::Todo),
        "in_progress" => Ok(TicketStatus::InProgress),
        "review" => Ok(TicketStatus::Review),
        "complete" => Ok(TicketStatus::Complete),
        _ => Err("invalid ticket status".into()),
    }
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn encode<T: Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(err)
}
fn decode_field<T: for<'de> Deserialize<'de>>(
    v: serde_json::Value,
    key: &str,
) -> Result<T, String> {
    serde_json::from_value(
        v.get("result")
            .and_then(|r| r.get(key))
            .cloned()
            .ok_or_else(|| "stored operation result is invalid".to_string())?,
    )
    .map_err(err)
}
fn conflict(t: &Ticket) -> String {
    json!({"code":"conflict","current":t}).to_string()
}
fn ensure_revision(t: &Ticket, expected: i64) -> Result<(), String> {
    if t.revision == expected {
        Ok(())
    } else {
        Err(conflict(t))
    }
}
async fn get_ticket_tx(tx: &mut sqlx::Transaction<'_, Sqlite>, id: &str) -> Result<Ticket, String> {
    let r = sqlx::query("SELECT * FROM tickets WHERE id=?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(err)?
        .ok_or_else(|| "ticket not found".to_string())?;
    row_ticket(r)
}
async fn insert_ticket(tx: &mut sqlx::Transaction<'_, Sqlite>, t: &Ticket) -> Result<(), String> {
    sqlx::query("INSERT INTO tickets(id,number,project_id,title,description,acceptance_criteria,agent_id,priority,status,position,revision,status_revision,archived,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)").bind(&t.id).bind(&t.number).bind(&t.project_id).bind(&t.title).bind(&t.description).bind(encode(&t.acceptance_criteria)?).bind(&t.agent_id).bind(priority_str(&t.priority)).bind(t.status.as_str()).bind(t.position).bind(t.revision).bind(t.status_revision).bind(t.archived as i64).bind(t.created_at).bind(t.updated_at).execute(&mut **tx).await.map_err(err)?;
    Ok(())
}
async fn persist_ticket(tx: &mut sqlx::Transaction<'_, Sqlite>, t: &Ticket) -> Result<(), String> {
    let n=sqlx::query("UPDATE tickets SET title=?,description=?,acceptance_criteria=?,agent_id=?,priority=?,status=?,position=?,revision=?,status_revision=?,archived=?,updated_at=? WHERE id=? AND revision=?").bind(&t.title).bind(&t.description).bind(encode(&t.acceptance_criteria)?).bind(&t.agent_id).bind(priority_str(&t.priority)).bind(t.status.as_str()).bind(t.position).bind(t.revision).bind(t.status_revision).bind(t.archived as i64).bind(t.updated_at).bind(&t.id).bind(t.revision-1).execute(&mut **tx).await.map_err(err)?;
    if n.rows_affected() != 1 {
        return Err(conflict(t));
    }
    Ok(())
}
fn row_ticket(r: sqlx::sqlite::SqliteRow) -> Result<Ticket, String> {
    Ok(Ticket {
        id: r.try_get("id").map_err(err)?,
        number: r.try_get("number").map_err(err)?,
        project_id: r.try_get("project_id").map_err(err)?,
        title: r.try_get("title").map_err(err)?,
        description: r.try_get("description").map_err(err)?,
        acceptance_criteria: serde_json::from_str(
            &r.try_get::<String, _>("acceptance_criteria").map_err(err)?,
        )
        .map_err(err)?,
        agent_id: r.try_get("agent_id").map_err(err)?,
        priority: parse_priority(&r.try_get::<String, _>("priority").map_err(err)?)?,
        status: parse_status(&r.try_get::<String, _>("status").map_err(err)?)?,
        position: r.try_get("position").map_err(err)?,
        revision: r.try_get("revision").map_err(err)?,
        status_revision: r.try_get("status_revision").map_err(err)?,
        archived: r.try_get::<i64, _>("archived").map_err(err)? != 0,
        created_at: r.try_get("created_at").map_err(err)?,
        updated_at: r.try_get("updated_at").map_err(err)?,
    })
}
async fn next_position(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    project: &str,
    status: &str,
) -> Result<i64, String> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position),0)+1000 FROM tickets WHERE project_id=? AND status=?",
    )
    .bind(project)
    .bind(status)
    .fetch_one(&mut **tx)
    .await
    .map_err(err)?;
    Ok(n)
}
async fn operation_result(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    op: &str,
    kind: &str,
    hash: &str,
) -> Result<Option<serde_json::Value>, String> {
    if let Some((saved_kind, result)) = sqlx::query_as::<_, (String, String)>(
        "SELECT kind,result FROM operation_results WHERE operation_id=?",
    )
    .bind(op)
    .fetch_optional(&mut **tx)
    .await
    .map_err(err)?
    {
        if saved_kind != kind {
            return Err("operation id was already used for another operation".into());
        }
        let val: serde_json::Value = serde_json::from_str(&result).map_err(err)?;
        if val.get("inputHash").and_then(|x| x.as_str()) != Some(hash) {
            return Err("operation id was already used with different input or actor".into());
        }
        Ok(Some(val))
    } else {
        Ok(None)
    }
}
async fn save_operation(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    m: &TicketMutation,
    kind: &str,
    hash: &str,
    result: &serde_json::Value,
) -> Result<(), String> {
    sqlx::query("INSERT INTO operation_results(operation_id,kind,result) VALUES(?,?,?)")
        .bind(&m.operation_id)
        .bind(kind)
        .bind(encode(
            &json!({"inputHash":hash,"actor":&m.actor,"result":result}),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(err)?;
    Ok(())
}
async fn record_activity(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    ticket: &str,
    kind: &str,
    payload: &serde_json::Value,
    actor: &TicketActor,
    stamp: i64,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO ticket_activity(ticket_id,kind,payload,actor,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(ticket)
    .bind(kind)
    .bind(encode(payload)?)
    .bind(encode(actor)?)
    .bind(stamp)
    .execute(&mut **tx)
    .await
    .map_err(err)?;
    Ok(())
}
fn row_comment(r: sqlx::sqlite::SqliteRow) -> Result<TicketComment, String> {
    Ok(TicketComment {
        id: r.try_get("id").map_err(err)?,
        sequence: r.try_get("sequence").map_err(err)?,
        ticket_id: r.try_get("ticket_id").map_err(err)?,
        body: r.try_get("body").map_err(err)?,
        actor: serde_json::from_str(&r.try_get::<String, _>("actor").map_err(err)?).map_err(err)?,
        created_at: r.try_get("created_at").map_err(err)?,
    })
}
fn row_activity(r: sqlx::sqlite::SqliteRow) -> Result<TicketActivity, String> {
    Ok(TicketActivity {
        id: r.try_get("id").map_err(err)?,
        ticket_id: r.try_get("ticket_id").map_err(err)?,
        kind: r.try_get("kind").map_err(err)?,
        payload: serde_json::from_str(&r.try_get::<String, _>("payload").map_err(err)?)
            .map_err(err)?,
        actor: serde_json::from_str(&r.try_get::<String, _>("actor").map_err(err)?).map_err(err)?,
        created_at: r.try_get("created_at").map_err(err)?,
    })
}
fn row_proposal(r: sqlx::sqlite::SqliteRow) -> Result<TicketRunProposal, String> {
    Ok(TicketRunProposal {
        id: r.try_get("id").map_err(err)?,
        ticket_id: r.try_get("ticket_id").map_err(err)?,
        ticket_revision: r.try_get("ticket_revision").map_err(err)?,
        hash: r.try_get("hash").map_err(err)?,
        status: r.try_get("status").map_err(err)?,
        snapshot: serde_json::from_str(&r.try_get::<String, _>("snapshot").map_err(err)?)
            .map_err(err)?,
        created_at: r.try_get("created_at").map_err(err)?,
    })
}
