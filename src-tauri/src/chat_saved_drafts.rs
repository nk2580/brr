//! Durable drafts for Edit-before-first-turn branches.
//!
//! The runtime intentionally omits empty forks from thread/list. A ready host
//! branch journal is the durable authority for discovering these drafts; this
//! module never materializes history or starts a runtime operation.
use super::*;
use serde_json::Value;

impl AgentHost {
    pub(crate) async fn list_saved_drafts(&self) -> Result<Value, String> {
        let mut drafts = self.chat.dispatch_storage("drafts.list", json!({})).await?;
        let out = drafts.as_array_mut().ok_or("Saved drafts are malformed")?;

        let operations = self
            .chat
            .dispatch_storage("operations.list", json!({}))
            .await?;
        let operations = operations
            .as_array()
            .ok_or("Saved branch operation journal is malformed")?;

        for operation in operations {
            if !is_ready_edit_branch(operation) {
                continue;
            }
            let Some(thread_id) = operation["threadId"].as_str() else {
                continue;
            };
            if !operation["result"]["thread"].is_object() {
                continue;
            }
            let Some(draft) = self
                .chat
                .dispatch_storage("drafts.get", json!({"id":thread_id}))
                .await
                .ok()
            else {
                continue;
            };
            let Ok(binding) = self.store.session_binding(thread_id).await else {
                continue;
            };
            let project_id = self.projects.membership(thread_id).await?;
            let project_id = match project_id {
                Some(id) => self
                    .projects
                    .read(&id)
                    .await?
                    .filter(|project| !project.hidden)
                    .map(|project| project.id),
                None => None,
            };
            if !candidate_matches(operation, &draft, thread_id, &binding)
                || !self
                    .draft_document_is_owned_and_valid(thread_id, &draft["document"])
                    .await?
            {
                continue;
            }
            if self.chat_branch_lineage(thread_id).await?.as_ref()
                != Some(&operation["result"]["thread"]["branchOf"])
            {
                continue;
            }
            // Only confirmed delivery moves this draft out of the branch
            // editor. Failed, cancelled, queued, and uncertain attempts leave
            // the original branch entry reachable; the queue owns retry safety.
            let has_accepted_delivery: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM chat_submissions WHERE thread_id = ? AND status = 'accepted')",
            )
            .bind(thread_id)
            .fetch_one(&self.chat.pool)
            .await
            .map_err(|e| e.to_string())?;
            if has_accepted_delivery {
                continue;
            }

            let mut row = draft;
            row["projectId"] = json!(project_id);
            row["branchEdit"] = json!(true);
            row["branchOperationId"] = operation["id"].clone();
            row["branchThread"] = operation["result"]["thread"].clone();
            row["branchThread"]["projectId"] = json!(project_id);
            out.push(row);
        }
        out.sort_by(|a, b| {
            b["updatedAt"]
                .as_str()
                .unwrap_or_default()
                .cmp(a["updatedAt"].as_str().unwrap_or_default())
        });
        Ok(drafts)
    }

    async fn chat_branch_lineage(&self, thread_id: &str) -> Result<Option<Value>, String> {
        let stored: Option<String> =
            sqlx::query_scalar("SELECT lineage FROM chat_branch_lineage WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&self.chat.pool)
                .await
                .map_err(|e| e.to_string())?;
        stored
            .map(|value| serde_json::from_str(&value).map_err(|e| e.to_string()))
            .transpose()
    }

    async fn draft_document_is_owned_and_valid(
        &self,
        owner_id: &str,
        document: &Value,
    ) -> Result<bool, String> {
        if !valid_draft_document(document) {
            return Ok(false);
        }
        for id in document["attachmentIds"].as_array().into_iter().flatten() {
            let Some(id) = id.as_str() else {
                return Ok(false);
            };
            let granted: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM chat_attachment_grants WHERE owner_id=? AND attachment_id=?)",
            )
            .bind(owner_id)
            .bind(id)
            .fetch_one(&self.chat.pool)
            .await
            .map_err(|e| e.to_string())?;
            if !granted {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) async fn record_branch_archive(
        &self,
        thread_id: &str,
        archived: bool,
    ) -> Result<(), String> {
        let operations = self
            .chat
            .dispatch_storage("operations.list", json!({}))
            .await?;
        let operations = operations
            .as_array()
            .ok_or("Saved branch operation journal is malformed")?;
        for operation in operations.iter().filter(|operation| {
            operation["kind"] == "branch"
                && operation["phase"] == "ready"
                && operation["threadId"] == thread_id
                && operation["result"]["thread"]["id"] == thread_id
        }) {
            if operation["archived"].as_bool() == Some(archived) {
                continue;
            }
            self.chat
                .dispatch_storage(
                    "operations.update",
                    json!({
                        "id":operation["id"],
                        "expectedRevision":operation["revision"],
                        "archived":archived
                    }),
                )
                .await?;
        }
        Ok(())
    }
}

