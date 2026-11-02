use super::{AgentStore, id, now, profiles::SessionBinding};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqlitePool, Transaction};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Agent,
    User,
    Project,
}
impl MemoryScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryAccess {
    thread_id: String,
    agent_id: Option<String>,
    project_id: Option<String>,
    user_id: String,
    eligible: bool,
    management: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySource {
    pub source_id: String,
    pub revision: i64,
    pub thread_id: String,
    pub agent_id: Option<String>,
    pub project_id: Option<String>,
    pub content: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecord {
    pub id: String,
    pub scope: MemoryScope,
    pub scope_id: String,
    pub kind: String,
    pub content: String,
    pub revision: i64,
    pub source_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub event_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub revision: i64,
    pub scope: MemoryScope,
    pub scope_id: String,
    pub source_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub(crate) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_sources(source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, thread_id TEXT NOT NULL, agent_id TEXT, project_id TEXT, content TEXT NOT NULL, active INTEGER NOT NULL, updated_at INTEGER NOT NULL, event_id INTEGER)").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_source_revisions(source_id TEXT NOT NULL, revision INTEGER NOT NULL, content TEXT NOT NULL, updated_at INTEGER NOT NULL, event_id INTEGER, PRIMARY KEY(source_id,revision), FOREIGN KEY(source_id) REFERENCES memory_sources(source_id))").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_records(id TEXT PRIMARY KEY, scope TEXT NOT NULL, scope_id TEXT NOT NULL, kind TEXT NOT NULL, content TEXT NOT NULL, revision INTEGER NOT NULL, active INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, event_id INTEGER)").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_record_sources(memory_id TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL, PRIMARY KEY(memory_id,source_id), FOREIGN KEY(memory_id) REFERENCES memory_records(id), FOREIGN KEY(source_id) REFERENCES memory_sources(source_id))").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(memory_id UNINDEXED, content)",
    )
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS learned_skills(id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL, content TEXT NOT NULL, revision INTEGER NOT NULL, scope TEXT NOT NULL, scope_id TEXT NOT NULL, active INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, event_id INTEGER)").execute(pool).await.map_err(|e|e.to_string())?;
    let source_event: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('memory_sources') WHERE name='event_id'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if source_event == 0 {
        sqlx::query("ALTER TABLE memory_sources ADD COLUMN event_id INTEGER")
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    let skill_event: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('learned_skills') WHERE name='event_id'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if skill_event == 0 {
        sqlx::query("ALTER TABLE learned_skills ADD COLUMN event_id INTEGER")
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_undo_results(event_id INTEGER PRIMARY KEY, undo_event_id INTEGER NOT NULL)").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS skill_sources(skill_id TEXT NOT NULL, source_id TEXT NOT NULL, source_revision INTEGER NOT NULL, PRIMARY KEY(skill_id,source_id), FOREIGN KEY(skill_id) REFERENCES learned_skills(id), FOREIGN KEY(source_id) REFERENCES memory_sources(source_id))").execute(pool).await.map_err(|e|e.to_string())?;
    Ok(())
}

impl AgentStore {
    pub async fn memory_access(&self, thread_id: &str) -> Result<MemoryAccess, String> {
        let binding = self.session_binding(thread_id).await?;
        Ok(access_from_binding(binding))
    }

