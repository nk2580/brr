use crate::{
    agent_host::AgentHost,
    projects::{ProjectPermission, ProjectRecord, ProjectStore},
};
use brr_agent_core::{
    AgentStore, NewAcceptanceCriterion, Task, TaskInput, TicketActor, TicketFilter, TicketInput,
    TicketMutation, TicketPriority, TicketStatus, TicketUpdate, WorkspacePermission,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovedTicketProject {
    pub reviewed: ProjectRecord,
    pub selected_folder: Option<String>,
    pub workspace_project: ProjectRecord,
}

/// Load and validate the persisted approved snapshot for a queued ticket task.
/// This runs before first-start workspace preparation; resumed threads use their saved workspace.
pub(crate) async fn approved_ticket_project(
    host: &AgentHost,
    task: &Task,
) -> Result<Option<ApprovedTicketProject>, String> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT ticket_id,snapshot FROM ticket_run_proposals WHERE task_id=? AND status='approved' ORDER BY created_at DESC,id DESC LIMIT 1",
    )
    .bind(&task.id)
    .fetch_optional(host.store.pool())
    .await
    .map_err(|error| error.to_string())?;
    let Some((ticket_id, encoded_snapshot)) = row else {
        return Ok(None);
    };
    let snapshot: TaskInput =
        serde_json::from_str(&encoded_snapshot).map_err(|error| error.to_string())?;
    if !task_matches_snapshot(task, &snapshot) {
        return Err("The approved ticket task no longer matches its reviewed snapshot".into());
    }
    let ticket = host.store.get_ticket(&ticket_id).await?;
    let reviewed = require_project(&host.projects, &ticket.project_id).await?;
    if snapshot.project_id.as_deref() != Some(reviewed.id.as_str()) {
        return Err(
            "The approved ticket project changed; propose and approve the run again".into(),
        );
    }
    let selected_folder = validate_selected_folder(&reviewed, snapshot.cwd.as_deref())?;
    validate_snapshot_project(&snapshot, &ticket, &reviewed, selected_folder.as_deref())?;
    if snapshot.workspace_permission.as_ref() != Some(&permission(reviewed.permission.as_ref())) {
        return Err(
            "The approved ticket workspace permission changed; propose and approve the run again"
                .into(),
        );
    }
    let profile = host.store.read_profile(&snapshot.agent_id).await?;
    if snapshot.model.as_deref() != Some(profile.model.as_str())
        || snapshot.effort.as_deref() != Some(profile.effort.as_str())
    {
        return Err("The approved ticket agent changed; propose and approve the run again".into());
    }
    let mut workspace_project = reviewed.clone();
    workspace_project.primary_folder = selected_folder.clone();
    Ok(Some(ApprovedTicketProject {
        reviewed,
        selected_folder,
        workspace_project,
    }))
}

pub(crate) async fn validate_approved_ticket_project(
    host: &AgentHost,
    task: &Task,
    approved: &ApprovedTicketProject,
) -> Result<(), String> {
    let current = approved_ticket_project(host, task)
        .await?
        .ok_or("The approved ticket task snapshot is missing")?;
    if &current != approved {
        return Err("The approved ticket project configuration changed before execution".into());
    }
    Ok(())
}

fn task_matches_snapshot(task: &Task, snapshot: &TaskInput) -> bool {
    task.agent_id == snapshot.agent_id
        && task.title == snapshot.title
        && task.instructions == snapshot.instructions
        && task.acceptance_criteria == snapshot.acceptance_criteria
        && task.context == snapshot.context
        && task.cwd == snapshot.cwd
        && task.project_id == snapshot.project_id
        && task.model == snapshot.model
        && task.effort == snapshot.effort
        && task.workspace_permission == snapshot.workspace_permission
        && task.dependencies == snapshot.dependencies
}

