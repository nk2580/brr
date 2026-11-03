use jiff::{
    Timestamp,
    tz::{AmbiguousOffset, TimeZone},
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::{
    AgentStore, id, now,
    tasks::{DelegationNode, Task, TaskInput, insert_task, task_from_input},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInput {
    pub name: String,
    pub task: TaskInput,
    pub expression: String,
    pub timezone: String,
    #[serde(default)]
    pub nodes: Option<Vec<DelegationNode>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub agent_id: String,
    pub expression: String,
    pub timezone: String,
    pub paused: bool,
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_status: Option<String>,
}

pub(crate) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("CREATE TABLE IF NOT EXISTS workflows(id TEXT PRIMARY KEY,name TEXT NOT NULL,agent_id TEXT NOT NULL,task TEXT NOT NULL,nodes TEXT,expression TEXT NOT NULL,timezone TEXT NOT NULL,paused INTEGER NOT NULL,next_run_at INTEGER,last_run_at INTEGER,last_status TEXT,active_occurrence TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL)")
        .execute(pool).await.map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS workflow_occurrences(id TEXT PRIMARY KEY,workflow_id TEXT NOT NULL REFERENCES workflows(id),occurrence_at INTEGER NOT NULL,graph_id TEXT,status TEXT NOT NULL,created_at INTEGER NOT NULL,UNIQUE(workflow_id,occurrence_at))")
        .execute(pool).await.map_err(err)?;
    let has_workflow_id: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('delegation_proposals') WHERE name='workflow_id'",
    )
    .fetch_one(pool)
    .await
    .map_err(err)?;
    if has_workflow_id == 0 {
        sqlx::query("ALTER TABLE delegation_proposals ADD COLUMN workflow_id TEXT")
            .execute(pool)
            .await
            .map_err(err)?;
    }
    Ok(())
}

