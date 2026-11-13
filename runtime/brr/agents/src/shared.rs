use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::collections::HashSet;
#[path = "shared_normalize.rs"]
mod normalize;
#[path = "shared_reconcile.rs"]
mod reconcile;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedTable {
    AgentProfiles,
    ProfileRevisions,
    ProfileDrafts,
    ProfileReviewReceipts,
    MemorySources,
    MemorySourceRevisions,
    MemoryRecords,
    MemoryRecordSources,
    LearnedSkills,
    SkillSources,
    TaskDefinitions,
    WorkflowDefinitions,
    Projects,
}

impl SharedTable {
    fn name(self) -> &'static str {
        match self {
            Self::AgentProfiles => "agent_profiles",
            Self::ProfileRevisions => "profile_revisions",
            Self::ProfileDrafts => "profile_drafts",
            Self::ProfileReviewReceipts => "profile_review_receipts",
            Self::MemorySources => "memory_sources",
            Self::MemorySourceRevisions => "memory_source_revisions",
            Self::MemoryRecords => "memory_records",
            Self::MemoryRecordSources => "memory_record_sources",
            Self::LearnedSkills => "learned_skills",
            Self::SkillSources => "skill_sources",
            Self::TaskDefinitions => "task_definitions",
            Self::WorkflowDefinitions => "workflow_definitions",
            Self::Projects => "projects",
        }
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::AgentProfiles => &["id", "revision", "candidate", "createdAt", "updatedAt"],
            Self::ProfileRevisions => &[
                "agentId",
                "revision",
                "candidate",
                "createdAt",
                "updatedAt",
                "draftId",
                "hash",
            ],
            Self::ProfileDrafts => &[
                "id",
                "threadId",
                "agentId",
                "candidate",
                "hash",
                "status",
                "baseRevision",
                "createdAt",
            ],
            Self::ProfileReviewReceipts => &["operationId", "kind", "result"],
            Self::MemorySources => &[
                "sourceId",
                "revision",
                "threadId",
                "agentId",
                "projectId",
                "content",
                "active",
                "updatedAt",
                "eventId",
            ],
            Self::MemorySourceRevisions => {
                &["sourceId", "revision", "content", "updatedAt", "eventId"]
            }
            Self::MemoryRecords => &[
                "id",
                "scope",
                "scopeId",
                "kind",
                "content",
                "revision",
                "active",
                "createdAt",
                "updatedAt",
                "eventId",
            ],
            Self::MemoryRecordSources => &["memoryId", "sourceId", "sourceRevision"],
            Self::LearnedSkills => &[
                "id",
                "name",
                "description",
                "content",
                "revision",
                "scope",
                "scopeId",
                "active",
                "createdAt",
                "updatedAt",
                "eventId",
            ],
            Self::SkillSources => &["skillId", "sourceId", "sourceRevision"],
            Self::TaskDefinitions => &[
                "id",
                "agentId",
                "title",
                "instructions",
                "acceptanceCriteria",
                "dependencies",
                "createdAt",
            ],
            Self::WorkflowDefinitions => &[
                "id",
                "name",
                "agentId",
                "task",
                "nodes",
                "expression",
                "timezone",
            ],
            Self::Projects => &[
                "id",
                "name",
                "revision",
                "model",
                "effort",
                "permission",
                "hidden",
            ],
        }
    }
}

/// Maps a Git-qualified identity into a safe local SQL identifier. Local
/// origin IDs are preserved by the importer; this mapping is for foreign rows.
pub fn shared_local_id(origin_node_id: &str, table: SharedTable, key: &str) -> String {
    let digest = Sha256::digest(format!("{origin_node_id}\0{}\0{key}", table.name()).as_bytes());
    format!("fleet_{}", hex(&digest))
}