pub(crate) async fn request_stores(
    store: &AgentStore,
    projects: &ProjectStore,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    match method {
        "projects" => {
            fields(&params, &[])?;
            Ok(json!({"data":projects.list().await?}))
        }
        "agents" => {
            fields(&params, &[])?;
            Ok(json!({"data":store.list_profiles().await?}))
        }
        "list" => {
            fields(
                &params,
                &[
                    "projectId",
                    "cursor",
                    "limit",
                    "status",
                    "priority",
                    "agentId",
                    "archived",
                    "query",
                ],
            )?;
            let project_id = required(&params, "projectId")?;
            require_project(projects, project_id).await?;
            let filter = TicketFilter {
                status: params.get("status").cloned().map(parse).transpose()?,
                priority: params.get("priority").cloned().map(parse).transpose()?,
                agent_id: optional(&params, "agentId").map(str::to_owned),
                query: optional(&params, "query").map(str::to_owned),
                archived: Some(
                    params
                        .get("archived")
                        .map(|v| v.as_bool().ok_or("archived must be a boolean"))
                        .transpose()?
                        .unwrap_or(false),
                ),
            };
            let page = store
                .list_tickets(
                    project_id,
                    optional(&params, "cursor"),
                    params["limit"].as_i64().unwrap_or(50),
                    Some(filter),
                )
                .await?;
            ticket_page_value(store, page).await
        }
        "read" => {
            fields(&params, &["ticketId", "commentsCursor", "activityCursor"])?;
            let detail = store
                .read_ticket(
                    required(&params, "ticketId")?,
                    params["commentsCursor"].as_i64(),
                    params["activityCursor"].as_i64(),
                )
                .await?;
            require_project(projects, &detail.ticket.project_id).await?;
            let mut response = json!(detail);
            response["ticket"] = ticket_value(store, &detail.ticket).await?;
            Ok(response)
        }
        "create" => {
            fields(
                &params,
                &[
                    "projectId",
                    "title",
                    "description",
                    "acceptanceCriteria",
                    "agentId",
                    "priority",
                    "operationId",
                ],
            )?;
            let project_id = required(&params, "projectId")?;
            require_project(projects, project_id).await?;
            let input = ticket_input(&params, project_id)?;
            let ticket = store
                .create_ticket(input, mutation(&params, TicketActor::human())?)
                .await?;
            ticket_value(store, &ticket).await
        }
        "update" => {
            fields(&params, &["ticketId", "revision", "changes", "operationId"])?;
            let ticket_id = required(&params, "ticketId")?;
            let current = store.get_ticket(ticket_id).await?;
            require_project(projects, &current.project_id).await?;
            fields(
                &params["changes"],
                &[
                    "title",
                    "description",
                    "acceptanceCriteria",
                    "agentId",
                    "priority",
                    "status",
                    "archived",
                ],
            )?;
            let update = ticket_update(&params["changes"])?;
            let ticket = store
                .update_ticket(
                    ticket_id,
                    revision(&params)?,
                    update,
                    mutation(&params, TicketActor::human())?,
                )
                .await?;
            ticket_value(store, &ticket).await
        }
        "move" => {
            fields(
                &params,
                &["ticketId", "revision", "status", "beforeId", "operationId"],
            )?;
            let ticket_id = required(&params, "ticketId")?;
            let current = store.get_ticket(ticket_id).await?;
            require_project(projects, &current.project_id).await?;
            let ticket = store
                .move_ticket(
                    ticket_id,
                    revision(&params)?,
                    parse(params["status"].clone())?,
                    optional(&params, "beforeId"),
                    mutation(&params, TicketActor::human())?,
                )
                .await?;
            ticket_value(store, &ticket).await
        }
        "comment" => {
            fields(&params, &["ticketId", "body", "operationId"])?;
            let ticket_id = required(&params, "ticketId")?;
            let current = store.get_ticket(ticket_id).await?;
            require_project(projects, &current.project_id).await?;
            Ok(json!(
                store
                    .comment_ticket(
                        ticket_id,
                        required(&params, "body")?,
                        mutation(&params, TicketActor::human())?
                    )
                    .await?
            ))
        }
        "proposeRun" => {
            fields(
                &params,
                &["ticketId", "revision", "agentId", "folder", "operationId"],
            )?;
            let ticket_id = required(&params, "ticketId")?;
            let ticket = store.get_ticket(ticket_id).await?;
            if revision(&params)? != ticket.revision {
                return Err(json!({"code":"conflict","current":ticket}).to_string());
            }
            let project = require_project(projects, &ticket.project_id).await?;
            let snapshot = run_snapshot(
                store,
                &ticket,
                &project,
                optional(&params, "agentId"),
                optional(&params, "folder"),
            )
            .await?;
            Ok(json!(
                store
                    .propose_ticket_run(
                        ticket_id,
                        ticket.revision,
                        snapshot,
                        mutation(&params, TicketActor::human())?
                    )
                    .await?
            ))
        }
        "proposals" => {
            fields(&params, &["ticketId"])?;
            let proposals = if let Some(ticket_id) = optional(&params, "ticketId") {
                let ticket = store.get_ticket(ticket_id).await?;
                require_project(projects, &ticket.project_id).await?;
                store.list_ticket_run_proposals(ticket_id).await?
            } else {
                let ids: Vec<String> = sqlx::query_scalar("SELECT DISTINCT ticket_id FROM ticket_run_proposals WHERE status='pending' ORDER BY ticket_id")
                    .fetch_all(store.pool()).await.map_err(|error|error.to_string())?;
                let mut proposals = Vec::new();
                for ticket_id in ids {
                    if let Ok(ticket) = store.get_ticket(&ticket_id).await {
                        if require_project(projects, &ticket.project_id).await.is_ok() {
                            proposals.extend(
                                store
                                    .list_ticket_run_proposals(&ticket_id)
                                    .await?
                                    .into_iter()
                                    .filter(|proposal| proposal.status == "pending"),
                            );
                        }
                    }
                }
                proposals
            };
            Ok(json!({"data":proposals}))
        }
        "rejectRun" => {
            fields(&params, &["ticketId", "hash", "operationId"])?;
            let ticket_id = required(&params, "ticketId")?;
            let ticket = store.get_ticket(ticket_id).await?;
            require_project(projects, &ticket.project_id).await?;
            Ok(json!(
                store
                    .reject_ticket_run(
                        ticket_id,
                        required(&params, "hash")?,
                        mutation(&params, TicketActor::human())?
                    )
                    .await?
            ))
        }
        "approveRun" => Err("Run approval requires the connected ChatGPT runtime".into()),
        _ => Err(format!("Unsupported Tickets method: {method}")),
    }
}

