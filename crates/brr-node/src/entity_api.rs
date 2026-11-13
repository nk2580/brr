//! Canonical shared entity IDs at the remote request/response boundary.

use super::*;
use brr_agent_core::{shared_entity_id, SharedTable};
use serde_json::{Map, Value};
use sqlx::Row;

impl Service {
    /// Translate canonical shared IDs to the exact IDs stored on this owner.
    /// Unknown canonical IDs fail closed. Desktop payloads stay byte-for-byte
    /// equivalent because they belong to a separate local bridge contract.
    pub async fn localize_scoped(&self, request: &Request) -> Result<Request, ApiError> {
        if request.channel == Channel::Desktop {
            return Ok(request.clone());
        }
        let mut localized = request.clone();
        let params = &mut localized.params;
        match request.channel {
            Channel::Agents => {
                if let Some(object) = params.as_object_mut() {
                    for (field, table) in [
                        ("agentId", SharedTable::AgentProfiles),
                        ("projectId", SharedTable::Projects),
                    ] {
                        self.localize_field(object, field, table).await?;
                    }
                    match request.method.as_str() {
                        "agents.approve" | "agents.reject" => {
                            self.localize_field(object, "draftId", SharedTable::ProfileDrafts)
                                .await?
                        }
                        "memory.revise" | "skills.revoke" => {
                            let table = if request.method == "memory.revise" {
                                SharedTable::MemoryRecords
                            } else {
                                SharedTable::LearnedSkills
                            };
                            self.localize_field(object, "id", table).await?;
                        }
                        "memory.forget" => {
                            self.localize_either_field(
                                object,
                                "id",
                                &[SharedTable::MemoryRecords, SharedTable::MemorySources],
                            )
                            .await?;
                        }
                        _ => {}
                    }
                    if request.method.starts_with("memory.")
                        || request.method.starts_with("skills.")
                    {
                        self.localize_source_ids(object).await?;
                    }
                    if matches!(request.method.as_str(), "tasks.create" | "workflows.create") {
                        self.localize_task_object(object).await?;
                    }
                }
            }
            Channel::Projects => {
                if let Some(object) = params.as_object_mut() {
                    self.localize_field(object, "projectId", SharedTable::Projects)
                        .await?;
                    if matches!(request.method.as_str(), "save" | "delete") {
                        self.localize_field(object, "id", SharedTable::Projects)
                            .await?;
                    }
                }
            }
            Channel::Sessions => {
                if let Some(object) = params.as_object_mut() {
                    self.localize_field(object, "agentId", SharedTable::AgentProfiles)
                        .await?;
                    self.localize_field(object, "projectId", SharedTable::Projects)
                        .await?;
                }
            }
            _ => {}
        }
        Ok(localized)
    }

