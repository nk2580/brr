//! Domain validation for portable shared rows before they become Git content.

use crate::{ProfileCandidate, SharedRow, SharedSnapshot, SharedTable, digest, shared_entity_id};
use serde_json::{Map, Value};
use std::collections::HashSet;

const PROJECT_EFFORTS: &[&str] = &["low", "minimal", "medium", "high", "xhigh", "none"];

#[cfg(test)]
pub(crate) fn validate(snapshot: &SharedSnapshot) -> Result<(), String> {
    validate_with_host_projects(snapshot, true)
}

pub(crate) fn validate_with_host_projects(
    snapshot: &SharedSnapshot,
    require_projects: bool,
) -> Result<(), String> {
    if snapshot.schema_version != 1 {
        return Err("unsupported shared snapshot schema version".into());
    }
    let mut identities = HashSet::new();
    for row in &snapshot.rows {
        if row.key.trim().is_empty() {
            return Err("shared row key is empty".into());
        }
        if row
            .origin_node_id
            .as_deref()
            .is_some_and(|origin| origin.trim().is_empty())
        {
            return Err("shared row origin node ID is empty".into());
        }
        let origin = row.origin_node_id.as_deref().unwrap_or("");
        if !identities.insert((origin, row.table, row.key.as_str())) {
            return Err(format!("duplicate shared row identity: {}", row.key));
        }
        let label = format!("{:?}", row.table);
        let data = object(&row.data, &label)?;
        exact_keys(data, fields(row.table), &label)?;
        validate_row(snapshot, row)?;
    }
    validate_relations(snapshot, require_projects)
}

