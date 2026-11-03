use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::collections::{BTreeMap, HashSet};

use crate::{AgentStore, id, now};

pub type QuestionAnswers = BTreeMap<String, Vec<String>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Option<Vec<QuestionOption>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewQuestionRequest {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    pub kind: String,
    pub questions: Vec<Question>,
    #[serde(default)]
    pub runtime_generation: Option<String>,
    #[serde(default)]
    pub runtime_request_id: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuestionRequest {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub kind: String,
    pub questions: Vec<Question>,
    pub runtime_generation: Option<String>,
    pub runtime_request_id: Option<serde_json::Value>,
    pub draft: QuestionAnswers,
    pub answers: Option<QuestionAnswers>,
    pub resolution: String,
    pub delivery: String,
    pub reason: Option<String>,
    pub client_message_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub(crate) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS question_requests (
            id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, turn_id TEXT NOT NULL,
            item_id TEXT NOT NULL, agent_id TEXT, task_id TEXT, run_id TEXT,
            kind TEXT NOT NULL, questions TEXT NOT NULL, runtime_generation TEXT,
            runtime_request_id TEXT, draft TEXT NOT NULL, answers TEXT,
            resolution TEXT NOT NULL, delivery TEXT NOT NULL, reason TEXT,
            client_message_id TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            UNIQUE(thread_id,turn_id,item_id)
        )",
    )
    .execute(pool)
    .await
    .map_err(err)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS questions_thread_state ON question_requests(thread_id,resolution,created_at)")
        .execute(pool).await.map_err(err)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS questions_run_state ON question_requests(run_id,resolution,delivery)")
        .execute(pool).await.map_err(err)?;
    Ok(())
}