pub fn shared_entity_id(origin_node_id: &str, table: SharedTable, key: &str) -> String {
    shared_local_id(origin_node_id, table, key)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedRow {
    /// `None` denotes the current local database. Imported Git rows must carry
    /// their immutable origin node ID.
    pub origin_node_id: Option<String>,
    pub table: SharedTable,
    pub key: String,
    pub data: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedSnapshot {
    pub schema_version: u32,
    pub rows: Vec<SharedRow>,
}

impl SharedSnapshot {
    pub fn validate(&self, require_origins: bool) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported shared snapshot schema version {}",
                self.schema_version
            ));
        }
        let mut identities = HashSet::new();
        for row in &self.rows {
            if row.key.trim().is_empty() {
                return Err("shared row key is empty".into());
            }
            if require_origins && row.origin_node_id.as_deref().is_none_or(str::is_empty) {
                return Err("materialized shared rows require an origin node ID".into());
            }
            if row.origin_node_id.as_deref().is_some_and(str::is_empty) {
                return Err("shared row origin node ID is empty".into());
            }
            let identity = (
                row.origin_node_id.as_deref().unwrap_or(""),
                row.table,
                row.key.as_str(),
            );
            if !identities.insert(identity) {
                return Err(format!("duplicate shared row identity: {}", row.key));
            }
            let object = row
                .data
                .as_object()
                .ok_or_else(|| format!("{} row must be an object", row.table.name()))?;
            for field in object.keys() {
                if !row.table.fields().contains(&field.as_str()) {
                    return Err(format!(
                        "unknown field {field} in shared {} row",
                        row.table.name()
                    ));
                }
            }
            for field in row.table.fields() {
                if !object.contains_key(*field) {
                    return Err(format!(
                        "missing field {field} in shared {} row",
                        row.table.name()
                    ));
                }
            }
            if !object.values().all(valid_scalar_or_json) {
                return Err(format!("invalid value in shared {} row", row.table.name()));
            }
        }
        let receipts = self
            .rows
            .iter()
            .filter(|row| row.table == SharedTable::ProfileReviewReceipts)
            .collect::<Vec<_>>();
        let mut published_drafts = HashSet::new();
        for revision in self
            .rows
            .iter()
            .filter(|row| row.table == SharedTable::ProfileRevisions)
        {
            let draft_id = revision.data["draftId"]
                .as_str()
                .ok_or_else(|| "profile revision has no source draft ID".to_owned())?;
            let hash = revision.data["hash"]
                .as_str()
                .ok_or_else(|| "profile revision has no review hash".to_owned())?;
            let origin = revision.origin_node_id.as_deref().unwrap_or("");
            let draft_row = self
                .resolve(origin, SharedTable::ProfileDrafts, draft_id)
                .ok_or_else(|| "profile revision draft is missing".to_owned())?;
            let draft = &draft_row.data;
            if !published_drafts
                .insert((draft_row.origin_node_id.as_deref(), draft_row.key.as_str()))
            {
                return Err("A reviewed draft cannot publish multiple profile revisions".into());
            }
            if draft["hash"].as_str() != Some(hash) || draft["status"].as_str() != Some("applied") {
                return Err("profile revision does not match its reviewed draft hash".into());
            }
            let candidate: crate::ProfileCandidate =
                serde_json::from_value(draft["candidate"].clone()).map_err(err)?;
            if crate::digest(&candidate)? != hash
                || draft["candidate"] != revision.data["candidate"]
            {
                return Err("profile revision candidate differs from its reviewed draft".into());
            }
            if let Some(agent_ref) = draft["agentId"].as_str() {
                let head = self
                    .resolve(
                        origin,
                        SharedTable::AgentProfiles,
                        revision.data["agentId"]
                            .as_str()
                            .ok_or("Missing revision agent ID")?,
                    )
                    .ok_or("Profile revision agent is missing")?;
                if self
                    .resolve(
                        draft_row.origin_node_id.as_deref().unwrap_or(""),
                        SharedTable::AgentProfiles,
                        agent_ref,
                    )
                    .is_none_or(|target| {
                        target.key != head.key || target.origin_node_id != head.origin_node_id
                    })
                {
                    return Err("Profile revision review targets another agent".into());
                }
            }
            let reviewed = receipts.iter().any(|receipt| {
                receipt.data["kind"].as_str() == Some("profile.approve")
                    && receipt.data["result"]["id"]
                        .as_str()
                        .and_then(|id| {
                            self.resolve(
                                receipt.origin_node_id.as_deref().unwrap_or(""),
                                SharedTable::ProfileDrafts,
                                id,
                            )
                        })
                        .is_some_and(|target| {
                            target.key == draft_row.key
                                && target.origin_node_id == draft_row.origin_node_id
                        })
                    && receipt.data["result"]["hash"].as_str() == Some(hash)
            });
            if !reviewed {
                return Err("profile revision has no matching user review receipt".into());
            }
        }
        for head in self
            .rows
            .iter()
            .filter(|row| row.table == SharedTable::AgentProfiles)
        {
            if head.data["id"].as_str() != Some(head.key.as_str()) {
                return Err("Profile identity differs from its key".into());
            }
            let candidate: crate::ProfileCandidate =
                serde_json::from_value(head.data["candidate"].clone()).map_err(err)?;
            let hash = crate::digest(&candidate)?;
            let revision = self
                .rows
                .iter()
                .find(|row| {
                    row.table == SharedTable::ProfileRevisions
                        && row.origin_node_id == head.origin_node_id
                        && row.data["agentId"] == head.data["id"]
                        && row.data["revision"] == head.data["revision"]
                })
                .ok_or("Shared profile has no reviewed published revision")?;
            if revision.data["candidate"] != head.data["candidate"]
                || revision.data["hash"].as_str() != Some(hash.as_str())
            {
                return Err("Shared profile differs from its reviewed published revision".into());
            }
        }
        // A standalone AgentStore cannot query the Host's separate project
        // database. Catalog publication joins it and requires every reference.
        crate::shared_validate::validate_with_host_projects(self, require_origins)?;
        Ok(())
    }

    fn resolve(&self, origin: &str, table: SharedTable, reference: &str) -> Option<&SharedRow> {
        let mut matches = self.rows.iter().filter(|row| {
            row.table == table
                && ((row.origin_node_id.as_deref().unwrap_or("") == origin && row.key == reference)
                    || shared_entity_id(
                        row.origin_node_id.as_deref().unwrap_or(""),
                        table,
                        &row.key,
                    ) == reference)
        });
        let row = matches.next()?;
        matches.next().is_none().then_some(row)
    }
}

fn valid_scalar_or_json(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        Value::Array(items) => items.iter().all(valid_scalar_or_json),
        Value::Object(fields) => fields.values().all(valid_scalar_or_json),
    }
}

