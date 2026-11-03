//! Durable, explicitly selected context for assigning a chat task.
//!
//! Assignment context is scoped to one idempotent task-create operation. It never
//! reads the source conversation history: only the submitted semantic document,
//! its validated references, and explicitly granted attachments are forwarded.
use crate::agent_host::AgentHost;
use serde_json::{json, Value};

impl AgentHost {
    pub(crate) async fn assign_chat_task(&self, p: Value) -> Result<Value, String> {
        let task = p
            .get("task")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or("task is required")?;
        let operation = match task["operationId"].as_str() {
            Some(id) => {
                let parsed = uuid::Uuid::parse_str(id).map_err(|_| "operationId must be a UUID")?;
                if parsed.to_string() != id.to_ascii_lowercase() {
                    return Err("operationId must be a canonical UUID".into());
                }
                id.to_ascii_lowercase()
            }
            None => uuid::Uuid::new_v4().to_string(),
        };
        let assignment_owner = format!("assignment-{operation}");
        // Serialize ordinary task creation and chat assignment around the shared
        // core operation ID namespace. A duplicate ID must be detected before
        // consulting mutable files or attachment grants.
        let _creation = self.foreground.task_creation.lock().await;
        let core_result: Option<(String, String)> =
            sqlx::query_as("SELECT kind,result FROM operation_results WHERE operation_id=?")
                .bind(&operation)
                .fetch_optional(self.store.pool())
                .await
                .map_err(|e| e.to_string())?;
        let existing_payload: Option<String> =
            sqlx::query_scalar("SELECT payload FROM chat_assignment_context WHERE operation_id=?")
                .bind(&operation)
                .fetch_optional(&self.chat.pool)
                .await
                .map_err(|e| e.to_string())?;
        if let Some((kind, result)) = core_result {
            if kind != "task.create" {
                return Err("operationId was already used for another operation".into());
            }
            let Some(payload) = existing_payload else {
                return Err(
                    "operationId already belongs to a task without a chat assignment".into(),
                );
            };
            let retained: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
            if retained["request"] != p {
                return Err("The task operation already belongs to another assignment".into());
            }
            let result: Value = serde_json::from_str(&result).map_err(|e| e.to_string())?;
            return result
                .get("task")
                .cloned()
                .ok_or("Stored task result is invalid".into());
        }
        if let Some(payload) = existing_payload {
            let retained: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
            if retained["request"] != p {
                return Err("The task operation already belongs to another assignment".into());
            }
            // The context and selected grants committed before task publication.
            // Complete from that immutable snapshot without re-resolving mutable inputs.
            return self
                .create_task_with_workspace(retained["task"].clone())
                .await;
        }

        self.require_account().await?;
        let owner = p["ownerId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("ownerId is required")?;
        let mut document = p.get("document").cloned().ok_or("document is required")?;
        super::input::validate_document(&document)?;
        const TASK_FIELDS: &[&str] = &[
            "agentId",
            "title",
            "instructions",
            "acceptanceCriteria",
            "cwd",
            "projectId",
            "model",
            "effort",
            "dependencies",
            "context",
            "sourceThreadId",
            "operationId",
        ];
        if task
            .as_object()
            .unwrap()
            .keys()
            .any(|key| !TASK_FIELDS.contains(&key.as_str()))
        {
            return Err("Task assignment contains an unsupported field".into());
        }
        let source = task["sourceThreadId"].as_str();
        if source.is_some_and(|thread| thread != owner) {
            return Err("The assignment owner must match the source chat".into());
        }
        let ids = p["attachmentIds"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or("Invalid attachment ID".to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        // Verify the draft owns every selected original before copying any grants.
        self.chat.attachment_paths(owner, &ids).await?;

        // Agent mentions select the assigned agent; they are not workspace context.
        if let Some(refs) = document["references"].as_array_mut() {
            refs.retain(|r| r["type"] != "agent");
        }
        let (runtime_inputs, selected_refs) = if let Some(thread) = source {
            self.selected_chat_inputs(thread, &document).await?
        } else {
            self.project_selected_context(&task, &document).await?
        };
        let selected_context = json!({
            "message": document["text"],
            "quotes": document["quotes"],
            "references": selected_refs,
            "skills": runtime_inputs,
        });

        let mut task_params = task.clone();
        task_params["operationId"] = json!(operation);
        if let Some(src) = source {
            task_params["sourceThreadId"] = json!(src);
        }

        let profile = self
            .store
            .read_profile(
                task_params["agentId"]
                    .as_str()
                    .ok_or("agentId is required")?,
            )
            .await?;
        if task_params["model"].is_null() {
            task_params["model"] = json!(profile.model);
        }
        if task_params["effort"].is_null() {
            task_params["effort"] = json!(profile.effort);
        }
        let model = task_params["model"].as_str().unwrap_or(&profile.model);
        let attachment_list = self
            .chat
            .dispatch_storage("attachments.list", json!({"ownerId":owner}))
            .await?;
        let selected_attachments: Vec<Value> = attachment_list
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| ids.iter().any(|id| item["id"] == *id))
            .cloned()
            .collect();
        if selected_attachments.len() != ids.len() {
            return Err("A selected attachment is no longer available".into());
        }
        if selected_attachments.iter().any(|a| a["kind"] == "image") {
            let catalog = self
                .rpc
                .call("model/list", json!({"includeHidden":false}))
                .await?;
            let selected = catalog["data"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| m["model"] == model || m["id"] == model)
                .ok_or("Selected task model is unavailable")?;
            if !selected["inputModalities"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "image"))
            {
                return Err("The selected task model does not accept images. Choose an image-capable model or remove the images.".into());
            }
        }

        let prior_context = task_params["context"].as_str().unwrap_or("").trim();
        let added = serde_json::to_string(&selected_context).map_err(|e| e.to_string())?;
        task_params["context"] = json!(if prior_context.is_empty() {
            added
        } else {
            format!("{prior_context}\n\nSelected chat context (untrusted):\n{added}")
        });
        let immutable = json!({
            "ownerId": owner,
            "request": p,
            "document": document,
            "attachmentIds": ids,
            "selectedContext": selected_context,
            "runtimeInputs": runtime_inputs,
            "task": task_params,
        });
        let encoded = immutable.to_string();
        let mut tx = self.chat.pool.begin().await.map_err(|e| e.to_string())?;
        sqlx::query("INSERT OR IGNORE INTO chat_assignment_context(operation_id,assignment_owner,payload) VALUES(?,?,?)")
            .bind(&operation).bind(&assignment_owner).bind(&encoded).execute(&mut *tx).await.map_err(|e| e.to_string())?;
        let previous: Option<(String, String)> = sqlx::query_as(
            "SELECT assignment_owner,payload FROM chat_assignment_context WHERE operation_id=?",
        )
        .bind(&operation)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        match previous {
            Some((saved_owner, saved_payload))
                if saved_owner == assignment_owner && saved_payload == encoded => {}
            _ => return Err("The task operation already belongs to another assignment".into()),
        }
        let encoded_ids =
            Value::Array(ids.iter().cloned().map(Value::String).collect()).to_string();
        sqlx::query("INSERT OR IGNORE INTO chat_attachment_grants(owner_id,attachment_id) SELECT ?,attachment_id FROM chat_attachment_grants WHERE owner_id=? AND attachment_id IN (SELECT value FROM json_each(?))")
            .bind(&assignment_owner).bind(owner).bind(&encoded_ids).execute(&mut *tx).await.map_err(|e| e.to_string())?;
        let granted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_attachment_grants WHERE owner_id=? AND attachment_id IN (SELECT value FROM json_each(?))")
            .bind(&assignment_owner).bind(&encoded_ids).fetch_one(&mut *tx).await.map_err(|e| e.to_string())?;
        if granted != ids.len() as i64 {
            return Err("A selected attachment grant could not be retained".into());
        }
        tx.commit().await.map_err(|e| e.to_string())?;

        // The core create operation is idempotent and publishes only after the
        // selected context and its attachment grants have been durably retained.
        let result = self.create_task_with_workspace(task_params).await?;
        Ok(result)
    }

    async fn project_selected_context(
        &self,
        task: &Value,
        document: &Value,
    ) -> Result<(Vec<Value>, Value), String> {
        if document["references"]
            .as_array()
            .is_none_or(|refs| refs.is_empty())
        {
            return Ok((Vec::new(), json!([])));
        }
        let project = self
            .project_for_start(task["projectId"].as_str(), task["cwd"].as_str())
            .await?
            .ok_or("Select a project or source chat before adding workspace references")?;
        let cwd = project
            .primary_folder
            .clone()
            .or_else(|| project.folders.first().cloned())
            .ok_or("The selected project has no folders")?;
        let roots = project.folders.clone();
        let mut contexts = Vec::new();
        let mut skills = Vec::new();
        let mut catalog = None;
        for reference in document["references"].as_array().into_iter().flatten() {
            let kind = reference["type"]
                .as_str()
                .ok_or("Invalid context reference")?;
            let id = reference["id"]
                .as_str()
                .ok_or("Invalid context reference")?;
            match kind {
                "file" | "folder" => {
                    let path = super::input::workspace_reference(
                        std::path::Path::new(&cwd),
                        &roots,
                        id,
                        kind == "folder",
                    )?;
                    contexts.push(json!({"kind":kind,"path":path}));
                }
                "skill" if id == "plan" => {}
                "skill" => {
                    if catalog.is_none() {
                        catalog = Some(
                            self.rpc
                                .call("skills/list", json!({"cwds":[cwd],"forceReload":false}))
                                .await?,
                        );
                    }
                    let entry = catalog.as_ref().unwrap()["data"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|g| g["skills"].as_array().into_iter().flatten())
                        .find(|s| s["path"] == id && s["enabled"] != false)
                        .ok_or("That selected skill is unavailable or disabled")?;
                    skills.push(json!({"type":"skill","name":entry["name"],"path":entry["path"]}));
                }
                "agent" => {}
                _ => return Err("Unsupported context reference".into()),
            }
        }
        Ok((skills, json!(contexts)))
    }

    /// Add the assignment's bounded context to a task turn. This is the only
    /// persisted source used here; parent chat turns are intentionally excluded.
    pub(crate) async fn prepare_assigned_chat_context(
        &self,
        thread: &str,
        params: &mut Value,
    ) -> Result<(), String> {
        let Ok(binding) = self.store.session_binding(thread).await else {
            return Ok(());
        };
        if binding.kind != "task" {
            return Ok(());
        }
        let run = self
            .store
            .get_run(
                binding
                    .task_run_id
                    .as_deref()
                    .ok_or("Task session has no run")?,
            )
            .await?;
        // The task operation result and assignment context live in separate
        // SQLite databases, so resolve the operation key first, then read the
        // context from ChatStore. Ordinary tasks stay on the existing path.
        let operation: Option<String> = sqlx::query_scalar("SELECT operation_id FROM operation_results WHERE kind='task.create' AND json_extract(result,'$.task.id')=?")
            .bind(&run.task_id).fetch_optional(self.store.pool()).await.map_err(|e| e.to_string())?;
        let Some(operation) = operation else {
            return Ok(());
        };
        let payload: Option<String> =
            sqlx::query_scalar("SELECT payload FROM chat_assignment_context WHERE operation_id=?")
                .bind(&operation)
                .fetch_optional(&self.chat.pool)
                .await
                .map_err(|e| e.to_string())?;
        let Some(payload) = payload else {
            return Ok(());
        };
        let data: Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
        let ids: Vec<String> =
            serde_json::from_value(data["attachmentIds"].clone()).map_err(|e| e.to_string())?;
        let assignment_owner = format!("assignment-{operation}");
        let input = params["input"]
            .as_array_mut()
            .ok_or("Task input is invalid")?;
        for selected in data["runtimeInputs"].as_array().into_iter().flatten() {
            input.push(selected.clone());
        }
        let staged = self
            .chat
            .stage_for_thread(thread, &assignment_owner, &ids)
            .await?;
        let attachments = self
            .chat
            .dispatch_storage("attachments.list", json!({"ownerId":assignment_owner}))
            .await?;
        let mut paths = Vec::new();
        let mut images = Vec::new();
        for attachment in attachments
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| ids.iter().any(|id| a["id"] == *id))
        {
            let id = attachment["id"]
                .as_str()
                .ok_or("Invalid retained attachment")?;
            let path = staged
                .iter()
                .find(|(candidate, _)| candidate == id)
                .map(|(_, p)| p)
                .ok_or("Selected attachment was not staged")?;
            if attachment["kind"] == "image" {
                images.push(path);
            } else {
                paths.push(json!({"name":attachment["name"],"readOnlyPath":path}));
            }
        }
        let image_count = images.len();
        if image_count > 0 {
            let model = params["model"]
                .as_str()
                .or(data["task"]["model"].as_str())
                .ok_or("Task model is missing")?;
            let catalog = self
                .rpc
                .call("model/list", json!({"includeHidden":false}))
                .await?;
            let selected = catalog["data"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| m["model"] == model || m["id"] == model)
                .ok_or("Selected task model is unavailable")?;
            if !selected["inputModalities"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "image"))
            {
                return Err("Task model no longer accepts selected images".into());
            }
            let input = params["input"]
                .as_array_mut()
                .ok_or("Task input is invalid")?;
            for path in images {
                input.push(json!({"type":"localImage","path":path}));
            }
        }
        if !params["additionalContext"].is_object() {
            params["additionalContext"] = json!({});
        }
        params["additionalContext"]["brr_assigned_context"] = json!({"kind":"untrusted","name":"brr_assigned_context","value":data["selectedContext"].to_string()});
        params["additionalContext"]["brr_assigned_attachments"] = json!({"kind":"untrusted","name":"brr_assigned_attachments","value":json!({"files":paths,"imageCount":image_count}).to_string()});
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::Engine;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tokio::process::Command;