pub(crate) async fn request(
    host: Option<&AgentHost>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let host = host.ok_or("Ticket host is unavailable")?;
    if method != "approveRun" {
        return request_stores(&host.store, &host.projects, method, params).await;
    }
    fields(&params, &["ticketId", "hash", "operationId"])?;
    host.require_account().await?;
    let ticket_id = required(&params, "ticketId")?;
    let hash = required(&params, "hash")?;
    let proposal = host
        .store
        .list_ticket_run_proposals(ticket_id)
        .await?
        .into_iter()
        .find(|p| p.hash == hash)
        .ok_or("ticket run proposal not found")?;
    if proposal.status == "approved" {
        let task = host
            .store
            .approve_ticket_run(ticket_id, hash, mutation(&params, TicketActor::human())?)
            .await?;
        return Ok(json!(task));
    }
    if proposal.status != "pending" {
        return Err("ticket run proposal is no longer pending".into());
    }
    let ticket = host.store.get_ticket(ticket_id).await?;
    let project = require_project(&host.projects, &ticket.project_id).await?;
    let selected_folder = validate_selected_folder(&project, proposal.snapshot.cwd.as_deref())?;
    let profile = host.store.read_profile(&proposal.snapshot.agent_id).await?;
    if proposal.snapshot.model.as_deref() != Some(profile.model.as_str())
        || proposal.snapshot.effort.as_deref() != Some(profile.effort.as_str())
    {
        return Err(
            "The selected agent model or reasoning effort changed; propose the ticket run again"
                .into(),
        );
    }
    let current_permission = permission(project.permission.as_ref());
    if proposal.snapshot.workspace_permission.as_ref() != Some(&current_permission) {
        return Err(
            "The project workspace permission changed; propose the ticket run again".into(),
        );
    }
    validate_snapshot_project(
        &proposal.snapshot,
        &ticket,
        &project,
        selected_folder.as_deref(),
    )?;
    let task = host
        .store
        .approve_ticket_run(ticket_id, hash, mutation(&params, TicketActor::human())?)
        .await?;
    host.changed();
    host.notify(json!({"method":"brr/tasks/dispatch","params":{"taskId":task.id}}));
    Ok(json!(task))
}

pub(crate) async fn runtime_tool(
    host: &AgentHost,
    binding: &brr_agent_core::SessionBinding,
    params: &Value,
    args: &Value,
) -> Result<Value, String> {
    let tool = required(params, "tool")?;
    if !matches!(binding.kind.as_str(), "baseline" | "agent" | "task") {
        return Err("Tickets are unavailable in this session".into());
    }
    match tool {
        "brr_project_list" => Ok(json!({"data":host.projects.list().await?})),
        "brr_ticket_list" => {
            fields(
                args,
                &[
                    "projectId",
                    "cursor",
                    "limit",
                    "status",
                    "priority",
                    "agentId",
                    "archived",
                    "query",
                ],
            )?;
            let project_id = optional(args, "projectId")
                .or(binding.project_id.as_deref())
                .ok_or("Choose a registered project before listing tickets")?;
            require_project(&host.projects, project_id).await?;
            let filter = TicketFilter {
                status: args.get("status").cloned().map(parse).transpose()?,
                priority: args.get("priority").cloned().map(parse).transpose()?,
                agent_id: optional(args, "agentId").map(str::to_owned),
                query: optional(args, "query").map(str::to_owned),
                archived: Some(
                    args.get("archived")
                        .map(|v| v.as_bool().ok_or("archived must be a boolean"))
                        .transpose()?
                        .unwrap_or(false),
                ),
            };
            let page = host
                .store
                .list_tickets(
                    project_id,
                    optional(args, "cursor"),
                    args["limit"].as_i64().unwrap_or(50),
                    Some(filter),
                )
                .await?;
            ticket_page_value(&host.store, page).await
        }
        "brr_ticket_read" => {
            fields(args, &["ticketId", "commentsCursor", "activityCursor"])?;
            let detail = host
                .store
                .read_ticket(
                    required(args, "ticketId")?,
                    args["commentsCursor"].as_i64(),
                    args["activityCursor"].as_i64(),
                )
                .await?;
            require_project(&host.projects, &detail.ticket.project_id).await?;
            let mut response = json!(detail);
            response["ticket"] = ticket_value(&host.store, &detail.ticket).await?;
            Ok(response)
        }
        "brr_ticket_create" => {
            fields(
                args,
                &[
                    "projectId",
                    "title",
                    "description",
                    "acceptanceCriteria",
                    "agentId",
                    "priority",
                ],
            )?;
            let selected = optional(args, "projectId")
                .or(binding.project_id.as_deref())
                .ok_or("Choose a registered project before creating a ticket")?;
            require_project(&host.projects, selected).await?;
            let input = ticket_input(args, selected)?;
            let created = host
                .store
                .create_ticket(input, tool_mutation(host, binding, params).await?)
                .await?;
            Ok(json!(created))
        }
        "brr_ticket_update" => {
            fields(
                args,
                &[
                    "ticketId",
                    "revision",
                    "title",
                    "description",
                    "acceptanceCriteria",
                    "agentId",
                    "priority",
                    "status",
                    "archived",
                ],
            )?;
            let id = required(args, "ticketId")?;
            let old = host.store.get_ticket(id).await?;
            require_project(&host.projects, &old.project_id).await?;
            let changes = ticket_update(args)?;
            let updated = host
                .store
                .update_ticket(
                    id,
                    args["revision"].as_i64().ok_or("revision is required")?,
                    changes,
                    tool_mutation(host, binding, params).await?,
                )
                .await?;
            Ok(json!(updated))
        }
        "brr_ticket_comment" => {
            fields(args, &["ticketId", "body"])?;
            let id = required(args, "ticketId")?;
            let old = host.store.get_ticket(id).await?;
            require_project(&host.projects, &old.project_id).await?;
            let comment = host
                .store
                .comment_ticket(
                    id,
                    required(args, "body")?,
                    tool_mutation(host, binding, params).await?,
                )
                .await?;
            Ok(json!(comment))
        }
        "brr_ticket_run_propose" => {
            fields(args, &["ticketId", "revision", "agentId", "folder"])?;
            let id = required(args, "ticketId")?;
            let ticket = host.store.get_ticket(id).await?;
            let expected = args["revision"]
                .as_i64()
                .filter(|revision| *revision > 0)
                .ok_or("revision is required")?;
            if expected != ticket.revision {
                return Err(json!({"code":"conflict","current":ticket}).to_string());
            }
            let project = require_project(&host.projects, &ticket.project_id).await?;
            let snapshot = run_snapshot(
                &host.store,
                &ticket,
                &project,
                optional(args, "agentId"),
                optional(args, "folder"),
            )
            .await?;
            let proposal = host
                .store
                .propose_ticket_run(
                    id,
                    ticket.revision,
                    snapshot,
                    tool_mutation(host, binding, params).await?,
                )
                .await?;
            Ok(json!(proposal))
        }
        _ => Err("Unsupported ticket tool".into()),
    }
}