impl AgentStore {
    pub async fn record_question(
        &self,
        request: NewQuestionRequest,
    ) -> Result<QuestionRequest, String> {
        validate_new(&request)?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(row) = sqlx::query(
            "SELECT * FROM question_requests WHERE thread_id=? AND turn_id=? AND item_id=?",
        )
        .bind(&request.thread_id)
        .bind(&request.turn_id)
        .bind(&request.item_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?
        {
            let mut existing = row_request(row)?;
            if existing.agent_id != request.agent_id
                || existing.task_id != request.task_id
                || existing.run_id != request.run_id
                || existing.kind != request.kind
                || existing.questions != request.questions
            {
                return Err(
                    "question source was already recorded with different immutable content".into(),
                );
            }
            existing.runtime_generation = request.runtime_generation;
            existing.runtime_request_id = request.runtime_request_id;
            existing.updated_at = now();
            sqlx::query("UPDATE question_requests SET runtime_generation=?,runtime_request_id=?,updated_at=? WHERE id=?")
                .bind(&existing.runtime_generation)
                .bind(existing.runtime_request_id.as_ref().map(|v| v.to_string()))
                .bind(existing.updated_at).bind(&existing.id).execute(&mut *tx).await.map_err(err)?;
            event(&mut tx, "question.refreshed", &existing.id, &existing).await?;
            tx.commit().await.map_err(err)?;
            return Ok(existing);
        }
        let stamp = now();
        let source = format!(
            "{}\0{}\0{}",
            request.thread_id, request.turn_id, request.item_id
        );
        let id = format!("q_{}", crate::digest(&source)?);
        let question = QuestionRequest {
            id,
            thread_id: request.thread_id,
            turn_id: request.turn_id,
            item_id: request.item_id,
            agent_id: request.agent_id,
            task_id: request.task_id,
            run_id: request.run_id,
            kind: request.kind,
            questions: request.questions,
            runtime_generation: request.runtime_generation,
            runtime_request_id: request.runtime_request_id,
            draft: BTreeMap::new(),
            answers: None,
            resolution: "pending".into(),
            delivery: "none".into(),
            reason: None,
            client_message_id: None,
            created_at: stamp,
            updated_at: stamp,
        };
        sqlx::query("INSERT INTO question_requests(id,thread_id,turn_id,item_id,agent_id,task_id,run_id,kind,questions,runtime_generation,runtime_request_id,draft,answers,resolution,delivery,reason,client_message_id,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,'pending','none',NULL,NULL,?,?)")
            .bind(&question.id).bind(&question.thread_id).bind(&question.turn_id).bind(&question.item_id)
            .bind(&question.agent_id).bind(&question.task_id).bind(&question.run_id).bind(&question.kind)
            .bind(encode(&question.questions)?).bind(&question.runtime_generation)
            .bind(question.runtime_request_id.as_ref().map(|v| v.to_string()))
            .bind(encode(&question.draft)?).bind(Option::<String>::None).bind(stamp).bind(stamp)
            .execute(&mut *tx).await.map_err(err)?;
        event(&mut tx, "question.recorded", &question.id, &question).await?;
        tx.commit().await.map_err(err)?;
        Ok(question)
    }

    pub async fn get_question(&self, id: &str) -> Result<QuestionRequest, String> {
        let row = sqlx::query("SELECT * FROM question_requests WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(err)?
            .ok_or_else(|| "question request not found".to_string())?;
        row_request(row)
    }

    pub async fn list_questions(
        &self,
        thread_id: Option<&str>,
        pending_only: bool,
    ) -> Result<Vec<QuestionRequest>, String> {
        let rows = match (thread_id, pending_only) {
            (Some(thread), true) => sqlx::query("SELECT * FROM question_requests WHERE thread_id=? AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain')) ORDER BY created_at,id").bind(thread).fetch_all(&self.pool).await.map_err(err)?,
            (Some(thread), false) => sqlx::query("SELECT * FROM question_requests WHERE thread_id=? ORDER BY created_at,id").bind(thread).fetch_all(&self.pool).await.map_err(err)?,
            (None, true) => sqlx::query("SELECT * FROM question_requests WHERE resolution='pending' OR delivery IN ('queued','dispatching','uncertain') ORDER BY created_at,id").fetch_all(&self.pool).await.map_err(err)?,
            (None, false) => sqlx::query("SELECT * FROM question_requests ORDER BY created_at,id").fetch_all(&self.pool).await.map_err(err)?,
        };
        rows.into_iter().map(row_request).collect()
    }

    pub async fn save_question_draft(
        &self,
        id: &str,
        draft: &QuestionAnswers,
    ) -> Result<QuestionRequest, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let mut question = get_tx(&mut tx, id).await?;
        if question.resolution != "pending" {
            return Err("question request is no longer pending".into());
        }
        validate_answer_ids(&question.questions, draft)?;
        question.draft = draft.clone();
        question.updated_at = now();
        sqlx::query(
            "UPDATE question_requests SET draft=?,updated_at=? WHERE id=? AND resolution='pending'",
        )
        .bind(encode(draft)?)
        .bind(question.updated_at)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        event(&mut tx, "question.draft_saved", id, &question).await?;
        tx.commit().await.map_err(err)?;
        Ok(question)
    }

    pub async fn resolve_question(
        &self,
        id: &str,
        answers: Option<QuestionAnswers>,
    ) -> Result<QuestionRequest, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let mut question = get_tx(&mut tx, id).await?;
        let resolution = if let Some(answer_values) = answers.as_ref() {
            validate_complete_answers(&question.questions, answer_values)?;
            "answered"
        } else {
            "dismissed"
        };
        if question.resolution != "pending" {
            let same = question.resolution == resolution && question.answers == answers;
            if same {
                tx.commit().await.map_err(err)?;
                return Ok(question);
            }
            return Err("question request was already resolved differently".into());
        }
        question.answers = answers;
        question.resolution = resolution.into();
        question.delivery = "queued".into();
        question.client_message_id = Some(id_fn());
        question.reason = None;
        question.updated_at = now();
        sqlx::query("UPDATE question_requests SET answers=?,resolution=?,delivery='queued',reason=NULL,client_message_id=?,updated_at=? WHERE id=? AND resolution='pending'")
            .bind(question.answers.as_ref().map(encode).transpose()?)
            .bind(&question.resolution).bind(&question.client_message_id)
            .bind(question.updated_at).bind(id).execute(&mut *tx).await.map_err(err)?;
        event(&mut tx, "question.resolved", id, &question).await?;
        tx.commit().await.map_err(err)?;
        Ok(question)
    }

