//! Durable recovery for queued messages whose host-owned runtime thread disappeared.
//! This module only lists runtime metadata and restores drafts; it never resumes, starts,
//! steers, or prepares a runtime turn.
use super::*;
use brr_agent_core::SessionBinding;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashSet;

struct RecoveryContext {
    project_id: Option<String>,
    agent_id: Option<String>,
    folderless_registered_project: bool,
    cwd: std::path::PathBuf,
    roots: Vec<String>,
    workspace: Option<crate::worktrees::SessionWorkspace>,
}

impl AgentHost {
    pub(crate) async fn recovery_messages_request(
        self: &Arc<Self>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match method {
            "recovery.list" => self.list_recovery_messages().await,
            "recovery.restore" => self.restore_recovery_message(params).await,
            "recovery.cancel" => self.cancel_recovery_message(params).await,
            _ => Err("Unknown recovery operation".into()),
        }
    }

    async fn list_recovery_messages(&self) -> Result<Value, String> {
        self.require_account().await?;
        let runtime_ids = runtime_chat_ids(self).await?;
        let bindings = self.store.list_bindings(None).await?;
        let models = self
            .rpc
            .call("model/list", json!({"includeHidden":false}))
            .await?;
        let profiles = self.store.list_profiles().await?;
        let mut items = Vec::new();

        for binding in bindings.into_iter() {
            if !matches!(binding.kind.as_str(), "baseline" | "agent")
                || binding.task_run_id.is_some()
                || runtime_ids.contains(&binding.thread_id)
            {
                continue;
            }
            if runtime_thread_available(self, &binding.thread_id).await? {
                continue;
            }
            let submissions = self
                .chat
                .dispatch_storage("submissions.list", json!({"threadId":binding.thread_id}))
                .await?;
            let entries = submissions
                .as_array()
                .ok_or("Saved chat submissions are malformed")?;
            for entry in entries.iter().filter(|entry| {
                matches!(
                    entry["status"].as_str(),
                    Some("queued" | "failed" | "uncertain" | "dispatching" | "accepted")
                )
            }) {
                let source_id = entry["id"].as_str().ok_or("Submission ID is missing")?;
                if self.recovery_was_restored("submission", source_id).await? {
                    continue;
                }
                let mut item = recovery_submission_item(entry, &binding);
                if matches!(entry["status"].as_str(), Some("uncertain" | "dispatching")) {
                    item["restorable"] = json!(false);
                    item["reason"] = json!("Delivery is unconfirmed. Inspect the conversation before retrying this message.");
                } else if entry["status"] == "accepted" {
                    item["restorable"] = json!(false);
                    item["reason"] = json!("Runtime accepted this message, but the chat is missing from the current inventory. Keep the receipt for review; do not resend it.");
                } else {
                    let owner_id = source_id;
                    let context = self
                        .validate_recovery_source(
                            &binding,
                            None,
                            None,
                            &entry["document"],
                            &entry["settings"],
                            &profiles,
                            &models,
                        )
                        .await;
                    let grants = self
                        .chat
                        .dispatch_storage("attachments.list", json!({"ownerId":owner_id}))
                        .await;
                    set_restorability(&mut item, context, grants, &entry["attachmentIds"]);
                }
                items.push(item);
            }

            let drafts = sqlx::query("SELECT id,thread_id,project_id,agent_id,document,settings,revision,updated_at FROM chat_drafts WHERE thread_id=? ORDER BY updated_at")
                .bind(&binding.thread_id)
                .fetch_all(&self.chat.pool)
                .await
                .map_err(|error| error.to_string())?;
            for row in drafts {
                let id: String = row.try_get("id").map_err(|error| error.to_string())?;
                if self.recovery_was_restored("draft", &id).await? {
                    continue;
                }
                let document = parse_column(&row, "document")?;
                if document["text"].as_str().unwrap_or("").trim().is_empty()
                    && document["references"].as_array().is_none_or(Vec::is_empty)
                    && document["quotes"].as_array().is_none_or(Vec::is_empty)
                    && document["attachmentIds"]
                        .as_array()
                        .is_none_or(Vec::is_empty)
                {
                    continue;
                }
                let settings = parse_column(&row, "settings")?;
                let source_project = row
                    .try_get::<Option<String>, _>("project_id")
                    .map_err(|e| e.to_string())?;
                let source_agent = row
                    .try_get::<Option<String>, _>("agent_id")
                    .map_err(|e| e.to_string())?;
                let revision: i64 = row.try_get("revision").map_err(|e| e.to_string())?;
                let updated_at: String = row.try_get("updated_at").map_err(|e| e.to_string())?;
                let attachment_ids = document["attachmentIds"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut item = json!({
                    "sourceId":id,"sourceKind":"draft","threadId":binding.thread_id,
                    "kind":binding.kind,"projectId":source_project,"agentId":source_agent,
                    "status":"draft","revision":revision,"text":document["text"],
                    "document":document,"settings":settings,"attachmentIds":attachment_ids,
                    "createdAt":updated_at,"restorable":false
                });
                let context = self
                    .validate_recovery_source(
                        &binding,
                        Some(source_project),
                        Some(source_agent),
                        &item["document"],
                        &item["settings"],
                        &profiles,
                        &models,
                    )
                    .await;
                let grants = self
                    .chat
                    .dispatch_storage("attachments.list", json!({"ownerId":binding.thread_id}))
                    .await;
                let selected = item["attachmentIds"].clone();
                if entries.iter().any(|entry| {
                    matches!(
                        entry["status"].as_str(),
                        Some("accepted" | "uncertain" | "dispatching")
                    )
                }) {
                    item["restorable"] = json!(false);
                    item["reason"] = json!("A previous message has an accepted or unconfirmed runtime receipt; restoring this draft could duplicate it.");
                } else {
                    set_restorability(&mut item, context, grants, &selected);
                }
                items.push(item);
            }
        }
        Ok(json!({"items":items}))
    }

    async fn recovery_was_restored(
        &self,
        source_kind: &str,
        source_id: &str,
    ) -> Result<bool, String> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM chat_recovery_map WHERE source_kind=? AND source_id=?)",
        )
        .bind(source_kind)
        .bind(source_id)
        .fetch_one(&self.chat.pool)
        .await
        .map_err(|error| error.to_string())
    }

    async fn validate_recovery_source(
        &self,
        binding: &SessionBinding,
        source_project: Option<Option<String>>,
        source_agent: Option<Option<String>>,
        document: &Value,
        settings: &Value,
        profiles: &[brr_agent_core::AgentProfile],
        models: &Value,
    ) -> Result<RecoveryContext, String> {
        if !matches!(binding.kind.as_str(), "baseline" | "agent") || binding.task_run_id.is_some() {
            return Err("Only baseline or agent chats can be recovered here".into());
        }
        let selected_agent = source_agent
            .clone()
            .unwrap_or_else(|| binding.agent_id.clone());
        let context = self
            .recovery_context(binding, source_project, source_agent)
            .await?;
        // The cache holds the workspace proof; draft-local agent/project overrides
        // are checked separately below and are never replaced by defaults.
        let agent_id = selected_agent;
        if let Some(agent_id) = agent_id.as_deref() {
            if !profiles.iter().any(|profile| profile.id == agent_id) {
                return Err(format!("Saved agent profile {agent_id} is unavailable"));
            }
        }
        validate_saved_settings(settings, models)?;
        validate_saved_references(self, &context, document, profiles).await?;
        Ok(RecoveryContext {
            agent_id,
            ..context
        })
    }

    async fn recovery_context(
        &self,
        binding: &SessionBinding,
        source_project: Option<Option<String>>,
        source_agent: Option<Option<String>>,
    ) -> Result<RecoveryContext, String> {
        let membership = self.projects.membership(&binding.thread_id).await?;
        let project_id = source_project
            .unwrap_or_else(|| membership.clone().or_else(|| binding.project_id.clone()));
        let agent_id = source_agent.unwrap_or_else(|| binding.agent_id.clone());
        let mut roots = Vec::new();
        let mut project_cwd = None;
        let mut folderless_registered_project = false;
        if let Some(project_id) = project_id.as_deref() {
            let project = self
                .projects
                .read(project_id)
                .await?
                .filter(|project| !project.hidden)
                .ok_or_else(|| format!("Saved project {project_id} is unavailable"))?;
            folderless_registered_project = project.folders.is_empty();
            for folder in &project.folders {
                let path = crate::project_files::validate_directory(folder)
                    .map_err(|_| format!("Saved project folder is unavailable: {folder}"))?;
                let root = path.to_string_lossy().into_owned();
                if !roots.contains(&root) {
                    roots.push(root);
                }
                if project.primary_folder.as_deref() == Some(folder.as_str()) {
                    project_cwd = Some(path);
                }
            }
            if roots.is_empty() && !folderless_registered_project {
                return Err(format!(
                    "Saved project {project_id} has no available registered folders"
                ));
            }
        }
        let workspace = self.worktrees.get_session(&binding.thread_id)?;
        let cwd = if let Some(workspace) = &workspace {
            self.worktrees.validate_session(workspace).await?;
            if project_id.is_none() {
                for root in &workspace.roots {
                    if !roots.contains(root) {
                        roots.push(root.clone());
                    }
                }
            }
            std::path::PathBuf::from(&workspace.cwd)
        } else if let Some(cwd) = project_cwd {
            cwd
        } else if let Some(root) = roots.first() {
            std::path::PathBuf::from(root)
        } else {
            let cwd = binding
                .cwd
                .as_deref()
                .ok_or("Saved chat has no trusted workspace path")?;
            crate::project_files::validate_directory(cwd)
                .map_err(|_| "Saved chat workspace is unavailable".to_string())?
        };
        if roots.is_empty() && project_id.is_none() {
            roots.push(cwd.to_string_lossy().into_owned());
        }
        Ok(RecoveryContext {
            project_id,
            agent_id,
            folderless_registered_project,
            cwd,
            roots,
            workspace,
        })
    }

    async fn restore_recovery_message(self: &Arc<Self>, params: Value) -> Result<Value, String> {
        self.require_account().await?;
        let source_id = super::string(&params, "sourceId")?.to_owned();
        let source_kind = super::string(&params, "sourceKind")?.to_owned();
        let operation_id = super::string(&params, "operationId")?.to_owned();
        if !matches!(source_kind.as_str(), "submission" | "draft") {
            return Err("Invalid recovery source kind".into());
        }
        let (thread_id, _) = self.read_recovery_source(&source_kind, &source_id).await?;
        let _guard = self.questions.thread_guard(&thread_id).await;
        let (thread_id, source) = self.read_recovery_source(&source_kind, &source_id).await?;
        let binding = self.store.session_binding(&thread_id).await?;
        if !matches!(binding.kind.as_str(), "baseline" | "agent") || binding.task_run_id.is_some() {
            return Err("Only baseline or agent chats can be recovered here".into());
        }
        if let Some(draft_id) = sqlx::query_scalar::<_, String>(
            "SELECT draft_id FROM chat_recovery_map WHERE source_kind=? AND source_id=?",
        )
        .bind(&source_kind)
        .bind(&source_id)
        .fetch_optional(&self.chat.pool)
        .await
        .map_err(|error| error.to_string())?
        {
            let draft = self
                .chat
                .dispatch_storage("drafts.get", json!({"id":draft_id}))
                .await?;
            if draft["threadId"].as_str().is_some_and(|id| !id.is_empty()) {
                return Ok(
                    json!({"restored":true,"target":"chat","threadId":draft["threadId"],"replayed":true}),
                );
            }
            let attachments = self
                .chat
                .dispatch_storage("attachments.list", json!({"ownerId":draft_id}))
                .await?;
            return Ok(
                json!({"draft":draft,"attachments":attachments,"restored":true,"target":"draft","replayed":true}),
            );
        }
        if source_kind == "submission"
            && !matches!(source["status"].as_str(), Some("queued" | "failed"))
        {
            return Err("Only queued or definitively failed messages can be restored".into());
        }
        if source_kind == "draft" && source["status"] != "draft" {
            return Err("Promoted draft is no longer available".into());
        }
        if source_kind == "draft" && self.thread_has_risky_receipt(&thread_id).await? {
            return Err("A previous message has an accepted or unconfirmed runtime receipt; restoring this draft could duplicate it.".into());
        }
        if runtime_chat_ids(self).await?.contains(&thread_id) {
            return Err(
                "This chat is present in runtime history; recovery is no longer needed".into(),
            );
        }
        if runtime_thread_available(self, &thread_id).await? {
            return Err(
                "This chat is still available in the runtime; recovery is no longer needed".into(),
            );
        }
        let profiles = self.store.list_profiles().await?;
        let models = self
            .rpc
            .call("model/list", json!({"includeHidden":false}))
            .await?;
        let context = self
            .validate_recovery_source(
                &binding,
                if source_kind == "draft" {
                    Some(source["projectId"].as_str().map(str::to_owned))
                } else {
                    None
                },
                if source_kind == "draft" {
                    Some(source["agentId"].as_str().map(str::to_owned))
                } else {
                    None
                },
                &source["document"],
                &source["settings"],
                &profiles,
                &models,
            )
            .await?;
        let restored_document = rebase_recovery_document(&context, &source["document"])?;
        let _ = (context.cwd, context.roots, context.workspace);
        let result = self.chat.dispatch_storage("recovery.restoreDraft", json!({
            "sourceId":source_id,"sourceKind":source_kind,"threadId":thread_id,
            "operationId":operation_id,"projectId":context.project_id,"agentId":context.agent_id,
            "document":restored_document
        })).await?;
        let draft_id = result["draftId"]
            .as_str()
            .ok_or("Recovery did not return a draft ID")?;
        let draft = self
            .chat
            .dispatch_storage("drafts.get", json!({"id":draft_id}))
            .await?;
        self.chat_changed(Some(&thread_id));
        if draft["threadId"].as_str().is_some_and(|id| !id.is_empty()) {
            Ok(json!({"restored":true,"target":"chat","threadId":draft["threadId"]}))
        } else {
            let attachments = self
                .chat
                .dispatch_storage("attachments.list", json!({"ownerId":draft_id}))
                .await?;
            Ok(
                json!({"draft":draft,"attachments":attachments,"restored":true,"target":"draft","replayed":result["replayed"] == true}),
            )
        }
    }

    async fn thread_has_risky_receipt(&self, thread_id: &str) -> Result<bool, String> {
        let submissions = self
            .chat
            .dispatch_storage("submissions.list", json!({"threadId":thread_id}))
            .await?;
        Ok(submissions.as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                matches!(
                    entry["status"].as_str(),
                    Some("accepted" | "uncertain" | "dispatching")
                )
            })
        }))
    }

    async fn read_recovery_source(&self, kind: &str, id: &str) -> Result<(String, Value), String> {
        match kind {
            "submission" => {
                let source = self
                    .chat
                    .dispatch_storage("submissions.get", json!({"id":id}))
                    .await?;
                let thread = super::string(&source, "threadId")?.to_owned();
                Ok((thread, source))
            }
            "draft" => {
                let source = self
                    .chat
                    .dispatch_storage("drafts.get", json!({"id":id}))
                    .await?;
                let thread = source["threadId"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("Only promoted drafts can be recovered")?
                    .to_owned();
                let mut value = source;
                value["status"] = json!("draft");
                Ok((thread, value))
            }
            _ => Err("Invalid recovery source kind".into()),
        }
    }

    async fn cancel_recovery_message(self: &Arc<Self>, params: Value) -> Result<Value, String> {
        let submission_id = super::string(&params, "submissionId")?.to_owned();
        let source = self
            .chat
            .dispatch_storage("submissions.get", json!({"id":submission_id}))
            .await?;
        let thread_id = super::string(&source, "threadId")?.to_owned();
        let binding = self.store.session_binding(&thread_id).await?;
        if !matches!(binding.kind.as_str(), "baseline" | "agent") || binding.task_run_id.is_some() {
            return Err("Only queued messages in baseline or agent chats can be cancelled".into());
        }
        let _guard = self.questions.thread_guard(&thread_id).await;
        let result = self
            .chat
            .dispatch_storage(
                "recovery.cancelQueued",
                json!({
                    "submissionId":submission_id,
                    "expectedRevision":params["expectedRevision"]
                }),
            )
            .await?;
        self.chat_changed(Some(&thread_id));
        Ok(result)
    }
}