pub(crate) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("CREATE TABLE IF NOT EXISTS shared_projection_state(id INTEGER PRIMARY KEY CHECK(id=1),revision TEXT NOT NULL)")
        .execute(pool).await.map_err(|e| e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_shared_projection(origin_node_id TEXT NOT NULL,table_name TEXT NOT NULL,entity_key TEXT NOT NULL,payload TEXT NOT NULL,revision TEXT NOT NULL,PRIMARY KEY(origin_node_id,table_name,entity_key))")
        .execute(pool).await.map_err(|e| e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_shared_identity(origin_node_id TEXT NOT NULL,table_name TEXT NOT NULL,entity_key TEXT NOT NULL,local_id TEXT NOT NULL,PRIMARY KEY(origin_node_id,table_name,entity_key),UNIQUE(table_name,local_id))")
        .execute(pool).await.map_err(|e| e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_shared_state(id INTEGER PRIMARY KEY CHECK(id=1),node_id TEXT NOT NULL)")
        .execute(pool).await.map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_shared_tombstones(origin_node_id TEXT NOT NULL,table_name TEXT NOT NULL,entity_key TEXT NOT NULL,PRIMARY KEY(origin_node_id,table_name,entity_key))")
        .execute(pool).await.map_err(err)?;
    Ok(())
}

pub(crate) async fn snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<SharedSnapshot, String> {
    let mut rows = Vec::new();
    for row in sqlx::query(
        "SELECT id,revision,candidate,created_at,updated_at FROM agent_profiles ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(err)?
    {
        let candidate: String = row.try_get("candidate").map_err(err)?;
        push(
            &mut rows,
            SharedTable::AgentProfiles,
            row.try_get("id").map_err(err)?,
            json!({
                "id": row.try_get::<String,_>("id").map_err(err)?,
                "revision": row.try_get::<i64,_>("revision").map_err(err)?,
                "candidate": parse_json(&candidate)?,
                "createdAt": row.try_get::<i64,_>("created_at").map_err(err)?,
                "updatedAt": row.try_get::<i64,_>("updated_at").map_err(err)?,
            }),
        );
    }
    for row in sqlx::query("SELECT agent_id,revision,candidate,created_at,updated_at,draft_id,hash FROM profile_revisions ORDER BY agent_id,revision")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let candidate: String = row.try_get("candidate").map_err(err)?;
        let agent: String = row.try_get("agent_id").map_err(err)?;
        let revision: i64 = row.try_get("revision").map_err(err)?;
        push(&mut rows, SharedTable::ProfileRevisions, format!("{agent}@{revision}"), json!({
            "agentId": agent, "revision": revision, "candidate": parse_json(&candidate)?,
            "createdAt": row.try_get::<i64,_>("created_at").map_err(err)?,
            "updatedAt": row.try_get::<i64,_>("updated_at").map_err(err)?,
            "draftId": row.try_get::<String,_>("draft_id").map_err(err)?,
            "hash": row.try_get::<String,_>("hash").map_err(err)?,
        }));
    }
    for row in sqlx::query("SELECT id,thread_id,agent_id,candidate,hash,status,base_revision,created_at FROM profile_drafts ORDER BY id")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let candidate: String = row.try_get("candidate").map_err(err)?;
        let id: String = row.try_get("id").map_err(err)?;
        push(&mut rows, SharedTable::ProfileDrafts, id.clone(), json!({
            "id": id, "threadId": row.try_get::<String,_>("thread_id").map_err(err)?,
            "agentId": row.try_get::<Option<String>,_>("agent_id").map_err(err)?,
            "candidate": parse_json(&candidate)?, "hash": row.try_get::<String,_>("hash").map_err(err)?,
            "status": row.try_get::<String,_>("status").map_err(err)?,
            "baseRevision": row.try_get::<Option<i64>,_>("base_revision").map_err(err)?,
            "createdAt": row.try_get::<i64,_>("created_at").map_err(err)?,
        }));
    }
    for row in sqlx::query("SELECT operation_id,kind,result FROM operation_results WHERE kind IN ('profile.approve','profile.apply') ORDER BY operation_id")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let operation_id: String = row.try_get("operation_id").map_err(err)?;
        let result: String = row.try_get("result").map_err(err)?;
        push(&mut rows, SharedTable::ProfileReviewReceipts, operation_id.clone(), json!({
            "operationId": operation_id, "kind": row.try_get::<String,_>("kind").map_err(err)?,
            "result": parse_json(&result)?,
        }));
    }
    capture_memory(tx, &mut rows).await?;
    capture_definition_rows(tx, &mut rows).await?;
    if let Some(node) =
        sqlx::query_scalar::<_, String>("SELECT node_id FROM fleet_shared_state WHERE id=1")
            .fetch_optional(&mut **tx)
            .await
            .map_err(err)?
    {
        rows = normalize::rows(tx, &node, rows).await?;
    }
    rows.sort_by(|a, b| (a.table as u8, &a.key).cmp(&(b.table as u8, &b.key)));
    let snapshot = SharedSnapshot {
        schema_version: 1,
        rows,
    };
    snapshot.validate(false)?;
    Ok(snapshot)
}

async fn capture_memory(
    tx: &mut Transaction<'_, Sqlite>,
    rows: &mut Vec<SharedRow>,
) -> Result<(), String> {
    for row in sqlx::query("SELECT source_id,revision,thread_id,agent_id,project_id,content,active,updated_at,event_id FROM memory_sources ORDER BY source_id")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let key: String = row.try_get("source_id").map_err(err)?;
        push(rows, SharedTable::MemorySources, key.clone(), json!({
            "sourceId": key, "revision": row.try_get::<i64,_>("revision").map_err(err)?,
            "threadId": row.try_get::<String,_>("thread_id").map_err(err)?,
            "agentId": row.try_get::<Option<String>,_>("agent_id").map_err(err)?,
            "projectId": row.try_get::<Option<String>,_>("project_id").map_err(err)?,
            "content": row.try_get::<String,_>("content").map_err(err)?,
            "active": row.try_get::<i64,_>("active").map_err(err)? != 0,
            "updatedAt": row.try_get::<i64,_>("updated_at").map_err(err)?,
            "eventId": row.try_get::<Option<i64>,_>("event_id").map_err(err)?,
        }));
    }
    for row in sqlx::query("SELECT source_id,revision,content,updated_at,event_id FROM memory_source_revisions ORDER BY source_id,revision")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let source: String = row.try_get("source_id").map_err(err)?;
        let revision: i64 = row.try_get("revision").map_err(err)?;
        push(rows, SharedTable::MemorySourceRevisions, format!("{source}@{revision}"), json!({
            "sourceId": source, "revision": revision, "content": row.try_get::<String,_>("content").map_err(err)?,
            "updatedAt": row.try_get::<i64,_>("updated_at").map_err(err)?,
            "eventId": row.try_get::<Option<i64>,_>("event_id").map_err(err)?,
        }));
    }
    for row in sqlx::query("SELECT id,scope,scope_id,kind,content,revision,active,created_at,updated_at,event_id FROM memory_records ORDER BY id")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let key: String = row.try_get("id").map_err(err)?;
        push(rows, SharedTable::MemoryRecords, key.clone(), json!({
            "id": key, "scope": row.try_get::<String,_>("scope").map_err(err)?,
            "scopeId": row.try_get::<String,_>("scope_id").map_err(err)?,
            "kind": row.try_get::<String,_>("kind").map_err(err)?, "content": row.try_get::<String,_>("content").map_err(err)?,
            "revision": row.try_get::<i64,_>("revision").map_err(err)?,
            "active": row.try_get::<i64,_>("active").map_err(err)? != 0,
            "createdAt": row.try_get::<i64,_>("created_at").map_err(err)?,
            "updatedAt": row.try_get::<i64,_>("updated_at").map_err(err)?,
            "eventId": row.try_get::<Option<i64>,_>("event_id").map_err(err)?,
        }));
    }
    for (table, query, id_field, key_field, other_field) in [
        (
            SharedTable::MemoryRecordSources,
            "SELECT memory_id,source_id,source_revision FROM memory_record_sources ORDER BY memory_id,source_id",
            "memory_id",
            "memoryId",
            "sourceId",
        ),
        (
            SharedTable::SkillSources,
            "SELECT skill_id,source_id,source_revision FROM skill_sources ORDER BY skill_id,source_id",
            "skill_id",
            "skillId",
            "sourceId",
        ),
    ] {
        for row in sqlx::query(query).fetch_all(&mut **tx).await.map_err(err)? {
            let id: String = row.try_get(id_field).map_err(err)?;
            let source: String = row.try_get("source_id").map_err(err)?;
            let revision: i64 = row.try_get("source_revision").map_err(err)?;
            let mut data = serde_json::Map::new();
            data.insert(key_field.into(), json!(id));
            data.insert(other_field.into(), json!(source));
            data.insert("sourceRevision".into(), json!(revision));
            push(rows, table, format!("{id}@{source}"), Value::Object(data));
        }
    }
    for row in sqlx::query("SELECT id,name,description,content,revision,scope,scope_id,active,created_at,updated_at,event_id FROM learned_skills ORDER BY id")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let key: String = row.try_get("id").map_err(err)?;
        push(rows, SharedTable::LearnedSkills, key.clone(), json!({
            "id": key, "name": row.try_get::<String,_>("name").map_err(err)?,
            "description": row.try_get::<String,_>("description").map_err(err)?,
            "content": row.try_get::<String,_>("content").map_err(err)?,
            "revision": row.try_get::<i64,_>("revision").map_err(err)?,
            "scope": row.try_get::<String,_>("scope").map_err(err)?,
            "scopeId": row.try_get::<String,_>("scope_id").map_err(err)?,
            "active": row.try_get::<i64,_>("active").map_err(err)? != 0,
            "createdAt": row.try_get::<i64,_>("created_at").map_err(err)?,
            "updatedAt": row.try_get::<i64,_>("updated_at").map_err(err)?,
            "eventId": row.try_get::<Option<i64>,_>("event_id").map_err(err)?,
        }));
    }
    Ok(())
}