async fn ticket_page_value(
    store: &AgentStore,
    page: brr_agent_core::TicketPage,
) -> Result<Value, String> {
    let mut data = Vec::with_capacity(page.data.len());
    for ticket in page.data {
        data.push(ticket_value(store, &ticket).await?);
    }
    Ok(json!({"data":data,"nextCursor":page.next_cursor}))
}

async fn ticket_value(
    store: &AgentStore,
    ticket: &brr_agent_core::Ticket,
) -> Result<Value, String> {
    let mut response = json!(ticket);
    let execution: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id,status,result,evidence FROM tasks WHERE ticket_id=? ORDER BY CASE WHEN status IN ('succeeded','failed','cancelled') THEN 1 ELSE 0 END,rowid DESC LIMIT 1",
    ).bind(&ticket.id).fetch_optional(store.pool()).await.map_err(|error| error.to_string())?;
    if let Some((task_id, status, result, evidence)) = execution {
        let evidence = evidence
            .map(|value| serde_json::from_str::<Vec<String>>(&value))
            .transpose()
            .map_err(|error| error.to_string())?;
        response["execution"] =
            json!({"taskId":task_id,"status":status,"result":result,"evidence":evidence});
    }
    Ok(response)
}

async fn tool_mutation(
    host: &AgentHost,
    binding: &brr_agent_core::SessionBinding,
    params: &Value,
) -> Result<TicketMutation, String> {
    let thread_id = required(params, "threadId")?;
    if thread_id != binding.thread_id {
        return Err("Ticket tool thread does not match its immutable binding".into());
    }
    let call_id = params
        .get("callId")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("runtime callId is required for ticket operation id")?;
    let task_id = match binding.kind.as_str() {
        "task" => {
            let run_id = binding
                .task_run_id
                .as_deref()
                .ok_or("task session has no run binding")?;
            let run = host.store.get_run(run_id).await?;
            Some(run.task_id)
        }
        _ => None,
    };
    Ok(TicketMutation {
        operation_id: format!("runtime:{}:{}", thread_id, call_id),
        actor: runtime_actor(binding, task_id)?,
    })
}

fn runtime_actor(
    binding: &brr_agent_core::SessionBinding,
    task_id: Option<String>,
) -> Result<TicketActor, String> {
    if !matches!(binding.kind.as_str(), "baseline" | "agent" | "task") {
        return Err("Tickets are unavailable in this session".into());
    }
    Ok(TicketActor {
        kind: brr_agent_core::TicketActorKind::Agent,
        agent_id: binding.agent_id.clone(),
        thread_id: Some(binding.thread_id.clone()),
        task_id,
    })
}