fn recovery_submission_item(entry: &Value, binding: &SessionBinding) -> Value {
    json!({
        "sourceId":entry["id"],"sourceKind":"submission","threadId":binding.thread_id,
        "kind":binding.kind,"projectId":null,"agentId":binding.agent_id,
        "status":entry["status"],"revision":entry["revision"],
        "text":entry["document"]["text"],"document":entry["document"],
        "settings":entry["settings"],"attachmentIds":entry["attachmentIds"],
        "createdAt":entry["createdAt"],"reason":entry["error"],"restorable":false
    })
}

fn set_restorability(
    item: &mut Value,
    context: Result<RecoveryContext, String>,
    grants: Result<Value, String>,
    selected: &Value,
) {
    let context = match context {
        Ok(context) => context,
        Err(error) => {
            item["reason"] = json!(error);
            item["restorable"] = json!(false);
            return;
        }
    };
    item["projectId"] = json!(context.project_id);
    item["agentId"] = json!(context.agent_id);
    let _ = (context.cwd, context.roots, context.workspace);
    let grants = match grants {
        Ok(grants) => grants,
        Err(error) => {
            item["reason"] = json!(error);
            item["restorable"] = json!(false);
            return;
        }
    };
    let granted = grants
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["id"].as_str())
        .collect::<HashSet<_>>();
    let selected = selected
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if selected.iter().any(|id| !granted.contains(id)) {
        item["reason"] = json!("One or more selected attachments are no longer available");
        item["restorable"] = json!(false);
        return;
    }
    item["restorable"] = json!(true);
    item.as_object_mut().unwrap().remove("reason");
}