async fn capture_definition_rows(
    tx: &mut Transaction<'_, Sqlite>,
    rows: &mut Vec<SharedRow>,
) -> Result<(), String> {
    for row in sqlx::query("SELECT id,agent_id,title,instructions,acceptance_criteria,dependencies,created_at FROM tasks ORDER BY id")
        .fetch_all(&mut **tx).await.map_err(err)? {
        let key: String = row.try_get("id").map_err(err)?;
        push(rows, SharedTable::TaskDefinitions, key.clone(), json!({
            "id": key, "agentId": row.try_get::<String,_>("agent_id").map_err(err)?,
            "title": row.try_get::<String,_>("title").map_err(err)?,
            "instructions": row.try_get::<String,_>("instructions").map_err(err)?,
            "acceptanceCriteria": parse_json(&row.try_get::<String,_>("acceptance_criteria").map_err(err)?)?,
            "dependencies": parse_json(&row.try_get::<String,_>("dependencies").map_err(err)?)?,
            "createdAt": row.try_get::<i64,_>("created_at").map_err(err)?,
        }));
    }
    for row in sqlx::query(
        "SELECT id,name,agent_id,task,nodes,expression,timezone FROM workflows ORDER BY id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(err)?
    {
        let key: String = row.try_get("id").map_err(err)?;
        let task: String = row.try_get("task").map_err(err)?;
        let nodes: Option<String> = row.try_get("nodes").map_err(err)?;
        push(
            rows,
            SharedTable::WorkflowDefinitions,
            key.clone(),
            json!({
                "id": key, "name": row.try_get::<String,_>("name").map_err(err)?,
                "agentId": row.try_get::<String,_>("agent_id").map_err(err)?,
                "task": shared_task_input(parse_json(&task)?)?,
                "nodes": nodes.map(|value| shared_workflow_nodes(parse_json(&value)?)).transpose()?,
                "expression": row.try_get::<String,_>("expression").map_err(err)?,
                "timezone": row.try_get::<String,_>("timezone").map_err(err)?,
            }),
        );
    }
    Ok(())
}

fn shared_task_input(value: Value) -> Result<Value, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "workflow task definition must be an object".to_owned())?;
    let mut shared = serde_json::Map::new();
    for key in [
        "agentId",
        "title",
        "instructions",
        "acceptanceCriteria",
        "dependencies",
        "model",
        "effort",
    ] {
        if let Some(value) = object.get(key) {
            shared.insert(key.into(), value.clone());
        }
    }
    Ok(Value::Object(shared))
}

fn shared_workflow_nodes(value: Value) -> Result<Value, String> {
    let nodes = value
        .as_array()
        .ok_or_else(|| "workflow node definitions must be an array".to_owned())?;
    let mut shared_nodes = Vec::with_capacity(nodes.len());
    for node in nodes {
        let object = node
            .as_object()
            .ok_or_else(|| "workflow node definition must be an object".to_owned())?;
        let key = object
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| "workflow node key is required".to_owned())?;
        let task = object
            .get("task")
            .cloned()
            .ok_or_else(|| "workflow node task is required".to_owned())?;
        shared_nodes.push(json!({"key":key,"task":shared_task_input(task)?}));
    }
    Ok(Value::Array(shared_nodes))
}

fn push(rows: &mut Vec<SharedRow>, table: SharedTable, key: String, data: Value) {
    rows.push(SharedRow {
        origin_node_id: None,
        table,
        key,
        data,
    });
}

fn parse_json(value: &str) -> Result<Value, String> {
    serde_json::from_str(value).map_err(|e| e.to_string())
}

pub(crate) async fn materialize(
    pool: &SqlitePool,
    local_node_id: &str,
    snapshot: &SharedSnapshot,
    revision: &str,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(err)?;
    materialize_tx(&mut tx, local_node_id, snapshot, revision).await?;
    tx.commit().await.map_err(err)
}