    pub async fn memory_access_for_user(
        &self,
        agent_id: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<MemoryAccess, String> {
        let binding = SessionBinding {
            thread_id: format!("memory-management-{}", id()),
            agent_id: agent_id.map(str::to_owned),
            kind: "user".into(),
            project_id: project_id.map(str::to_owned),
            cwd: None,
            task_run_id: None,
            learning_eligible: true,
        };
        self.bind_session(binding.clone()).await?;
        let mut access = access_from_binding(binding);
        access.management = true;
        Ok(access)
    }

    pub async fn list_sources(
        &self,
        access: &MemoryAccess,
        limit: i64,
    ) -> Result<Vec<MemorySource>, String> {
        let rows = if access.management {
            sqlx::query_as::<_,(String,i64,String,Option<String>,Option<String>,String,i64)>("SELECT source_id,revision,thread_id,agent_id,project_id,content,active FROM memory_sources WHERE active=1 ORDER BY updated_at DESC,source_id LIMIT ?").bind(limit.clamp(1,500)).fetch_all(&self.pool).await.map_err(|e|e.to_string())?
        } else {
            sqlx::query_as::<_,(String,i64,String,Option<String>,Option<String>,String,i64)>("SELECT source_id,revision,thread_id,agent_id,project_id,content,active FROM memory_sources WHERE active=1 AND ((? IS NOT NULL AND agent_id=?) OR thread_id=?) ORDER BY updated_at DESC,source_id LIMIT ?").bind(&access.agent_id).bind(&access.agent_id).bind(&access.thread_id).bind(limit.clamp(1,500)).fetch_all(&self.pool).await.map_err(|e|e.to_string())?
        };
        Ok(rows
            .into_iter()
            .map(|r| MemorySource {
                source_id: r.0,
                revision: r.1,
                thread_id: r.2,
                agent_id: r.3,
                project_id: r.4,
                content: r.5,
                active: r.6 != 0,
            })
            .filter(|source| source_visible(access, source))
            .collect())
    }

    pub async fn record_source(
        &self,
        access: &MemoryAccess,
        source_id: &str,
        revision: i64,
        content: &str,
    ) -> Result<MemorySource, String> {
        if !access.eligible {
            return Err("learning is disabled for this session".into());
        }
        if source_id.is_empty() || revision < 1 || content.trim().is_empty() {
            return Err("source evidence requires an id, positive revision and content".into());
        }
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        if let Some(source) = source_by_id(&mut tx, source_id).await? {
            if source.revision == revision
                && source.thread_id == access.thread_id
                && source.content == content
                && source.active
            {
                tx.commit().await.map_err(|e| e.to_string())?;
                return Ok(source);
            }
            return Err("source id has already been recorded or forgotten".into());
        }
        let source = MemorySource {
            source_id: source_id.into(),
            revision,
            thread_id: access.thread_id.clone(),
            agent_id: access.agent_id.clone(),
            project_id: access.project_id.clone(),
            content: content.into(),
            active: true,
        };
        sqlx::query("INSERT INTO memory_sources(source_id,revision,thread_id,agent_id,project_id,content,active,updated_at) VALUES(?,?,?,?,?,?,1,?)").bind(source_id).bind(revision).bind(&source.thread_id).bind(&source.agent_id).bind(&source.project_id).bind(content).bind(now()).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        sqlx::query("INSERT INTO memory_source_revisions(source_id,revision,content,updated_at) VALUES(?,?,?,?)").bind(source_id).bind(revision).bind(content).bind(now()).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        let event = event_tx(&mut tx, "memory.source_recorded", source_id, &source).await?;
        sqlx::query(
            "UPDATE memory_source_revisions SET event_id=? WHERE source_id=? AND revision=?",
        )
        .bind(event)
        .bind(source_id)
        .bind(revision)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("UPDATE memory_sources SET event_id=? WHERE source_id=?")
            .bind(event)
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(source)
    }

    pub async fn revise_source(
        &self,
        access: &MemoryAccess,
        source_id: &str,
        expected_revision: i64,
        content: &str,
    ) -> Result<MemorySource, String> {
        if !access.eligible {
            return Err("learning is disabled for this session".into());
        }
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let mut source = source_by_id(&mut tx, source_id)
            .await?
            .ok_or_else(|| "source not found".to_string())?;
        if !source.active
            || source.revision != expected_revision
            || !source_visible(access, &source)
        {
            return Err("source changed or is outside this session's scope".into());
        }
        source.revision += 1;
        source.content = content.into();
        sqlx::query(
            "UPDATE memory_sources SET revision=?,content=?,updated_at=? WHERE source_id=?",
        )
        .bind(source.revision)
        .bind(content)
        .bind(now())
        .bind(source_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO memory_source_revisions(source_id,revision,content,updated_at) VALUES(?,?,?,?)").bind(source_id).bind(source.revision).bind(content).bind(now()).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        let affected: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT skill_id FROM skill_sources WHERE source_id=?")
                .bind(source_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        let event = event_tx(&mut tx, "memory.source_revised", source_id, &source).await?;
        sqlx::query("UPDATE memory_sources SET event_id=? WHERE source_id=?")
            .bind(event)
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query(
            "UPDATE memory_source_revisions SET event_id=? WHERE source_id=? AND revision=?",
        )
        .bind(event)
        .bind(source_id)
        .bind(source.revision)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        let _ = event;
        for skill_id in affected {
            self.remove_skill_package(&skill_id).await?;
        }
        Ok(source)
    }

    pub async fn publish_memory(
        &self,
        access: &MemoryAccess,
        scope: MemoryScope,
        kind: &str,
        content: &str,
        source_ids: &[String],
    ) -> Result<MemoryRecord, String> {
        if !access.eligible {
            return Err("learning is disabled for this session".into());
        }
        let scope_id = scope_id(access, scope)?;
        if source_ids.is_empty() {
            return Err("memory records require source evidence".into());
        }
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let revisions = validate_sources(&mut tx, access, scope, source_ids).await?;
        let mut expected_links = revisions.clone();
        expected_links.sort();
        let candidates=sqlx::query_scalar::<_,String>("SELECT id FROM memory_records WHERE scope=? AND scope_id=? AND kind=? AND content=? AND active=1")
            .bind(scope.as_str()).bind(&scope_id).bind(kind).bind(content).fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
        for candidate in candidates {
            let links: Vec<(String, i64)> = sqlx::query_as(
                "SELECT source_id,source_revision FROM memory_record_sources WHERE memory_id=?",
            )
            .bind(&candidate)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            let mut links = links;
            links.sort();
            if links == expected_links {
                let existing = record_by_id(&mut tx, &candidate)
                    .await?
                    .ok_or_else(|| "memory record disappeared".to_string())?;
                tx.commit().await.map_err(|e| e.to_string())?;
                return Ok(existing);
            }
        }
        let mut record = MemoryRecord {
            id: id(),
            scope,
            scope_id,
            kind: kind.into(),
            content: content.into(),
            revision: 1,
            source_ids: canonical_source_ids(source_ids),
            created_at: now(),
            updated_at: now(),
            event_id: None,
        };
        sqlx::query("INSERT INTO memory_records(id,scope,scope_id,kind,content,revision,active,created_at,updated_at,event_id) VALUES(?,?,?,?,?,1,1,?,?,NULL)").bind(&record.id).bind(scope.as_str()).bind(&record.scope_id).bind(kind).bind(content).bind(record.created_at).bind(record.updated_at).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        insert_links(&mut tx, &record.id, &revisions).await?;
        sqlx::query("INSERT INTO memory_fts(memory_id,content) VALUES(?,?)")
            .bind(&record.id)
            .bind(content)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let event_id = event_tx(&mut tx, "memory.published", &record.id, &record).await?;
        record.event_id = Some(event_id);
        sqlx::query("UPDATE memory_records SET event_id=? WHERE id=?")
            .bind(event_id)
            .bind(&record.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("UPDATE events SET payload=? WHERE seq=?")
            .bind(serde_json::to_string(&record).map_err(|e| e.to_string())?)
            .bind(event_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(record)
    }

    pub async fn revise_memory(
        &self,
        access: &MemoryAccess,
        memory_id: &str,
        expected_revision: i64,
        content: &str,
        source_ids: &[String],
    ) -> Result<MemoryRecord, String> {
        if !access.eligible {
            return Err("learning is disabled for this session".into());
        }
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let old = record_by_id(&mut tx, memory_id)
            .await?
            .ok_or_else(|| "memory record not found".to_string())?;
        if !can_access(access, &old.scope, &old.scope_id) {
            return Err("memory record is outside this session's scope".into());
        }
        if old.revision != expected_revision {
            return Err("memory revision changed".into());
        }
        let sources = validate_sources(&mut tx, access, old.scope, source_ids).await?;
        sqlx::query(
            "UPDATE memory_records SET content=?,revision=revision+1,updated_at=? WHERE id=?",
        )
        .bind(content)
        .bind(now())
        .bind(memory_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM memory_record_sources WHERE memory_id=?")
            .bind(memory_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        insert_links(&mut tx, memory_id, &sources).await?;
        sqlx::query("DELETE FROM memory_fts WHERE memory_id=?")
            .bind(memory_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO memory_fts(memory_id,content) VALUES(?,?)")
            .bind(memory_id)
            .bind(content)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let mut updated = old;
        updated.content = content.into();
        updated.revision += 1;
        updated.updated_at = now();
        updated.source_ids = canonical_source_ids(source_ids);
        let event = event_tx(&mut tx, "memory.revised", memory_id, &updated).await?;
        sqlx::query("UPDATE memory_records SET event_id=? WHERE id=?")
            .bind(event)
            .bind(memory_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        updated.event_id = Some(event);
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(updated)
    }

    pub async fn revise_memory_record(
        &self,
        access: &MemoryAccess,
        memory_id: &str,
        expected_revision: i64,
        content: &str,
    ) -> Result<MemoryRecord, String> {
        let _row=sqlx::query_as::<_,(String,String,String,String,i64,i64,i64,Option<i64>)>("SELECT id,scope,scope_id,kind,revision,created_at,updated_at,event_id FROM memory_records WHERE id=? AND active=1").bind(memory_id).fetch_optional(&self.pool).await.map_err(|e|e.to_string())?.ok_or_else(||"memory record not found".to_string())?;
        let source_refs: Vec<String> = sqlx::query_scalar("SELECT rs.source_id || '@' || s.revision FROM memory_record_sources rs JOIN memory_sources s ON s.source_id=rs.source_id WHERE rs.memory_id=? ORDER BY rs.source_id")
            .bind(memory_id).fetch_all(&self.pool).await.map_err(|e| e.to_string())?;
        self.revise_memory(access, memory_id, expected_revision, content, &source_refs)
            .await
    }

    pub async fn list_memory(&self, access: &MemoryAccess) -> Result<Vec<MemoryRecord>, String> {
        let rows=sqlx::query_as::<_,(String,String,String,String,i64,i64,i64,Option<i64>)>("SELECT id,scope,scope_id,kind,revision,created_at,updated_at,event_id FROM memory_records WHERE active=1 ORDER BY updated_at DESC,id").fetch_all(&self.pool).await.map_err(|e|e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let scope = parse_scope(&row.1)?;
            if can_access(access, &scope, &row.2) && self.record_is_current(&row.0).await? {
                out.push(self.load_record(row).await?);
            }
        }
        Ok(out)
    }
    pub async fn search_memory(
        &self,
        access: &MemoryAccess,
        query: &str,
    ) -> Result<Vec<MemoryRecord>, String> {
        if query.trim().is_empty() {
            return self.list_memory(access).await;
        }
        let match_query = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let ids=sqlx::query_scalar::<_,String>("SELECT f.memory_id FROM memory_fts f JOIN memory_records r ON r.id=f.memory_id WHERE memory_fts MATCH ? AND r.active=1 AND (? OR (r.scope='agent' AND r.scope_id=?) OR (r.scope='user' AND r.scope_id=?) OR (r.scope='project' AND r.scope_id=?)) AND NOT EXISTS (SELECT 1 FROM memory_record_sources rs LEFT JOIN memory_sources s ON s.source_id=rs.source_id WHERE rs.memory_id=r.id AND (s.source_id IS NULL OR s.active=0 OR rs.source_revision!=s.revision)) ORDER BY rank LIMIT 100").bind(match_query).bind(access.management).bind(&access.agent_id).bind(&access.user_id).bind(&access.project_id).fetch_all(&self.pool).await.map_err(|e|e.to_string())?;
        let mut out = Vec::new();
        for id in ids {
            let row=sqlx::query_as::<_,(String,String,String,String,i64,i64,i64,Option<i64>)>("SELECT id,scope,scope_id,kind,revision,created_at,updated_at,event_id FROM memory_records WHERE id=?").bind(&id).fetch_one(&self.pool).await.map_err(|e|e.to_string())?;
            let scope = parse_scope(&row.1)?;
            if can_access(access, &scope, &row.2) && self.record_is_current(&id).await? {
                out.push(self.load_record(row).await?);
            }
        }
        Ok(out)
    }

    pub async fn forget_memory(
        &self,
        access: &MemoryAccess,
        id_or_source: &str,
    ) -> Result<i64, String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        if let Some(record) = record_by_id(&mut tx, id_or_source).await? {
            if !can_access(access, &record.scope, &record.scope_id) {
                return Err("memory is outside this session's scope".into());
            }
            let sources: Vec<String> =
                sqlx::query_scalar("SELECT source_id FROM memory_record_sources WHERE memory_id=?")
                    .bind(id_or_source)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            let mut memories = Vec::new();
            let mut skills = Vec::new();
            for source in &sources {
                memories.extend(
                    sqlx::query_scalar::<_, String>(
                        "SELECT DISTINCT memory_id FROM memory_record_sources WHERE source_id=?",
                    )
                    .bind(source)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?,
                );
                skills.extend(
                    sqlx::query_scalar::<_, String>(
                        "SELECT DISTINCT skill_id FROM skill_sources WHERE source_id=?",
                    )
                    .bind(source)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?,
                );
            }
            memories.sort();
            memories.dedup();
            skills.sort();
            skills.dedup();
            for source in &sources {
                sqlx::query("UPDATE memory_sources SET active=0,updated_at=? WHERE source_id=?")
                    .bind(now())
                    .bind(source)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            for mid in &memories {
                sqlx::query("UPDATE memory_records SET active=0,updated_at=? WHERE id=?")
                    .bind(now())
                    .bind(mid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            for sid in &skills {
                sqlx::query("UPDATE learned_skills SET active=0,updated_at=? WHERE id=?")
                    .bind(now())
                    .bind(sid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let event = event_tx(
                &mut tx,
                "memory.forgotten",
                id_or_source,
                &serde_json::json!({"memoryIds":memories,"sourceIds":sources,"skillIds":skills}),
            )
            .await?;
            for source in &sources {
                sqlx::query("UPDATE memory_sources SET event_id=? WHERE source_id=?")
                    .bind(event)
                    .bind(source)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            for mid in &memories {
                sqlx::query("UPDATE memory_records SET event_id=? WHERE id=?")
                    .bind(event)
                    .bind(mid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            for sid in &skills {
                sqlx::query("UPDATE learned_skills SET event_id=? WHERE id=?")
                    .bind(event)
                    .bind(sid)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            for source in sources {
                self.remove_skill_packages_for_sources(&source).await?;
            }
            return Ok(event);
        }
        let source = source_by_id(&mut tx, id_or_source)
            .await?
            .ok_or_else(|| "memory or source not found".to_string())?;
        if !source_visible(access, &source) {
            return Err("source is outside this session's scope".into());
        }
        let memories: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT memory_id FROM memory_record_sources WHERE source_id=?",
        )
        .bind(id_or_source)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        let skills: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT skill_id FROM skill_sources WHERE source_id=?")
                .bind(id_or_source)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        sqlx::query("UPDATE memory_sources SET active=0,updated_at=? WHERE source_id=?")
            .bind(now())
            .bind(id_or_source)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        for mid in &memories {
            sqlx::query("UPDATE memory_records SET active=0,updated_at=? WHERE id=?")
                .bind(now())
                .bind(mid)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        for sid in &skills {
            sqlx::query("UPDATE learned_skills SET active=0,updated_at=? WHERE id=?")
                .bind(now())
                .bind(sid)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        let event = event_tx(
            &mut tx,
            "memory.source_forgotten",
            id_or_source,
            &serde_json::json!({"memoryIds":memories,"skillIds":skills,"sourceIds":[id_or_source]}),
        )
        .await?;
        sqlx::query("UPDATE memory_sources SET event_id=? WHERE source_id=?")
            .bind(event)
            .bind(id_or_source)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        for mid in &memories {
            sqlx::query("UPDATE memory_records SET event_id=? WHERE id=?")
                .bind(event)
                .bind(mid)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        for sid in &skills {
            sqlx::query("UPDATE learned_skills SET event_id=? WHERE id=?")
                .bind(event)
                .bind(sid)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        self.remove_skill_packages_for_sources(id_or_source).await?;
        Ok(event)
    }

    pub async fn undo_memory_event(
        &self,
        access: &MemoryAccess,
        event_id: i64,
    ) -> Result<(), String> {
        if sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_undo_results WHERE event_id=?")
            .bind(event_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| e.to_string())?
            > 0
        {
            return Ok(());
        }
        let row =
            sqlx::query_as::<_, (String, String)>("SELECT kind,payload FROM events WHERE seq=?")
                .bind(event_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "memory event not found".to_string())?;
        if row.0 != "memory.forgotten" && row.0 != "memory.source_forgotten" {
            return Err("event cannot be undone".into());
        }
        let payload: serde_json::Value = serde_json::from_str(&row.1).map_err(|e| e.to_string())?;
        let mids: Vec<String> =
            serde_json::from_value(payload.get("memoryIds").cloned().unwrap_or_default())
                .map_err(|e| e.to_string())?;
        let sids: Vec<String> =
            serde_json::from_value(payload.get("skillIds").cloned().unwrap_or_default())
                .map_err(|e| e.to_string())?;
        let sources: Vec<String> =
            serde_json::from_value(payload.get("sourceIds").cloned().unwrap_or_default())
                .map_err(|e| e.to_string())?;
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        if sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_undo_results WHERE event_id=?")
            .bind(event_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?
            > 0
        {
            tx.rollback().await.map_err(|e| e.to_string())?;
            return Ok(());
        }
        for mid in &mids {
            let r = record_by_id(&mut tx, mid)
                .await?
                .ok_or_else(|| "forgotten memory no longer exists".to_string())?;
            if !can_access(access, &r.scope, &r.scope_id) {
                return Err("memory is outside this session's scope".into());
            }
        }
        for sid in &sids {
            let row = sqlx::query_as::<_, (String, String)>(
                "SELECT scope,scope_id FROM learned_skills WHERE id=?",
            )
            .bind(sid)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "forgotten skill no longer exists".to_string())?;
            if !can_access(access, &parse_scope(&row.0)?, &row.1) {
                return Err("skill is outside this session's scope".into());
            }
        }
        for src in &sources {
            let source = source_by_id(&mut tx, src)
                .await?
                .ok_or_else(|| "forgotten source no longer exists".to_string())?;
            if !source_visible(access, &source) {
                return Err("source is outside this session's scope".into());
            }
        }
        let undo_event = event_tx(
            &mut tx,
            "memory.forget_undone",
            &event_id.to_string(),
            &serde_json::json!({"eventId":event_id}),
        )
        .await?;
        sqlx::query("INSERT INTO memory_undo_results(event_id,undo_event_id) VALUES(?,?)")
            .bind(event_id)
            .bind(undo_event)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        for src in sources {
            sqlx::query("UPDATE memory_sources SET active=1,updated_at=?,event_id=? WHERE source_id=? AND active=0 AND event_id=?")
                .bind(now()).bind(undo_event).bind(src).bind(event_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        for mid in mids {
            sqlx::query("UPDATE memory_records SET active=1,updated_at=?,event_id=? WHERE id=? AND active=0 AND event_id=?")
                .bind(now())
                .bind(undo_event).bind(mid).bind(event_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        for sid in &sids {
            sqlx::query("UPDATE learned_skills SET active=1,updated_at=?,event_id=? WHERE id=? AND active=0 AND event_id=?")
                .bind(now())
                .bind(undo_event).bind(sid).bind(event_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        for sid in sids {
            let active: i64 = sqlx::query_scalar("SELECT active FROM learned_skills WHERE id=?")
                .bind(&sid)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| e.to_string())?
                .unwrap_or(0);
            if active != 0 {
                if let Some(skill) = self.skill_by_id(&sid).await? {
                    if self.skill_is_current(&sid).await? {
                        self.materialize_skill(&skill)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Trusted UI management path can undo a forget that affected multiple scopes.
    pub async fn undo_memory_event_for_user(&self, event_id: i64) -> Result<(), String> {
        let access = MemoryAccess {
            thread_id: "memory-management".into(),
            agent_id: None,
            project_id: None,
            user_id: "local-user".into(),
            eligible: true,
            management: true,
        };
        self.undo_memory_event(&access, event_id).await
    }

    pub async fn publish_skill(
        &self,
        access: &MemoryAccess,
        scope: MemoryScope,
        name: &str,
        description: &str,
        content: &str,
        source_ids: &[String],
    ) -> Result<LearnedSkill, String> {
        if !access.eligible {
            return Err("learning is disabled for this session".into());
        }
        if source_ids.is_empty() {
            return Err("skills require source evidence".into());
        }
        let scope_id = scope_id(access, scope)?;
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let sources = validate_sources(&mut tx, access, scope, source_ids).await?;
        let mut expected_links = sources.clone();
        expected_links.sort();
        let candidates=sqlx::query_scalar::<_,String>("SELECT id FROM learned_skills WHERE scope=? AND scope_id=? AND name=? AND description=? AND content=? AND active=1")
            .bind(scope.as_str()).bind(&scope_id).bind(name).bind(description).bind(content).fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
        for candidate in candidates {
            let mut links: Vec<(String, i64)> = sqlx::query_as(
                "SELECT source_id,source_revision FROM skill_sources WHERE skill_id=?",
            )
            .bind(&candidate)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            links.sort();
            if links == expected_links {
                tx.commit().await.map_err(|e| e.to_string())?;
                let skill = self
                    .skill_by_id(&candidate)
                    .await?
                    .ok_or_else(|| "learned skill disappeared".to_string())?;
                self.materialize_skill(&skill)?;
                return Ok(skill);
            }
        }
        let skill = LearnedSkill {
            id: id(),
            name: name.into(),
            description: description.into(),
            content: content.into(),
            revision: 1,
            scope,
            scope_id,
            source_ids: canonical_source_ids(source_ids),
            created_at: now(),
            updated_at: now(),
        };
        sqlx::query("INSERT INTO learned_skills(id,name,description,content,revision,scope,scope_id,active,created_at,updated_at) VALUES(?,?,?,?,1,?,?,1,?,?)").bind(&skill.id).bind(name).bind(description).bind(content).bind(scope.as_str()).bind(&skill.scope_id).bind(skill.created_at).bind(skill.updated_at).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        insert_skill_links(&mut tx, &skill.id, &sources).await?;
        let event = event_tx(&mut tx, "skill.published", &skill.id, &skill).await?;
        sqlx::query("UPDATE learned_skills SET event_id=? WHERE id=?")
            .bind(event)
            .bind(&skill.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        self.materialize_skill(&skill)?;
        Ok(skill)
    }
    pub async fn list_skills(&self, access: &MemoryAccess) -> Result<Vec<LearnedSkill>, String> {
        let rows=sqlx::query_as::<_,(String,String,String,String,i64,String,String,i64,i64)>("SELECT id,name,description,content,revision,scope,scope_id,created_at,updated_at FROM learned_skills WHERE active=1 ORDER BY updated_at DESC,id").fetch_all(&self.pool).await.map_err(|e|e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            let scope = parse_scope(&r.5)?;
            if !can_access(access, &scope, &r.6) {
                continue;
            }
            if self.skill_is_current(&r.0).await? {
                let skill = self.load_skill(r).await?;
                self.materialize_skill(&skill)?;
                out.push(skill);
            } else {
                self.remove_skill_package(&r.0).await?;
            }
        }
        Ok(out)
    }
    pub async fn revoke_skill(&self, access: &MemoryAccess, skill_id: &str) -> Result<(), String> {
        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT scope,scope_id FROM learned_skills WHERE id=? AND active=1",
        )
        .bind(skill_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "skill not found".to_string())?;
        let scope = parse_scope(&row.0)?;
        if !can_access(access, &scope, &row.1) {
            return Err("skill is outside this session's scope".into());
        }
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        sqlx::query("UPDATE learned_skills SET active=0,updated_at=? WHERE id=?")
            .bind(now())
            .bind(skill_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let event = event_tx(
            &mut tx,
            "skill.revoked",
            skill_id,
            &serde_json::json!({"id":skill_id}),
        )
        .await?;
        sqlx::query("UPDATE learned_skills SET event_id=? WHERE id=?")
            .bind(event)
            .bind(skill_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        self.remove_skill_package(skill_id).await
    }
    pub async fn compact_context(
        &self,
        access: &MemoryAccess,
        query: &str,
        max_chars: usize,
    ) -> Result<String, String> {
        let records = self.search_memory(access, query).await?;
        let skills = self.list_skills(access).await?;
        let mut out = String::new();
        for r in records {
            let line = format!("- [{}] {}\n", r.kind, r.content);
            if out.chars().count() + line.chars().count() > max_chars {
                break;
            }
            out.push_str(&line)
        }
        for s in skills {
            let line = format!("- Skill {}: {}\n", s.name, s.description);
            if out.chars().count() + line.chars().count() > max_chars {
                break;
            }
            out.push_str(&line)
        }
        Ok(out)
    }

    async fn record_is_current(&self, id: &str) -> Result<bool, String> {
        let good:i64=sqlx::query_scalar("SELECT COUNT(*) FROM memory_record_sources rs JOIN memory_sources s ON s.source_id=rs.source_id WHERE rs.memory_id=? AND s.active=1 AND rs.source_revision=s.revision").bind(id).fetch_one(&self.pool).await.map_err(|e|e.to_string())?;
        let all: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM memory_record_sources WHERE memory_id=?")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| e.to_string())?;
        Ok(good == all && all > 0)
    }
    async fn skill_is_current(&self, id: &str) -> Result<bool, String> {
        let good:i64=sqlx::query_scalar("SELECT COUNT(*) FROM skill_sources ss JOIN memory_sources s ON s.source_id=ss.source_id WHERE ss.skill_id=? AND s.active=1 AND ss.source_revision=s.revision").bind(id).fetch_one(&self.pool).await.map_err(|e|e.to_string())?;
        let all: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_sources WHERE skill_id=?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok(good == all && all > 0)
    }
    async fn skill_by_id(&self, id: &str) -> Result<Option<LearnedSkill>, String> {
        let row=sqlx::query_as::<_,(String,String,String,String,i64,String,String,i64,i64)>("SELECT id,name,description,content,revision,scope,scope_id,created_at,updated_at FROM learned_skills WHERE id=?").bind(id).fetch_optional(&self.pool).await.map_err(|e|e.to_string())?;
        match row {
            Some(r) => Ok(Some(self.load_skill(r).await?)),
            None => Ok(None),
        }
    }
    async fn remove_skill_packages_for_ids(&self, ids: &[String]) -> Result<(), String> {
        for id in ids {
            self.remove_skill_package(id).await?;
        }
        Ok(())
    }
    async fn remove_skill_packages_for_sources(&self, source_id: &str) -> Result<(), String> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT skill_id FROM skill_sources WHERE source_id=?")
                .bind(source_id)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| e.to_string())?;
        self.remove_skill_packages_for_ids(&ids).await
    }
    async fn remove_skill_package(&self, id: &str) -> Result<(), String> {
        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT scope,scope_id FROM learned_skills WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        if let Some((scope, scope_id)) = row {
            let path = self
                .root
                .join("skills")
                .join(safe_component(&scope))
                .join(safe_component(&scope_id))
                .join(id);
            if path.exists() {
                std::fs::remove_dir_all(path).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    async fn load_record(
        &self,
        r: (String, String, String, String, i64, i64, i64, Option<i64>),
    ) -> Result<MemoryRecord, String> {
        let source_ids = sqlx::query_scalar(
            "SELECT source_id FROM memory_record_sources WHERE memory_id=? ORDER BY source_id",
        )
        .bind(&r.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let content = sqlx::query_scalar("SELECT content FROM memory_records WHERE id=?")
            .bind(&r.0)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok(MemoryRecord {
            id: r.0,
            scope: parse_scope(&r.1)?,
            scope_id: r.2,
            kind: r.3,
            content,
            revision: r.4,
            source_ids,
            created_at: r.5,
            updated_at: r.6,
            event_id: r.7,
        })
    }
    async fn load_skill(
        &self,
        r: (
            String,
            String,
            String,
            String,
            i64,
            String,
            String,
            i64,
            i64,
        ),
    ) -> Result<LearnedSkill, String> {
        let source_ids = sqlx::query_scalar(
            "SELECT source_id FROM skill_sources WHERE skill_id=? ORDER BY source_id",
        )
        .bind(&r.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(LearnedSkill {
            id: r.0,
            name: r.1,
            description: r.2,
            content: r.3,
            revision: r.4,
            scope: parse_scope(&r.5)?,
            scope_id: r.6,
            source_ids,
            created_at: r.7,
            updated_at: r.8,
        })
    }
    fn materialize_skill(&self, skill: &LearnedSkill) -> Result<(), String> {
        let d = self
            .root
            .join("skills")
            .join(safe_component(skill.scope.as_str()))
            .join(safe_component(&skill.scope_id))
            .join(&skill.id)
            .join(skill.revision.to_string());
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let text = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}",
            skill.name, skill.description, skill.content
        );
        let path = d.join("SKILL.md");
        let temp = d.join(format!("SKILL-{}.tmp", id()));
        std::fs::write(&temp, text).map_err(|e| e.to_string())?;
        std::fs::rename(temp, path).map_err(|e| e.to_string())
    }
}

fn access_from_binding(b: SessionBinding) -> MemoryAccess {
    MemoryAccess {
        thread_id: b.thread_id,
        agent_id: b.agent_id,
        project_id: b.project_id,
        user_id: "local-user".into(),
        eligible: b.learning_eligible,
        management: false,
    }
}
fn scope_id(a: &MemoryAccess, s: MemoryScope) -> Result<String, String> {
    match s {
        MemoryScope::Agent => a
            .agent_id
            .clone()
            .ok_or_else(|| "agent scope requires a named agent session".into()),
        MemoryScope::User => Ok(a.user_id.clone()),
        MemoryScope::Project => a
            .project_id
            .clone()
            .ok_or_else(|| "project scope requires a project session".into()),
    }
}
fn can_access(a: &MemoryAccess, s: &MemoryScope, id: &str) -> bool {
    if a.management {
        return true;
    }
    match s {
        MemoryScope::Agent => a.agent_id.as_deref() == Some(id),
        MemoryScope::User => a.user_id == id,
        MemoryScope::Project => a.project_id.as_deref() == Some(id),
    }
}
fn source_visible(a: &MemoryAccess, s: &MemorySource) -> bool {
    a.user_id == "local-user"
        && (a.management
            || (a.agent_id.is_some() && a.agent_id == s.agent_id)
            || a.thread_id == s.thread_id)
}
fn parse_scope(s: &str) -> Result<MemoryScope, String> {
    match s {
        "agent" => Ok(MemoryScope::Agent),
        "user" => Ok(MemoryScope::User),
        "project" => Ok(MemoryScope::Project),
        _ => Err("unknown memory scope".into()),
    }
}
fn safe_component(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn canonical_source_ids(ids: &[String]) -> Vec<String> {
    ids.iter()
        .map(|reference| {
            reference
                .split_once('@')
                .map_or(reference.as_str(), |(id, _)| id)
                .to_owned()
        })
        .collect()
}
async fn source_by_id(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<MemorySource>, String> {
    let r=sqlx::query_as::<_,(String,i64,String,Option<String>,Option<String>,String,i64)>("SELECT source_id,revision,thread_id,agent_id,project_id,content,active FROM memory_sources WHERE source_id=?").bind(id).fetch_optional(&mut **tx).await.map_err(|e|e.to_string())?;
    Ok(r.map(|r| MemorySource {
        source_id: r.0,
        revision: r.1,
        thread_id: r.2,
        agent_id: r.3,
        project_id: r.4,
        content: r.5,
        active: r.6 != 0,
    }))
}
async fn record_by_id(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<MemoryRecord>, String> {
    let r=sqlx::query_as::<_,(String,String,String,String,i64,i64,i64,Option<i64>)>("SELECT id,scope,scope_id,kind,revision,created_at,updated_at,event_id FROM memory_records WHERE id=?").bind(id).fetch_optional(&mut **tx).await.map_err(|e|e.to_string())?;
    if let Some(r) = r {
        let sources = sqlx::query_scalar(
            "SELECT source_id FROM memory_record_sources WHERE memory_id=? ORDER BY source_id",
        )
        .bind(id)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
        let content = sqlx::query_scalar("SELECT content FROM memory_records WHERE id=?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Some(MemoryRecord {
            id: r.0,
            scope: parse_scope(&r.1)?,
            scope_id: r.2,
            kind: r.3,
            content,
            revision: r.4,
            source_ids: sources,
            created_at: r.5,
            updated_at: r.6,
            event_id: r.7,
        }))
    } else {
        Ok(None)
    }
}
async fn validate_sources(
    tx: &mut Transaction<'_, Sqlite>,
    access: &MemoryAccess,
    scope: MemoryScope,
    ids: &[String],
) -> Result<Vec<(String, i64)>, String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for reference in ids {
        let (id, revision_token) = reference
            .split_once('@')
            .unwrap_or((reference.as_str(), ""));
        if id.is_empty() || !seen.insert(id.to_owned()) {
            return Err("source evidence contains a duplicate id".into());
        }
        let s = source_by_id(tx, id)
            .await?
            .ok_or_else(|| format!("source {id} has not been recorded"))?;
        if (!access.management && revision_token.is_empty())
            || (!revision_token.is_empty()
                && revision_token.parse::<i64>().ok() != Some(s.revision))
        {
            return Err(format!("source {id} revision is stale or missing"));
        }
        if !s.active || !source_visible(access, &s) {
            return Err(format!("source {id} is unavailable to this session"));
        }
        match scope {
            MemoryScope::Agent if s.agent_id != access.agent_id => {
                return Err("private memory cannot use another agent's evidence".into());
            }
            MemoryScope::Project if s.project_id != access.project_id => {
                return Err("project memory cannot use another project's evidence".into());
            }
            _ => {}
        }
        out.push((id.to_owned(), s.revision));
    }
    Ok(out)
}
async fn insert_links(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    links: &[(String, i64)],
) -> Result<(), String> {
    for (source, revision) in links {
        sqlx::query(
            "INSERT INTO memory_record_sources(memory_id,source_id,source_revision) VALUES(?,?,?)",
        )
        .bind(id)
        .bind(source)
        .bind(revision)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
async fn insert_skill_links(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    links: &[(String, i64)],
) -> Result<(), String> {
    for (source, revision) in links {
        sqlx::query("INSERT INTO skill_sources(skill_id,source_id,source_revision) VALUES(?,?,?)")
            .bind(id)
            .bind(source)
            .bind(revision)
            .execute(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
async fn event_tx<T: Serialize>(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    entity: &str,
    payload: &T,
) -> Result<i64, String> {
    sqlx::query("INSERT INTO events(kind,entity_id,payload,created_at) VALUES(?,?,?,?)")
        .bind(kind)
        .bind(entity)
        .bind(serde_json::to_string(payload).map_err(|e| e.to_string())?)
        .bind(now())
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query_scalar("SELECT last_insert_rowid()")
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| e.to_string())
}