    pub async fn claim_question_delivery(
        &self,
        id: &str,
    ) -> Result<Option<QuestionRequest>, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let changed = sqlx::query("UPDATE question_requests SET delivery='dispatching',updated_at=? WHERE id=? AND delivery='queued'")
            .bind(now()).bind(id).execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() == 0 {
            tx.rollback().await.map_err(err)?;
            return Ok(None);
        }
        let question = get_tx(&mut tx, id).await?;
        event(&mut tx, "question.delivery_claimed", id, &question).await?;
        tx.commit().await.map_err(err)?;
        Ok(Some(question))
    }

    pub async fn finish_question_delivery(
        &self,
        id: &str,
        state: &str,
        reason: Option<&str>,
    ) -> Result<QuestionRequest, String> {
        if !["delivered", "uncertain", "queued"].contains(&state) {
            return Err("question delivery state must be delivered, uncertain, or queued".into());
        }
        if state == "uncertain" && reason.map(str::trim).unwrap_or("").is_empty() {
            return Err("uncertain question delivery needs a reason".into());
        }
        let mut tx = self.pool.begin().await.map_err(err)?;
        let mut question = get_tx(&mut tx, id).await?;
        if question.delivery == state {
            tx.commit().await.map_err(err)?;
            return Ok(question);
        }
        let expected_delivery = if question.delivery == "uncertain" && state == "delivered" {
            "uncertain"
        } else {
            "dispatching"
        };
        if question.delivery != expected_delivery {
            return Err("question delivery is not claimed by a dispatcher".into());
        }
        question.delivery = state.into();
        question.reason = reason.map(str::to_owned);
        question.updated_at = now();
        sqlx::query("UPDATE question_requests SET delivery=?,reason=?,updated_at=? WHERE id=? AND delivery=?")
            .bind(state).bind(&question.reason).bind(question.updated_at).bind(id).bind(expected_delivery).execute(&mut *tx).await.map_err(err)?;
        event(&mut tx, "question.delivery_finished", id, &question).await?;
        tx.commit().await.map_err(err)?;
        Ok(question)
    }

    pub async fn close_questions(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        reason: &str,
    ) -> Result<Vec<QuestionRequest>, String> {
        if reason.trim().is_empty() {
            return Err("closing questions needs a reason".into());
        }
        let mut tx = self.pool.begin().await.map_err(err)?;
        let rows = if let Some(turn) = turn_id {
            sqlx::query("SELECT * FROM question_requests WHERE thread_id=? AND turn_id=? AND resolution!='closed' AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain'))")
                .bind(thread_id).bind(turn).fetch_all(&mut *tx).await.map_err(err)?
        } else {
            sqlx::query(
                "SELECT * FROM question_requests WHERE thread_id=? AND resolution!='closed' AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain'))",
            )
            .bind(thread_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(err)?
        };
        let mut closed = Vec::new();
        for row in rows {
            let mut question = row_request(row)?;
            question.resolution = "closed".into();
            question.delivery = "none".into();
            question.reason = Some(reason.into());
            question.updated_at = now();
            sqlx::query("UPDATE question_requests SET resolution='closed',delivery='none',reason=?,updated_at=? WHERE id=? AND resolution!='closed' AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain'))")
                .bind(reason).bind(question.updated_at).bind(&question.id).execute(&mut *tx).await.map_err(err)?;
            event(&mut tx, "question.closed", &question.id, &question).await?;
            closed.push(question);
        }
        tx.commit().await.map_err(err)?;
        Ok(closed)
    }

    pub async fn pending_run_questions(&self, run_id: &str) -> Result<bool, String> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM question_requests WHERE run_id=? AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain')))")
            .bind(run_id).fetch_one(&self.pool).await.map_err(err)
    }
}