pub(crate) async fn materialize_from<F>(
    pool: &SqlitePool,
    node: &str,
    read: F,
) -> Result<String, String>
where
    F: FnOnce() -> Result<(String, SharedSnapshot), String> + Send,
{
    // SQL writers acquire Git only from their active SQL transaction. Recovery
    // follows that same order and reads HEAD after every earlier writer exits.
    let mut tx = pool.begin().await.map_err(err)?;
    let (revision, snapshot) = read()?;
    materialize_tx(&mut tx, node, &snapshot, &revision).await?;
    tx.commit().await.map_err(err)?;
    Ok(revision)
}

async fn materialize_tx(
    tx: &mut Transaction<'_, Sqlite>,
    local_node_id: &str,
    snapshot: &SharedSnapshot,
    revision: &str,
) -> Result<(), String> {
    if local_node_id.trim().is_empty() {
        return Err("Shared materialization requires a node identity".into());
    }
    if revision.trim().is_empty() {
        return Err("Git materialization revision is required".into());
    }
    snapshot.validate(true)?;
    if let Some(existing) =
        sqlx::query_scalar::<_, String>("SELECT node_id FROM fleet_shared_state WHERE id=1")
            .fetch_optional(&mut **tx)
            .await
            .map_err(err)?
    {
        if existing != local_node_id {
            return Err("Shared store belongs to another node identity".into());
        }
    }
    sqlx::query("INSERT OR IGNORE INTO fleet_shared_state(id,node_id) VALUES(1,?)")
        .bind(local_node_id)
        .execute(&mut **tx)
        .await
        .map_err(err)?;
    reconcile::reconcile(tx, local_node_id, snapshot).await?;
    materialize_reviews(tx, local_node_id, snapshot).await?;
    materialize_memory(tx, local_node_id, snapshot).await?;
    for row in &snapshot.rows {
        if row.table == SharedTable::AgentProfiles {
            let origin = row.origin_node_id.as_deref().unwrap();
            let version = row.data["revision"]
                .as_i64()
                .filter(|value| *value > 0)
                .ok_or("Invalid shared profile revision")?;
            let candidate: crate::ProfileCandidate =
                serde_json::from_value(row.data["candidate"].clone()).map_err(err)?;
            let hash = crate::digest(&candidate)?;
            let reviewed = snapshot
                .rows
                .iter()
                .find(|revision| {
                    revision.table == SharedTable::ProfileRevisions
                        && revision.origin_node_id.as_deref() == Some(origin)
                        && revision.data["agentId"] == row.data["id"]
                        && revision.data["revision"].as_i64() == Some(version)
                })
                .ok_or("Shared profile has no reviewed published revision")?;
            if reviewed.data["candidate"] != row.data["candidate"]
                || reviewed.data["hash"].as_str() != Some(hash.as_str())
            {
                return Err("Shared profile differs from its reviewed published revision".into());
            }
            let local_id = if origin == local_node_id {
                row.key.clone()
            } else {
                shared_local_id(origin, row.table, &row.key)
            };
            sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,candidate=excluded.candidate,created_at=excluded.created_at,updated_at=excluded.updated_at")
                .bind(&local_id).bind(version).bind(serde_json::to_string(&candidate).map_err(err)?)
                .bind(row.data["createdAt"].as_i64().ok_or("Invalid profile creation timestamp")?).bind(row.data["updatedAt"].as_i64().ok_or("Invalid profile update timestamp")?)
                .execute(&mut **tx).await.map_err(err)?;
            sqlx::query("INSERT INTO fleet_shared_identity(origin_node_id,table_name,entity_key,local_id) VALUES(?,?,?,?) ON CONFLICT(origin_node_id,table_name,entity_key) DO UPDATE SET local_id=excluded.local_id")
                .bind(origin).bind(row.table.name()).bind(&row.key).bind(&local_id).execute(&mut **tx).await.map_err(err)?;
        }
    }
    persist_projection(tx, snapshot, revision).await?;
    Ok(())
}

pub(crate) async fn persist_projection(
    tx: &mut Transaction<'_, Sqlite>,
    snapshot: &SharedSnapshot,
    revision: &str,
) -> Result<(), String> {
    // Standalone legacy stores are qualified by the service during activation.
    if snapshot.rows.iter().all(|row| row.origin_node_id.is_some()) {
        sqlx::query("DELETE FROM fleet_shared_projection")
            .execute(&mut **tx)
            .await
            .map_err(err)?;
        for row in &snapshot.rows {
            sqlx::query("INSERT INTO fleet_shared_projection(origin_node_id,table_name,entity_key,payload,revision) VALUES(?,?,?,?,?)")
            .bind(row.origin_node_id.as_deref().unwrap())
            .bind(row.table.name())
            .bind(&row.key)
            .bind(serde_json::to_string(&row.data).map_err(err)?)
            .bind(revision)
            .execute(&mut **tx).await.map_err(err)?;
        }
    }
    sqlx::query("INSERT INTO shared_projection_state(id,revision) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision")
        .bind(revision).execute(&mut **tx).await.map_err(err)?;
    Ok(())
}