fn ticket_input(params: &Value, project_id: &str) -> Result<TicketInput, String> {
    let criteria = params
        .get("acceptanceCriteria")
        .map(parse_criteria)
        .transpose()?
        .unwrap_or_default();
    Ok(TicketInput {
        project_id: project_id.into(),
        title: required(params, "title")?.into(),
        description: optional(params, "description").unwrap_or("").into(),
        acceptance_criteria: criteria,
        agent_id: optional(params, "agentId").map(str::to_owned),
        priority: params
            .get("priority")
            .cloned()
            .map(parse)
            .transpose()?
            .unwrap_or(TicketPriority::None),
        status: TicketStatus::Todo,
    })
}

fn ticket_update(params: &Value) -> Result<TicketUpdate, String> {
    let agent_id = params
        .get("agentId")
        .map(|v| {
            if v.is_null() {
                Ok(None)
            } else {
                v.as_str()
                    .map(|s| Some(s.to_owned()))
                    .ok_or("agentId must be a string or null")
            }
        })
        .transpose()?;
    Ok(TicketUpdate {
        title: params
            .get("title")
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("title must be a string")
            })
            .transpose()?,
        description: params
            .get("description")
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("description must be a string")
            })
            .transpose()?,
        acceptance_criteria: params
            .get("acceptanceCriteria")
            .map(parse_criteria)
            .transpose()?,
        agent_id,
        priority: params.get("priority").cloned().map(parse).transpose()?,
        status: params.get("status").cloned().map(parse).transpose()?,
        archived: params
            .get("archived")
            .map(|v| v.as_bool().ok_or("archived must be a boolean"))
            .transpose()?,
    })
}

fn parse_criteria(value: &Value) -> Result<Vec<NewAcceptanceCriterion>, String> {
    value
        .as_array()
        .ok_or("acceptanceCriteria must be an array")?
        .iter()
        .map(|item| {
            if let Some(text) = item.as_str() {
                return Ok(NewAcceptanceCriterion {
                    id: None,
                    text: text.into(),
                    checked: false,
                });
            }
            parse(item.clone())
        })
        .collect()
}

async fn run_snapshot(
    store: &AgentStore,
    ticket: &brr_agent_core::Ticket,
    project: &ProjectRecord,
    selected_agent: Option<&str>,
    selected_folder: Option<&str>,
) -> Result<TaskInput, String> {
    if ticket.description.trim().is_empty()
        || ticket.acceptance_criteria.is_empty()
        || ticket
            .acceptance_criteria
            .iter()
            .any(|c| c.text.trim().is_empty())
    {
        return Err(
            "A run proposal needs a description and at least one acceptance criterion".into(),
        );
    }
    let selected_folder = choose_project_folder(project, selected_folder)?;
    let agent_id = selected_agent
        .or(ticket.agent_id.as_deref())
        .ok_or("Choose or assign an agent before proposing a ticket run")?;
    if ticket
        .agent_id
        .as_deref()
        .is_some_and(|assigned| assigned != agent_id)
    {
        return Err("The selected agent does not match the ticket assignment".into());
    }
    let agent = store.read_profile(agent_id).await?;
    let workspace_permission = permission(project.permission.as_ref());
    Ok(TaskInput {
        agent_id: agent.id,
        title: ticket.title.clone(),
        instructions: ticket.description.clone(),
        acceptance_criteria: ticket
            .acceptance_criteria
            .iter()
            .map(|c| c.text.clone())
            .collect(),
        context: Some(run_context(ticket, project, selected_folder.as_deref())?),
        cwd: selected_folder,
        project_id: Some(project.id.clone()),
        model: Some(agent.model),
        effort: Some(agent.effort),
        workspace_permission: Some(workspace_permission),
        dependencies: vec![],
    })
}

async fn require_project(
    projects: &ProjectStore,
    project_id: &str,
) -> Result<ProjectRecord, String> {
    projects
        .read(project_id)
        .await?
        .filter(|project| !project.hidden)
        .ok_or_else(|| "Ticket project is not registered".into())
}

fn run_context(
    ticket: &brr_agent_core::Ticket,
    project: &ProjectRecord,
    selected_folder: Option<&str>,
) -> Result<String, String> {
    let snapshot = json!({
        "id": &project.id,
        "revision": project.revision,
        "folders": &project.folders,
        "primaryFolder": &project.primary_folder,
        "model": &project.model,
        "effort": &project.effort,
        "permission": &project.permission,
        "selectedFolder": selected_folder,
    });
    Ok(format!(
        "Execute ticket {} ({})\nBrr project snapshot: {}",
        ticket.number,
        ticket.id,
        serde_json::to_string(&snapshot).map_err(|error| error.to_string())?
    ))
}

fn validate_snapshot_project(
    snapshot: &TaskInput,
    ticket: &brr_agent_core::Ticket,
    project: &ProjectRecord,
    selected_folder: Option<&str>,
) -> Result<(), String> {
    if snapshot.project_id.as_deref() != Some(project.id.as_str())
        || snapshot.cwd.as_deref() != selected_folder
    {
        return Err("The ticket run project changed; propose the ticket run again".into());
    }
    if snapshot.context.as_deref() != Some(run_context(ticket, project, selected_folder)?.as_str())
    {
        return Err("The project configuration changed; propose the ticket run again".into());
    }
    Ok(())
}