fn fields(table: SharedTable) -> &'static [&'static str] {
    use SharedTable::*;
    match table {
        AgentProfiles => &["id", "revision", "candidate", "createdAt", "updatedAt"],
        ProfileRevisions => &[
            "agentId",
            "revision",
            "candidate",
            "createdAt",
            "updatedAt",
            "draftId",
            "hash",
        ],
        ProfileDrafts => &[
            "id",
            "threadId",
            "agentId",
            "candidate",
            "hash",
            "status",
            "baseRevision",
            "createdAt",
        ],
        ProfileReviewReceipts => &["operationId", "kind", "result"],
        MemorySources => &[
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
        MemorySourceRevisions => &["sourceId", "revision", "content", "updatedAt", "eventId"],
        MemoryRecords => &[
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
        MemoryRecordSources => &["memoryId", "sourceId", "sourceRevision"],
        LearnedSkills => &[
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
        SkillSources => &["skillId", "sourceId", "sourceRevision"],
        TaskDefinitions => &[
            "id",
            "agentId",
            "title",
            "instructions",
            "acceptanceCriteria",
            "dependencies",
            "createdAt",
        ],
        WorkflowDefinitions => &[
            "id",
            "name",
            "agentId",
            "task",
            "nodes",
            "expression",
            "timezone",
        ],
        Projects => &[
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

fn validate_row(snapshot: &SharedSnapshot, row: &SharedRow) -> Result<(), String> {
    use SharedTable::*;
    let data = &row.data;
    match row.table {
        AgentProfiles => {
            self_key(row, "id")?;
            safe_filesystem_id(&row.key)?;
            positive(data, "revision")?;
            timestamp(data, "createdAt")?;
            timestamp(data, "updatedAt")?;
            candidate(&data["candidate"])?;
        }
        ProfileDrafts => {
            self_key(row, "id")?;
            required_text(data, "threadId", 256)?;
            optional_text(data, "agentId", 256)?;
            candidate_hash(&data["candidate"], &required_text(data, "hash", 64)?)?;
            if !matches!(
                data["status"].as_str(),
                Some("proposed" | "approved" | "rejected" | "applied")
            ) {
                return Err("invalid profile draft status".into());
            }
            if !data["baseRevision"].is_null() {
                positive(data, "baseRevision")?;
            }
            timestamp(data, "createdAt")?;
            if let Some(agent_id) = data["agentId"].as_str() {
                resolve(snapshot, row_origin(row), AgentProfiles, agent_id)?;
            }
        }
        ProfileRevisions => {
            positive(data, "revision")?;
            timestamp(data, "createdAt")?;
            timestamp(data, "updatedAt")?;
            required_text(data, "hash", 64)?;
            required_text(data, "draftId", 256)?;
            let agent = required_text(data, "agentId", 256)?;
            if row.key != format!("{agent}@{}", data["revision"].as_i64().unwrap_or_default()) {
                return Err("profile revision key does not match its identity".into());
            }
            candidate_hash(&data["candidate"], &required_text(data, "hash", 64)?)?;
        }
        ProfileReviewReceipts => {
            self_key(row, "operationId")?;
            let kind = required_text(data, "kind", 64)?;
            if !matches!(kind.as_str(), "profile.approve" | "profile.apply") {
                return Err("invalid profile review receipt kind".into());
            }
            validate_review_receipt(&data["result"], &kind)?;
        }
        MemorySources => {
            self_key(row, "sourceId")?;
            positive(data, "revision")?;
            required_text(data, "threadId", 512)?;
            optional_text(data, "agentId", 256)?;
            optional_text(data, "projectId", 256)?;
            required_text(data, "content", 1_000_000)?;
            boolean(data, "active")?;
            timestamp(data, "updatedAt")?;
            optional_event(data, "eventId")?;
        }
        MemorySourceRevisions => {
            positive(data, "revision")?;
            required_text(data, "sourceId", 256)?;
            required_text(data, "content", 1_000_000)?;
            timestamp(data, "updatedAt")?;
            optional_event(data, "eventId")?;
            if row.key
                != format!(
                    "{}@{}",
                    data["sourceId"].as_str().unwrap_or_default(),
                    data["revision"].as_i64().unwrap_or_default()
                )
            {
                return Err("memory source revision key does not match its identity".into());
            }
        }
        MemoryRecords | LearnedSkills => {
            self_key(row, "id")?;
            if row.table == LearnedSkills {
                safe_filesystem_id(&row.key)?;
            }
            positive(data, "revision")?;
            validate_scope(data)?;
            boolean(data, "active")?;
            timestamp(data, "createdAt")?;
            timestamp(data, "updatedAt")?;
            optional_event(data, "eventId")?;
            if row.table == MemoryRecords {
                required_text(data, "kind", 128)?;
                required_text(data, "content", 1_000_000)?;
            } else {
                required_text(data, "name", 256)?;
                required_text(data, "description", 4096)?;
                required_text(data, "content", 1_000_000)?;
            }
        }
        MemoryRecordSources | SkillSources => {
            let parent_field = if row.table == MemoryRecordSources {
                "memoryId"
            } else {
                "skillId"
            };
            let parent_table = if row.table == MemoryRecordSources {
                MemoryRecords
            } else {
                LearnedSkills
            };
            let parent = required_text(data, parent_field, 512)?;
            let source = required_text(data, "sourceId", 512)?;
            positive(data, "sourceRevision")?;
            let parent_row = resolve(snapshot, row_origin(row), parent_table, &parent)?;
            let source_row = resolve(snapshot, row_origin(row), MemorySources, &source)?;
            let parent_key = canonical_reference(row_origin(row), parent_row);
            let source_key = canonical_reference(row_origin(row), source_row);
            if row.key != format!("{parent_key}@{source_key}") {
                return Err("memory evidence link key does not match its references".into());
            }
        }
        TaskDefinitions => {
            self_key(row, "id")?;
            required_text(data, "agentId", 256)?;
            required_text(data, "title", 256)?;
            required_text(data, "instructions", 1_000_000)?;
            string_array(data, "acceptanceCriteria", 1000)?;
            string_array(data, "dependencies", 1000)?;
            timestamp(data, "createdAt")?;
        }
        WorkflowDefinitions => {
            self_key(row, "id")?;
            required_text(data, "name", 256)?;
            required_text(data, "agentId", 256)?;
            required_text(data, "expression", 512)?;
            required_text(data, "timezone", 256)?;
            validate_task_input(&data["task"], false)?;
            validate_workflow_nodes(&data["nodes"])?;
        }
        Projects => {
            self_key(row, "id")?;
            let name = required_text(data, "name", 256)?;
            if name.trim().is_empty() {
                return Err("shared project name is empty".into());
            }
            positive(data, "revision")?;
            optional_bounded_text(data, "model", 256)?;
            if let Some(effort) = optional_bounded_text(data, "effort", 32)? {
                if !PROJECT_EFFORTS.contains(&effort) {
                    return Err("invalid shared project effort".into());
                }
            }
            if let Some(permission) = optional_bounded_text(data, "permission", 32)? {
                if !matches!(permission, "read-only" | "workspace-write") {
                    return Err("invalid shared project permission".into());
                }
            }
            boolean(data, "hidden")?;
        }
    }
    Ok(())
}

fn validate_relations(snapshot: &SharedSnapshot, require_projects: bool) -> Result<(), String> {
    use SharedTable::*;
    for row in &snapshot.rows {
        let origin = row_origin(row);
        let data = &row.data;
        match row.table {
            AgentProfiles => {
                let revision = snapshot
                    .rows
                    .iter()
                    .find(|candidate| {
                        candidate.table == ProfileRevisions
                            && row_origin(candidate) == origin
                            && candidate.data["agentId"] == data["id"]
                            && candidate.data["revision"] == data["revision"]
                    })
                    .ok_or("shared profile has no matching published revision")?;
                if revision.data["candidate"] != data["candidate"] {
                    return Err("shared profile differs from its published revision".into());
                }
            }
            ProfileRevisions => {
                let agent = resolve(
                    snapshot,
                    origin,
                    AgentProfiles,
                    data["agentId"]
                        .as_str()
                        .ok_or("profile revision agent is invalid")?,
                )?;
                let draft = resolve(
                    snapshot,
                    origin,
                    ProfileDrafts,
                    data["draftId"]
                        .as_str()
                        .ok_or("profile revision draft is invalid")?,
                )?;
                if data["candidate"] != draft.data["candidate"]
                    || data["hash"] != draft.data["hash"]
                    || draft.data["status"] != "applied"
                {
                    return Err("profile revision does not match its applied draft".into());
                }
                if let Some(draft_agent) = draft.data["agentId"].as_str() {
                    let owner = resolve(snapshot, origin, AgentProfiles, draft_agent)?;
                    if owner.key != agent.key || row_origin(owner) != row_origin(agent) {
                        return Err("profile revision draft targets another agent".into());
                    }
                }
                let reviewed = snapshot.rows.iter().any(|receipt| {
                    receipt.table == ProfileReviewReceipts
                        && receipt.data["kind"] == "profile.approve"
                        && receipt.data["result"]["hash"] == data["hash"]
                        && receipt.data["result"]["id"]
                            .as_str()
                            .and_then(|id| {
                                resolve(snapshot, row_origin(receipt), ProfileDrafts, id).ok()
                            })
                            .is_some_and(|target| {
                                target.key == draft.key && row_origin(target) == row_origin(draft)
                            })
                });
                if !reviewed {
                    return Err("profile revision has no matching review receipt".into());
                }
            }
            ProfileDrafts => {
                if let Some(agent) = data["agentId"].as_str() {
                    resolve(snapshot, origin, AgentProfiles, agent)?;
                }
            }
            ProfileReviewReceipts => validate_review_references(snapshot, row)?,
            MemorySources => {
                if let Some(agent) = data["agentId"].as_str() {
                    resolve(snapshot, origin, AgentProfiles, agent)?;
                }
                if let Some(project) = data["projectId"].as_str().filter(|_| require_projects) {
                    resolve(snapshot, origin, Projects, project)?;
                }
                let revision = data["revision"]
                    .as_i64()
                    .ok_or("memory source revision is invalid")?;
                let history = snapshot
                    .rows
                    .iter()
                    .find(|history| {
                        history.table == MemorySourceRevisions
                            && row_origin(history) == origin
                            && history.data["sourceId"].as_str().is_some_and(|value| {
                                resolve(snapshot, origin, MemorySources, value).is_ok_and(
                                    |source| source.key == row.key && row_origin(source) == origin,
                                )
                            })
                            && history.data["revision"].as_i64() == Some(revision)
                    })
                    .ok_or("memory source is missing its current revision evidence")?;
                if history.data["content"] != data["content"] {
                    return Err("memory source content differs from its current revision".into());
                }
            }
            MemorySourceRevisions => {
                resolve(
                    snapshot,
                    origin,
                    MemorySources,
                    data["sourceId"]
                        .as_str()
                        .ok_or("memory source revision source is invalid")?,
                )?;
            }
            MemoryRecords | LearnedSkills => {
                let scope = data["scope"].as_str().unwrap_or_default();
                if scope == "agent" {
                    resolve(
                        snapshot,
                        origin,
                        AgentProfiles,
                        data["scopeId"]
                            .as_str()
                            .ok_or("agent memory scope is invalid")?,
                    )?;
                } else if scope == "project" && require_projects {
                    resolve(
                        snapshot,
                        origin,
                        Projects,
                        data["scopeId"]
                            .as_str()
                            .ok_or("project memory scope is invalid")?,
                    )?;
                }
            }
            MemoryRecordSources | SkillSources => {
                let source = resolve(
                    snapshot,
                    origin,
                    MemorySources,
                    data["sourceId"]
                        .as_str()
                        .ok_or("memory evidence source is invalid")?,
                )?;
                let revision = data["sourceRevision"]
                    .as_i64()
                    .ok_or("memory evidence revision is invalid")?;
                let found = snapshot.rows.iter().any(|history| {
                    history.table == MemorySourceRevisions
                        && row_origin(history) == row_origin(source)
                        && history.data["revision"].as_i64() == Some(revision)
                        && history.data["sourceId"].as_str().is_some_and(|reference| {
                            resolve(snapshot, row_origin(source), MemorySources, reference)
                                .is_ok_and(|target| {
                                    target.key == source.key
                                        && row_origin(target) == row_origin(source)
                                })
                        })
                });
                if !found {
                    return Err("memory evidence references a missing source revision".into());
                }
            }
            TaskDefinitions => {
                validate_dependencies(snapshot, origin, &data["dependencies"])?;
            }
            WorkflowDefinitions => {
                let agent = data["agentId"]
                    .as_str()
                    .ok_or("workflow agent is invalid")?;
                let task = &data["task"];
                if task["agentId"].as_str() != Some(agent) {
                    return Err("workflow task uses a different agent".into());
                }
                validate_task_references(snapshot, origin, task)?;
                if let Some(nodes) = data["nodes"].as_array() {
                    let keys = nodes
                        .iter()
                        .filter_map(|node| node["key"].as_str())
                        .collect::<HashSet<_>>();
                    for node in nodes {
                        let node_task = &node["task"];
                        if node_task["agentId"].as_str().is_none() {
                            return Err("workflow node agent is invalid".into());
                        }
                        validate_task_references(snapshot, origin, node_task)?;
                        if let Some(dependencies) = node["dependsOn"].as_array() {
                            for dependency in dependencies {
                                let dependency = dependency
                                    .as_str()
                                    .ok_or("workflow node dependency is invalid")?;
                                if !keys.contains(dependency) {
                                    return Err("workflow node dependency is missing".into());
                                }
                            }
                        }
                    }
                }
            }
            Projects => {}
        }
    }
    Ok(())
}

fn validate_task_references(
    snapshot: &SharedSnapshot,
    origin: &str,
    task: &Value,
) -> Result<(), String> {
    if let Some(dependencies) = task["dependencies"].as_array() {
        validate_dependencies(snapshot, origin, &Value::Array(dependencies.clone()))?;
    }
    Ok(())
}

fn validate_dependencies(
    snapshot: &SharedSnapshot,
    origin: &str,
    dependencies: &Value,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for dependency in dependencies
        .as_array()
        .ok_or("task dependencies must be an array")?
    {
        let id = dependency
            .as_str()
            .ok_or("task dependency must be a string")?;
        if id.trim().is_empty() || !seen.insert(id) {
            return Err("task dependency is empty or duplicated".into());
        }
        resolve(snapshot, origin, SharedTable::TaskDefinitions, id)?;
    }
    Ok(())
}

fn validate_task_input(task: &Value, require_acceptance: bool) -> Result<(), String> {
    let object = object(task, "workflow task")?;
    const ALLOWED: &[&str] = &[
        "agentId",
        "title",
        "instructions",
        "acceptanceCriteria",
        "dependencies",
        "model",
        "effort",
    ];
    only_keys(object, ALLOWED, "workflow task")?;
    required_text(task, "agentId", 256)?;
    required_text(task, "title", 256)?;
    required_text(task, "instructions", 1_000_000)?;
    if require_acceptance || object.contains_key("acceptanceCriteria") {
        string_array(task, "acceptanceCriteria", 1000)?;
    }
    if let Some(dependencies) = object.get("dependencies") {
        string_array_value(dependencies, "task dependencies", 1000)?;
    }
    for field in ["model", "effort"] {
        if let Some(value) = object.get(field) {
            if !value.is_null()
                && (value.as_str().is_none_or(str::is_empty)
                    || value.as_str().is_some_and(|s| s.len() > 256))
            {
                return Err(format!("workflow task {field} is invalid"));
            }
        }
    }
    Ok(())
}

fn validate_workflow_nodes(value: &Value) -> Result<(), String> {
    if value.is_null() {
        return Ok(());
    }
    let nodes = value
        .as_array()
        .ok_or("workflow nodes must be an array or null")?;
    let mut keys = HashSet::new();
    for node in nodes {
        let object = object(node, "workflow node")?;
        const ALLOWED: &[&str] = &["key", "task", "dependsOn"];
        only_keys(object, ALLOWED, "workflow node")?;
        if !object.contains_key("key") || !object.contains_key("task") {
            return Err("workflow node is missing required fields".into());
        }
        let key = required_text(node, "key", 128)?;
        if !keys.insert(key) {
            return Err("workflow node keys must be unique".into());
        }
        validate_task_input(&object["task"], false)?;
        if let Some(dependencies) = object.get("dependsOn") {
            string_array_value(dependencies, "workflow node dependencies", 1000)?;
        }
    }
    Ok(())
}

fn validate_scope(data: &Value) -> Result<(), String> {
    let scope = required_text(data, "scope", 32)?;
    if !matches!(scope.as_str(), "agent" | "user" | "project") {
        return Err("invalid shared memory scope".into());
    }
    required_text(data, "scopeId", 512)?;
    Ok(())
}

fn validate_review_receipt(result: &Value, kind: &str) -> Result<(), String> {
    let result_object = object(result, "profile review result")?;
    if kind == "profile.approve" {
        const FIELDS: &[&str] = &[
            "id",
            "threadId",
            "agentId",
            "candidate",
            "hash",
            "status",
            "baseRevision",
        ];
        exact_keys(result_object, FIELDS, "profile approval result")?;
        required_text(result, "id", 256)?;
        required_text(result, "threadId", 256)?;
        optional_text(result, "agentId", 256)?;
        candidate_hash(
            &result_object["candidate"],
            &required_text(result, "hash", 64)?,
        )?;
        if result_object["status"] != "approved" {
            return Err("profile approval receipt has invalid status".into());
        }
        if !result_object["baseRevision"].is_null()
            && result_object["baseRevision"]
                .as_i64()
                .is_none_or(|value| value < 1)
        {
            return Err("profile approval base revision is invalid".into());
        }
    } else {
        const FIELDS: &[&str] = &["draftId", "hash", "profile"];
        exact_keys(result_object, FIELDS, "profile application result")?;
        required_text(result, "draftId", 256)?;
        let hash = required_text(result, "hash", 64)?;
        let profile_value = &result_object["profile"];
        let profile = object(profile_value, "applied profile")?;
        const PROFILE_FIELDS: &[&str] = &[
            "id",
            "revision",
            "name",
            "description",
            "instructions",
            "soul",
            "model",
            "effort",
            "createdAt",
            "updatedAt",
        ];
        exact_keys(profile, PROFILE_FIELDS, "applied profile")?;
        let candidate = json_candidate(profile_value)?;
        candidate_hash(&candidate, &hash)?;
        required_text(profile_value, "id", 256)?;
        positive(profile_value, "revision")?;
        timestamp(profile_value, "createdAt")?;
        timestamp(profile_value, "updatedAt")?;
    }
    Ok(())
}

fn validate_review_references(snapshot: &SharedSnapshot, row: &SharedRow) -> Result<(), String> {
    let origin = row_origin(row);
    let result = &row.data["result"];
    if row.data["kind"] == "profile.approve" {
        let draft = resolve(
            snapshot,
            origin,
            SharedTable::ProfileDrafts,
            result["id"]
                .as_str()
                .ok_or("review receipt draft ID is invalid")?,
        )?;
        if result["hash"] != draft.data["hash"] || result["candidate"] != draft.data["candidate"] {
            return Err("approval receipt differs from its reviewed draft".into());
        }
    } else {
        let draft = resolve(
            snapshot,
            origin,
            SharedTable::ProfileDrafts,
            result["draftId"]
                .as_str()
                .ok_or("application receipt draft ID is invalid")?,
        )?;
        if result["hash"] != draft.data["hash"] {
            return Err("application receipt differs from its reviewed draft".into());
        }
    }
    Ok(())
}

fn json_candidate(profile: &Value) -> Result<Value, String> {
    Ok(serde_json::json!({
        "name": required_text(profile, "name", 256)?,
        "description": required_text(profile, "description", 4096)?,
        "instructions": required_text(profile, "instructions", 1_000_000)?,
        "soul": required_text(profile, "soul", 1_000_000)?,
        "model": required_text(profile, "model", 256)?,
        "effort": required_text(profile, "effort", 128)?,
    }))
}

fn candidate(value: &Value) -> Result<ProfileCandidate, String> {
    let object = object(value, "profile candidate")?;
    const FIELDS: &[&str] = &[
        "name",
        "description",
        "instructions",
        "soul",
        "model",
        "effort",
    ];
    exact_keys(object, FIELDS, "profile candidate")?;
    let candidate: ProfileCandidate = serde_json::from_value(value.clone())
        .map_err(|_| "profile candidate fields have invalid types".to_owned())?;
    if candidate.name.trim().is_empty()
        || candidate.name.len() > 256
        || candidate.description.len() > 4096
        || candidate.instructions.len() > 1_000_000
        || candidate.soul.len() > 1_000_000
        || candidate.model.trim().is_empty()
        || candidate.model.len() > 256
        || candidate.effort.trim().is_empty()
        || candidate.effort.len() > 128
    {
        return Err("profile candidate contains invalid text".into());
    }
    Ok(candidate)
}

fn candidate_hash(value: &Value, expected: &str) -> Result<(), String> {
    let candidate = candidate(value)?;
    if digest(&candidate)? != expected {
        return Err("profile candidate hash does not match its contents".into());
    }
    Ok(())
}

fn resolve<'a>(
    snapshot: &'a SharedSnapshot,
    origin: &str,
    table: SharedTable,
    reference: &str,
) -> Result<&'a SharedRow, String> {
    let mut matches = snapshot.rows.iter().filter(|row| {
        if row.table != table {
            return false;
        }
        let target_origin = row_origin(row);
        (target_origin == origin && row.key == reference)
            || shared_entity_id(target_origin, table, &row.key) == reference
    });
    let row = matches
        .next()
        .ok_or_else(|| format!("missing shared reference to {table:?}"))?;
    if matches.next().is_some() {
        return Err("ambiguous shared reference".into());
    }
    Ok(row)
}

fn canonical_reference(origin: &str, row: &SharedRow) -> String {
    if row_origin(row) == origin {
        row.key.clone()
    } else {
        shared_entity_id(row_origin(row), row.table, &row.key)
    }
}

fn row_origin(row: &SharedRow) -> &str {
    row.origin_node_id.as_deref().unwrap_or("")
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))
}

fn exact_keys(object: &Map<String, Value>, allowed: &[&str], label: &str) -> Result<(), String> {
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || allowed.iter().any(|key| !object.contains_key(*key))
    {
        return Err(format!("{label} contains unknown or missing fields"));
    }
    Ok(())
}

fn only_keys(object: &Map<String, Value>, allowed: &[&str], label: &str) -> Result<(), String> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{label} contains unknown fields"));
    }
    Ok(())
}