async fn materialize_reviews(
    tx: &mut Transaction<'_, Sqlite>,
    node: &str,
    snapshot: &SharedSnapshot,
) -> Result<(), String> {
    for row in &snapshot.rows {
        let origin = row.origin_node_id.as_deref().unwrap();
        let data = &row.data;
        let required = |field: &str| {
            data[field]
                .as_str()
                .ok_or_else(|| format!("Invalid review {field}"))
        };
        match row.table {
            SharedTable::ProfileDrafts if origin == node => {
                let agent = data["agentId"].as_str().map(|id| {
                    local_reference(snapshot, node, origin, SharedTable::AgentProfiles, id)
                });
                sqlx::query("INSERT INTO profile_drafts(id,thread_id,agent_id,candidate,hash,status,base_revision,created_at) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET thread_id=excluded.thread_id,agent_id=excluded.agent_id,candidate=excluded.candidate,hash=excluded.hash,status=excluded.status,base_revision=excluded.base_revision,created_at=excluded.created_at")
                    .bind(&row.key).bind(required("threadId")?).bind(agent).bind(serde_json::to_string(&data["candidate"]).map_err(err)?).bind(required("hash")?).bind(required("status")?).bind(data["baseRevision"].as_i64()).bind(data["createdAt"].as_i64().ok_or("Invalid draft timestamp")?).execute(&mut **tx).await.map_err(err)?;
            }
            SharedTable::ProfileRevisions => {
                let agent = local_reference(
                    snapshot,
                    node,
                    origin,
                    SharedTable::AgentProfiles,
                    required("agentId")?,
                );
                let draft = local_reference(
                    snapshot,
                    node,
                    origin,
                    SharedTable::ProfileDrafts,
                    required("draftId")?,
                );
                sqlx::query("INSERT INTO profile_revisions(agent_id,revision,candidate,created_at,updated_at,draft_id,hash) VALUES(?,?,?,?,?,?,?) ON CONFLICT(agent_id,revision) DO UPDATE SET candidate=excluded.candidate,created_at=excluded.created_at,updated_at=excluded.updated_at,draft_id=excluded.draft_id,hash=excluded.hash")
                    .bind(agent).bind(data["revision"].as_i64().ok_or("Invalid profile revision")?).bind(serde_json::to_string(&data["candidate"]).map_err(err)?).bind(data["createdAt"].as_i64().ok_or("Invalid revision timestamp")?).bind(data["updatedAt"].as_i64().ok_or("Invalid revision timestamp")?).bind(draft).bind(required("hash")?).execute(&mut **tx).await.map_err(err)?;
            }
            SharedTable::ProfileReviewReceipts if origin == node => {
                let mut result = data["result"].clone();
                if data["kind"] == "profile.approve" {
                    if let Some(id) = result["id"].as_str() {
                        result["id"] = json!(local_reference(
                            snapshot,
                            node,
                            origin,
                            SharedTable::ProfileDrafts,
                            id
                        ));
                    }
                    if let Some(id) = result["agentId"].as_str() {
                        result["agentId"] = json!(local_reference(
                            snapshot,
                            node,
                            origin,
                            SharedTable::AgentProfiles,
                            id
                        ));
                    }
                } else if data["kind"] == "profile.apply" {
                    if let Some(id) = result["draftId"].as_str() {
                        result["draftId"] = json!(local_reference(
                            snapshot,
                            node,
                            origin,
                            SharedTable::ProfileDrafts,
                            id
                        ));
                    }
                    if let Some(id) = result["profile"]["id"].as_str() {
                        result["profile"]["id"] = json!(local_reference(
                            snapshot,
                            node,
                            origin,
                            SharedTable::AgentProfiles,
                            id
                        ));
                    }
                } else {
                    return Err("Invalid profile review receipt kind".into());
                }
                if let Some(kind) = sqlx::query_scalar::<_, String>(
                    "SELECT kind FROM operation_results WHERE operation_id=?",
                )
                .bind(&row.key)
                .fetch_optional(&mut **tx)
                .await
                .map_err(err)?
                {
                    if kind != required("kind")? {
                        return Err("Review receipt conflicts with another local operation".into());
                    }
                }
                sqlx::query("INSERT INTO operation_results(operation_id,kind,result) VALUES(?,?,?) ON CONFLICT(operation_id) DO UPDATE SET result=excluded.result")
                    .bind(&row.key).bind(required("kind")?).bind(serde_json::to_string(&result).map_err(err)?).execute(&mut **tx).await.map_err(err)?;
            }
            _ => (),
        }
    }
    Ok(())
}

fn local_reference(
    snapshot: &SharedSnapshot,
    local_node: &str,
    origin: &str,
    table: SharedTable,
    key: &str,
) -> String {
    let target = snapshot.rows.iter().find(|row| {
        row.table == table
            && shared_entity_id(
                row.origin_node_id.as_deref().unwrap_or(origin),
                table,
                &row.key,
            ) == key
    });
    let (origin, key) = target
        .map(|row| (row.origin_node_id.as_deref().unwrap(), row.key.as_str()))
        .unwrap_or((origin, key));
    if origin == local_node {
        key.into()
    } else {
        shared_local_id(origin, table, key)
    }
}