fn parse_column(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Value, String> {
    let value: String = row.try_get(column).map_err(|error| error.to_string())?;
    serde_json::from_str(&value).map_err(|error| error.to_string())
}

fn validate_saved_settings(settings: &Value, models: &Value) -> Result<(), String> {
    let model = settings["model"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Saved model selection is missing")?;
    let effort = settings["effort"]
        .as_str()
        .ok_or("Saved reasoning effort is missing")?;
    if !matches!(
        effort,
        "minimal" | "low" | "medium" | "high" | "xhigh" | "none"
    ) {
        return Err("Saved reasoning effort is no longer supported".into());
    }
    if !matches!(settings["mode"].as_str(), Some("default" | "plan")) {
        return Err("Saved collaboration mode is no longer supported".into());
    }
    if model != "_runtime_default"
        && !models["data"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|candidate| candidate["id"] == model || candidate["model"] == model)
    {
        return Err(format!(
            "Saved model {model} is unavailable; choose a supported model before restoring"
        ));
    }
    Ok(())
}

async fn validate_saved_references(
    host: &AgentHost,
    context: &RecoveryContext,
    document: &Value,
    profiles: &[brr_agent_core::AgentProfile],
) -> Result<(), String> {
    let mut skills = None;
    let document = rebase_recovery_document(context, document)?;
    for reference in document["references"].as_array().into_iter().flatten() {
        let kind = reference["type"]
            .as_str()
            .ok_or("Saved reference type is missing")?;
        let id = reference["id"]
            .as_str()
            .ok_or("Saved reference ID is missing")?;
        match kind {
            "agent" if profiles.iter().any(|profile| profile.id == id) => {}
            "agent" => return Err(format!("Referenced agent {id} is unavailable")),
            "file" | "folder" => {
                let result = super::input::workspace_reference(
                    &context.cwd,
                    &context.roots,
                    id,
                    kind == "folder",
                );
                result.map_err(|error| {
                    format!("Saved {kind} reference {id} is unavailable: {error}")
                })?;
            }
            "skill" if id == "plan" => {}
            "skill" => {
                if skills.is_none() {
                    skills = Some(
                        host.rpc
                            .call(
                                "skills/list",
                                json!({"cwds":[context.cwd],"forceReload":false}),
                            )
                            .await?,
                    );
                }
                let catalog = skills.as_ref().unwrap();
                if !catalog["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|group| group["skills"].as_array().into_iter().flatten())
                    .any(|skill| skill["path"] == id && skill["enabled"] != false)
                {
                    return Err(format!("Referenced skill {id} is unavailable or disabled"));
                }
            }
            _ => return Err(format!("Unsupported saved reference type: {kind}")),
        }
    }
    Ok(())
}

fn rebase_recovery_document(context: &RecoveryContext, document: &Value) -> Result<Value, String> {
    let mut rebased = document.clone();
    let Some(references) = rebased["references"].as_array_mut() else {
        return Ok(rebased);
    };
    for reference in references {
        let Some(kind) = reference["type"].as_str() else {
            return Err("Saved reference type is missing".into());
        };
        if !matches!(kind, "file" | "folder") {
            continue;
        }
        let id = reference["id"]
            .as_str()
            .ok_or("Saved file reference ID is missing")?;
        if context.folderless_registered_project && context.roots.is_empty() {
            return Err(
                "Saved project has no registered folders; file references cannot be recovered"
                    .into(),
            );
        }
        let path = if let Some(workspace) = &context.workspace {
            if let Ok(path) = super::input::workspace_reference(
                &context.cwd,
                &context.roots,
                id,
                kind == "folder",
            ) {
                path
            } else {
                let old_path =
                    super::input::session_workspace_reference(workspace, id, kind == "folder")?;
                let mut mapped = Vec::new();
                for folder in &workspace.folders {
                    let folder_path =
                        std::fs::canonicalize(&folder.path).map_err(|error| error.to_string())?;
                    let Ok(suffix) = old_path.strip_prefix(&folder_path) else {
                        continue;
                    };
                    if suffix.components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::ParentDir
                                | std::path::Component::RootDir
                                | std::path::Component::Prefix(_)
                        )
                    }) {
                        return Err("Reference escapes its saved session folder".into());
                    }
                    let source_root = std::fs::canonicalize(&folder.source).map_err(|_| {
                        format!(
                            "Saved reference source folder is unavailable: {}",
                            folder.source
                        )
                    })?;
                    if !context
                        .roots
                        .iter()
                        .any(|root| source_root == std::path::Path::new(root))
                    {
                        continue;
                    }
                    let candidate = source_root.join(suffix);
                    let candidate = super::input::workspace_reference(
                        &context.cwd,
                        &context.roots,
                        &candidate.to_string_lossy(),
                        kind == "folder",
                    )?;
                    mapped.push(candidate);
                }
                mapped.sort();
                mapped.dedup();
                match mapped.as_slice() {
                    [path] => path.clone(),
                    [] => {
                        return Err(format!(
                            "Saved {kind} reference {id} cannot be mapped to a registered project folder"
                        ))
                    }
                    _ => {
                        return Err(format!(
                            "Saved {kind} reference {id} matches ambiguous project folders"
                        ))
                    }
                }
            }
        } else {
            super::input::workspace_reference(&context.cwd, &context.roots, id, kind == "folder")?
        };
        let path = path.to_string_lossy().into_owned();
        reference["id"] = json!(path);
        if reference.get("path").is_some() {
            reference["path"] = json!(path);
        }
    }
    Ok(rebased)
}