fn is_ready_edit_branch(operation: &Value) -> bool {
    operation["id"]
        .as_str()
        .is_some_and(|id| !id.trim().is_empty())
        && operation["kind"] == "branch"
        && operation["phase"] == "ready"
        && operation["archived"] != true
        && operation["before"] == true
        && operation["workspaceComplete"] == true
        && operation["editingDraft"].is_object()
        && !is_empty_document(&operation["editingDraft"]["document"])
        && operation["editingDraft"]["sourceSubmissionId"]
            .as_str()
            .is_some_and(|id| !id.trim().is_empty())
        && operation["turnId"]
            .as_str()
            .is_some_and(|id| !id.trim().is_empty())
        && operation["sourceThreadId"]
            .as_str()
            .is_some_and(|id| !id.trim().is_empty())
}

fn candidate_matches(
    operation: &Value,
    draft: &Value,
    thread_id: &str,
    binding: &brr_agent_core::SessionBinding,
) -> bool {
    let thread = &operation["result"]["thread"];
    let lineage = &thread["branchOf"];
    let workspace = &operation["workspace"];
    let agent = operation["agentId"].as_str();
    let brr = &thread["brr"];

    operation["threadId"] == thread_id
        && thread["id"] == thread_id
        && operation["result"]["thread"]["workspace"] == *workspace
        && !workspace.is_null()
        && !workspace["cwd"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty()
        && lineage["threadId"] == operation["sourceThreadId"]
        && lineage["turnId"] == operation["turnId"]
        && binding.thread_id == thread_id
        && binding.kind == operation["agentKind"].as_str().unwrap_or_default()
        && binding.agent_id.as_deref() == agent
        && binding.learning_eligible == operation["learningEligible"].as_bool().unwrap_or(true)
        && binding.project_id.as_deref() == workspace["scopeProjectId"].as_str()
        && binding.cwd.as_deref() == workspace["cwd"].as_str()
        && brr["threadId"] == thread_id
        && brr["kind"] == operation["agentKind"]
        && brr["agentId"] == operation["agentId"]
        && brr["learningEligible"] == operation["learningEligible"]
        && brr["projectId"] == workspace["scopeProjectId"]
        && brr["cwd"] == workspace["cwd"]
        && draft["id"] == thread_id
        && draft["threadId"] == thread_id
        && draft["settings"].is_object()
        && draft["agentId"].as_str() == agent
}

fn is_empty_document(value: &Value) -> bool {
    if value.is_null() || value.as_str().is_some_and(|s| s.trim().is_empty()) {
        return true;
    }
    value["text"].as_str().unwrap_or("").trim().is_empty()
        && value["references"].as_array().is_none_or(Vec::is_empty)
        && value["quotes"].as_array().is_none_or(Vec::is_empty)
        && value["attachmentIds"].as_array().is_none_or(Vec::is_empty)
}

fn valid_draft_document(document: &Value) -> bool {
    let Some(object) = document.as_object() else {
        return false;
    };
    if document["version"] != 1 || !document["text"].is_string() {
        return false;
    }
    let text_len = document["text"]
        .as_str()
        .unwrap_or_default()
        .encode_utf16()
        .count() as u64;
    for key in ["references", "quotes"] {
        if !document[key].as_array().is_some() {
            return false;
        }
    }
    for reference in document["references"].as_array().into_iter().flatten() {
        if !reference.is_object()
            || !matches!(
                reference["type"].as_str(),
                Some("agent" | "file" | "folder" | "skill")
            )
            || reference["id"].as_str().is_none()
            || reference["name"].as_str().is_none()
            || !valid_span(reference, text_len)
            || (reference.get("path").is_some() && !reference["path"].is_string())
        {
            return false;
        }
    }
    for quote in document["quotes"].as_array().into_iter().flatten() {
        if !quote.is_object() || quote["text"].as_str().is_none() || !valid_span(quote, text_len) {
            return false;
        }
    }
    if document.get("selection").is_some() {
        let selection = &document["selection"];
        if !selection.is_object()
            || selection["anchor"].as_u64().is_none_or(|at| at > text_len)
            || selection["focus"].as_u64().is_none_or(|at| at > text_len)
        {
            return false;
        }
    }
    if let Some(ids) = document.get("attachmentIds") {
        if !ids
            .as_array()
            .is_some_and(|ids| ids.iter().all(Value::is_string))
        {
            return false;
        }
    }
    object.get("text").is_some()
}

fn valid_span(value: &Value, text_len: u64) -> bool {
    let start = value["start"].as_u64();
    let end = value["end"].as_u64();
    matches!((start, end), (Some(start), Some(end)) if start <= end && end <= text_len)
}
