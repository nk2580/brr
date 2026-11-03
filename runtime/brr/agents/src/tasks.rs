use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

use crate::{AgentStore, digest, id, now};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    pub agent_id: String,
    pub title: String,
    pub instructions: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub workspace_permission: Option<WorkspacePermission>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspacePermission {
    ReadOnly,
    WorkspaceWrite,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub instructions: String,
    pub acceptance_criteria: Vec<String>,
    pub context: Option<String>,
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub workspace_permission: Option<WorkspacePermission>,
    pub dependencies: Vec<String>,
    pub status: String,
    pub reason: Option<String>,
    pub run_id: Option<String>,
    pub thread_id: Option<String>,
    pub result: Option<String>,
    pub evidence: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskRun {
    pub id: String,
    pub task_id: String,
    pub agent_id: String,
    pub worker_id: String,
    pub lease_token: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub state: String,
    pub created_at: i64,
    pub settled_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskAction {
    Pause,
    Resume,
    Cancel,
    Retry,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskSettings {
    pub max_background: u8,
    pub max_per_agent: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DelegationNode {
    pub key: String,
    pub task: TaskInput,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DelegationProposal {
    pub id: String,
    pub parent_task_id: String,
    pub hash: String,
    pub status: String,
    pub tasks: Vec<DelegationNode>,
    pub created_at: i64,
}

pub(crate) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, title TEXT NOT NULL,
            instructions TEXT NOT NULL, acceptance_criteria TEXT NOT NULL,
            context TEXT, cwd TEXT, project_id TEXT, model TEXT, effort TEXT,
            dependencies TEXT NOT NULL, status TEXT NOT NULL, reason TEXT,
            run_id TEXT, thread_id TEXT, result TEXT, evidence TEXT,
            parent_task_id TEXT, delegation_id TEXT, delegation_index INTEGER, workflow_id TEXT, occurrence_id TEXT,
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            workspace_permission TEXT, ticket_id TEXT, ticket_status_revision INTEGER,
            ticket_status_started INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await
    .map_err(err)?;
    for (column, migration) in [
        (
            "workspace_permission",
            "ALTER TABLE tasks ADD COLUMN workspace_permission TEXT",
        ),
        ("ticket_id", "ALTER TABLE tasks ADD COLUMN ticket_id TEXT"),
        (
            "ticket_status_revision",
            "ALTER TABLE tasks ADD COLUMN ticket_status_revision INTEGER",
        ),
        (
            "ticket_status_started",
            "ALTER TABLE tasks ADD COLUMN ticket_status_started INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name=?")
                .bind(column)
                .fetch_one(pool)
                .await
                .map_err(err)?;
        if exists == 0 {
            sqlx::query(migration).execute(pool).await.map_err(err)?;
        }
    }
    sqlx::query("CREATE UNIQUE INDEX IF NOT EXISTS one_unfinished_task_per_ticket ON tasks(ticket_id) WHERE ticket_id IS NOT NULL AND status NOT IN ('succeeded','failed','cancelled')")
        .execute(pool).await.map_err(err)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS tasks_ticket_history ON tasks(ticket_id)")
        .execute(pool).await.map_err(err)?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS task_runs (
            id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id),
            agent_id TEXT NOT NULL, worker_id TEXT NOT NULL, lease_token TEXT NOT NULL,
            thread_id TEXT, turn_id TEXT, state TEXT NOT NULL,
            proposed_result TEXT, proposed_evidence TEXT,
            created_at INTEGER NOT NULL, settled_at INTEGER
        )",
    )
    .execute(pool)
    .await
    .map_err(err)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS task_runs_unsettled ON task_runs(state,agent_id)")
        .execute(pool)
        .await
        .map_err(err)?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS delegation_proposals (
            id TEXT PRIMARY KEY, parent_task_id TEXT NOT NULL REFERENCES tasks(id),
            hash TEXT NOT NULL, status TEXT NOT NULL, tasks TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .map_err(err)?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS task_settings (
            id INTEGER PRIMARY KEY CHECK(id=1), max_background INTEGER NOT NULL,
            max_per_agent INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .map_err(err)?;
    sqlx::query(
        "INSERT OR IGNORE INTO task_settings(id,max_background,max_per_agent) VALUES(1,4,2)",
    )
    .execute(pool)
    .await
    .map_err(err)?;
    Ok(())
}

impl AgentStore {
    pub async fn create_task(&self, input: TaskInput) -> Result<Task, String> {
        validate_input(&input)?;
        validate_agent(&self.pool, &input.agent_id).await?;
        validate_dependencies(&self.pool, &input.dependencies, None).await?;
        let task = task_from_input(id(), input, "queued", None, None, None, None, now());
        let mut tx = self.pool.begin().await.map_err(err)?;
        insert_task(&mut tx, &task, None, None).await?;
        event(&mut tx, "task.created", &task.id, &task).await?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    /// Idempotent creation for host retries after a lost invoke response.
    pub async fn create_task_once(
        &self,
        input: TaskInput,
        operation_id: &str,
    ) -> Result<Task, String> {
        validate_input(&input)?;
        if operation_id.trim().is_empty() {
            return Err("operation id is required".into());
        }
        let input_hash = digest(&input)?;
        let mut tx = self.pool.begin().await.map_err(err)?;
        validate_agent_tx(&mut tx, &input.agent_id).await?;
        if let Some((kind, existing)) = sqlx::query_as::<_, (String, String)>(
            "SELECT kind,result FROM operation_results WHERE operation_id=?",
        )
        .bind(operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?
        {
            tx.rollback().await.map_err(err)?;
            if kind != "task.create" {
                return Err("operation id was already used for another operation".into());
            }
            let stored: serde_json::Value = serde_json::from_str(&existing).map_err(serr)?;
            if stored.get("inputHash").and_then(|v| v.as_str()) != Some(input_hash.as_str()) {
                return Err("operation id was already used with different task input".into());
            }
            return parse(
                stored
                    .get("task")
                    .cloned()
                    .ok_or_else(|| "stored task result is invalid".to_string())?
                    .to_string(),
            );
        }
        validate_dependencies_tx(&mut tx, &input.dependencies, None).await?;
        let task = task_from_input(id(), input, "queued", None, None, None, None, now());
        insert_task(&mut tx, &task, None, None).await?;
        event(&mut tx, "task.created", &task.id, &task).await?;
        sqlx::query(
            "INSERT INTO operation_results(operation_id,kind,result) VALUES(?,'task.create',?)",
        )
        .bind(operation_id)
        .bind(serde_json::to_string(&json!({"inputHash":input_hash,"task":task})).map_err(serr)?)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    pub async fn create_task_paused(&self, input: TaskInput) -> Result<Task, String> {
        validate_input(&input)?;
        validate_agent(&self.pool, &input.agent_id).await?;
        validate_dependencies(&self.pool, &input.dependencies, None).await?;
        let task = task_from_input(
            id(),
            input,
            "paused",
            Some("awaiting delegation review".into()),
            None,
            None,
            None,
            now(),
        );
        let mut tx = self.pool.begin().await.map_err(err)?;
        insert_task(&mut tx, &task, None, None).await?;
        event(&mut tx, "task.created", &task.id, &task).await?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    pub async fn list_tasks(&self, agent_id: Option<&str>) -> Result<Vec<Task>, String> {
        let rows = if let Some(agent_id) = agent_id {
            sqlx::query("SELECT * FROM tasks WHERE agent_id=? ORDER BY created_at,id")
                .bind(agent_id)
                .fetch_all(&self.pool)
                .await
                .map_err(err)?
        } else {
            sqlx::query("SELECT * FROM tasks ORDER BY created_at,id")
                .fetch_all(&self.pool)
                .await
                .map_err(err)?
        };
        rows.into_iter().map(row_task).collect()
    }

    /// Fetches an ordered page; `after` is the last task ID from the prior page.
    pub async fn list_tasks_page(
        &self,
        agent_id: Option<&str>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Task>, String> {
        let limit = limit.clamp(1, 200);
        let rows=match (agent_id,after) {
            (Some(agent),Some(cursor))=>sqlx::query("SELECT t.* FROM tasks t WHERE t.agent_id=? AND (t.created_at,t.id)>(SELECT c.created_at,c.id FROM tasks c WHERE c.id=?) ORDER BY t.created_at,t.id LIMIT ?").bind(agent).bind(cursor).bind(limit).fetch_all(&self.pool).await.map_err(err)?,
            (Some(agent),None)=>sqlx::query("SELECT * FROM tasks WHERE agent_id=? ORDER BY created_at,id LIMIT ?").bind(agent).bind(limit).fetch_all(&self.pool).await.map_err(err)?,
            (None,Some(cursor))=>sqlx::query("SELECT t.* FROM tasks t WHERE (t.created_at,t.id)>(SELECT c.created_at,c.id FROM tasks c WHERE c.id=?) ORDER BY t.created_at,t.id LIMIT ?").bind(cursor).bind(limit).fetch_all(&self.pool).await.map_err(err)?,
            (None,None)=>sqlx::query("SELECT * FROM tasks ORDER BY created_at,id LIMIT ?").bind(limit).fetch_all(&self.pool).await.map_err(err)?,
        };
        rows.into_iter().map(row_task).collect()
    }

    pub async fn get_task(&self, task_id: &str) -> Result<Task, String> {
        let row = sqlx::query("SELECT * FROM tasks WHERE id=?")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(err)?
            .ok_or_else(|| "task not found".to_string())?;
        row_task(row)
    }

    pub async fn claim_ready(&self, worker_id: &str) -> Result<Option<TaskRun>, String> {
        self.claim_ready_excluding_workspaces(worker_id, &[]).await
    }

    /// Claim a ready task while leaving workspaces held by active host runs available to resume.
    /// The exclusion is applied inside the same transaction as capacity admission and claiming.
    pub async fn claim_ready_excluding_workspaces(
        &self,
        worker_id: &str,
        excluded_cwds: &[String],
    ) -> Result<Option<TaskRun>, String> {
        if worker_id.trim().is_empty() {
            return Err("worker id is required".into());
        }
        let mut tx = self.pool.begin().await.map_err(err)?;
        let settings =
            sqlx::query("SELECT max_background,max_per_agent FROM task_settings WHERE id=1")
                .fetch_one(&mut *tx)
                .await
                .map_err(err)?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_runs WHERE state='running'")
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
        if total >= settings.get::<i64, _>("max_background") {
            tx.rollback().await.map_err(err)?;
            return Ok(None);
        }
        let mut candidate_query = QueryBuilder::<Sqlite>::new(
            "SELECT t.* FROM tasks t WHERE t.status='queued'
             AND NOT EXISTS (SELECT 1 FROM json_each(t.dependencies) d
                 LEFT JOIN tasks dep ON dep.id=d.value WHERE dep.id IS NULL OR dep.status!='succeeded')
             AND (t.parent_task_id IS NULL OR EXISTS (
                 SELECT 1 FROM delegation_proposals p WHERE p.id=t.delegation_id AND p.status='approved'))",
        );
        if !excluded_cwds.is_empty() {
            candidate_query.push(" AND (t.cwd IS NULL OR t.cwd NOT IN (");
            let mut separated = candidate_query.separated(",");
            for cwd in excluded_cwds {
                separated.push_bind(cwd);
            }
            candidate_query.push("))");
        }
        let candidates = candidate_query
            .push(" ORDER BY t.created_at,t.id")
            .build()
            .fetch_all(&mut *tx)
            .await
            .map_err(err)?;
        let mut chosen = None;
        for row in candidates {
            let task = row_task(row)?;
            let running: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM task_runs WHERE state='running' AND agent_id=?",
            )
            .bind(&task.agent_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
            if running < settings.get::<i64, _>("max_per_agent") {
                chosen = Some(task);
                break;
            }
        }
        let Some(mut task) = chosen else {
            tx.rollback().await.map_err(err)?;
            return Ok(None);
        };
        let stamp = now();
        let run = if let Some(existing_id) = task.run_id.as_deref() {
            let token = id();
            let changed = sqlx::query("UPDATE task_runs SET worker_id=?,lease_token=?,state='running',settled_at=NULL,turn_id=NULL,proposed_result=NULL,proposed_evidence=NULL WHERE id=? AND task_id=? AND state IN ('delegated','paused','waiting_input')")
                .bind(worker_id).bind(&token).bind(existing_id).bind(&task.id).execute(&mut *tx).await.map_err(err)?;
            if changed.rows_affected() != 1 {
                tx.rollback().await.map_err(err)?;
                return Ok(None);
            }
            let mut run = get_run(&mut tx, existing_id).await?;
            run.state = "running".into();
            run.settled_at = None;
            run
        } else {
            let run = TaskRun {
                id: id(),
                task_id: task.id.clone(),
                agent_id: task.agent_id.clone(),
                worker_id: worker_id.to_owned(),
                lease_token: id(),
                thread_id: None,
                turn_id: None,
                state: "running".into(),
                created_at: stamp,
                settled_at: None,
            };
            sqlx::query("INSERT INTO task_runs(id,task_id,agent_id,worker_id,lease_token,state,created_at) VALUES(?,?,?,?,?,'running',?)")
                .bind(&run.id).bind(&run.task_id).bind(&run.agent_id).bind(&run.worker_id).bind(&run.lease_token).bind(stamp)
                .execute(&mut *tx).await.map_err(err)?;
            run
        };
        let changed = sqlx::query("UPDATE tasks SET status='running',run_id=?,reason=NULL,updated_at=? WHERE id=? AND status='queued'")
            .bind(&run.id).bind(stamp).bind(&task.id).execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() != 1 {
            tx.rollback().await.map_err(err)?;
            return Ok(None);
        }
        task.status = "running".into();
        task.run_id = Some(run.id.clone());
        task.updated_at = stamp;
        event(
            &mut tx,
            "task.claimed",
            &task.id,
            &json!({"task":task,"run":run}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(Some(run))
    }

    pub async fn attach_run(
        &self,
        task_id: &str,
        run_id: &str,
        thread_id: &str,
        turn_id: &str,
        lease_token: &str,
    ) -> Result<TaskRun, String> {
        let stamp = now();
        let mut tx = self.pool.begin().await.map_err(err)?;
        let result = sqlx::query("UPDATE task_runs SET thread_id=?,turn_id=? WHERE id=? AND task_id=? AND state='running' AND lease_token=? AND thread_id IS NULL")
            .bind(thread_id).bind(turn_id).bind(run_id).bind(task_id).bind(lease_token)
            .execute(&mut *tx).await.map_err(err)?;
        if result.rows_affected() != 1 {
            return Err("task run lease is stale or already attached".into());
        }
        sqlx::query("UPDATE tasks SET thread_id=?,updated_at=? WHERE id=? AND run_id=?")
            .bind(thread_id)
            .bind(stamp)
            .bind(task_id)
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        let run = get_run(&mut tx, run_id).await?;
        event(&mut tx, "task.run_attached", task_id, &run).await?;
        crate::tickets::auto_lifecycle_status(
            &mut tx,
            task_id,
            crate::TicketStatus::InProgress,
            stamp,
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(run)
    }

    /// Persist a runtime thread as soon as it exists, before the host starts its first turn.
    pub async fn bind_run_thread(
        &self,
        task_id: &str,
        run_id: &str,
        thread_id: &str,
        lease_token: &str,
    ) -> Result<TaskRun, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT thread_id FROM task_runs WHERE id=? AND task_id=? AND state='running' AND lease_token=?",
        )
        .bind(run_id)
        .bind(task_id)
        .bind(lease_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?
        .flatten();
        if let Some(existing) = existing.as_deref() {
            if existing != thread_id {
                return Err("task run is already bound to another thread".into());
            }
            let run = get_run(&mut tx, run_id).await?;
            tx.commit().await.map_err(err)?;
            return Ok(run);
        }
        let changed=sqlx::query("UPDATE task_runs SET thread_id=? WHERE id=? AND task_id=? AND state='running' AND lease_token=? AND thread_id IS NULL")
            .bind(thread_id).bind(run_id).bind(task_id).bind(lease_token).execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() != 1 {
            return Err("task run cannot bind this thread".into());
        }
        sqlx::query("UPDATE tasks SET thread_id=?,updated_at=? WHERE id=? AND run_id=?")
            .bind(thread_id)
            .bind(now())
            .bind(task_id)
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        let run = get_run(&mut tx, run_id).await?;
        event(&mut tx, "task.thread_bound", task_id, &run).await?;
        tx.commit().await.map_err(err)?;
        Ok(run)
    }

    /// Bind the next runtime turn to the existing task run after the host resumes its thread.
    pub async fn attach_continuation(
        &self,
        task_id: &str,
        run_id: &str,
        thread_id: &str,
        turn_id: &str,
        lease_token: &str,
    ) -> Result<TaskRun, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let changed=sqlx::query("UPDATE task_runs SET thread_id=?,turn_id=? WHERE id=? AND task_id=? AND state='running' AND lease_token=? AND ((turn_id IS NULL AND (thread_id IS NULL OR thread_id=?)) OR (turn_id=? AND thread_id=?))")
            .bind(thread_id).bind(turn_id).bind(run_id).bind(task_id).bind(lease_token).bind(thread_id).bind(turn_id).bind(thread_id).execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() != 1 {
            return Err("task run cannot accept this continuation binding".into());
        }
        crate::tickets::auto_lifecycle_status(
            &mut tx,
            task_id,
            crate::TicketStatus::InProgress,
            now(),
        )
        .await?;
        sqlx::query("UPDATE tasks SET thread_id=?,updated_at=? WHERE id=? AND run_id=?")
            .bind(thread_id)
            .bind(now())
            .bind(task_id)
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        let run = get_run(&mut tx, run_id).await?;
        event(&mut tx, "task.continuation_attached", task_id, &run).await?;
        tx.commit().await.map_err(err)?;
        Ok(run)
    }

    /// Clears the completed/interrupted question source turn before its answer follow-up starts.
    /// The host must verify the source outcome before calling this fenced transition.
    pub async fn prepare_question_continuation(
        &self,
        task_id: &str,
        run_id: &str,
        source_turn_id: &str,
        lease_token: &str,
    ) -> Result<TaskRun, String> {
        if source_turn_id.trim().is_empty() {
            return Err("question source turn id is required".into());
        }
        let mut tx = self.pool.begin().await.map_err(err)?;
        let run_row = sqlx::query(
            "SELECT turn_id FROM task_runs WHERE id=? AND task_id=? AND state='running' AND lease_token=?",
        )
        .bind(run_id)
        .bind(task_id)
        .bind(lease_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?
        .ok_or_else(|| "question continuation requires the active task run lease".to_string())?;
        let current_turn: Option<String> = run_row.try_get("turn_id").map_err(err)?;
        if current_turn
            .as_deref()
            .is_some_and(|turn_id| turn_id != source_turn_id)
        {
            return Err("question source turn does not match the active task run".into());
        }
        sqlx::query("UPDATE task_runs SET turn_id=NULL,proposed_result=NULL,proposed_evidence=NULL WHERE id=? AND task_id=? AND state='running' AND lease_token=?")
            .bind(run_id)
            .bind(task_id)
            .bind(lease_token)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        let run = get_run(&mut tx, run_id).await?;
        event(
            &mut tx,
            "task.question_continuation_prepared",
            task_id,
            &json!({"sourceTurnId":source_turn_id,"run":run}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(run)
    }

    /// Record a successful runtime turn that has not proposed task completion; the same run remains active.
    pub async fn settle_turn(
        &self,
        task_id: &str,
        run_id: &str,
        turn_id: &str,
        lease_token: &str,
    ) -> Result<Task, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let row=sqlx::query("SELECT turn_id,lease_token,proposed_result,proposed_evidence FROM task_runs WHERE id=? AND task_id=? AND state='running'")
            .bind(run_id).bind(task_id).fetch_optional(&mut *tx).await.map_err(err)?.ok_or_else(||"task run is not active".to_string())?;
        if row
            .try_get::<Option<String>, _>("turn_id")
            .map_err(err)?
            .as_deref()
            != Some(turn_id)
            || row.try_get::<String, _>("lease_token").map_err(err)? != lease_token
        {
            return Err("runtime turn or task lease does not match the active run".into());
        }
        if row
            .try_get::<Option<String>, _>("proposed_result")
            .map_err(err)?
            .is_some()
            || row
                .try_get::<Option<String>, _>("proposed_evidence")
                .map_err(err)?
                .is_some()
        {
            return Err("a turn with a completion proposal must use settle_run".into());
        }
        sqlx::query("UPDATE task_runs SET turn_id=NULL WHERE id=? AND state='running'")
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        let task = get_task_tx(&mut tx, task_id).await?;
        event(
            &mut tx,
            "task.turn_settled",
            task_id,
            &json!({"runId":run_id,"turnId":turn_id,"task":task}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    /// Yields a worker slot while an authoritative runtime approval remains pending.
    pub async fn waiting_run(
        &self,
        task_id: &str,
        run_id: &str,
        lease_token: &str,
    ) -> Result<Task, String> {
        self.set_run_pause_state(
            task_id,
            run_id,
            lease_token,
            "waiting_approval",
            "waiting for runtime approval",
        )
        .await
    }

    /// Yields a worker slot while the host waits for an answer to a durable question request.
    /// The runtime thread and workspace remain bound to this run for host-owned continuation.
    pub async fn wait_for_input(
        &self,
        task_id: &str,
        run_id: &str,
        lease_token: &str,
    ) -> Result<Task, String> {
        self.set_run_pause_state(
            task_id,
            run_id,
            lease_token,
            "waiting_input",
            "waiting for user input",
        )
        .await
    }

    /// Blocks uncertain or unsupported runtime outcomes without replaying the turn.
    pub async fn block_run(
        &self,
        task_id: &str,
        run_id: &str,
        lease_token: &str,
        reason: &str,
        unknown: bool,
    ) -> Result<Task, String> {
        if reason.trim().is_empty() {
            return Err("blocked task needs a reason".into());
        }
        let state = if unknown { "unknown" } else { "blocked" };
        self.set_run_pause_state(task_id, run_id, lease_token, state, reason)
            .await
    }

    async fn set_run_pause_state(
        &self,
        task_id: &str,
        run_id: &str,
        lease_token: &str,
        state: &str,
        reason: &str,
    ) -> Result<Task, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let task_status = match state {
            "waiting_approval" => "waiting_approval",
            "waiting_input" => "waiting_input",
            _ => "blocked",
        };
        let stamp = now();
        let changed=sqlx::query("UPDATE task_runs SET state=?,settled_at=NULL WHERE id=? AND task_id=? AND state='running' AND lease_token=?")
            .bind(state).bind(run_id).bind(task_id).bind(lease_token).execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() != 1 {
            return Err("task run is not owned by this lease".into());
        }
        sqlx::query("UPDATE tasks SET status=?,reason=?,updated_at=? WHERE id=? AND run_id=?")
            .bind(task_status)
            .bind(reason)
            .bind(stamp)
            .bind(task_id)
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        let task = get_task_tx(&mut tx, task_id).await?;
        event(
            &mut tx,
            "task.run_blocked",
            task_id,
            &json!({"state":state,"task":task}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    /// Reacquires the same run after approval or a host recovery decision, rotating its fence.
    pub async fn resume_run(
        &self,
        task_id: &str,
        run_id: &str,
        lease_token: &str,
        worker_id: &str,
    ) -> Result<TaskRun, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let agent_id:String=sqlx::query_scalar("SELECT agent_id FROM task_runs WHERE id=? AND task_id=? AND lease_token=? AND state IN ('waiting_approval','waiting_input','unknown','blocked','paused')")
            .bind(run_id).bind(task_id).bind(lease_token).fetch_optional(&mut *tx).await.map_err(err)?
            .ok_or_else(||"task run cannot be resumed with this lease".to_string())?;
        let settings =
            sqlx::query("SELECT max_background,max_per_agent FROM task_settings WHERE id=1")
                .fetch_one(&mut *tx)
                .await
                .map_err(err)?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_runs WHERE state='running'")
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
        let per_agent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM task_runs WHERE state='running' AND agent_id=?",
        )
        .bind(&agent_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(err)?;
        if total >= settings.get::<i64, _>("max_background")
            || per_agent >= settings.get::<i64, _>("max_per_agent")
        {
            return Err("task worker capacity is full".into());
        }
        let token = id();
        let stamp = now();
        let changed=sqlx::query("UPDATE task_runs SET state='running',worker_id=?,lease_token=?,settled_at=NULL WHERE id=? AND task_id=? AND state IN ('waiting_approval','waiting_input','unknown','blocked','paused') AND lease_token=?")
            .bind(worker_id).bind(&token).bind(run_id).bind(task_id).bind(lease_token).execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() != 1 {
            return Err("task run cannot be resumed with this lease".into());
        }
        sqlx::query(
            "UPDATE tasks SET status='running',reason=NULL,updated_at=? WHERE id=? AND run_id=?",
        )
        .bind(stamp)
        .bind(task_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        let run = get_run(&mut tx, run_id).await?;
        event(&mut tx, "task.run_resumed", task_id, &run).await?;
        tx.commit().await.map_err(err)?;
        Ok(run)
    }

    pub async fn propose_completion(
        &self,
        task_id: &str,
        run_id: &str,
        lease_token: &str,
        result: &str,
        evidence: Vec<String>,
    ) -> Result<(), String> {
        if result.trim().is_empty()
            || evidence.is_empty()
            || evidence.iter().any(|e| e.trim().is_empty())
        {
            return Err("completion requires a proposed result and non-empty evidence".into());
        }
        let mut tx = self.pool.begin().await.map_err(err)?;
        let pending_questions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM question_requests WHERE run_id=? AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain'))")
            .bind(run_id).fetch_one(&mut *tx).await.map_err(err)?;
        if pending_questions > 0 {
            return Err("task run has unresolved question requests".into());
        }
        let changed = sqlx::query("UPDATE task_runs SET proposed_result=?,proposed_evidence=? WHERE id=? AND task_id=? AND state='running' AND turn_id IS NOT NULL AND lease_token=?")
            .bind(result).bind(serde_json::to_string(&evidence).map_err(serr)?).bind(run_id).bind(task_id).bind(lease_token)
            .execute(&mut *tx).await.map_err(err)?;
        if changed.rows_affected() != 1 {
            return Err("completion requires the active attached task run".into());
        }
        event(
            &mut tx,
            "task.completion_proposed",
            task_id,
            &json!({"runId":run_id,"result":result,"evidence":evidence}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(())
    }

    /// Settle only after the host has observed the authoritative runtime turn outcome.
    pub async fn settle_run(
        &self,
        task_id: &str,
        run_id: &str,
        turn_id: &str,
        lease_token: &str,
        success: bool,
    ) -> Result<Task, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let row =
            sqlx::query("SELECT * FROM task_runs WHERE id=? AND task_id=? AND state='running'")
                .bind(run_id)
                .bind(task_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(err)?
                .ok_or_else(|| "task run is not active".to_string())?;
        if row
            .try_get::<Option<String>, _>("turn_id")
            .map_err(err)?
            .as_deref()
            != Some(turn_id)
        {
            return Err("runtime turn does not match the attached task run".into());
        }
        if row.try_get::<String, _>("lease_token").map_err(err)? != lease_token {
            return Err("task run lease is stale".into());
        }
        let proposed_result: Option<String> = row.try_get("proposed_result").map_err(err)?;
        let proposed_evidence: Option<String> = row.try_get("proposed_evidence").map_err(err)?;
        if success && (proposed_result.is_none() || proposed_evidence.is_none()) {
            return Err("successful turn requires a proposed result and evidence".into());
        }
        if success {
            let pending_questions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM question_requests WHERE run_id=? AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain'))")
                .bind(run_id).fetch_one(&mut *tx).await.map_err(err)?;
            if pending_questions > 0 {
                return Err("task run has unresolved question requests".into());
            }
        }
        let stamp = now();
        let state = if success { "succeeded" } else { "failed" };
        sqlx::query("UPDATE task_runs SET state=?,settled_at=? WHERE id=? AND state='running'")
            .bind(state)
            .bind(stamp)
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        sqlx::query("UPDATE tasks SET status=?,result=?,evidence=?,reason=?,updated_at=? WHERE id=? AND run_id=?")
            .bind(state).bind(proposed_result).bind(proposed_evidence).bind(if success { None } else { Some("runtime turn failed") }).bind(stamp).bind(task_id).bind(run_id)
            .execute(&mut *tx).await.map_err(err)?;
        if !success {
            block_failed_dependencies(&mut tx, stamp).await?;
        } else {
            crate::tickets::auto_lifecycle_status(
                &mut tx,
                task_id,
                crate::TicketStatus::Review,
                stamp,
            )
            .await?;
            unblock_ready_dependents(&mut tx, stamp).await?;
        }
        maybe_resume_parent(&mut tx, task_id, stamp).await?;
        crate::schedule::update_workflow_after_settlement(&mut tx, task_id, stamp).await?;
        let task = get_task_tx(&mut tx, task_id).await?;
        event(
            &mut tx,
            "task.settled",
            task_id,
            &json!({"task":task,"turnId":turn_id,"authoritativeSuccess":success}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    pub async fn control_task(&self, task_id: &str, action: TaskAction) -> Result<Task, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let mut task = get_task_tx(&mut tx, task_id).await?;
        let stamp = now();
        match action {
            TaskAction::Pause if task.status == "queued" => {
                task.status = "paused".into();
                task.reason = Some("paused by user".into());
            }
            TaskAction::Pause if task.status == "running" => {
                sqlx::query("UPDATE task_runs SET state='paused',settled_at=? WHERE id=? AND state='running'").bind(stamp).bind(task.run_id.as_deref().unwrap_or("")).execute(&mut *tx).await.map_err(err)?;
                task.status = "paused".into();
                task.reason = Some("paused by user".into());
            }
            TaskAction::Pause if task.status == "waiting_input" => {
                sqlx::query("UPDATE task_runs SET state='paused',settled_at=? WHERE id=? AND state='waiting_input'")
                    .bind(stamp).bind(task.run_id.as_deref().unwrap_or("")).execute(&mut *tx).await.map_err(err)?;
                task.status = "paused".into();
                task.reason = Some("paused by user".into());
            }
            TaskAction::Resume if task.status == "paused" => {
                let pending_question = if let Some(run_id) = task.run_id.as_deref() {
                    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM question_requests WHERE run_id=? AND (resolution='pending' OR delivery IN ('queued','dispatching','uncertain')))")
                        .bind(run_id).fetch_one(&mut *tx).await.map_err(err)?
                } else {
                    false
                };
                if pending_question {
                    if let Some(run_id) = task.run_id.as_deref() {
                        sqlx::query("UPDATE task_runs SET state='waiting_input',settled_at=NULL WHERE id=? AND task_id=? AND state='paused'")
                            .bind(run_id).bind(task_id).execute(&mut *tx).await.map_err(err)?;
                    }
                    task.status = "waiting_input".into();
                    task.reason = Some("waiting for user input".into());
                } else {
                    task.status = "queued".into();
                    task.reason = None;
                }
            }
            TaskAction::Cancel
                if matches!(
                    task.status.as_str(),
                    "queued"
                        | "paused"
                        | "running"
                        | "blocked"
                        | "waiting_approval"
                        | "waiting_input"
                ) =>
            {
                sqlx::query("UPDATE task_runs SET state='cancelled',settled_at=? WHERE task_id=? AND state IN ('running','waiting_approval','waiting_input','unknown','blocked','delegated','paused')").bind(stamp).bind(task_id).execute(&mut *tx).await.map_err(err)?;
                task.status = "cancelled".into();
                task.reason = Some("cancelled by user".into());
            }
            TaskAction::Retry
                if matches!(task.status.as_str(), "failed" | "blocked" | "cancelled") =>
            {
                if let Some(ticket_id) = sqlx::query_scalar::<_, Option<String>>(
                    "SELECT ticket_id FROM tasks WHERE id=?",
                )
                .bind(task_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(err)?
                {
                    let unfinished: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM tasks WHERE ticket_id=? AND id<>? AND status NOT IN ('succeeded','failed','cancelled')",
                    )
                    .bind(ticket_id)
                    .bind(task_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(err)?;
                    if unfinished > 0 {
                        return Err("ticket already has an unfinished task".into());
                    }
                }
                sqlx::query("UPDATE task_runs SET state='cancelled',settled_at=? WHERE task_id=? AND state IN ('running','waiting_approval','waiting_input','unknown','blocked','delegated','paused')")
                    .bind(stamp).bind(task_id).execute(&mut *tx).await.map_err(err)?;
                task.status = "queued".into();
                task.reason = None;
                task.run_id = None;
                task.thread_id = None;
                task.result = None;
                task.evidence = None;
                sqlx::query("UPDATE tasks SET ticket_status_revision=(SELECT status_revision FROM tickets WHERE id=tasks.ticket_id),ticket_status_started=0 WHERE id=? AND ticket_id IS NOT NULL")
                    .bind(task_id).execute(&mut *tx).await.map_err(err)?;
            }
            _ => return Err(format!("cannot {:?} task in {} state", action, task.status)),
        }
        task.updated_at = stamp;
        update_task(&mut tx, &task).await?;
        if task.status == "cancelled" {
            block_failed_dependencies(&mut tx, stamp).await?;
        }
        maybe_resume_parent(&mut tx, task_id, stamp).await?;
        event(&mut tx, "task.controlled", task_id, &task).await?;
        tx.commit().await.map_err(err)?;
        Ok(task)
    }

    pub async fn unresolved_runs(&self) -> Result<Vec<TaskRun>, String> {
        let rows =
            sqlx::query("SELECT * FROM task_runs WHERE state IN ('running','waiting_approval','waiting_input','unknown','blocked','paused') ORDER BY created_at,id")
            .fetch_all(&self.pool)
                .await
                .map_err(err)?;
        rows.into_iter().map(row_run).collect()
    }

    /// A process restart may continue the same host-owned binding only after it has read runtime state.
    pub async fn recover_run(&self, run_id: &str, lease_token: &str) -> Result<TaskRun, String> {
        let row =
            sqlx::query("SELECT * FROM task_runs WHERE id=? AND state IN ('running','waiting_input') AND lease_token=?")
                .bind(run_id)
                .bind(lease_token)
                .fetch_optional(&self.pool)
                .await
                .map_err(err)?
                .ok_or_else(|| "task run is not recoverable with this lease".to_string())?;
        row_run(row)
    }

    pub async fn get_run(&self, run_id: &str) -> Result<TaskRun, String> {
        let row = sqlx::query("SELECT * FROM task_runs WHERE id=?")
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(err)?
            .ok_or_else(|| "task run not found".to_string())?;
        row_run(row)
    }

    pub async fn get_task_settings(&self) -> Result<TaskSettings, String> {
        let row = sqlx::query("SELECT max_background,max_per_agent FROM task_settings WHERE id=1")
            .fetch_one(&self.pool)
            .await
            .map_err(err)?;
        Ok(TaskSettings {
            max_background: row.get::<i64, _>("max_background") as u8,
            max_per_agent: row.get::<i64, _>("max_per_agent") as u8,
        })
    }

    pub async fn update_task_settings(
        &self,
        settings: TaskSettings,
    ) -> Result<TaskSettings, String> {
        if !(1..=32).contains(&settings.max_background)
            || !(1..=settings.max_background).contains(&settings.max_per_agent)
            || settings.max_per_agent > settings.max_background
        {
            return Err("task settings must allow 1-32 background workers and no more per agent than global".into());
        }
        sqlx::query("UPDATE task_settings SET max_background=?,max_per_agent=? WHERE id=1")
            .bind(settings.max_background as i64)
            .bind(settings.max_per_agent as i64)
            .execute(&self.pool)
            .await
            .map_err(err)?;
        Ok(settings)
    }

    pub async fn propose_delegation(
        &self,
        parent_task_id: &str,
        tasks: Vec<DelegationNode>,
    ) -> Result<DelegationProposal, String> {
        validate_graph(&tasks)?;
        if tasks.is_empty() {
            return Err("delegation needs at least one child task".into());
        }
        let parent = self.get_task(parent_task_id).await?;
        if !matches!(parent.status.as_str(), "running" | "paused") {
            return Err("delegation parent must be running or paused for review".into());
        }
        if parent.status == "paused" && parent.run_id.is_some() {
            return Err("a paused delegation parent must not have a prior run".into());
        }
        let hash = digest(&tasks)?;
        let proposal = DelegationProposal {
            id: id(),
            parent_task_id: parent_task_id.into(),
            hash,
            status: "proposed".into(),
            tasks,
            created_at: now(),
        };
        let mut tx = self.pool.begin().await.map_err(err)?;
        for node in &proposal.tasks {
            validate_input(&node.task)?;
            validate_agent_tx(&mut tx, &node.task.agent_id).await?;
            validate_dependencies_tx(&mut tx, &node.task.dependencies, None).await?;
        }
        sqlx::query("INSERT INTO delegation_proposals(id,parent_task_id,hash,status,tasks,created_at) VALUES(?,?,?,'proposed',?,?)")
            .bind(&proposal.id).bind(parent_task_id).bind(&proposal.hash).bind(serde_json::to_string(&proposal.tasks).map_err(serr)?).bind(proposal.created_at)
            .execute(&mut *tx).await.map_err(err)?;
        event(
            &mut tx,
            "task.delegation_proposed",
            parent_task_id,
            &proposal,
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(proposal)
    }

    pub async fn list_proposals(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<DelegationProposal>, String> {
        let rows = if let Some(agent_id) = agent_id {
            sqlx::query("SELECT p.* FROM delegation_proposals p JOIN tasks t ON t.id=p.parent_task_id WHERE t.agent_id=? ORDER BY p.created_at,p.id")
                .bind(agent_id).fetch_all(&self.pool).await.map_err(err)?
        } else {
            sqlx::query("SELECT * FROM delegation_proposals ORDER BY created_at,id")
                .fetch_all(&self.pool)
                .await
                .map_err(err)?
        };
        rows.into_iter().map(row_proposal).collect()
    }

    pub async fn approve_delegation(
        &self,
        proposal_id: &str,
        hash: &str,
    ) -> Result<Vec<Task>, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let row = sqlx::query("SELECT * FROM delegation_proposals WHERE id=?")
            .bind(proposal_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(err)?
            .ok_or_else(|| "delegation proposal not found".to_string())?;
        let proposal = row_proposal(row)?;
        if proposal.hash != hash || digest(&proposal.tasks)? != hash {
            return Err("delegation review hash does not match the proposed graph".into());
        }
        if proposal.status == "approved" {
            let rows =
                sqlx::query("SELECT * FROM tasks WHERE delegation_id=? ORDER BY delegation_index")
                    .bind(proposal_id)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(err)?;
            let tasks = rows
                .into_iter()
                .map(row_task)
                .collect::<Result<Vec<_>, _>>()?;
            tx.commit().await.map_err(err)?;
            return Ok(tasks);
        }
        if proposal.status != "proposed" {
            return Err("delegation proposal is no longer pending".into());
        }
        let mut parent = get_task_tx(&mut tx, &proposal.parent_task_id).await?;
        if !matches!(parent.status.as_str(), "running" | "paused") {
            return Err("delegation parent is no longer available for dispatch".into());
        }
        let mut ids = std::collections::HashMap::new();
        for node in &proposal.tasks {
            ids.insert(node.key.clone(), id());
        }
        let stamp = now();
        let mut result = Vec::new();
        for (index, node) in proposal.tasks.iter().enumerate() {
            let mut input = node.task.clone();
            let local_dependencies = node
                .depends_on
                .iter()
                .map(|key| {
                    ids.get(key)
                        .cloned()
                        .ok_or_else(|| format!("unknown dependency key {key}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            input.dependencies.extend(local_dependencies);
            input.dependencies.sort();
            input.dependencies.dedup();
            validate_input(&input)?;
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
            insert_task(
                &mut tx,
                &task,
                Some(&proposal.parent_task_id),
                Some(proposal_id),
            )
            .await?;
            sqlx::query("UPDATE tasks SET delegation_index=? WHERE id=?")
                .bind(index as i64)
                .bind(&task.id)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
            result.push(task);
        }
        sqlx::query(
            "UPDATE delegation_proposals SET status='approved' WHERE id=? AND status='proposed'",
        )
        .bind(proposal_id)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        if let Some(run_id) = parent.run_id.as_deref() {
            sqlx::query("UPDATE task_runs SET state='delegated',settled_at=? WHERE id=? AND state='running'").bind(stamp).bind(run_id).execute(&mut *tx).await.map_err(err)?;
        }
        parent.status = "waiting_children".into();
        parent.reason = None;
        parent.updated_at = stamp;
        update_task(&mut tx, &parent).await?;
        event(
            &mut tx,
            "task.delegation_approved",
            &parent.id,
            &json!({"proposalId":proposal_id,"tasks":result}),
        )
        .await?;
        tx.commit().await.map_err(err)?;
        Ok(result)
    }
}

fn validate_graph(tasks: &[DelegationNode]) -> Result<(), String> {
    let keys: std::collections::HashSet<&str> = tasks.iter().map(|n| n.key.as_str()).collect();
    if keys.len() != tasks.len() || tasks.iter().any(|n| n.key.trim().is_empty()) {
        return Err("delegation keys must be non-empty and unique".into());
    }
    let mut indegree = std::collections::HashMap::<&str, usize>::new();
    let mut children = std::collections::HashMap::<&str, Vec<&str>>::new();
    for n in tasks {
        indegree.insert(n.key.as_str(), 0);
    }
    for n in tasks {
        let mut unique = std::collections::HashSet::new();
        for dep in &n.depends_on {
            if !keys.contains(dep.as_str()) {
                return Err(format!("delegation dependency {dep} does not exist"));
            }
            if !unique.insert(dep) {
                return Err("delegation dependencies must be unique".into());
            }
            *indegree.get_mut(n.key.as_str()).unwrap() += 1;
            children
                .entry(dep.as_str())
                .or_default()
                .push(n.key.as_str());
        }
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter_map(|(k, d)| (*d == 0).then_some(*k))
        .collect();
    let mut visited = 0;
    while let Some(key) = ready.pop() {
        visited += 1;
        for child in children.get(key).into_iter().flatten() {
            let degree = indegree.get_mut(child).unwrap();
            *degree -= 1;
            if *degree == 0 {
                ready.push(child);
            }
        }
    }
    if visited != tasks.len() {
        return Err("delegation graph contains a cycle".into());
    }
    Ok(())
}

fn row_proposal(row: sqlx::sqlite::SqliteRow) -> Result<DelegationProposal, String> {
    Ok(DelegationProposal {
        id: row.try_get("id").map_err(err)?,
        parent_task_id: row.try_get("parent_task_id").map_err(err)?,
        hash: row.try_get("hash").map_err(err)?,
        status: row.try_get("status").map_err(err)?,
        tasks: parse(row.try_get("tasks").map_err(err)?)?,
        created_at: row.try_get("created_at").map_err(err)?,
    })
}

pub(crate) fn validate_input(input: &TaskInput) -> Result<(), String> {
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
    Ok(())
}

pub(crate) async fn validate_dependencies(
    pool: &SqlitePool,
    dependencies: &[String],
    omit: Option<&str>,
) -> Result<(), String> {
    for dep in dependencies {
        if omit == Some(dep.as_str()) {
            return Err("task cannot depend on itself".into());
        }
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM tasks WHERE id=?")
            .bind(dep)
            .fetch_optional(pool)
            .await
            .map_err(err)?;
        if exists.is_none() {
            return Err(format!("dependency task {dep} does not exist"));
        }
    }
    Ok(())
}

pub(crate) async fn validate_agent(pool: &SqlitePool, agent_id: &str) -> Result<(), String> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_profiles WHERE id=?")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(err)?;
    if exists == 1 {
        Ok(())
    } else {
        Err(format!("agent profile {agent_id} does not exist"))
    }
}

pub(crate) async fn validate_agent_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    agent_id: &str,
) -> Result<(), String> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_profiles WHERE id=?")
        .bind(agent_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(err)?;
    if exists == 1 {
        Ok(())
    } else {
        Err(format!("agent profile {agent_id} does not exist"))
    }
}

async fn validate_dependencies_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    dependencies: &[String],
    omit: Option<&str>,
) -> Result<(), String> {
    for dep in dependencies {
        if omit == Some(dep.as_str()) {
            return Err("task cannot depend on itself".into());
        }
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM tasks WHERE id=?")
            .bind(dep)
            .fetch_optional(&mut **tx)
            .await
            .map_err(err)?;
        if exists.is_none() {
            return Err(format!("dependency task {dep} does not exist"));
        }
    }
    Ok(())
}

pub(crate) fn task_from_input(
    id: String,
    input: TaskInput,
    status: &str,
    reason: Option<String>,
    run_id: Option<String>,
    thread_id: Option<String>,
    result: Option<String>,
    stamp: i64,
) -> Task {
    Task {
        id,
        agent_id: input.agent_id,
        title: input.title,
        instructions: input.instructions,
        acceptance_criteria: input.acceptance_criteria,
        context: input.context,
        cwd: input.cwd,
        project_id: input.project_id,
        model: input.model,
        effort: input.effort,
        workspace_permission: input.workspace_permission,
        dependencies: input.dependencies,
        status: status.into(),
        reason,
        run_id,
        thread_id,
        result,
        evidence: None,
        created_at: stamp,
        updated_at: stamp,
    }
}

pub(crate) async fn insert_task(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task: &Task,
    parent: Option<&str>,
    delegation: Option<&str>,
) -> Result<(), String> {
    sqlx::query("INSERT INTO tasks(id,agent_id,title,instructions,acceptance_criteria,context,cwd,project_id,model,effort,dependencies,status,reason,run_id,thread_id,result,evidence,parent_task_id,delegation_id,created_at,updated_at,workspace_permission,ticket_id,ticket_status_revision) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&task.id).bind(&task.agent_id).bind(&task.title).bind(&task.instructions)
        .bind(serde_json::to_string(&task.acceptance_criteria).map_err(serr)?).bind(&task.context).bind(&task.cwd).bind(&task.project_id).bind(&task.model).bind(&task.effort)
        .bind(serde_json::to_string(&task.dependencies).map_err(serr)?).bind(&task.status).bind(&task.reason).bind(&task.run_id).bind(&task.thread_id).bind(&task.result)
        .bind(task.evidence.as_ref().map(serde_json::to_string).transpose().map_err(serr)?).bind(parent).bind(delegation).bind(task.created_at).bind(task.updated_at)
        .bind(task.workspace_permission.as_ref().map(|p| serde_json::to_string(p).unwrap_or_default())).bind(Option::<String>::None).bind(Option::<i64>::None)
        .execute(&mut **tx).await.map_err(err)?;
    Ok(())
}

fn row_task(row: sqlx::sqlite::SqliteRow) -> Result<Task, String> {
    Ok(Task {
        id: row.try_get("id").map_err(err)?,
        agent_id: row.try_get("agent_id").map_err(err)?,
        title: row.try_get("title").map_err(err)?,
        instructions: row.try_get("instructions").map_err(err)?,
        acceptance_criteria: parse(
            row.try_get::<String, _>("acceptance_criteria")
                .map_err(err)?,
        )?,
        context: row.try_get("context").map_err(err)?,
        cwd: row.try_get("cwd").map_err(err)?,
        project_id: row.try_get("project_id").map_err(err)?,
        model: row.try_get("model").map_err(err)?,
        effort: row.try_get("effort").map_err(err)?,
        workspace_permission: row
            .try_get::<Option<String>, _>("workspace_permission")
            .map_err(err)?
            .map(parse)
            .transpose()?,
        dependencies: parse(row.try_get::<String, _>("dependencies").map_err(err)?)?,
        status: row.try_get("status").map_err(err)?,
        reason: row.try_get("reason").map_err(err)?,
        run_id: row.try_get("run_id").map_err(err)?,
        thread_id: row.try_get("thread_id").map_err(err)?,
        result: row.try_get("result").map_err(err)?,
        evidence: row
            .try_get::<Option<String>, _>("evidence")
            .map_err(err)?
            .map(parse)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(err)?,
        updated_at: row.try_get("updated_at").map_err(err)?,
    })
}

pub(crate) async fn get_task_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
) -> Result<Task, String> {
    row_task(
        sqlx::query("SELECT * FROM tasks WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(err)?
            .ok_or_else(|| "task not found".to_string())?,
    )
}

async fn update_task(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task: &Task,
) -> Result<(), String> {
    sqlx::query("UPDATE tasks SET status=?,reason=?,run_id=?,thread_id=?,result=?,evidence=?,updated_at=? WHERE id=?")
        .bind(&task.status).bind(&task.reason).bind(&task.run_id).bind(&task.thread_id).bind(&task.result)
        .bind(task.evidence.as_ref().map(serde_json::to_string).transpose().map_err(serr)?).bind(task.updated_at).bind(&task.id)
        .execute(&mut **tx).await.map_err(err)?;
    Ok(())
}

async fn get_run(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
) -> Result<TaskRun, String> {
    row_run(
        sqlx::query("SELECT * FROM task_runs WHERE id=?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .map_err(err)?,
    )
}

fn row_run(row: sqlx::sqlite::SqliteRow) -> Result<TaskRun, String> {
    Ok(TaskRun {
        id: row.try_get("id").map_err(err)?,
        task_id: row.try_get("task_id").map_err(err)?,
        agent_id: row.try_get("agent_id").map_err(err)?,
        worker_id: row.try_get("worker_id").map_err(err)?,
        lease_token: row.try_get("lease_token").map_err(err)?,
        thread_id: row.try_get("thread_id").map_err(err)?,
        turn_id: row.try_get("turn_id").map_err(err)?,
        state: row.try_get("state").map_err(err)?,
        created_at: row.try_get("created_at").map_err(err)?,
        settled_at: row.try_get("settled_at").map_err(err)?,
    })
}

async fn block_failed_dependencies(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    stamp: i64,
) -> Result<(), String> {
    loop {
        let rows=sqlx::query("SELECT id FROM tasks WHERE status IN ('queued','paused') AND EXISTS (SELECT 1 FROM json_each(tasks.dependencies) d JOIN tasks dep ON dep.id=d.value WHERE dep.status IN ('failed','blocked','cancelled'))")
            .fetch_all(&mut **tx).await.map_err(err)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let task_id: String = row.try_get("id").map_err(err)?;
            sqlx::query("UPDATE tasks SET status='blocked',reason='dependency failed',updated_at=? WHERE id=? AND status IN ('queued','paused')")
                .bind(stamp).bind(&task_id).execute(&mut **tx).await.map_err(err)?;
            event(
                tx,
                "task.blocked",
                &task_id,
                &json!({"reason":"dependency failed"}),
            )
            .await?;
        }
    }
    Ok(())
}

async fn unblock_ready_dependents(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    stamp: i64,
) -> Result<(), String> {
    loop {
        let rows=sqlx::query("SELECT id FROM tasks WHERE status='blocked' AND reason='dependency failed' AND NOT EXISTS (SELECT 1 FROM json_each(tasks.dependencies) d LEFT JOIN tasks dep ON dep.id=d.value WHERE dep.id IS NULL OR dep.status!='succeeded')")
            .fetch_all(&mut **tx).await.map_err(err)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let task_id: String = row.try_get("id").map_err(err)?;
            sqlx::query("UPDATE tasks SET status='queued',reason=NULL,updated_at=? WHERE id=? AND status='blocked' AND reason='dependency failed'")
                .bind(stamp).bind(&task_id).execute(&mut **tx).await.map_err(err)?;
            event(
                tx,
                "task.unblocked",
                &task_id,
                &json!({"reason":"dependencies succeeded"}),
            )
            .await?;
        }
    }
    Ok(())
}

async fn maybe_resume_parent(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    child_id: &str,
    stamp: i64,
) -> Result<(), String> {
    let parent: Option<String> = sqlx::query_scalar("SELECT parent_task_id FROM tasks WHERE id=?")
        .bind(child_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(err)?
        .flatten();
    let Some(parent_id) = parent else {
        return Ok(());
    };
    let failed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE delegation_id=(SELECT delegation_id FROM tasks WHERE id=?) AND status IN ('failed','blocked','cancelled')")
        .bind(child_id).fetch_one(&mut **tx).await.map_err(err)?;
    if failed > 0 {
        sqlx::query("UPDATE tasks SET status='blocked',reason='delegated child failed',updated_at=? WHERE id=? AND status='waiting_children'")
            .bind(stamp).bind(parent_id).execute(&mut **tx).await.map_err(err)?;
        return Ok(());
    }
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE delegation_id=(SELECT delegation_id FROM tasks WHERE id=?) AND status!='succeeded'")
        .bind(child_id).fetch_one(&mut **tx).await.map_err(err)?;
    if remaining == 0 {
        sqlx::query("UPDATE tasks SET status='queued',reason=NULL,updated_at=? WHERE id=? AND (status='waiting_children' OR (status='blocked' AND reason='delegated child failed'))")
            .bind(stamp).bind(parent_id).execute(&mut **tx).await.map_err(err)?;
    }
    Ok(())
}

pub(crate) async fn event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    kind: &str,
    entity: &str,
    payload: &impl Serialize,
) -> Result<(), String> {
    sqlx::query("INSERT INTO events(kind,entity_id,payload,created_at) VALUES(?,?,?,?)")
        .bind(kind)
        .bind(entity)
        .bind(serde_json::to_string(payload).map_err(serr)?)
        .bind(now())
        .execute(&mut **tx)
        .await
        .map_err(err)?;
    Ok(())
}

fn parse<T: for<'de> Deserialize<'de>>(s: String) -> Result<T, String> {
    serde_json::from_str(&s).map_err(serr)
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn serr(e: impl std::fmt::Display) -> String {
    e.to_string()
}