fn self_key(row: &SharedRow, field: &str) -> Result<(), String> {
    if row.data[field].as_str() != Some(row.key.as_str()) {
        return Err(format!(
            "shared {:?} identity differs from its key",
            row.table
        ));
    }
    Ok(())
}

fn required_text(data: &Value, field: &str, max: usize) -> Result<String, String> {
    let value = data[field]
        .as_str()
        .ok_or_else(|| format!("shared {field} must be text"))?;
    if value.trim().is_empty() || value.len() > max {
        return Err(format!("shared {field} is empty or too long"));
    }
    Ok(value.to_owned())
}

fn optional_text<'a>(data: &'a Value, field: &str, max: usize) -> Result<Option<&'a str>, String> {
    if data[field].is_null() {
        return Ok(None);
    }
    let value = data[field]
        .as_str()
        .ok_or_else(|| format!("shared {field} must be text or null"))?;
    if value.trim().is_empty() || value.len() > max {
        return Err(format!("shared {field} is empty or too long"));
    }
    Ok(Some(value))
}

fn optional_bounded_text<'a>(
    data: &'a Value,
    field: &str,
    max: usize,
) -> Result<Option<&'a str>, String> {
    if data[field].is_null() {
        return Ok(None);
    }
    let value = data[field]
        .as_str()
        .ok_or_else(|| format!("shared {field} must be text or null"))?;
    if value.trim().is_empty() || value.len() > max {
        return Err(format!("shared {field} is empty or too long"));
    }
    Ok(Some(value))
}