    /// Publicize only known Host DTOs, preserving local runtime/thread IDs.
    pub async fn publicize_scoped(
        &self,
        request: &Request,
        response: &mut Value,
    ) -> Result<(), ApiError> {
        match (request.channel, request.method.as_str()) {
            (Channel::Agents, "agents.list" | "agents.read" | "agents.approve") => {
                if request.method == "agents.approve" {
                    if let Some(agent) = response.get_mut("agent") {
                        self.publicize_entity(agent, SharedTable::AgentProfiles)
                            .await?;
                    }
                } else {
                    self.publicize_entity_collection(response, SharedTable::AgentProfiles)
                        .await?;
                }
            }
            (Channel::Agents, "agents.drafts") => {
                self.publicize_entity_collection(response, SharedTable::ProfileDrafts)
                    .await?;
                self.publicize_refs_in_collection(response).await?;
            }
            (Channel::Agents, "memory.list" | "memory.search") => {
                self.publicize_entity_collection(response, SharedTable::MemoryRecords)
                    .await?;
                if let Some(sources) = response.get_mut("sources") {
                    self.publicize_collection(sources, SharedTable::MemorySources)
                        .await?;
                }
            }
            (Channel::Agents, "skills.list") => {
                self.publicize_entity_collection(response, SharedTable::LearnedSkills)
                    .await?;
            }
            (Channel::Agents, "tasks.list" | "tasks.proposals" | "workflows.list") => {
                self.publicize_refs_in_collection(response).await?;
            }
            (Channel::Projects, "list" | "save" | "create") => {
                self.publicize_project_collection(response).await?;
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn publicize_snapshot(&self, snapshot: &mut Snapshot) -> Result<(), ApiError> {
        let mut agents = Value::Array(snapshot.agents.clone());
        self.publicize_collection(&mut agents, SharedTable::AgentProfiles)
            .await?;
        snapshot.agents = agents.as_array().cloned().unwrap_or_default();
        let mut projects = Value::Array(snapshot.projects.clone());
        self.publicize_project_collection(&mut projects).await?;
        snapshot.projects = projects.as_array().cloned().unwrap_or_default();
        let mut tasks = Value::Array(snapshot.tasks.clone());
        self.publicize_refs_in_collection(&mut tasks).await?;
        snapshot.tasks = tasks.as_array().cloned().unwrap_or_default();
        Ok(())
    }

    async fn localize_field(
        &self,
        object: &mut Map<String, Value>,
        field: &str,
        table: SharedTable,
    ) -> Result<(), ApiError> {
        let Some(value) = object.get(field).and_then(Value::as_str).map(str::to_owned) else {
            return Ok(());
        };
        let local = match table {
            SharedTable::Projects => self.localize_project_id(&value).await?,
            _ => self.localize_agent_id(table, &value).await?,
        };
        object.insert(field.into(), Value::String(local));
        Ok(())
    }

    async fn localize_either_field(
        &self,
        object: &mut Map<String, Value>,
        field: &str,
        tables: &[SharedTable],
    ) -> Result<(), ApiError> {
        let Some(value) = object.get(field).and_then(Value::as_str).map(str::to_owned) else {
            return Ok(());
        };
        if !is_canonical_id(&value) {
            return Ok(());
        }
        for table in tables {
            if let Ok(local) = self.localize_agent_id(*table, &value).await {
                object.insert(field.into(), Value::String(local));
                return Ok(());
            }
        }
        Err(unknown_shared_id(&value))
    }

    async fn localize_source_ids(&self, object: &mut Map<String, Value>) -> Result<(), ApiError> {
        if let Some(Value::Array(items)) = object.get_mut("sourceIds") {
            for item in items {
                if let Some(value) = item.as_str().map(str::to_owned) {
                    *item = Value::String(self.localize_source_reference(&value).await?);
                }
            }
        }
        if let Some(Value::String(value)) = object.get("sourceId") {
            let value = value.clone();
            object.insert(
                "sourceId".into(),
                Value::String(self.localize_source_reference(&value).await?),
            );
        }
        Ok(())
    }

    async fn localize_source_reference(&self, value: &str) -> Result<String, ApiError> {
        if let Some((source, revision)) = value
            .rsplit_once('@')
            .filter(|(_, rev)| !rev.is_empty() && rev.bytes().all(|b| b.is_ascii_digit()))
        {
            let local = self
                .localize_agent_id(SharedTable::MemorySources, source)
                .await?;
            return Ok(format!("{local}@{revision}"));
        }
        self.localize_agent_id(SharedTable::MemorySources, value)
            .await
    }

    async fn localize_task_object(&self, object: &mut Map<String, Value>) -> Result<(), ApiError> {
        self.localize_nested_refs(object).await?;
        if let Some(Value::Object(task)) = object.get_mut("task") {
            self.localize_nested_refs(task).await?;
        }
        if let Some(Value::Array(nodes)) = object.get_mut("nodes") {
            for node in nodes {
                if let Some(task) = node.get_mut("task").and_then(Value::as_object_mut) {
                    self.localize_nested_refs(task).await?;
                }
            }
        }
        Ok(())
    }

    async fn localize_nested_refs(&self, object: &mut Map<String, Value>) -> Result<(), ApiError> {
        self.localize_field(object, "agentId", SharedTable::AgentProfiles)
            .await?;
        self.localize_field(object, "projectId", SharedTable::Projects)
            .await
    }

    async fn localize_agent_id(
        &self,
        table: SharedTable,
        canonical: &str,
    ) -> Result<String, ApiError> {
        if !is_canonical_id(canonical) {
            return Ok(canonical.to_owned());
        }
        let store = self.host.agent_store();
        let sql = match table {
            SharedTable::AgentProfiles => "SELECT id FROM agent_profiles",
            SharedTable::ProfileDrafts => "SELECT id FROM profile_drafts",
            SharedTable::MemorySources => "SELECT source_id AS id FROM memory_sources",
            SharedTable::MemoryRecords => "SELECT id FROM memory_records",
            SharedTable::LearnedSkills => "SELECT id FROM learned_skills",
            _ => return Err(unknown_shared_id(canonical)),
        };
        let rows = sqlx::query(sql)
            .fetch_all(store.pool())
            .await
            .map_err(host_error)?;
        for row in rows {
            let local_id: String = row.try_get("id").map_err(host_error)?;
            let identity = store
                .shared_origin(table, &local_id)
                .await
                .map_err(host_error)?;
            let (origin, key) =
                identity.unwrap_or_else(|| (self.node_id().to_owned(), local_id.clone()));
            if shared_entity_id(&origin, table, &key) == canonical {
                return Ok(local_id);
            }
        }
        Err(unknown_shared_id(canonical))
    }

    async fn localize_project_id(&self, canonical: &str) -> Result<String, ApiError> {
        if !is_canonical_id(canonical) {
            return Ok(canonical.to_owned());
        }
        for row in self
            .host
            .project_store()
            .shared_snapshot(self.node_id())
            .await
            .map_err(host_error)?
        {
            let origin = row.origin_node_id.as_deref().unwrap_or(self.node_id());
            if shared_entity_id(origin, SharedTable::Projects, &row.key) == canonical {
                return Ok(if origin == self.node_id() {
                    row.key
                } else {
                    shared_entity_id(origin, SharedTable::Projects, &row.key)
                });
            }
        }
        Err(unknown_shared_id(canonical))
    }

    async fn publicize_entity_collection(
        &self,
        value: &mut Value,
        table: SharedTable,
    ) -> Result<(), ApiError> {
        if let Some(items) = value.get_mut("data").and_then(Value::as_array_mut) {
            for item in items {
                self.publicize_entity(item, table).await?;
            }
        } else {
            self.publicize_collection(value, table).await?;
        }
        Ok(())
    }

    async fn publicize_collection(
        &self,
        value: &mut Value,
        table: SharedTable,
    ) -> Result<(), ApiError> {
        if let Some(items) = value.as_array_mut() {
            for item in items {
                self.publicize_entity(item, table).await?;
            }
        } else if value.is_object() {
            self.publicize_entity(value, table).await?;
        }
        Ok(())
    }

    async fn publicize_project_collection(&self, value: &mut Value) -> Result<(), ApiError> {
        if let Some(items) = value.get_mut("data").and_then(Value::as_array_mut) {
            for item in items {
                self.publicize_project(item).await?;
            }
        } else if let Some(items) = value.as_array_mut() {
            for item in items {
                self.publicize_project(item).await?;
            }
        } else {
            self.publicize_project(value).await?;
        }
        Ok(())
    }

    async fn publicize_project(&self, value: &mut Value) -> Result<(), ApiError> {
        let Some(object) = value.as_object_mut() else {
            return Ok(());
        };
        let Some(local_id) = object.get("id").and_then(Value::as_str).map(str::to_owned) else {
            return Ok(());
        };
        if let Some((origin, key)) = self.project_identity_for_local_id(&local_id).await? {
            object.insert(
                "id".into(),
                Value::String(shared_entity_id(&origin, SharedTable::Projects, &key)),
            );
            object.insert("originNodeId".into(), Value::String(origin));
        }
        Ok(())
    }

    async fn publicize_entity(
        &self,
        value: &mut Value,
        table: SharedTable,
    ) -> Result<(), ApiError> {
        let Some(object) = value.as_object_mut() else {
            return Ok(());
        };
        let id_field = if table == SharedTable::MemorySources {
            "sourceId"
        } else {
            "id"
        };
        let Some(input_id) = object
            .get(id_field)
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(());
        };
        let local_id = if table != SharedTable::Projects && is_canonical_id(&input_id) {
            self.localize_agent_id(table, &input_id).await?
        } else {
            input_id
        };
        let (origin, key) = if table == SharedTable::Projects {
            match self.project_identity_for_local_id(&local_id).await? {
                Some((origin, key)) => (origin, key),
                None => return Ok(()),
            }
        } else {
            let store = self.host.agent_store();
            match store
                .shared_origin(table, &local_id)
                .await
                .map_err(host_error)?
            {
                Some((origin, key)) => (origin, key),
                None => (self.node_id().to_owned(), local_id.clone()),
            }
        };
        object.insert(
            id_field.into(),
            Value::String(shared_entity_id(&origin, table, &key)),
        );
        object.insert("originNodeId".into(), Value::String(origin));
        if matches!(
            table,
            SharedTable::MemoryRecords | SharedTable::LearnedSkills
        ) {
            if let Some(scope) = object.get("scope").and_then(Value::as_str) {
                let scope_table = match scope {
                    "agent" => Some(SharedTable::AgentProfiles),
                    "project" => Some(SharedTable::Projects),
                    _ => None,
                };
                if let (Some(scope_table), Some(scope_id)) = (
                    scope_table,
                    object
                        .get("scopeId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                ) {
                    let public_id = self.publicize_ref(scope_table, &scope_id).await;
                    object.insert("scopeId".into(), Value::String(public_id));
                }
            }
            if let Some(Value::Array(source_ids)) = object.get_mut("sourceIds") {
                for source_id in source_ids {
                    if let Some(value) = source_id.as_str().map(str::to_owned) {
                        *source_id = Value::String(self.publicize_source_reference(&value).await?);
                    }
                }
            }
        }
        if let Some(agent_id) = object
            .get("agentId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            object.insert(
                "agentId".into(),
                Value::String(
                    self.publicize_ref(SharedTable::AgentProfiles, &agent_id)
                        .await,
                ),
            );
        }
        if let Some(project_id) = object
            .get("projectId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            object.insert(
                "projectId".into(),
                Value::String(self.publicize_ref(SharedTable::Projects, &project_id).await),
            );
        }
        Ok(())
    }

    async fn publicize_refs_in_collection(&self, value: &mut Value) -> Result<(), ApiError> {
        if let Some(items) = value.get_mut("data").and_then(Value::as_array_mut) {
            for item in items {
                self.publicize_refs_in_object(item).await?;
            }
        } else if let Some(items) = value.as_array_mut() {
            for item in items {
                self.publicize_refs_in_object(item).await?;
            }
        } else {
            self.publicize_refs_in_object(value).await?;
        }
        Ok(())
    }

    async fn publicize_refs_in_object(&self, value: &mut Value) -> Result<(), ApiError> {
        self.publicize_direct_refs(value).await?;
        let Some(object) = value.as_object_mut() else {
            return Ok(());
        };
        // Workflows can carry a typed task graph. Inspect only those known DTO
        // fields; arbitrary context, approval, and tool JSON is left intact.
        if let Some(Value::String(task_json)) = object.get("task") {
            if let Ok(mut task) = serde_json::from_str::<Value>(task_json) {
                self.publicize_direct_refs(&mut task).await?;
                object.insert(
                    "task".into(),
                    Value::String(serde_json::to_string(&task).map_err(host_error)?),
                );
            }
        }
        if let Some(Value::Object(task)) = object.get_mut("task") {
            let mut nested = Value::Object(std::mem::take(task));
            self.publicize_direct_refs(&mut nested).await?;
            if let Value::Object(updated) = nested {
                *task = updated;
            }
        }
        if let Some(Value::Array(nodes)) = object.get_mut("nodes") {
            for node in nodes {
                if let Some(task) = node.get_mut("task") {
                    if task.is_object() {
                        self.publicize_direct_refs(task).await?;
                    } else if let Some(text) = task.as_str() {
                        if let Ok(mut nested) = serde_json::from_str::<Value>(text) {
                            self.publicize_direct_refs(&mut nested).await?;
                            *task =
                                Value::String(serde_json::to_string(&nested).map_err(host_error)?);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn publicize_direct_refs(&self, value: &mut Value) -> Result<(), ApiError> {
        let Some(object) = value.as_object_mut() else {
            return Ok(());
        };
        if let Some(agent_id) = object
            .get("agentId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            object.insert(
                "agentId".into(),
                Value::String(
                    self.publicize_ref(SharedTable::AgentProfiles, &agent_id)
                        .await,
                ),
            );
        }
        if let Some(project_id) = object
            .get("projectId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            object.insert(
                "projectId".into(),
                Value::String(self.publicize_ref(SharedTable::Projects, &project_id).await),
            );
        }
        Ok(())
    }

    async fn publicize_ref(&self, table: SharedTable, local_id: &str) -> String {
        if table == SharedTable::Projects {
            if let Ok(Some((origin, key))) = self.project_identity_for_local_id(local_id).await {
                return shared_entity_id(&origin, table, &key);
            }
            return local_id.to_owned();
        }
        let store = self.host.agent_store();
        let resolved = if is_canonical_id(local_id) {
            match self.localize_agent_id(table, local_id).await {
                Ok(local) => local,
                Err(_) => return local_id.to_owned(),
            }
        } else {
            local_id.to_owned()
        };
        match store.shared_origin(table, &resolved).await {
            Ok(Some((origin, key))) => shared_entity_id(&origin, table, &key),
            _ => shared_entity_id(self.node_id(), table, &resolved),
        }
    }

    async fn publicize_source_reference(&self, value: &str) -> Result<String, ApiError> {
        if let Some((source, revision)) = value
            .rsplit_once('@')
            .filter(|(_, rev)| !rev.is_empty() && rev.bytes().all(|b| b.is_ascii_digit()))
        {
            return Ok(format!(
                "{}@{revision}",
                self.publicize_ref(SharedTable::MemorySources, source).await
            ));
        }
        Ok(self.publicize_ref(SharedTable::MemorySources, value).await)
    }

    async fn project_identity_for_local_id(
        &self,
        local_id: &str,
    ) -> Result<Option<(String, String)>, ApiError> {
        for row in self
            .host
            .project_store()
            .shared_snapshot(self.node_id())
            .await
            .map_err(host_error)?
        {
            let origin = row
                .origin_node_id
                .as_deref()
                .unwrap_or(self.node_id())
                .to_owned();
            let key = row.key.clone();
            let resolved_local = if origin == self.node_id() {
                key.clone()
            } else {
                shared_entity_id(&origin, SharedTable::Projects, &key)
            };
            if resolved_local == local_id
                || shared_entity_id(&origin, SharedTable::Projects, &key) == local_id
            {
                return Ok(Some((origin, key)));
            }
        }
        Ok(None)
    }
}

fn is_canonical_id(value: &str) -> bool {
    let Some(digest) = value.strip_prefix("fleet_") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unknown_shared_id(id: &str) -> ApiError {
    ApiError::new(
        "unknown_shared_entity",
        format!("Shared entity {id} is not present on the selected owner"),
    )
}