impl AgentStore {
    pub async fn create_workflow(&self, input: WorkflowInput) -> Result<Workflow, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let workflow = create_workflow_tx(&mut tx, input).await?;
        tx.commit().await.map_err(err)?;
        Ok(workflow)
    }

    /// Turns one reviewed, immutable delegation graph into a recurring workflow.
    /// The workflow insert and proposal transition commit atomically.
    pub async fn schedule_delegation(
        &self,
        proposal_id: &str,
        hash: &str,
        name: &str,
        expression: &str,
        timezone: &str,
    ) -> Result<Workflow, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let row = sqlx::query("SELECT parent_task_id,hash,status,tasks,workflow_id FROM delegation_proposals WHERE id=?")
            .bind(proposal_id).fetch_optional(&mut *tx).await.map_err(err)?
            .ok_or_else(|| "delegation proposal not found".to_string())?;
        let parent_id: String = row.try_get("parent_task_id").map_err(err)?;
        let stored_hash: String = row.try_get("hash").map_err(err)?;
        let status: String = row.try_get("status").map_err(err)?;
        let nodes_json: String = row.try_get("tasks").map_err(err)?;
        let workflow_id: Option<String> = row.try_get("workflow_id").map_err(err)?;
        let nodes: Vec<DelegationNode> = serde_json::from_str(&nodes_json).map_err(err)?;
        if hash != stored_hash || crate::digest(&nodes)? != stored_hash {
            return Err("delegation review hash does not match the proposed graph".into());
        }
        if status == "scheduled" {
            let workflow_id =
                workflow_id.ok_or_else(|| "scheduled proposal has no workflow link".to_string())?;
            let workflow_row = sqlx::query("SELECT * FROM workflows WHERE id=?")
                .bind(&workflow_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(err)?
                .ok_or_else(|| "scheduled workflow no longer exists".to_string())?;
            let saved_nodes: Option<String> = workflow_row.try_get("nodes").map_err(err)?;
            let workflow = row_workflow(workflow_row)?;
            let saved_nodes: Vec<DelegationNode> = serde_json::from_str(
                saved_nodes
                    .as_deref()
                    .ok_or_else(|| "scheduled workflow has no graph".to_string())?,
            )
            .map_err(err)?;
            if workflow.name != name
                || workflow.expression != expression
                || workflow.timezone != timezone
                || saved_nodes != nodes
            {
                return Err("proposal was already scheduled with different arguments".into());
            }
            tx.commit().await.map_err(err)?;
            return Ok(workflow);
        }
        if status != "proposed" || workflow_id.is_some() {
            return Err("delegation proposal is no longer pending for scheduling".into());
        }
        let parent = sqlx::query("SELECT * FROM tasks WHERE id=?")
            .bind(&parent_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(err)?
            .ok_or_else(|| "delegation parent task not found".to_string())?;
        let template = task_input_from_row(&parent)?;
        let workflow = create_workflow_tx(
            &mut tx,
            WorkflowInput {
                name: name.into(),
                task: template,
                expression: expression.into(),
                timezone: timezone.into(),
                nodes: Some(nodes),
            },
        )
        .await?;
        let changed = sqlx::query("UPDATE delegation_proposals SET status='scheduled',workflow_id=? WHERE id=? AND status='proposed' AND hash=?")
            .bind(&workflow.id).bind(proposal_id).bind(hash).execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() != 1 {
            return Err("delegation proposal changed while it was being scheduled".into());
        }
        let payload = serde_json::json!({"proposalId":proposal_id,"hash":hash,"workflow":workflow});
        sqlx::query("INSERT INTO events(kind,entity_id,payload,created_at) VALUES('task.delegation_scheduled',?,?,?)")
            .bind(proposal_id).bind(serde_json::to_string(&payload).map_err(err)?).bind(now())
            .execute(&mut *tx).await.map_err(err)?;
        tx.commit().await.map_err(err)?;
        Ok(workflow)
    }

    pub async fn list_workflows(&self, agent_id: Option<&str>) -> Result<Vec<Workflow>, String> {
        let rows = if let Some(agent_id) = agent_id {
            sqlx::query("SELECT * FROM workflows WHERE agent_id=? ORDER BY created_at,id")
                .bind(agent_id)
                .fetch_all(&self.pool)
                .await
                .map_err(err)?
        } else {
            sqlx::query("SELECT * FROM workflows ORDER BY created_at,id")
                .fetch_all(&self.pool)
                .await
                .map_err(err)?
        };
        rows.into_iter().map(row_workflow).collect()
    }

    pub async fn control_workflow(
        &self,
        workflow_id: &str,
        paused: bool,
    ) -> Result<Workflow, String> {
        let stamp = now();
        let result = sqlx::query("UPDATE workflows SET paused=?,updated_at=? WHERE id=?")
            .bind(paused)
            .bind(stamp)
            .bind(workflow_id)
            .execute(&self.pool)
            .await
            .map_err(err)?;
        if result.rows_affected() != 1 {
            return Err("workflow not found".into());
        }
        row_workflow(
            sqlx::query("SELECT * FROM workflows WHERE id=?")
                .bind(workflow_id)
                .fetch_one(&self.pool)
                .await
                .map_err(err)?,
        )
    }

    /// Materializes due occurrences atomically. Missed intervals coalesce to the latest due minute.
    pub async fn tick_workflows(&self, at: i64) -> Result<Vec<Task>, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let rows=sqlx::query("SELECT * FROM workflows w WHERE paused=0 AND ((next_run_at IS NOT NULL AND next_run_at<=?) OR (active_occurrence IS NULL AND EXISTS (SELECT 1 FROM workflow_occurrences o WHERE o.workflow_id=w.id AND o.status='pending'))) ORDER BY id")
            .bind(at).fetch_all(&mut *tx).await.map_err(err)?;
        let mut created = Vec::new();
        for row in rows {
            let active: Option<String> = row.try_get("active_occurrence").map_err(err)?;
            let workflow = row_workflow(row)?;
            let cron = Cron::parse(&workflow.expression)?;
            let zone = TimeZone::get(&workflow.timezone).map_err(err)?;
            let due = if workflow.next_run_at.is_some_and(|n| n <= at) {
                Some(latest_match(
                    &cron,
                    &zone,
                    workflow.next_run_at.unwrap(),
                    at,
                )?)
            } else {
                None
            };
            let following = due.map(|at| next_match(&cron, &zone, at)).transpose()?;
            if let Some(due) = due {
                if active.is_some() {
                    sqlx::query(
                        "DELETE FROM workflow_occurrences WHERE workflow_id=? AND status='pending'",
                    )
                    .bind(&workflow.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
                    sqlx::query("INSERT OR IGNORE INTO workflow_occurrences(id,workflow_id,occurrence_at,status,created_at) VALUES(?,?,?,'pending',?)")
                    .bind(id()).bind(&workflow.id).bind(due).bind(at).execute(&mut *tx).await.map_err(err)?;
                } else {
                    // Keep the latest missed occurrence, even if a prior pending row was awaiting dispatch.
                    sqlx::query(
                        "DELETE FROM workflow_occurrences WHERE workflow_id=? AND status='pending'",
                    )
                    .bind(&workflow.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
                    let occurrence_id = id();
                    sqlx::query("INSERT OR IGNORE INTO workflow_occurrences(id,workflow_id,occurrence_at,status,created_at) VALUES(?,?,?,'active',?)")
                    .bind(&occurrence_id).bind(&workflow.id).bind(due).bind(at).execute(&mut *tx).await.map_err(err)?;
                    let inserted=sqlx::query("SELECT id,status FROM workflow_occurrences WHERE workflow_id=? AND occurrence_at=?")
                    .bind(&workflow.id).bind(due).fetch_one(&mut *tx).await.map_err(err)?;
                    let occurrence_id: String = inserted.get("id");
                    if inserted.get::<String, _>("status") == "active" {
                        let graph =
                            materialize_workflow(&mut tx, &workflow, &occurrence_id, at).await?;
                        created.extend(graph);
                        sqlx::query("UPDATE workflow_occurrences SET graph_id=? WHERE id=?")
                            .bind(&occurrence_id)
                            .bind(&occurrence_id)
                            .execute(&mut *tx)
                            .await
                            .map_err(err)?;
                        sqlx::query("UPDATE workflows SET active_occurrence=?,last_run_at=?,last_status='running' WHERE id=?")
                        .bind(&occurrence_id).bind(due).bind(&workflow.id).execute(&mut *tx).await.map_err(err)?;
                    }
                }
            } else if active.is_none() {
                let pending=sqlx::query("SELECT id,occurrence_at FROM workflow_occurrences WHERE workflow_id=? AND status='pending' ORDER BY occurrence_at DESC LIMIT 1")
                    .bind(&workflow.id).fetch_optional(&mut *tx).await.map_err(err)?;
                if let Some(pending) = pending {
                    let occurrence_id: String = pending.get("id");
                    let pending_at: i64 = pending.get("occurrence_at");
                    sqlx::query("DELETE FROM workflow_occurrences WHERE workflow_id=? AND status='pending' AND id!=?").bind(&workflow.id).bind(&occurrence_id).execute(&mut *tx).await.map_err(err)?;
                    sqlx::query("UPDATE workflow_occurrences SET status='active' WHERE id=? AND status='pending'").bind(&occurrence_id).execute(&mut *tx).await.map_err(err)?;
                    let graph =
                        materialize_workflow(&mut tx, &workflow, &occurrence_id, at).await?;
                    created.extend(graph);
                    sqlx::query("UPDATE workflow_occurrences SET graph_id=? WHERE id=?")
                        .bind(&occurrence_id)
                        .bind(&occurrence_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(err)?;
                    sqlx::query("UPDATE workflows SET active_occurrence=?,last_run_at=?,last_status='running' WHERE id=?").bind(&occurrence_id).bind(pending_at).bind(&workflow.id).execute(&mut *tx).await.map_err(err)?;
                }
            }
            if let Some(following) = following {
                sqlx::query("UPDATE workflows SET next_run_at=?,updated_at=? WHERE id=?")
                    .bind(following)
                    .bind(at)
                    .bind(&workflow.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
            }
        }
        tx.commit().await.map_err(err)?;
        Ok(created)
    }
}

pub(crate) async fn update_workflow_after_settlement(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: &str,
    stamp: i64,
) -> Result<(), String> {
    let row = sqlx::query("SELECT workflow_id,occurrence_id FROM tasks WHERE id=?")
        .bind(task_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(err)?;
    let Some(row) = row else { return Ok(()) };
    let workflow_id: Option<String> = row.get("workflow_id");
    let occurrence_id: Option<String> = row.get("occurrence_id");
    let (Some(workflow_id), Some(occurrence_id)) = (workflow_id, occurrence_id) else {
        return Ok(());
    };
    let failed:i64=sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE occurrence_id=? AND status IN ('failed','blocked','cancelled')").bind(&occurrence_id).fetch_one(&mut **tx).await.map_err(err)?;
    if failed > 0 {
        sqlx::query("UPDATE workflows SET last_status='blocked',updated_at=? WHERE id=?")
            .bind(stamp)
            .bind(workflow_id)
            .execute(&mut **tx)
            .await
            .map_err(err)?;
        return Ok(());
    }
    let incomplete: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tasks WHERE occurrence_id=? AND status!='succeeded'",
    )
    .bind(&occurrence_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(err)?;
    if incomplete == 0 {
        sqlx::query("UPDATE workflow_occurrences SET status='succeeded' WHERE id=?")
            .bind(&occurrence_id)
            .execute(&mut **tx)
            .await
            .map_err(err)?;
        sqlx::query("UPDATE workflows SET active_occurrence=NULL,last_status='succeeded',updated_at=? WHERE id=? AND active_occurrence=?").bind(stamp).bind(workflow_id).bind(occurrence_id).execute(&mut **tx).await.map_err(err)?;
    }
    Ok(())
}

async fn create_workflow_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: WorkflowInput,
) -> Result<Workflow, String> {
    validate_workflow_input_tx(tx, &input).await?;
    let cron = Cron::parse(&input.expression)?;
    let zone = TimeZone::get(&input.timezone).map_err(err)?;
    let stamp = now();
    let next = next_match(&cron, &zone, stamp)?;
    let workflow = Workflow {
        id: id(),
        name: input.name,
        agent_id: input.task.agent_id.clone(),
        expression: input.expression,
        timezone: input.timezone,
        paused: false,
        next_run_at: Some(next),
        last_run_at: None,
        last_status: None,
    };
    sqlx::query("INSERT INTO workflows(id,name,agent_id,task,nodes,expression,timezone,paused,next_run_at,created_at,updated_at) VALUES(?,?,?,?,?,?,?,0,?,?,?)")
        .bind(&workflow.id).bind(&workflow.name).bind(&workflow.agent_id)
        .bind(serde_json::to_string(&input.task).map_err(err)?)
        .bind(input.nodes.as_ref().map(serde_json::to_string).transpose().map_err(err)?)
        .bind(&workflow.expression).bind(&workflow.timezone).bind(next).bind(stamp).bind(stamp)
        .execute(&mut **tx).await.map_err(err)?;
    Ok(workflow)
}

async fn validate_workflow_input_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &WorkflowInput,
) -> Result<(), String> {
    if input.name.trim().is_empty() {
        return Err("workflow name and agent are required".into());
    }
    validate_workflow_task_tx(tx, &input.task).await?;
    if let Some(nodes) = &input.nodes {
        validate_nodes(nodes)?;
        if nodes.is_empty() {
            return Err("workflow graph needs at least one node".into());
        }
        for node in nodes {
            validate_workflow_task_tx(tx, &node.task).await?;
        }
    }
    Ok(())
}

async fn validate_workflow_task_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &TaskInput,
) -> Result<(), String> {
    if input.agent_id.trim().is_empty()
        || input.title.trim().is_empty()
        || input.instructions.trim().is_empty()
    {
        return Err("agent, title and instructions are required".into());
    }
    if input
        .dependencies
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != input.dependencies.len()
    {
        return Err("task dependencies must be unique".into());
    }
    let has_profile: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_profiles WHERE id=?")
        .bind(&input.agent_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(err)?;
    if has_profile == 0 {
        return Err(format!("agent profile {} does not exist", input.agent_id));
    }
    for dependency in &input.dependencies {
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE id=?")
            .bind(dependency)
            .fetch_one(&mut **tx)
            .await
            .map_err(err)?;
        if exists == 0 {
            return Err(format!("dependency task {dependency} does not exist"));
        }
    }
    Ok(())
}

fn task_input_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<TaskInput, String> {
    Ok(TaskInput {
        agent_id: row.try_get("agent_id").map_err(err)?,
        title: row.try_get("title").map_err(err)?,
        instructions: row.try_get("instructions").map_err(err)?,
        acceptance_criteria: serde_json::from_str(
            &row.try_get::<String, _>("acceptance_criteria")
                .map_err(err)?,
        )
        .map_err(err)?,
        context: row.try_get("context").map_err(err)?,
        cwd: row.try_get("cwd").map_err(err)?,
        project_id: row.try_get("project_id").map_err(err)?,
        model: row.try_get("model").map_err(err)?,
        effort: row.try_get("effort").map_err(err)?,
        workspace_permission: row
            .try_get::<Option<String>, _>("workspace_permission")
            .map_err(err)?
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(err)?,
        dependencies: serde_json::from_str(&row.try_get::<String, _>("dependencies").map_err(err)?)
            .map_err(err)?,
    })
}

async fn materialize_workflow(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    workflow: &Workflow,
    occurrence: &str,
    stamp: i64,
) -> Result<Vec<Task>, String> {
    let row = sqlx::query("SELECT task,nodes FROM workflows WHERE id=?")
        .bind(&workflow.id)
        .fetch_one(&mut **tx)
        .await
        .map_err(err)?;
    let root: TaskInput = serde_json::from_str(&row.get::<String, _>("task")).map_err(err)?;
    let nodes: Option<String> = row.get("nodes");
    let Some(nodes_json) = nodes else {
        let task = task_from_input(id(), root, "queued", None, None, None, None, stamp);
        insert_task(tx, &task, None, None).await?;
        sqlx::query("UPDATE tasks SET workflow_id=?,occurrence_id=? WHERE id=?")
            .bind(&workflow.id)
            .bind(occurrence)
            .bind(&task.id)
            .execute(&mut **tx)
            .await
            .map_err(err)?;
        return Ok(vec![task]);
    };
    let nodes: Vec<DelegationNode> = serde_json::from_str(&nodes_json).map_err(err)?;
    let mut ids = std::collections::HashMap::new();
    for node in &nodes {
        ids.insert(node.key.clone(), id());
    }
    let mut out = Vec::new();
    for node in &nodes {
        let mut input = node.task.clone();
        let local_dependencies = node
            .depends_on
            .iter()
            .map(|key| {
                ids.get(key)
                    .cloned()
                    .ok_or_else(|| format!("unknown workflow dependency {key}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        input.dependencies.extend(local_dependencies);
        input.dependencies.sort();
        input.dependencies.dedup();
        let task = task_from_input(
            ids[&node.key].clone(),
            input,
            "queued",
            None,
            None,
            None,
            None,
            stamp,
        );
        insert_task(tx, &task, None, None).await?;
        sqlx::query("UPDATE tasks SET workflow_id=?,occurrence_id=? WHERE id=?")
            .bind(&workflow.id)
            .bind(occurrence)
            .bind(&task.id)
            .execute(&mut **tx)
            .await
            .map_err(err)?;
        out.push(task);
    }
    Ok(out)
}

fn row_workflow(row: sqlx::sqlite::SqliteRow) -> Result<Workflow, String> {
    Ok(Workflow {
        id: row.try_get("id").map_err(err)?,
        name: row.try_get("name").map_err(err)?,
        agent_id: row.try_get("agent_id").map_err(err)?,
        expression: row.try_get("expression").map_err(err)?,
        timezone: row.try_get("timezone").map_err(err)?,
        paused: row.try_get::<i64, _>("paused").map_err(err)? != 0,
        next_run_at: row.try_get("next_run_at").map_err(err)?,
        last_run_at: row.try_get("last_run_at").map_err(err)?,
        last_status: row.try_get("last_status").map_err(err)?,
    })
}

#[derive(Clone)]
struct Cron {
    min: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
    dom_star: bool,
    dow_star: bool,
}
#[derive(Clone)]
struct Field {
    values: Vec<bool>,
    min: u8,
    max: u8,
}
impl Cron {
    fn parse(expr: &str) -> Result<Self, String> {
        let parts: Vec<_> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err("cron expression must contain five fields".into());
        }
        let dom = Field::parse(parts[2], 1, 31)?;
        let dow = Field::parse(parts[4], 0, 7)?.normalize_sunday();
        let dom_star = dom.is_all();
        let dow_star = dow.is_all();
        Ok(Self {
            min: Field::parse(parts[0], 0, 59)?,
            hour: Field::parse(parts[1], 0, 23)?,
            dom,
            month: Field::parse(parts[3], 1, 12)?,
            dow,
            dom_star,
            dow_star,
        })
    }
    fn matches(&self, dt: jiff::civil::DateTime) -> bool {
        let weekday = dt.weekday().to_sunday_zero_offset() as u8;
        self.min.has(dt.minute() as u8)
            && self.hour.has(dt.hour() as u8)
            && self.month.has(dt.month() as u8)
            && if self.dom_star && self.dow_star {
                self.dom.has(dt.day() as u8) && self.dow.has(weekday)
            } else if self.dom_star {
                self.dow.has(weekday)
            } else if self.dow_star {
                self.dom.has(dt.day() as u8)
            } else {
                self.dom.has(dt.day() as u8) || self.dow.has(weekday)
            }
    }
}
impl Field {
    fn is_all(&self) -> bool {
        (self.min..=self.max).all(|value| self.has(value))
    }

    fn parse(input: &str, min: u8, max: u8) -> Result<Self, String> {
        let mut values = vec![false; max as usize + 1];
        for part in input.split(',') {
            if part.is_empty() {
                return Err("empty cron list item".into());
            }
            let mut step_split = part.split('/');
            let base = step_split.next().unwrap();
            let step = step_split
                .next()
                .map(|s| s.parse::<u8>().map_err(err))
                .transpose()?
                .unwrap_or(1);
            if step == 0 || step_split.next().is_some() {
                return Err("cron step must be a single positive integer".into());
            }
            let (start, end) = if base == "*" {
                (min, max)
            } else if let Some((a, b)) = base.split_once('-') {
                (parse_num(a, min, max)?, parse_num(b, min, max)?)
            } else {
                let n = parse_num(base, min, max)?;
                (n, if part.contains('/') { max } else { n })
            };
            if start > end {
                return Err("cron range must increase".into());
            }
            let mut n = start;
            while n <= end {
                values[n as usize] = true;
                match n.checked_add(step) {
                    Some(x) => n = x,
                    None => break,
                }
            }
        }
        Ok(Self { values, min, max })
    }
    fn has(&self, v: u8) -> bool {
        v >= self.min && v <= self.max && self.values[v as usize]
    }
    fn normalize_sunday(mut self) -> Self {
        if self.has(7) {
            self.values[0] = true;
        }
        self
    }
}
fn parse_num(s: &str, min: u8, max: u8) -> Result<u8, String> {
    let n = s
        .parse::<u8>()
        .map_err(|_| "cron fields accept numeric values only".to_string())?;
    if n < min || n > max {
        Err(format!("cron value {n} is outside {min}..={max}"))
    } else {
        Ok(n)
    }
}

fn next_match(cron: &Cron, zone: &TimeZone, after: i64) -> Result<i64, String> {
    let mut candidate = after.div_euclid(60).saturating_add(1).saturating_mul(60);
    let end = candidate.saturating_add(60 * 60 * 24 * 366 * 8);
    while candidate <= end {
        if let Some(z) = Timestamp::from_second(candidate)
            .ok()
            .map(|ts| ts.to_zoned(zone.clone()))
        {
            let dt = z.datetime();
            if cron.matches(dt) {
                let ambiguous = zone.to_ambiguous_timestamp(dt);
                match ambiguous.offset() {
                    AmbiguousOffset::Gap { .. } => {}
                    AmbiguousOffset::Fold { .. }
                        if ambiguous.earlier().map_err(err)?.as_second() != candidate => {}
                    _ => return Ok(candidate),
                }
            }
        }
        candidate += 60;
    }
    Err("cron expression has no occurrence within eight years".into())
}
fn latest_match(cron: &Cron, zone: &TimeZone, first: i64, through: i64) -> Result<i64, String> {
    let mut candidate = first;
    let mut latest = None;
    while candidate <= through {
        if let Some(z) = Timestamp::from_second(candidate)
            .ok()
            .map(|ts| ts.to_zoned(zone.clone()))
        {
            let dt = z.datetime();
            if cron.matches(dt) {
                let ambiguous = zone.to_ambiguous_timestamp(dt);
                match ambiguous.offset() {
                    AmbiguousOffset::Gap { .. } => {}
                    AmbiguousOffset::Fold { .. }
                        if ambiguous.earlier().map_err(err)?.as_second() != candidate => {}
                    _ => latest = Some(candidate),
                }
            }
        }
        candidate = candidate.saturating_add(60);
    }
    latest.ok_or_else(|| "no due cron occurrence found".into())
}
fn validate_nodes(nodes: &[DelegationNode]) -> Result<(), String> {
    let keys: std::collections::HashSet<&str> = nodes.iter().map(|n| n.key.as_str()).collect();
    if keys.len() != nodes.len() || keys.contains("") {
        return Err("workflow node keys must be non-empty and unique".into());
    }
    let mut edges = std::collections::HashMap::<&str, Vec<&str>>::new();
    let mut degrees = std::collections::HashMap::<&str, usize>::new();
    for n in nodes {
        degrees.insert(n.key.as_str(), 0);
    }
    for n in nodes {
        for dep in &n.depends_on {
            if !keys.contains(dep.as_str()) {
                return Err(format!("unknown workflow dependency {dep}"));
            }
            *degrees.get_mut(n.key.as_str()).unwrap() += 1;
            edges.entry(dep).or_default().push(n.key.as_str());
        }
    }
    let mut q: Vec<_> = degrees
        .iter()
        .filter_map(|(k, d)| (*d == 0).then_some(*k))
        .collect();
    let mut seen = 0;
    while let Some(k) = q.pop() {
        seen += 1;
        for child in edges.get(k).into_iter().flatten() {
            let d = degrees.get_mut(child).unwrap();
            *d -= 1;
            if *d == 0 {
                q.push(child)
            }
        }
    }
    if seen != nodes.len() {
        return Err("workflow graph contains a cycle".into());
    }
    Ok(())
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[cfg(test)]
mod cron_tests {
    use super::Cron;

    #[test]
    fn full_step_wildcard_uses_wildcard_day_of_month_semantics() {
        let dt = jiff::civil::Date::new(2026, 11, 3).unwrap().at(0, 0, 0, 0);
        let explicit = Cron::parse("0 0 * 11 1").unwrap();
        let stepped = Cron::parse("0 0 */1 11 1").unwrap();
        assert_eq!(stepped.matches(dt), explicit.matches(dt));
        assert!(!stepped.matches(dt));
    }
}