fn positive(data: &Value, field: &str) -> Result<i64, String> {
    let value = data[field]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("shared {field} must be a positive integer"))?;
    Ok(value)
}

fn timestamp(data: &Value, field: &str) -> Result<i64, String> {
    data[field]
        .as_i64()
        .filter(|value| *value >= 0)
        .ok_or_else(|| format!("shared {field} must be a nonnegative integer"))
}

fn optional_event(data: &Value, field: &str) -> Result<(), String> {
    if data[field].is_null() || data[field].as_i64().is_some_and(|value| value > 0) {
        Ok(())
    } else {
        Err(format!("shared {field} must be a positive integer or null"))
    }
}

fn boolean(data: &Value, field: &str) -> Result<bool, String> {
    data[field]
        .as_bool()
        .ok_or_else(|| format!("shared {field} must be boolean"))
}

fn string_array(data: &Value, field: &str, max: usize) -> Result<(), String> {
    string_array_value(&data[field], field, max)
}

fn string_array_value(value: &Value, label: &str, max: usize) -> Result<(), String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array"))?;
    if values.len() > max
        || values
            .iter()
            .any(|value| value.as_str().is_none_or(str::is_empty))
    {
        return Err(format!("{label} contains invalid entries"));
    }
    Ok(())
}

fn safe_filesystem_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("shared identity is not safe for a derived filesystem path".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SharedTable;
    use serde_json::{Value, json};

    fn row(table: SharedTable, key: &str, data: Value) -> SharedRow {
        SharedRow {
            origin_node_id: Some("node-a".into()),
            table,
            key: key.into(),
            data,
        }
    }

    fn snapshot(rows: Vec<SharedRow>) -> SharedSnapshot {
        SharedSnapshot {
            schema_version: 1,
            rows,
        }
    }

    #[test]
    fn project_metadata_rejects_unknown_permissions_and_wrong_scalar_types() {
        let invalid_permission = snapshot(vec![row(
            SharedTable::Projects,
            "project-1",
            json!({"id":"project-1","name":"Research","revision":1,"model":"future-model-x","effort":"high","permission":"allow-all","hidden":false}),
        )]);
        assert!(validate(&invalid_permission).is_err());

        let invalid_hidden = snapshot(vec![row(
            SharedTable::Projects,
            "project-1",
            json!({"id":"project-1","name":"Research","revision":1,"model":null,"effort":null,"permission":null,"hidden":0}),
        )]);
        assert!(validate(&invalid_hidden).is_err());
    }

    #[test]
    fn portable_project_metadata_allows_unrecognized_but_well_formed_models() {
        let valid = snapshot(vec![row(
            SharedTable::Projects,
            "project-1",
            json!({"id":"project-1","name":"Research","revision":1,"model":"future-model-x","effort":"high","permission":"read-only","hidden":true}),
        )]);
        assert!(validate(&valid).is_ok());
    }

    #[test]
    fn profile_candidate_rejects_unknown_fields() {
        let invalid = snapshot(vec![row(
            SharedTable::ProfileDrafts,
            "draft-1",
            json!({
                "id":"draft-1","threadId":"thread-1","agentId":null,
                "candidate":{"name":"Helper","description":"","instructions":"Help","soul":"Be clear","model":"custom-model","effort":"medium","apiKey":"secret"},
                "hash":"not-a-hash","status":"proposed","baseRevision":null,"createdAt":1
            }),
        )]);
        assert!(validate(&invalid).is_err());
    }

    #[test]
    fn portable_task_rejects_nested_execution_context() {
        let invalid = snapshot(vec![row(
            SharedTable::TaskDefinitions,
            "task-1",
            json!({
                "id":"task-1","agentId":"agent-1","title":"Task",
                "instructions":"Do the work","acceptanceCriteria":[],"dependencies":[],
                "createdAt":1,"cwd":"/private/worktree"
            }),
        )]);
        assert!(validate(&invalid).is_err());
    }

    #[test]
    fn workflow_tasks_reject_execution_and_secret_fields_at_nested_depths() {
        let invalid = snapshot(vec![row(
            SharedTable::WorkflowDefinitions,
            "workflow-1",
            json!({
                "id":"workflow-1","name":"Nightly","agentId":"agent-1",
                "task":{"agentId":"agent-1","title":"Run","instructions":"Run safely","acceptanceCriteria":[],"dependencies":[]},
                "nodes":[{"key":"first","task":{"agentId":"agent-1","title":"Run","instructions":"Run safely","apiKey":"secret"},"dependsOn":[]}],
                "expression":"0 0 * * *","timezone":"Australia/Brisbane"
            }),
        )]);
        assert!(validate(&invalid).is_err());
    }

    #[test]
    fn inactive_source_history_remains_valid_for_exact_historical_evidence_links() {
        let valid = snapshot(vec![
            row(
                SharedTable::MemorySources,
                "source:old-thread",
                json!({
                    "sourceId":"source:old-thread","revision":2,"threadId":"deleted-thread",
                    "agentId":null,"projectId":null,"content":"current content","active":false,
                    "updatedAt":3,"eventId":null
                }),
            ),
            row(
                SharedTable::MemorySourceRevisions,
                "source:old-thread@1",
                json!({"sourceId":"source:old-thread","revision":1,"content":"original evidence","updatedAt":1,"eventId":null}),
            ),
            row(
                SharedTable::MemorySourceRevisions,
                "source:old-thread@2",
                json!({"sourceId":"source:old-thread","revision":2,"content":"current content","updatedAt":3,"eventId":null}),
            ),
            row(
                SharedTable::MemoryRecords,
                "memory-1",
                json!({
                    "id":"memory-1","scope":"user","scopeId":"user-1","kind":"fact",
                    "content":"Keep the historical wording","revision":1,"active":true,
                    "createdAt":1,"updatedAt":2,"eventId":null
                }),
            ),
            row(
                SharedTable::MemoryRecordSources,
                "memory-1@source:old-thread",
                json!({"memoryId":"memory-1","sourceId":"source:old-thread","sourceRevision":1}),
            ),
        ]);
        assert!(validate(&valid).is_ok());

        let mut broken = valid.clone();
        broken
            .rows
            .iter_mut()
            .find(|row| row.table == SharedTable::MemoryRecordSources)
            .unwrap()
            .data["sourceRevision"] = json!(7);
        assert!(validate(&broken).is_err());
    }

    #[test]
    fn profile_draft_requires_matching_hash_and_known_status() {
        let candidate = ProfileCandidate {
            name: "Helper".into(),
            description: "Careful assistant".into(),
            instructions: "Cite evidence".into(),
            soul: "Be precise".into(),
            model: "future-model-x".into(),
            effort: "persistent".into(),
        };
        let hash = crate::digest(&candidate).unwrap();
        let draft = row(
            SharedTable::ProfileDrafts,
            "draft-1",
            json!({
                "id":"draft-1","threadId":"thread-1","agentId":null,
                "candidate":candidate,"hash":hash,"status":"proposed",
                "baseRevision":null,"createdAt":1
            }),
        );
        assert!(validate(&snapshot(vec![draft.clone()])).is_ok());

        let mut bad_hash = draft.clone();
        bad_hash.data["hash"] = json!("00");
        assert!(validate(&snapshot(vec![bad_hash])).is_err());
        let mut bad_status = draft;
        bad_status.data["status"] = json!("executed");
        assert!(validate(&snapshot(vec![bad_status])).is_err());
    }

    #[test]
    fn profile_and_skill_ids_cannot_escape_derived_storage_paths() {
        let unsafe_profile = snapshot(vec![row(
            SharedTable::AgentProfiles,
            "../outside",
            json!({
                "id":"../outside","revision":1,
                "candidate":{"name":"Helper","description":"","instructions":"Help","soul":"Be clear","model":"model","effort":"medium"},
                "createdAt":1,"updatedAt":1
            }),
        )]);
        assert!(validate(&unsafe_profile).is_err());

        let unsafe_skill = snapshot(vec![row(
            SharedTable::LearnedSkills,
            "skill/../outside",
            json!({
                "id":"skill/../outside","name":"Skill","description":"d","content":"c",
                "revision":1,"scope":"user","scopeId":"user-1","active":false,
                "createdAt":1,"updatedAt":1,"eventId":null
            }),
        )]);
        assert!(validate(&unsafe_skill).is_err());
    }
}
