//! SQL uses stable local IDs; Git uses immutable origin/key pairs. Only the
//! projection boundary translates them. Execution bindings are never rewritten.
use super::*;
use std::collections::HashMap;

type Identity = (String, String);
type Aliases = HashMap<(SharedTable, String), Identity>;

fn reference(aliases: &Aliases, origin: &str, table: SharedTable, value: &mut Value) {
    let Some(id) = value.as_str() else { return };
    if let Some((target_origin, key)) = aliases.get(&(table, id.to_owned())) {
        *value = json!(if target_origin == origin {
            key.clone()
        } else {
            shared_entity_id(target_origin, table, key)
        });
    }
}

fn field(aliases: &Aliases, origin: &str, table: SharedTable, data: &mut Value, name: &str) {
    if let Some(value) = data.get_mut(name) {
        reference(aliases, origin, table, value);
    }
}

fn task(aliases: &Aliases, origin: &str, data: &mut Value) {
    field(aliases, origin, SharedTable::AgentProfiles, data, "agentId");
    if let Some(dependencies) = data["dependencies"].as_array_mut() {
        for value in dependencies {
            reference(aliases, origin, SharedTable::TaskDefinitions, value);
        }
    }
}

pub(super) async fn rows(
    tx: &mut Transaction<'_, Sqlite>,
    node: &str,
    raw: Vec<SharedRow>,
) -> Result<Vec<SharedRow>, String> {
    let mut aliases = Aliases::new();
    let mut projected = HashMap::new();
    for row in sqlx::query(
        "SELECT origin_node_id,table_name,entity_key,payload FROM fleet_shared_projection",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(err)?
    {
        let origin: String = row.try_get("origin_node_id").map_err(err)?;
        let table = table_from_name(row.try_get::<&str, _>("table_name").map_err(err)?)?;
        let key: String = row.try_get("entity_key").map_err(err)?;
        aliases.insert(
            (table, shared_entity_id(&origin, table, &key)),
            (origin.clone(), key.clone()),
        );
        if origin == node {
            aliases.insert((table, key.clone()), (origin.clone(), key.clone()));
        }
        let payload: String = row.try_get("payload").map_err(err)?;
        projected.insert(
            (origin.clone(), table, key.clone()),
            SharedRow {
                origin_node_id: Some(origin),
                table,
                key,
                data: parse_json(&payload)?,
            },
        );
    }
    for row in sqlx::query(
        "SELECT origin_node_id,table_name,entity_key,local_id FROM fleet_shared_identity",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(err)?
    {
        aliases.insert(
            (
                table_from_name(row.try_get::<&str, _>("table_name").map_err(err)?)?,
                row.try_get("local_id").map_err(err)?,
            ),
            (
                row.try_get("origin_node_id").map_err(err)?,
                row.try_get("entity_key").map_err(err)?,
            ),
        );
    }
    for row in &raw {
        if matches!(
            row.table,
            SharedTable::AgentProfiles
                | SharedTable::ProfileDrafts
                | SharedTable::ProfileReviewReceipts
                | SharedTable::MemorySources
                | SharedTable::MemoryRecords
                | SharedTable::LearnedSkills
                | SharedTable::TaskDefinitions
                | SharedTable::WorkflowDefinitions
        ) {
            aliases
                .entry((row.table, row.key.clone()))
                .or_insert_with(|| (node.into(), row.key.clone()));
        }
    }
    let mut observed = HashSet::new();
    let tombstones = sqlx::query_as::<_, (String, String, String)>(
        "SELECT origin_node_id,table_name,entity_key FROM fleet_shared_tombstones",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(err)?
    .into_iter()
    .map(|(origin, table, key)| Ok((origin, table_from_name(&table)?, key)))
    .collect::<Result<HashSet<_>, String>>()?;
    for mut row in raw {
        let (origin, key) = match row.table {
            SharedTable::ProfileRevisions => {
                let parent = row.data["agentId"]
                    .as_str()
                    .ok_or("Missing revision agent")?;
                let (origin, key) = aliases
                    .get(&(SharedTable::AgentProfiles, parent.into()))
                    .cloned()
                    .ok_or("Unknown revision agent")?;
                (
                    origin,
                    format!(
                        "{key}@{}",
                        row.data["revision"].as_i64().ok_or("Missing revision")?
                    ),
                )
            }
            SharedTable::MemorySourceRevisions => {
                let parent = row.data["sourceId"]
                    .as_str()
                    .ok_or("Missing source revision")?;
                let (origin, key) = aliases
                    .get(&(SharedTable::MemorySources, parent.into()))
                    .cloned()
                    .ok_or("Unknown revision source")?;
                (
                    origin,
                    format!(
                        "{key}@{}",
                        row.data["revision"]
                            .as_i64()
                            .ok_or("Missing source revision")?
                    ),
                )
            }
            SharedTable::MemoryRecordSources | SharedTable::SkillSources => {
                let (table, name) = if row.table == SharedTable::MemoryRecordSources {
                    (SharedTable::MemoryRecords, "memoryId")
                } else {
                    (SharedTable::LearnedSkills, "skillId")
                };
                let parent = row.data[name].as_str().ok_or("Missing evidence parent")?;
                let (origin, key) = aliases
                    .get(&(table, parent.into()))
                    .cloned()
                    .ok_or("Unknown evidence parent")?;
                field(
                    &aliases,
                    &origin,
                    SharedTable::MemorySources,
                    &mut row.data,
                    "sourceId",
                );
                (
                    origin,
                    format!(
                        "{key}@{}",
                        row.data["sourceId"]
                            .as_str()
                            .ok_or("Missing evidence source")?
                    ),
                )
            }
            _ => aliases
                .get(&(row.table, row.key.clone()))
                .cloned()
                .ok_or("Missing shared identity")?,
        };
        row.origin_node_id = Some(origin.clone());
        row.key = key;
        match row.table {
            SharedTable::AgentProfiles
            | SharedTable::MemoryRecords
            | SharedTable::LearnedSkills
            | SharedTable::ProfileDrafts
            | SharedTable::TaskDefinitions
            | SharedTable::WorkflowDefinitions => row.data["id"] = json!(row.key),
            SharedTable::MemorySources => row.data["sourceId"] = json!(row.key),
            SharedTable::ProfileReviewReceipts => row.data["operationId"] = json!(row.key),
            _ => (),
        }
        match row.table {
            SharedTable::ProfileRevisions => {
                field(
                    &aliases,
                    &origin,
                    SharedTable::AgentProfiles,
                    &mut row.data,
                    "agentId",
                );
                field(
                    &aliases,
                    &origin,
                    SharedTable::ProfileDrafts,
                    &mut row.data,
                    "draftId",
                );
            }
            SharedTable::ProfileDrafts => field(
                &aliases,
                &origin,
                SharedTable::AgentProfiles,
                &mut row.data,
                "agentId",
            ),
            SharedTable::ProfileReviewReceipts => {
                if row.data["kind"] == "profile.approve" {
                    field(
                        &aliases,
                        &origin,
                        SharedTable::ProfileDrafts,
                        &mut row.data["result"],
                        "id",
                    );
                    field(
                        &aliases,
                        &origin,
                        SharedTable::AgentProfiles,
                        &mut row.data["result"],
                        "agentId",
                    );
                } else {
                    field(
                        &aliases,
                        &origin,
                        SharedTable::ProfileDrafts,
                        &mut row.data["result"],
                        "draftId",
                    );
                    field(
                        &aliases,
                        &origin,
                        SharedTable::AgentProfiles,
                        &mut row.data["result"]["profile"],
                        "id",
                    );
                }
            }
            SharedTable::MemorySources => {
                field(
                    &aliases,
                    &origin,
                    SharedTable::AgentProfiles,
                    &mut row.data,
                    "agentId",
                );
                field(
                    &aliases,
                    &origin,
                    SharedTable::Projects,
                    &mut row.data,
                    "projectId",
                );
                if origin != node {
                    let prefix = format!("{origin}:");
                    if let Some(thread) = row.data["threadId"]
                        .as_str()
                        .and_then(|s| s.strip_prefix(&prefix))
                    {
                        row.data["threadId"] = json!(thread);
                    }
                }
            }
            SharedTable::MemorySourceRevisions => field(
                &aliases,
                &origin,
                SharedTable::MemorySources,
                &mut row.data,
                "sourceId",
            ),
            SharedTable::MemoryRecords | SharedTable::LearnedSkills => {
                if row.data["scope"] == "agent" {
                    field(
                        &aliases,
                        &origin,
                        SharedTable::AgentProfiles,
                        &mut row.data,
                        "scopeId",
                    );
                }
                if row.data["scope"] == "project" {
                    field(
                        &aliases,
                        &origin,
                        SharedTable::Projects,
                        &mut row.data,
                        "scopeId",
                    );
                }
            }
            SharedTable::MemoryRecordSources => field(
                &aliases,
                &origin,
                SharedTable::MemoryRecords,
                &mut row.data,
                "memoryId",
            ),
            SharedTable::SkillSources => field(
                &aliases,
                &origin,
                SharedTable::LearnedSkills,
                &mut row.data,
                "skillId",
            ),
            SharedTable::TaskDefinitions => task(&aliases, &origin, &mut row.data),
            SharedTable::WorkflowDefinitions => {
                field(
                    &aliases,
                    &origin,
                    SharedTable::AgentProfiles,
                    &mut row.data,
                    "agentId",
                );
                task(&aliases, &origin, &mut row.data["task"]);
                if let Some(nodes) = row.data["nodes"].as_array_mut() {
                    for n in nodes {
                        task(&aliases, &origin, &mut n["task"]);
                    }
                }
            }
            _ => (),
        }
        let identity = (origin, row.table, row.key.clone());
        if tombstones.contains(&identity) {
            continue;
        }
        observed.insert(identity.clone());
        if let Some(old) = projected.get(&identity) {
            // Imported event counters are provenance, never local undo handles.
            if row.data.get("eventId").is_some_and(Value::is_null) {
                row.data["eventId"] = old.data["eventId"].clone();
            }
            // Shared templates cannot rewrite immutable execution snapshots.
            if matches!(
                row.table,
                SharedTable::TaskDefinitions | SharedTable::WorkflowDefinitions
            ) {
                continue;
            }
        }
        projected.insert(identity, row);
    }
    projected.retain(|identity, _| {
        !matches!(
            identity.1,
            SharedTable::MemoryRecordSources | SharedTable::SkillSources
        ) || observed.contains(identity)
    });
    let mut rows = projected.into_values().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        (&a.origin_node_id, a.table as u8, &a.key).cmp(&(&b.origin_node_id, b.table as u8, &b.key))
    });
    Ok(rows)
}