fn choose_project_folder(
    project: &ProjectRecord,
    folder: Option<&str>,
) -> Result<Option<String>, String> {
    let selected = match (project.folders.as_slice(), folder) {
        ([], None) => return Ok(None),
        ([], Some(_)) => return Err("Choose a folder registered to this project".into()),
        ([only], None) => only.as_str(),
        (_, Some(selected)) => selected,
        (_, None) => return Err("Choose a project folder for this ticket run".into()),
    };
    let normalized = crate::projects::normalize_project_path(selected)?;
    if !project.folders.contains(&normalized) {
        return Err("Choose a folder registered to this project".into());
    }
    let metadata = std::fs::metadata(&normalized).map_err(|_| "The selected project folder is missing; tickets remain editable, but a run cannot be proposed or started")?;
    if !metadata.is_dir() {
        return Err("The selected project path is not a folder".into());
    }
    let canonical = std::fs::canonicalize(&normalized).map_err(|error| error.to_string())?;
    if canonical.to_string_lossy() != normalized {
        return Err(
            "The selected project folder changed; update the project before running the ticket"
                .into(),
        );
    }
    Ok(Some(normalized))
}

fn validate_selected_folder(
    project: &ProjectRecord,
    folder: Option<&str>,
) -> Result<Option<String>, String> {
    if project.folders.is_empty() {
        if folder.is_some() {
            return Err("Choose a folder registered to this project".into());
        }
        return Ok(None);
    }
    let folder = folder.ok_or("The ticket run snapshot has no selected project folder")?;
    choose_project_folder(project, Some(folder))
}

fn permission(permission: Option<&ProjectPermission>) -> WorkspacePermission {
    match permission {
        Some(ProjectPermission::ReadOnly) => WorkspacePermission::ReadOnly,
        Some(ProjectPermission::WorkspaceWrite) | None => WorkspacePermission::WorkspaceWrite,
    }
}

