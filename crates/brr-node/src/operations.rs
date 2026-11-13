use brr_remote_protocol::{ApiError, Request};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Outcome {
    pub result: Option<Value>,
    pub error: Option<ApiError>,
}

impl Outcome {
    pub fn from_result(result: Result<Value, ApiError>) -> Self {
        match result {
            Ok(result) => Self {
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                result: None,
                error: Some(error),
            },
        }
    }
    pub fn into_result(self) -> Result<Value, ApiError> {
        match (self.result, self.error) {
            (Some(value), None) => Ok(value),
            (_, Some(error)) => Err(error),
            _ => Err(ApiError::new(
                "invalid_receipt",
                "The operation receipt is invalid",
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Claim {
    New,
    Replay(Outcome),
    Uncertain,
}

#[derive(Clone)]
pub struct Operations {
    pool: SqlitePool,
}

impl Operations {
    pub async fn open(path: &Path) -> Result<Self, ApiError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(storage)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(storage)?;
        }
        sqlx::query("CREATE TABLE IF NOT EXISTS operation_receipts(actor_id TEXT NOT NULL, operation_id TEXT NOT NULL, request_hash TEXT NOT NULL, state TEXT NOT NULL, outcome TEXT, created_at INTEGER NOT NULL DEFAULT(unixepoch()), PRIMARY KEY(actor_id,operation_id))")
            .execute(&pool).await.map_err(storage)?;
        Ok(Self { pool })
    }

    pub async fn begin(&self, actor: &str, request: &Request) -> Result<Claim, ApiError> {
        let operation = operation_id(request)?;
        let hash = request_hash(request)?;
        let inserted = sqlx::query("INSERT OR IGNORE INTO operation_receipts(actor_id,operation_id,request_hash,state) VALUES(?,?,?,'started')")
            .bind(actor).bind(operation).bind(&hash).execute(&self.pool).await.map_err(storage)?;
        if inserted.rows_affected() == 1 {
            return Ok(Claim::New);
        }
        let row = sqlx::query("SELECT request_hash,state,outcome FROM operation_receipts WHERE actor_id=? AND operation_id=?")
            .bind(actor).bind(operation).fetch_one(&self.pool).await.map_err(storage)?;
        if row.get::<String, _>("request_hash") != hash {
            return Err(ApiError::new(
                "operation_conflict",
                "This operationId was already used for different work",
            ));
        }
        let state: String = row.get("state");
        if state == "completed" {
            let result: String = row.get("outcome");
            Ok(Claim::Replay(
                serde_json::from_str(&result).map_err(storage)?,
            ))
        } else {
            Ok(Claim::Uncertain)
        }
    }

    pub async fn finish(
        &self,
        actor: &str,
        request: &Request,
        outcome: Outcome,
    ) -> Result<(), ApiError> {
        let operation = operation_id(request)?;
        let outcome = serde_json::to_string(&outcome).map_err(storage)?;
        let written = sqlx::query("UPDATE operation_receipts SET state='completed',outcome=? WHERE actor_id=? AND operation_id=? AND request_hash=? AND state='started'")
            .bind(outcome).bind(actor).bind(operation).bind(request_hash(request)?).execute(&self.pool).await.map_err(storage)?;
        if written.rows_affected() != 1 {
            return Err(ApiError::new(
                "operation_conflict",
                "The operation receipt no longer matches this command",
            ));
        }
        Ok(())
    }
}

fn storage(error: impl std::fmt::Display) -> ApiError {
    ApiError::new("storage_error", error.to_string())
}

fn operation_id(request: &Request) -> Result<&str, ApiError> {
    request
        .operation_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ApiError::new("operation_id_required", "An operationId is required"))
}

fn request_hash(request: &Request) -> Result<String, ApiError> {
    let mut value = serde_json::to_value(request).map_err(storage)?;
    value.as_object_mut().unwrap().remove("id");
    let bytes = serde_json::to_vec(&canonical(value)).map_err(storage)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brr_remote_protocol::Channel;
    use serde_json::json;

    fn request() -> Request {
        Request {
            id: "request-1".into(),
            channel: Channel::Agents,
            method: "tasks.create".into(),
            params: json!({"title":"one task"}),
            operation_id: Some("operation-1".into()),
            expected_epoch: None,
            expected_revision: None,
            target_node_id: None,
        }
    }

    #[tokio::test]
    async fn lost_acknowledgement_replays_the_original_result_without_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.sqlite");
        let first = Operations::open(&path).await.unwrap();
        let command = request();
        assert!(matches!(
            first.begin("phone", &command).await.unwrap(),
            Claim::New
        ));
        first
            .finish(
                "phone",
                &command,
                Outcome::from_result(Ok(json!({"taskId":"original"}))),
            )
            .await
            .unwrap();
        drop(first);
        let restarted = Operations::open(&path).await.unwrap();
        let mut retry = command;
        retry.id = "new-transport-id".into();
        match restarted.begin("phone", &retry).await.unwrap() {
            Claim::Replay(receipt) => {
                assert_eq!(receipt.into_result().unwrap()["taskId"], "original")
            }
            other => panic!("Operation would be dispatched again: {other:?}"),
        }
    }

    #[tokio::test]
    async fn reusing_an_operation_identity_for_different_work_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let operations = Operations::open(&dir.path().join("operations.sqlite"))
            .await
            .unwrap();
        let command = request();
        operations.begin("phone", &command).await.unwrap();
        operations
            .finish(
                "phone",
                &command,
                Outcome::from_result(Ok(json!({"taskId":"one"}))),
            )
            .await
            .unwrap();
        let mut changed = command;
        changed.params = json!({"title":"a different task"});
        assert_eq!(
            operations.begin("phone", &changed).await.unwrap_err().code,
            "operation_conflict"
        );
    }

    #[tokio::test]
    async fn a_crash_before_recording_a_result_never_authorizes_redispatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operations.sqlite");
        let journal = Operations::open(&path).await.unwrap();
        assert!(matches!(
            journal.begin("phone", &request()).await.unwrap(),
            Claim::New
        ));
        drop(journal);
        let restarted = Operations::open(&path).await.unwrap();
        assert!(matches!(
            restarted.begin("phone", &request()).await.unwrap(),
            Claim::Uncertain
        ));
        assert!(matches!(
            restarted.begin("other-device", &request()).await.unwrap(),
            Claim::New
        ));
    }
}