    fn assert_runtime_context_names_are_valid(params: &Value) {
        for (key, entry) in params["additionalContext"].as_object().unwrap() {
            let name = entry["name"].as_str().unwrap();
            assert!(
                !name.is_empty()
                    && name.len() <= 64
                    && name.as_bytes()[0].is_ascii_lowercase()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    }),
                "additionalContext.{key} has a name rejected by the runtime: {name:?}"
            );
        }
    }

    async fn fixture() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("runtime.py");
        std::fs::write(
            &script,
            r#"
import json,sys
for line in sys.stdin:
 m=json.loads(line)
 if 'id' not in m: continue
 method=m['method']
 if method=='model/list': result={'data':[{'model':'fixture','inputModalities':['text','image']}]}
 elif method=='account/read': result={'account':{'type':'chatgpt'}}
 elif method=='skills/list': result={'data':[]}
 else: result={}
 print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#,
        )
        .unwrap();
        let mut command = Command::new("python3");
        command.arg(script).current_dir(dir.path());
        let engine = Engine::connect(command, Arc::new(|_| {})).await.unwrap();
        (dir, engine)
    }

    #[tokio::test]
    async fn task_assignment_keeps_only_selected_context_and_retries_by_operation_id() {
        let (directory, engine) = fixture().await;
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::write(project_root.join("notes.txt"), "selected file bytes").unwrap();
        let project = engine
            .agents
            .projects
            .create(crate::projects::ProjectInput {
                name: "Assignment project".into(),
                folders: vec![project_root.to_string_lossy().into_owned()],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let draft = engine
            .agents
            .store
            .propose_profile(
                "assignment-test",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Reader".into(),
                    description: "Reads selected context".into(),
                    instructions: "Use only assigned context.".into(),
                    soul: "Careful".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        engine
            .agents
            .store
            .approve_profile(&draft.id, &draft.hash, "assignment-review")
            .await
            .unwrap();
        let profile = engine
            .agents
            .store
            .apply_profile(&draft.id, &draft.hash, "assignment-create")
            .await
            .unwrap();
        let regular_operation = uuid::Uuid::new_v4().to_string();
        let regular = engine.agents_request("tasks.create", json!({
            "agentId":profile.id,"title":"Original regular task","instructions":"Keep this context.",
            "acceptanceCriteria":["Do not replace this."],"context":"Original core task context.","operationId":regular_operation
        })).await.unwrap();
        let imported = engine.agents.chat.dispatch_storage("attachments.importBytes", json!({
            "ownerId":"draft-1", "files":[
                {"name":"extra.txt","mime":"text/plain","data":"c2VsZWN0ZWQgYXR0YWNobWVudA=="},
                {"name":"selected.png","mime":"image/png","data":"iVBORw0KGgo="},
                {"name":"not-selected.txt","mime":"text/plain","data":"bm90IHNlbGVjdGVk"}
            ]
        })).await.unwrap();
        let attachment = imported[0].clone();
        let image = imported[1].clone();
        let unselected = imported[2].clone();
        let failed_operation = uuid::Uuid::new_v4().to_string();
        let failed_assignment = engine.agents.chat_request("tasks.assign", json!({
            "ownerId":"draft-1","attachmentIds":[attachment["id"]],
            "document":{"version":1,"text":"Selected but invalid task","references":[],"quotes":[]},
            "task":{"agentId":profile.id,"title":"","instructions":"This must fail before task publication.","acceptanceCriteria":["Do not publish."],"operationId":failed_operation}
        })).await;
        assert!(failed_assignment.is_err());
        let core_operation: Option<String> =
            sqlx::query_scalar("SELECT result FROM operation_results WHERE operation_id=?")
                .bind(&failed_operation)
                .fetch_optional(engine.agents.store.pool())
                .await
                .unwrap();
        assert!(
            core_operation.is_none(),
            "failed task validation must leave its operation unpublished"
        );
        let later_regular = engine.agents_request("tasks.create", json!({
            "agentId":profile.id,"title":"A different task","instructions":"Must be rejected by the assignment reservation.",
            "acceptanceCriteria":["Do not reuse the operation."],"operationId":failed_operation
        })).await;
        assert!(
            later_regular.is_err(),
            "a regular task must not consume a failed assignment's reserved ID"
        );
        let original = engine
            .agents
            .chat
            .attachment_paths("draft-1", &[attachment["id"].as_str().unwrap().to_owned()])
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(&original[0]).unwrap(),
            b"selected attachment",
            "the original attachment remains unchanged"
        );
        let collision = engine.agents.chat_request("tasks.assign", json!({
            "ownerId":"draft-1","attachmentIds":[attachment["id"]],
            "document":{"version":1,"text":"Different assignment","references":[],"quotes":[]},
            "task":{"agentId":profile.id,"title":"Collision","instructions":"Must not replace regular task.","acceptanceCriteria":["Reject operation collision."],"operationId":regular_operation}
        })).await;
        assert!(
            collision.is_err(),
            "a regular task operation cannot be reused for an assignment"
        );
        let assignment_row: Option<String> =
            sqlx::query_scalar("SELECT payload FROM chat_assignment_context WHERE operation_id=?")
                .bind(&regular_operation)
                .fetch_optional(&engine.agents.chat.pool)
                .await
                .unwrap();
        assert!(
            assignment_row.is_none(),
            "reject the collision before reserving assignment context"
        );
        assert!(
            engine
                .agents
                .chat
                .attachment_paths(
                    &format!("assignment-{regular_operation}"),
                    &[attachment["id"].as_str().unwrap().to_owned()]
                )
                .await
                .is_err(),
            "collision must not copy attachment grants"
        );
        assert_eq!(
            engine
                .agents
                .store
                .get_task(regular["id"].as_str().unwrap())
                .await
                .unwrap()
                .context
                .as_deref(),
            Some("Original core task context.")
        );
        let operation = uuid::Uuid::new_v4().to_string();
        let request = json!({
            "ownerId":"draft-1", "attachmentIds":[attachment["id"],image["id"]],
            "document":{"version":1,"text":"Inspect notes.txt","references":[{"type":"file","id":"notes.txt","name":"notes.txt","start":8,"end":17}],"quotes":[]},
            "task":{"agentId":profile.id,"title":"Review selected note","instructions":"Summarize the supplied note.","acceptanceCriteria":["Use the selected note."],"projectId":project.id,"operationId":operation}
        });
        let assigned = engine
            .agents
            .chat_request("tasks.assign", request.clone())
            .await
            .unwrap();
        assert_eq!(assigned["status"], "queued");
        assert!(assigned["context"]
            .as_str()
            .unwrap()
            .contains("Inspect notes.txt"));
        assert!(assigned["context"].as_str().unwrap().contains("notes.txt"));
        assert!(!assigned["context"]
            .as_str()
            .unwrap()
            .contains("parent history"));
        let copied = engine
            .agents
            .chat
            .attachment_paths(
                &format!("assignment-{operation}"),
                &[attachment["id"].as_str().unwrap().to_owned()],
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(&copied[0]).unwrap(), b"selected attachment");
        let unselected_copy = engine
            .agents
            .chat
            .attachment_paths(
                &format!("assignment-{operation}"),
                &[unselected["id"].as_str().unwrap().to_owned()],
            )
            .await;
        assert!(
            unselected_copy.is_err(),
            "unselected grants must not be inherited"
        );
        let task_id = assigned["id"].as_str().unwrap();
        let run_id = format!("assignment-run-{operation}");
        sqlx::query("INSERT INTO task_runs(id,task_id,agent_id,worker_id,lease_token,state,created_at) VALUES(?,?,?,?,?,'running',0)")
            .bind(&run_id).bind(task_id).bind(&profile.id).bind("assignment-test-worker").bind("assignment-test-lease")
            .execute(engine.agents.store.pool()).await.unwrap();
        engine
            .agents
            .store
            .bind_session(brr_agent_core::SessionBinding {
                thread_id: "assigned-thread".into(),
                agent_id: Some(profile.id.clone()),
                kind: "task".into(),
                project_id: Some(project.id.clone()),
                cwd: Some(project_root.to_string_lossy().into_owned()),
                task_run_id: Some(run_id),
                learning_eligible: true,
            })
            .await
            .unwrap();
        let mut turn =
            json!({"input":[{"type":"text","text":"Assigned task prompt"}],"additionalContext":{}});
        engine
            .agents
            .prepare_assigned_chat_context("assigned-thread", &mut turn)
            .await
            .unwrap();
        assert_runtime_context_names_are_valid(&turn);
        assert!(turn["additionalContext"]["brr_assigned_context"]["value"]
            .as_str()
            .unwrap()
            .contains("Inspect notes.txt"));
        let staged_image = turn["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "localImage")
            .unwrap()["path"]
            .as_str()
            .unwrap();
        assert!(std::path::Path::new(staged_image).is_file());
        let staged_text = turn["additionalContext"]["brr_assigned_attachments"]["value"]
            .as_str()
            .unwrap();
        assert!(staged_text.contains("extra.txt"));
        assert!(!staged_text.contains("not-selected.txt"));
        let retried = engine
            .agents
            .chat_request("tasks.assign", request.clone())
            .await
            .unwrap();
        assert_eq!(retried["id"], assigned["id"]);
        let mut altered = request;
        altered["task"]["title"] = json!("Changed intent");
        assert!(engine
            .agents
            .chat_request("tasks.assign", altered)
            .await
            .is_err());
        let plain = engine.agents.chat_request("tasks.assign", json!({
            "ownerId":"draft-plain","document":{"version":1,"text":"Do this selected task","references":[],"quotes":[]},
            "task":{"agentId":profile.id,"title":"Plain draft","instructions":"Do the selected request.","acceptanceCriteria":["Complete it."],"operationId":uuid::Uuid::new_v4().to_string()}
        })).await.unwrap();
        assert_eq!(plain["title"], "Plain draft");
        let mut injected = json!({
            "ownerId":"draft-injected","document":{"version":1,"text":"Do this","references":[],"quotes":[]},
            "task":{"agentId":profile.id,"title":"Injected","instructions":"Do this.","acceptanceCriteria":["Complete it."],"workspacePermission":"workspace-write","operationId":uuid::Uuid::new_v4().to_string()}
        });
        assert!(engine
            .agents
            .chat_request("tasks.assign", injected.take())
            .await
            .is_err());
        engine.shutdown().await;
    }
}