fn mutation(params: &Value, actor: TicketActor) -> Result<TicketMutation, String> {
    Ok(TicketMutation {
        operation_id: required(params, "operationId")?.into(),
        actor,
    })
}
fn revision(params: &Value) -> Result<i64, String> {
    params
        .get("revision")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .ok_or("revision must be a positive integer".into())
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}
fn optional<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("request params must be an object")?;
    if let Some(field) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("Unsupported ticket field: {field}"));
    }
    Ok(())
}
fn parse<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::ProjectStore;

    #[tokio::test]
    async fn ticket_crud_is_available_without_a_runtime_and_scoped_to_registered_projects() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        let store = AgentStore::open(dir.path().join("engine/agents"))
            .await
            .unwrap();
        let projects = ProjectStore::open(dir.path().join("projects.sqlite"))
            .await
            .unwrap();
        let registered = projects
            .create(crate::projects::ProjectInput {
                name: "repo".into(),
                folders: vec![project.to_string_lossy().into_owned()],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let ticket=request_stores(&store,&projects,"create",json!({"projectId":registered.id,"title":"Offline ticket","description":"Edit while runtime is offline","acceptanceCriteria":["can be read"],"operationId":"native-create-1"})).await.unwrap();
        assert_eq!(ticket["status"], "todo");
        let id = ticket["id"].as_str().unwrap();
        let read = request_stores(&store, &projects, "read", json!({"ticketId":id}))
            .await
            .unwrap();
        assert_eq!(read["ticket"]["title"], "Offline ticket");
        assert!(
            request_stores(&store, &projects, "read", json!({"ticketId":"unknown"}))
                .await
                .is_err()
        );
        assert!(request_stores(
            &store,
            &projects,
            "create",
            json!({"projectId":"/not/registered","title":"bad","operationId":"native-create-2"})
        )
        .await
        .is_err());
    }

    #[test]
    fn native_mutations_reject_forged_actors_and_unknown_fields() {
        assert!(fields(
            &json!({"operationId":"x","actor":{"kind":"human"}}),
            &["operationId"]
        )
        .is_err());
    }

    #[tokio::test]
    async fn ticket_list_exposes_execution_badges_without_runtime_transcripts() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentStore::open(directory.path().join("agents"))
            .await
            .unwrap();
        let projects = ProjectStore::open(directory.path().join("projects.sqlite"))
            .await
            .unwrap();
        let project = projects
            .create(crate::projects::ProjectInput {
                name: "fixture".into(),
                folders: vec![directory.path().to_string_lossy().into_owned()],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let draft = store
            .propose_profile(
                "user",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Fixture agent".into(),
                    description: "Ticket status fixture".into(),
                    instructions: "Inspect only".into(),
                    soul: "Be concise".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, "badge-review")
            .await
            .unwrap();
        let agent = store
            .apply_profile(&draft.id, &draft.hash, "badge-agent")
            .await
            .unwrap();
        let ticket = request_stores(&store, &projects, "create", json!({"projectId":project.id,"title":"Badge fixture","description":"Inspect fixture","acceptanceCriteria":["Evidence recorded"],"agentId":agent.id,"operationId":"badge-ticket"})).await.unwrap();
        let proposal = request_stores(&store, &projects, "proposeRun", json!({"ticketId":ticket["id"],"revision":ticket["revision"],"operationId":"badge-proposal"})).await.unwrap();
        let task = store
            .approve_ticket_run(
                ticket["id"].as_str().unwrap(),
                proposal["hash"].as_str().unwrap(),
                TicketMutation {
                    operation_id: "badge-approve".into(),
                    actor: TicketActor::human(),
                },
            )
            .await
            .unwrap();
        let page = request_stores(&store, &projects, "list", json!({"projectId":project.id}))
            .await
            .unwrap();
        assert_eq!(page["data"][0]["execution"]["status"], "queued");
        assert_eq!(page["data"][0]["execution"]["taskId"], task.id);
        assert!(page["data"][0]["execution"].get("threadId").is_none());
        assert!(page["data"][0]["execution"].get("instructions").is_none());
    }

    #[tokio::test]
    async fn folderless_project_can_propose_execution_in_a_private_workspace() {
        let d = tempfile::tempdir().unwrap();
        let store = AgentStore::open(d.path().join("agents")).await.unwrap();
        let projects = ProjectStore::open(d.path().join("projects.sqlite"))
            .await
            .unwrap();
        let project = projects
            .create(crate::projects::ProjectInput {
                name: "Scratch".into(),
                folders: vec![],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let draft = store
            .propose_profile(
                "user",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Ticket agent".into(),
                    description: "Runs scratch tickets".into(),
                    instructions: "Work carefully".into(),
                    soul: "Be useful".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, "scratch-review")
            .await
            .unwrap();
        let agent = store
            .apply_profile(&draft.id, &draft.hash, "scratch-agent")
            .await
            .unwrap();
        let ticket = request_stores(&store, &projects, "create", json!({
            "projectId": project.id, "title": "Scratch task", "description": "Run without a folder",
            "acceptanceCriteria": ["uses the project identity"], "agentId": agent.id,
            "operationId": "scratch-ticket"
        })).await.unwrap();
        let proposal = request_stores(&store, &projects, "proposeRun", json!({
            "ticketId": ticket["id"], "revision": ticket["revision"], "operationId": "scratch-proposal"
        })).await.unwrap();
        assert_eq!(proposal["snapshot"]["projectId"], project.id);
        assert!(proposal["snapshot"]["cwd"].is_null());
        assert!(proposal["snapshot"]["context"]
            .as_str()
            .unwrap()
            .contains("\"folders\":[]"));
        let snapshot: TaskInput = serde_json::from_value(proposal["snapshot"].clone()).unwrap();
        let native_ticket = store
            .get_ticket(ticket["id"].as_str().unwrap())
            .await
            .unwrap();
        validate_snapshot_project(&snapshot, &native_ticket, &project, None).unwrap();
        let edited = request_stores(
            &store,
            &projects,
            "update",
            json!({
                "ticketId": ticket["id"], "revision": ticket["revision"],
                "changes": {"title":"Still editable"}, "operationId":"scratch-update"
            }),
        )
        .await
        .unwrap();
        assert_eq!(edited["title"], "Still editable");
    }

    #[test]
    fn multi_folder_run_requires_a_registered_existing_folder() {
        let d = tempfile::tempdir().unwrap();
        let first = d.path().join("first");
        let second = d.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let project = ProjectRecord {
            id: "project-1".into(),
            name: "Multi-folder".into(),
            folders: vec![
                first.canonicalize().unwrap().to_string_lossy().into_owned(),
                second
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ],
            primary_folder: Some(first.canonicalize().unwrap().to_string_lossy().into_owned()),
            revision: 1,
            model: None,
            effort: None,
            permission: None,
            hidden: false,
        };
        assert!(choose_project_folder(&project, None)
            .unwrap_err()
            .contains("Choose a project folder"));
        assert!(
            choose_project_folder(&project, Some(d.path().to_string_lossy().as_ref()))
                .unwrap_err()
                .contains("registered to this project")
        );
        assert_eq!(
            choose_project_folder(&project, Some(project.folders[1].as_str())).unwrap(),
            Some(project.folders[1].clone())
        );
    }

    #[tokio::test]
    async fn project_configuration_changes_stale_a_run_proposal() {
        let d = tempfile::tempdir().unwrap();
        let folder = d.path().join("repo");
        std::fs::create_dir_all(&folder).unwrap();
        let store = AgentStore::open(d.path().join("agents")).await.unwrap();
        let projects = ProjectStore::open(d.path().join("projects.sqlite"))
            .await
            .unwrap();
        let project = projects
            .create(crate::projects::ProjectInput {
                name: "Repo".into(),
                folders: vec![folder.to_string_lossy().into_owned()],
                primary_folder: None,
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let draft = store
            .propose_profile(
                "user",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Agent".into(),
                    description: "Ticket runner".into(),
                    instructions: "Work carefully".into(),
                    soul: "Helpful".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, "approve-profile")
            .await
            .unwrap();
        let agent = store
            .apply_profile(&draft.id, &draft.hash, "apply-profile")
            .await
            .unwrap();
        let ticket = request_stores(&store, &projects, "create", json!({
            "projectId":project.id,"title":"Ticket","description":"Do work",
            "acceptanceCriteria":["Verify work"],"agentId":agent.id,"operationId":"create-ticket"
        })).await.unwrap();
        let proposal = request_stores(
            &store,
            &projects,
            "proposeRun",
            json!({
                "ticketId":ticket["id"],"revision":ticket["revision"],"operationId":"propose"
            }),
        )
        .await
        .unwrap();
        let snapshot: TaskInput = serde_json::from_value(proposal["snapshot"].clone()).unwrap();
        let mut changed = project;
        changed.permission = Some(ProjectPermission::ReadOnly);
        let changed = projects.save(changed).await.unwrap();
        let native_ticket = store
            .get_ticket(ticket["id"].as_str().unwrap())
            .await
            .unwrap();
        assert!(validate_snapshot_project(
            &snapshot,
            &native_ticket,
            &changed,
            snapshot.cwd.as_deref()
        )
        .unwrap_err()
        .contains("configuration changed"));
    }

    #[tokio::test]
    async fn multi_folder_ticket_proposals_require_a_registered_folder_choice() {
        let d = tempfile::tempdir().unwrap();
        let first = d.path().join("first");
        let second = d.path().join("second");
        let outside = d.path().join("outside");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let store = AgentStore::open(d.path().join("agents")).await.unwrap();
        let projects = ProjectStore::open(d.path().join("projects.sqlite"))
            .await
            .unwrap();
        let project = projects
            .create(crate::projects::ProjectInput {
                name: "Two folders".into(),
                folders: vec![
                    first.to_string_lossy().into_owned(),
                    second.to_string_lossy().into_owned(),
                ],
                primary_folder: Some(first.to_string_lossy().into_owned()),
                model: None,
                effort: None,
                permission: None,
            })
            .await
            .unwrap();
        let draft = store
            .propose_profile(
                "user",
                None,
                brr_agent_core::ProfileCandidate {
                    name: "Agent".into(),
                    description: "Ticket runner".into(),
                    instructions: "Work carefully".into(),
                    soul: "Helpful".into(),
                    model: "fixture".into(),
                    effort: "medium".into(),
                },
            )
            .await
            .unwrap();
        store
            .approve_profile(&draft.id, &draft.hash, "approve-profile")
            .await
            .unwrap();
        let agent = store
            .apply_profile(&draft.id, &draft.hash, "apply-profile")
            .await
            .unwrap();
        let ticket = request_stores(&store, &projects, "create", json!({
            "projectId":project.id,"title":"Ticket","description":"Do work",
            "acceptanceCriteria":["Verify work"],"agentId":agent.id,"operationId":"create-ticket"
        })).await.unwrap();
        let base = json!({"ticketId":ticket["id"],"revision":ticket["revision"]});
        let mut no_folder = base.clone();
        no_folder["operationId"] = json!("no-folder");
        assert!(request_stores(&store, &projects, "proposeRun", no_folder)
            .await
            .unwrap_err()
            .contains("Choose a project folder"));
        let outside_request = json!({"ticketId":ticket["id"],"revision":ticket["revision"],"folder":outside,"operationId":"outside-folder"});
        assert!(
            request_stores(&store, &projects, "proposeRun", outside_request)
                .await
                .unwrap_err()
                .contains("registered to this project")
        );
        let proposal = request_stores(&store, &projects, "proposeRun", json!({
            "ticketId":ticket["id"],"revision":ticket["revision"],"folder":second,"operationId":"selected-folder"
        })).await.unwrap();
        assert_eq!(proposal["snapshot"]["projectId"], project.id);
        assert_eq!(
            proposal["snapshot"]["cwd"],
            second.canonicalize().unwrap().to_string_lossy().as_ref()
        );
        assert!(proposal["snapshot"]["context"]
            .as_str()
            .unwrap()
            .contains("selectedFolder"));
    }

    #[test]
    fn harness_actor_is_derived_from_binding_and_task_identity() {
        let baseline = brr_agent_core::SessionBinding {
            thread_id: "chat-1".into(),
            agent_id: None,
            kind: "baseline".into(),
            project_id: None,
            cwd: Some("/wrong/cwd".into()),
            task_run_id: None,
            learning_eligible: true,
        };
        let actor = runtime_actor(&baseline, None).unwrap();
        assert_eq!(actor.kind, brr_agent_core::TicketActorKind::Agent);
        assert_eq!(actor.thread_id.as_deref(), Some("chat-1"));
        assert_eq!(actor.task_id, None);
        assert!(runtime_actor(
            &brr_agent_core::SessionBinding {
                kind: "learning".into(),
                ..baseline
            },
            None
        )
        .is_err());
        let task = brr_agent_core::SessionBinding {
            thread_id: "task-thread".into(),
            agent_id: Some("agent-1".into()),
            kind: "task".into(),
            project_id: Some("/registered/project".into()),
            cwd: Some("/worktree/path".into()),
            task_run_id: Some("run-1".into()),
            learning_eligible: false,
        };
        let actor = runtime_actor(&task, Some("task-1".into())).unwrap();
        assert_eq!(actor.kind, brr_agent_core::TicketActorKind::Agent);
        assert_eq!(actor.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(actor.task_id.as_deref(), Some("task-1"));
    }
}