async fn materialize_memory(
    tx: &mut Transaction<'_, Sqlite>,
    local_node: &str,
    snapshot: &SharedSnapshot,
) -> Result<(), String> {
    // Foreign execution bindings and event counters are never reconstructed.
    // Replace evidence sets for each materialized record as a unit; removed
    // links must not survive a three-way merge or crash recovery.
    for row in &snapshot.rows {
        let origin = row.origin_node_id.as_deref().unwrap();
        if row.table == SharedTable::MemoryRecords {
            let id = local_reference(snapshot, local_node, origin, row.table, &row.key);
            sqlx::query("DELETE FROM memory_record_sources WHERE memory_id=?")
                .bind(id)
                .execute(&mut **tx)
                .await
                .map_err(err)?;
        } else if row.table == SharedTable::LearnedSkills {
            let id = local_reference(snapshot, local_node, origin, row.table, &row.key);
            sqlx::query("DELETE FROM skill_sources WHERE skill_id=?")
                .bind(id)
                .execute(&mut **tx)
                .await
                .map_err(err)?;
        }
    }
    for table in [
        SharedTable::MemorySources,
        SharedTable::MemorySourceRevisions,
        SharedTable::MemoryRecords,
        SharedTable::MemoryRecordSources,
        SharedTable::LearnedSkills,
        SharedTable::SkillSources,
    ] {
        for row in snapshot.rows.iter().filter(|row| row.table == table) {
            let origin = row.origin_node_id.as_deref().unwrap();
            let data = &row.data;
            let required = |field: &str| {
                data[field]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("Invalid shared {field}"))
            };
            let reference = |field: &str, target| -> Result<Value, String> {
                Ok(json!(local_reference(
                    snapshot,
                    local_node,
                    origin,
                    target,
                    &required(field)?
                )))
            };
            let optional_reference = |field: &str, target| -> Result<Value, String> {
                if data[field].is_null() {
                    Ok(Value::Null)
                } else {
                    reference(field, target)
                }
            };
            let own_id = if origin == local_node {
                row.key.clone()
            } else {
                shared_local_id(origin, table, &row.key)
            };
            let (columns, mut values, keys): (&[&str], Vec<Value>, &[&str]) = match table {
                SharedTable::MemorySources => (
                    &[
                        "source_id",
                        "revision",
                        "thread_id",
                        "agent_id",
                        "project_id",
                        "content",
                        "active",
                        "updated_at",
                        "event_id",
                    ],
                    vec![
                        json!(own_id),
                        data["revision"].clone(),
                        json!(if origin == local_node {
                            required("threadId")?
                        } else {
                            format!("{origin}:{}", required("threadId")?)
                        }),
                        optional_reference("agentId", SharedTable::AgentProfiles)?,
                        optional_reference("projectId", SharedTable::Projects)?,
                        json!(required("content")?),
                        data["active"].clone(),
                        data["updatedAt"].clone(),
                        json!(
                            preserve_local_event_id(
                                tx,
                                local_node,
                                row,
                                SharedTable::MemorySources,
                                &own_id
                            )
                            .await?
                        ),
                    ],
                    &["source_id"],
                ),
                SharedTable::MemorySourceRevisions => (
                    &["source_id", "revision", "content", "updated_at", "event_id"],
                    vec![
                        reference("sourceId", SharedTable::MemorySources)?,
                        data["revision"].clone(),
                        json!(required("content")?),
                        data["updatedAt"].clone(),
                        json!(
                            preserve_local_event_id(
                                tx,
                                local_node,
                                row,
                                SharedTable::MemorySourceRevisions,
                                &own_id
                            )
                            .await?
                        ),
                    ],
                    &["source_id", "revision"],
                ),
                SharedTable::MemoryRecords => {
                    let scope = required("scope")?;
                    if !matches!(scope.as_str(), "agent" | "user" | "project") {
                        return Err("Invalid shared memory scope".into());
                    }
                    let scope_id = if scope == "agent" || scope == "project" {
                        reference(
                            "scopeId",
                            if scope == "agent" {
                                SharedTable::AgentProfiles
                            } else {
                                SharedTable::Projects
                            },
                        )?
                    } else {
                        json!(required("scopeId")?)
                    };
                    (
                        &[
                            "id",
                            "scope",
                            "scope_id",
                            "kind",
                            "content",
                            "revision",
                            "active",
                            "created_at",
                            "updated_at",
                            "event_id",
                        ],
                        vec![
                            json!(own_id),
                            json!(scope),
                            scope_id,
                            json!(required("kind")?),
                            json!(required("content")?),
                            data["revision"].clone(),
                            data["active"].clone(),
                            data["createdAt"].clone(),
                            data["updatedAt"].clone(),
                            json!(
                                preserve_local_event_id(
                                    tx,
                                    local_node,
                                    row,
                                    SharedTable::MemoryRecords,
                                    &own_id
                                )
                                .await?
                            ),
                        ],
                        &["id"],
                    )
                }
                SharedTable::MemoryRecordSources => (
                    &["memory_id", "source_id", "source_revision"],
                    vec![
                        reference("memoryId", SharedTable::MemoryRecords)?,
                        reference("sourceId", SharedTable::MemorySources)?,
                        data["sourceRevision"].clone(),
                    ],
                    &["memory_id", "source_id"],
                ),
                SharedTable::LearnedSkills => {
                    let scope = required("scope")?;
                    if !matches!(scope.as_str(), "agent" | "user" | "project") {
                        return Err("Invalid shared skill scope".into());
                    }
                    let scope_id = if scope == "agent" || scope == "project" {
                        reference(
                            "scopeId",
                            if scope == "agent" {
                                SharedTable::AgentProfiles
                            } else {
                                SharedTable::Projects
                            },
                        )?
                    } else {
                        json!(required("scopeId")?)
                    };
                    (
                        &[
                            "id",
                            "name",
                            "description",
                            "content",
                            "revision",
                            "scope",
                            "scope_id",
                            "active",
                            "created_at",
                            "updated_at",
                            "event_id",
                        ],
                        vec![
                            json!(own_id),
                            json!(required("name")?),
                            json!(required("description")?),
                            json!(required("content")?),
                            data["revision"].clone(),
                            json!(scope),
                            scope_id,
                            data["active"].clone(),
                            data["createdAt"].clone(),
                            data["updatedAt"].clone(),
                            json!(
                                preserve_local_event_id(
                                    tx,
                                    local_node,
                                    row,
                                    SharedTable::LearnedSkills,
                                    &own_id
                                )
                                .await?
                            ),
                        ],
                        &["id"],
                    )
                }
                SharedTable::SkillSources => (
                    &["skill_id", "source_id", "source_revision"],
                    vec![
                        reference("skillId", SharedTable::LearnedSkills)?,
                        reference("sourceId", SharedTable::MemorySources)?,
                        data["sourceRevision"].clone(),
                    ],
                    &["skill_id", "source_id"],
                ),
                _ => unreachable!(),
            };
            for (column, value) in columns.iter().zip(&values) {
                if matches!(
                    *column,
                    "revision" | "source_revision" | "created_at" | "updated_at"
                ) && value.as_i64().is_none()
                {
                    return Err(format!("Invalid shared integer {column}"));
                }
                if *column == "active" && value.as_bool().is_none() {
                    return Err("Invalid shared active flag".into());
                }
            }
            let assignments = columns
                .iter()
                .filter(|column| !keys.contains(column))
                .map(|column| format!("{column}=excluded.{column}"))
                .collect::<Vec<_>>()
                .join(",");
            let statement = format!(
                "INSERT INTO {}({}) VALUES({}) ON CONFLICT({}) DO UPDATE SET {assignments}",
                table.name(),
                columns.join(","),
                vec!["?"; columns.len()].join(","),
                keys.join(",")
            );
            // Identifiers come exclusively from the enum and literal column lists above;
            // all row values are bound below. No imported text enters SQL syntax.
            let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str()));
            for value in values.drain(..) {
                query = match value {
                    Value::Null => query.bind(Option::<String>::None),
                    Value::String(value) => query.bind(value),
                    Value::Bool(value) => query.bind(value),
                    Value::Number(value) => {
                        query.bind(value.as_i64().ok_or("Invalid shared integer")?)
                    }
                    _ => return Err("Invalid shared SQL scalar".into()),
                };
            }
            query.execute(&mut **tx).await.map_err(err)?;
            if matches!(
                table,
                SharedTable::MemorySources
                    | SharedTable::MemoryRecords
                    | SharedTable::LearnedSkills
            ) {
                sqlx::query("INSERT INTO fleet_shared_identity(origin_node_id,table_name,entity_key,local_id) VALUES(?,?,?,?) ON CONFLICT(origin_node_id,table_name,entity_key) DO UPDATE SET local_id=excluded.local_id")
                    .bind(origin).bind(table.name()).bind(&row.key).bind(&own_id).execute(&mut **tx).await.map_err(err)?;
            }
            if table == SharedTable::MemoryRecords {
                sqlx::query("DELETE FROM memory_fts WHERE memory_id=?")
                    .bind(&own_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
                if data["active"] == true {
                    sqlx::query("INSERT INTO memory_fts(memory_id,content) VALUES(?,?)")
                        .bind(&own_id)
                        .bind(required("content")?)
                        .execute(&mut **tx)
                        .await
                        .map_err(err)?;
                }
            }
        }
    }
    Ok(())
}

