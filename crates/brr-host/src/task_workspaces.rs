//! Native capture of a foreground chat's workspace for a separately assigned task.
use crate::{agent_host::AgentHost, projects::ProjectRecord};
use brr_agent_core::TaskInput;
use serde_json::{json, Value};

impl AgentHost {
    pub(crate) async fn create_task_with_workspace(&self, params: Value) -> Result<Value, String> {
        let mut input: TaskInput =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        let Some(source_id) = params["sourceThreadId"].as_str() else {
            self.validate_task(&mut input).await?;
            let task = if let Some(operation) = params["operationId"].as_str() {
                self.reserve_task_workspace(operation, "", "null").await?;
                self.store.create_task_once(input, operation).await?
            } else {
                self.store.create_task(input).await?
            };
            return Ok(json!(task));
        };
        let binding = self.store.session_binding(source_id).await?;
        if !matches!(binding.kind.as_str(), "baseline" | "agent") {
            return Err("Assign follow-up work from an ordinary or named-agent chat".into());
        }
        let workspace = self
            .worktrees
            .get_session(source_id)?
            .ok_or("The source chat has no saved workspace")?;
        self.worktrees.validate_session(&workspace).await?;
        if input.project_id.is_some() && input.project_id != workspace.scope_project_id {
            return Err(
                "The task project does not match the source chat's saved execution scope".into(),
            );
        }
        let primary = workspace
            .folders
            .iter()
            .find(|folder| folder.path == workspace.cwd)
            .or_else(|| workspace.folders.first())
            .map(|folder| folder.source.clone());
        let project = ProjectRecord {
            id: workspace.scope_project_id.clone().unwrap_or_default(),
            name: "Saved chat workspace".into(),
            folders: workspace
                .folders
                .iter()
                .map(|folder| folder.source.clone())
                .collect(),
            primary_folder: primary.clone(),
            revision: workspace.project_revision.unwrap_or(0),
            model: None,
            effort: None,
            permission: None,
            hidden: false,
        };
        // Validate task content and paths without resolving the mutable project registry again.
        input.project_id = None;
        input.cwd = primary;
        self.validate_task(&mut input).await?;
        input.project_id = workspace.scope_project_id;
        let operation = params["operationId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let encoded = serde_json::to_string(&project).map_err(|e| e.to_string())?;
        self.reserve_task_workspace(&operation, source_id, &encoded)
            .await?;
        // The core commits the task and operation result together. Workers can therefore
        // discover this previously captured workspace as soon as the task becomes visible.
        Ok(json!(self.store.create_task_once(input, &operation).await?))
    }

    /// Reserve source identity before publishing a task. Both source-bound and
    /// source-less requests reserve it, so concurrent retries cannot change roots.
    async fn reserve_task_workspace(
        &self,
        operation: &str,
        source: &str,
        encoded: &str,
    ) -> Result<(), String> {
        sqlx::query("INSERT OR IGNORE INTO host_task_source_workspaces(operation_id,source_thread_id,workspace_project) SELECT ?,?,? WHERE NOT EXISTS(SELECT 1 FROM operation_results WHERE operation_id=?)")
            .bind(operation).bind(source).bind(encoded).bind(operation)
            .execute(self.store.pool()).await.map_err(|e| e.to_string())?;
        let stored: Option<(String, String)> = sqlx::query_as("SELECT source_thread_id,workspace_project FROM host_task_source_workspaces WHERE operation_id=?")
            .bind(operation).fetch_optional(self.store.pool()).await.map_err(|e| e.to_string())?;
        match stored {
            Some(stored) if stored == (source.to_owned(), encoded.to_owned()) => Ok(()),
            // Existing source-less operations may be retried, but can never acquire
            // a source association after their task has already been committed.
            None if source.is_empty() => Ok(()),
            _ => Err("The task operation already belongs to another source workspace".into()),
        }
    }

    pub(crate) async fn task_workspace_project(
        &self,
        task_id: &str,
    ) -> Result<Option<ProjectRecord>, String> {
        let encoded: Option<String> = sqlx::query_scalar("SELECT w.workspace_project FROM host_task_source_workspaces w JOIN operation_results o ON o.operation_id=w.operation_id WHERE w.source_thread_id<>'' AND o.kind='task.create' AND json_extract(o.result,'$.task.id')=?")
            .bind(task_id).fetch_optional(self.store.pool()).await.map_err(|e| e.to_string())?;
        encoded
            .map(|value| serde_json::from_str(&value).map_err(|e| e.to_string()))
            .transpose()
    }
}