fn validate_new(request: &NewQuestionRequest) -> Result<(), String> {
    if [
        request.thread_id.as_str(),
        request.turn_id.as_str(),
        request.item_id.as_str(),
    ]
    .iter()
    .any(|value| value.trim().is_empty())
    {
        return Err("question source thread, turn, and item ids are required".into());
    }
    if !["blocking", "async"].contains(&request.kind.as_str()) {
        return Err("question kind must be blocking or async".into());
    }
    if request.questions.is_empty() || request.questions.len() > 3 {
        return Err("question requests must contain 1-3 questions".into());
    }
    let mut ids = HashSet::new();
    for question in &request.questions {
        if question.id.trim().is_empty() || !ids.insert(&question.id) {
            return Err("question ids must be non-empty and unique".into());
        }
        if question.header.trim().is_empty() || question.question.trim().is_empty() {
            return Err("question header and text are required".into());
        }
        if let Some(options) = &question.options {
            if options.is_empty() || options.iter().any(|o| o.label.trim().is_empty()) {
                return Err("question options must be non-empty and include labels".into());
            }
        }
    }
    Ok(())
}

fn validate_answer_ids(questions: &[Question], answers: &QuestionAnswers) -> Result<(), String> {
    let valid: HashSet<&str> = questions.iter().map(|q| q.id.as_str()).collect();
    if answers.keys().any(|key| !valid.contains(key.as_str())) {
        return Err("answers contain an unknown question id".into());
    }
    if answers
        .values()
        .flatten()
        .any(|value| value.trim().is_empty())
    {
        return Err("answer values must be non-empty".into());
    }
    Ok(())
}

fn validate_complete_answers(
    questions: &[Question],
    answers: &QuestionAnswers,
) -> Result<(), String> {
    validate_answer_ids(questions, answers)?;
    if answers.len() != questions.len()
        || questions.iter().any(|q| {
            answers
                .get(&q.id)
                .map(|values| values.len() != 1 || values[0].trim().is_empty())
                .unwrap_or(true)
        })
    {
        return Err("answers must include exactly one non-empty answer for every question".into());
    }
    Ok(())
}

fn id_fn() -> String {
    id()
}

async fn get_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<QuestionRequest, String> {
    let row = sqlx::query("SELECT * FROM question_requests WHERE id=?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(err)?
        .ok_or_else(|| "question request not found".to_string())?;
    row_request(row)
}

fn row_request(row: sqlx::sqlite::SqliteRow) -> Result<QuestionRequest, String> {
    let runtime_request_id: Option<String> = row.try_get("runtime_request_id").map_err(err)?;
    Ok(QuestionRequest {
        id: row.try_get("id").map_err(err)?,
        thread_id: row.try_get("thread_id").map_err(err)?,
        turn_id: row.try_get("turn_id").map_err(err)?,
        item_id: row.try_get("item_id").map_err(err)?,
        agent_id: row.try_get("agent_id").map_err(err)?,
        task_id: row.try_get("task_id").map_err(err)?,
        run_id: row.try_get("run_id").map_err(err)?,
        kind: row.try_get("kind").map_err(err)?,
        questions: decode(row.try_get("questions").map_err(err)?)?,
        runtime_generation: row.try_get("runtime_generation").map_err(err)?,
        runtime_request_id: runtime_request_id
            .map(|value| serde_json::from_str(&value).map_err(|e| e.to_string()))
            .transpose()?,
        draft: decode(row.try_get("draft").map_err(err)?)?,
        answers: row
            .try_get::<Option<String>, _>("answers")
            .map_err(err)?
            .map(decode)
            .transpose()?,
        resolution: row.try_get("resolution").map_err(err)?,
        delivery: row.try_get("delivery").map_err(err)?,
        reason: row.try_get("reason").map_err(err)?,
        client_message_id: row.try_get("client_message_id").map_err(err)?,
        created_at: row.try_get("created_at").map_err(err)?,
        updated_at: row.try_get("updated_at").map_err(err)?,
    })
}

async fn event<T: Serialize>(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    entity_id: &str,
    payload: &T,
) -> Result<(), String> {
    sqlx::query("INSERT INTO events(kind,entity_id,payload,created_at) VALUES(?,?,?,?)")
        .bind(kind)
        .bind(entity_id)
        .bind(serde_json::to_string(payload).map_err(|e| e.to_string())?)
        .bind(now())
        .execute(&mut **tx)
        .await
        .map_err(err)?;
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

fn decode<T: for<'de> Deserialize<'de>>(value: String) -> Result<T, String> {
    serde_json::from_str(&value).map_err(|e| e.to_string())
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