async fn runtime_chat_ids(host: &AgentHost) -> Result<HashSet<String>, String> {
    let mut ids = HashSet::new();
    for archived in [false, true] {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        loop {
            let mut params = json!({"limit":100,"sortKey":"updated_at","sortDirection":"desc","archived":archived});
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            let page = host.rpc.call("thread/list", params).await?;
            let threads = page["data"]
                .as_array()
                .ok_or("thread/list returned malformed recovery inventory")?;
            for thread in threads {
                let id = thread["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("thread/list returned a thread without an ID")?;
                ids.insert(id.to_owned());
            }
            match page.get("nextCursor") {
                Some(Value::Null) => break,
                Some(Value::String(next)) if !next.is_empty() => {
                    if !cursors.insert(next.clone()) {
                        return Err("thread/list repeated a recovery cursor".into());
                    }
                    cursor = Some(next.clone());
                }
                _ => return Err("thread/list returned malformed recovery pagination".into()),
            }
        }
    }
    Ok(ids)
}

async fn runtime_thread_available(host: &AgentHost, thread_id: &str) -> Result<bool, String> {
    match host
        .rpc
        .call(
            "thread/read",
            json!({"threadId":thread_id,"includeTurns":false}),
        )
        .await
    {
        Ok(response) => {
            let actual = response["thread"]["id"]
                .as_str()
                .ok_or("thread/read returned malformed recovery metadata")?;
            if actual != thread_id {
                return Err(format!(
                    "thread/read returned {actual} while verifying saved chat {thread_id}"
                ));
            }
            Ok(true)
        }
        Err(error)
            if error == format!("thread not loaded: {thread_id}")
                || error == format!("thread not found: {thread_id}") =>
        {
            Ok(false)
        }
        Err(error) => Err(format!(
            "Could not verify saved chat {thread_id} in the runtime: {error}"
        )),
    }
}