/// Carries a local SQLite event handle across an idempotent Git projection
/// materialization only when both the previously committed projection and the
/// current SQL row prove that exact local event. Event numbers from another
/// node, changed rows, and Git-ahead-of-SQL recovery deliberately become NULL.
async fn preserve_local_event_id(
    tx: &mut Transaction<'_, Sqlite>,
    local_node: &str,
    row: &SharedRow,
    table: SharedTable,
    local_id: &str,
) -> Result<Option<i64>, String> {
    if row.origin_node_id.as_deref() != Some(local_node) {
        return Ok(None);
    }
    let Some(event_id) = row.data["eventId"].as_i64() else {
        return Ok(None);
    };
    let prior_payload: Option<String> = sqlx::query_scalar(
        "SELECT payload FROM fleet_shared_projection WHERE origin_node_id=? AND table_name=? AND entity_key=?",
    )
    .bind(local_node)
    .bind(table.name())
    .bind(&row.key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(err)?;
    let Some(prior_payload) = prior_payload else {
        return Ok(None);
    };
    if parse_json(&prior_payload)? != row.data {
        return Ok(None);
    }
    let current_event_id: Option<i64> = match table {
        SharedTable::MemorySources => {
            sqlx::query_scalar("SELECT event_id FROM memory_sources WHERE source_id=?")
                .bind(local_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(err)?
                .flatten()
        }
        SharedTable::MemorySourceRevisions => {
            let source_id = row.data["sourceId"]
                .as_str()
                .ok_or("Invalid shared source id")?;
            let revision = row.data["revision"]
                .as_i64()
                .ok_or("Invalid shared source revision")?;
            sqlx::query_scalar(
                "SELECT event_id FROM memory_source_revisions WHERE source_id=? AND revision=?",
            )
            .bind(source_id)
            .bind(revision)
            .fetch_optional(&mut **tx)
            .await
            .map_err(err)?
            .flatten()
        }
        SharedTable::MemoryRecords => {
            sqlx::query_scalar("SELECT event_id FROM memory_records WHERE id=?")
                .bind(local_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(err)?
                .flatten()
        }
        SharedTable::LearnedSkills => {
            sqlx::query_scalar("SELECT event_id FROM learned_skills WHERE id=?")
                .bind(local_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(err)?
                .flatten()
        }
        _ => return Ok(None),
    };
    if current_event_id != Some(event_id) {
        return Ok(None);
    }
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE seq=?")
        .bind(event_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(err)?;
    Ok((exists == 1).then_some(event_id))
}

pub(crate) async fn origin(
    pool: &SqlitePool,
    table: SharedTable,
    local_id: &str,
) -> Result<Option<(String, String)>, String> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT origin_node_id,entity_key FROM fleet_shared_identity WHERE table_name=? AND local_id=?",
    )
    .bind(table.name())
    .bind(local_id)
    .fetch_optional(pool)
    .await
    .map_err(err)
}

pub(crate) async fn materialized(
    pool: &SqlitePool,
) -> Result<Option<(String, SharedSnapshot)>, String> {
    let revision =
        sqlx::query_scalar::<_, String>("SELECT revision FROM shared_projection_state WHERE id=1")
            .fetch_optional(pool)
            .await
            .map_err(err)?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    let rows = sqlx::query("SELECT origin_node_id,table_name,entity_key,payload FROM fleet_shared_projection ORDER BY table_name,origin_node_id,entity_key")
        .fetch_all(pool).await.map_err(err)?;
    let mut shared_rows = Vec::new();
    for row in rows {
        let origin: String = row.try_get("origin_node_id").map_err(err)?;
        let table: String = row.try_get("table_name").map_err(err)?;
        let key: String = row.try_get("entity_key").map_err(err)?;
        let payload: String = row.try_get("payload").map_err(err)?;
        shared_rows.push(SharedRow {
            origin_node_id: Some(origin),
            table: table_from_name(&table)?,
            key,
            data: parse_json(&payload)?,
        });
    }
    let snapshot = SharedSnapshot {
        schema_version: 1,
        rows: shared_rows,
    };
    snapshot.validate(true)?;
    Ok(Some((revision, snapshot)))
}

fn table_from_name(name: &str) -> Result<SharedTable, String> {
    [
        SharedTable::AgentProfiles,
        SharedTable::ProfileRevisions,
        SharedTable::ProfileDrafts,
        SharedTable::ProfileReviewReceipts,
        SharedTable::MemorySources,
        SharedTable::MemorySourceRevisions,
        SharedTable::MemoryRecords,
        SharedTable::MemoryRecordSources,
        SharedTable::LearnedSkills,
        SharedTable::SkillSources,
        SharedTable::TaskDefinitions,
        SharedTable::WorkflowDefinitions,
        SharedTable::Projects,
    ]
    .into_iter()
    .find(|table| table.name() == name)
    .ok_or_else(|| format!("unknown shared projection table: {name}"))
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
