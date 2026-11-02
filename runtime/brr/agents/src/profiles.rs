use super::{AgentStore, digest, id, now};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCandidate {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub soul: String,
    pub model: String,
    pub effort: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub id: String,
    pub revision: i64,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub soul: String,
    pub model: String,
    pub effort: String,
    pub created_at: i64,
    pub updated_at: i64,
}
impl AgentProfile {
    fn candidate(&self) -> ProfileCandidate {
        ProfileCandidate {
            name: self.name.clone(),
            description: self.description.clone(),
            instructions: self.instructions.clone(),
            soul: self.soul.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDraft {
    pub id: String,
    pub thread_id: String,
    pub agent_id: Option<String>,
    pub candidate: ProfileCandidate,
    pub hash: String,
    pub status: String,
    pub base_revision: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBinding {
    pub thread_id: String,
    pub agent_id: Option<String>,
    pub kind: String,
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub task_run_id: Option<String>,
    pub learning_eligible: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppliedProfileOperation {
    draft_id: String,
    hash: String,
    profile: AgentProfile,
}

pub(crate) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("CREATE TABLE IF NOT EXISTS agent_profiles (id TEXT PRIMARY KEY, revision INTEGER NOT NULL, candidate TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS profile_revisions (agent_id TEXT NOT NULL, revision INTEGER NOT NULL, candidate TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, draft_id TEXT NOT NULL UNIQUE, hash TEXT NOT NULL, PRIMARY KEY(agent_id,revision))").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS profile_drafts (id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, agent_id TEXT, candidate TEXT NOT NULL, hash TEXT NOT NULL, status TEXT NOT NULL, base_revision INTEGER, created_at INTEGER NOT NULL)").execute(pool).await.map_err(|e|e.to_string())?;
    sqlx::query("CREATE TABLE IF NOT EXISTS session_bindings (thread_id TEXT PRIMARY KEY, agent_id TEXT, kind TEXT NOT NULL, project_id TEXT, cwd TEXT, task_run_id TEXT, learning_eligible INTEGER NOT NULL)").execute(pool).await.map_err(|e|e.to_string())?;
    Ok(())
}

impl AgentStore {
    pub async fn propose_profile(
        &self,
        thread_id: impl Into<String>,
        agent_id: Option<String>,
        candidate: ProfileCandidate,
    ) -> Result<ProfileDraft, String> {
        let thread_id = thread_id.into();
        let hash = digest(&candidate)?;
        let base_revision = if let Some(agent) = agent_id.as_deref() {
            Some(
                sqlx::query_scalar::<_, i64>("SELECT revision FROM agent_profiles WHERE id=?")
                    .bind(agent)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "profile not found".to_string())?,
            )
        } else {
            None
        };
        let draft = ProfileDraft {
            id: id(),
            thread_id,
            agent_id,
            candidate,
            hash,
            status: "proposed".into(),
            base_revision,
        };
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO profile_drafts (id,thread_id,agent_id,candidate,hash,status,base_revision,created_at) VALUES (?,?,?,?,?,'proposed',?,?)")
            .bind(&draft.id).bind(&draft.thread_id).bind(&draft.agent_id).bind(serde_json::to_string(&draft.candidate).map_err(|e|e.to_string())?).bind(&draft.hash).bind(draft.base_revision).bind(now())
            .execute(&mut *tx).await.map_err(|e|e.to_string())?;
        event_tx(&mut tx, "profile.proposed", &draft.id, &draft).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(draft)
    }

    pub async fn approve_profile(
        &self,
        draft_id: &str,
        hash: &str,
        user_action_id: &str,
    ) -> Result<ProfileDraft, String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        if let Some((kind, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT kind,result FROM operation_results WHERE operation_id=?",
        )
        .bind(user_action_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        {
            if kind != "profile.approve" {
                return Err("operation id already used for another action".into());
            }
            let prior: ProfileDraft = serde_json::from_str(&result).map_err(|e| e.to_string())?;
            if prior.id != draft_id || prior.hash != hash {
                return Err("review action does not match this draft".into());
            }
            tx.commit().await.map_err(|e| e.to_string())?;
            return Ok(prior);
        }
        let draft = draft_by_id(&mut tx, draft_id).await?;
        if draft.hash != hash || digest(&draft.candidate)? != hash {
            return Err("profile draft changed since review".into());
        }
        if draft.status != "proposed" && draft.status != "approved" {
            return Err("profile draft is not reviewable".into());
        }
        sqlx::query("UPDATE profile_drafts SET status='approved' WHERE id=?")
            .bind(draft_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let approved = ProfileDraft {
            status: "approved".into(),
            ..draft
        };
        sqlx::query("INSERT INTO operation_results(operation_id,kind,result) VALUES(?,?,?)")
            .bind(user_action_id)
            .bind("profile.approve")
            .bind(serde_json::to_string(&approved).map_err(|e| e.to_string())?)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        event_tx(&mut tx, "profile.approved", draft_id, &approved).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(approved)
    }

    pub async fn apply_profile(
        &self,
        draft_id: &str,
        hash: &str,
        operation_id: &str,
    ) -> Result<AgentProfile, String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        if let Some((kind, result)) = sqlx::query_as::<_, (String, String)>(
            "SELECT kind,result FROM operation_results WHERE operation_id=?",
        )
        .bind(operation_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        {
            if kind != "profile.apply" {
                return Err("operation id already used for another action".into());
            }
            let applied: AppliedProfileOperation =
                serde_json::from_str(&result).map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            if applied.draft_id != draft_id || applied.hash != hash {
                return Err("operation id belongs to a different profile draft".into());
            }
            self.materialize(&applied.profile)?;
            return Ok(applied.profile);
        }
        let draft = draft_by_id(&mut tx, draft_id).await?;
        if draft.hash != hash || digest(&draft.candidate)? != hash {
            return Err("profile draft changed since review".into());
        }
        if draft.status == "applied" {
            let row=sqlx::query_as::<_,(String,i64,String,i64,i64,String)>("SELECT agent_id,revision,candidate,created_at,updated_at,hash FROM profile_revisions WHERE draft_id=?").bind(draft_id).fetch_optional(&mut *tx).await.map_err(|e|e.to_string())?.ok_or_else(||"applied profile revision is missing".to_string())?;
            if row.5 != hash {
                return Err("applied profile hash does not match".into());
            }
            let profile = decode_profile((row.0, row.1, row.2, row.3, row.4))?;
            let result = AppliedProfileOperation {
                draft_id: draft_id.into(),
                hash: hash.into(),
                profile: profile.clone(),
            };
            sqlx::query("INSERT INTO operation_results(operation_id,kind,result) VALUES(?,?,?)")
                .bind(operation_id)
                .bind("profile.apply")
                .bind(serde_json::to_string(&result).map_err(|e| e.to_string())?)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            self.materialize(&profile)?;
            return Ok(profile);
        }
        if draft.status != "approved" {
            return Err("profile draft must be approved before application".into());
        }
        let profile_id = draft.agent_id.clone().unwrap_or_else(id);
        let existing = sqlx::query_as::<_, (i64, i64)>(
            "SELECT revision,created_at FROM agent_profiles WHERE id=?",
        )
        .bind(&profile_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        if existing.as_ref().map(|v| v.0) != draft.base_revision {
            return Err("profile changed after this draft was proposed".into());
        }
        let (revision, created_at) = existing.map(|(r, c)| (r + 1, c)).unwrap_or((1, now()));
        let profile = AgentProfile {
            id: profile_id,
            revision,
            name: draft.candidate.name.clone(),
            description: draft.candidate.description.clone(),
            instructions: draft.candidate.instructions.clone(),
            soul: draft.candidate.soul.clone(),
            model: draft.candidate.model.clone(),
            effort: draft.candidate.effort.clone(),
            created_at,
            updated_at: now(),
        };
        sqlx::query("INSERT INTO agent_profiles(id,revision,candidate,created_at,updated_at) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,candidate=excluded.candidate,updated_at=excluded.updated_at")
            .bind(&profile.id).bind(profile.revision).bind(serde_json::to_string(&profile.candidate()).map_err(|e|e.to_string())?).bind(profile.created_at).bind(profile.updated_at).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        sqlx::query("INSERT INTO profile_revisions(agent_id,revision,candidate,created_at,updated_at,draft_id,hash) VALUES(?,?,?,?,?,?,?)").bind(&profile.id).bind(profile.revision).bind(serde_json::to_string(&profile.candidate()).map_err(|e|e.to_string())?).bind(profile.created_at).bind(profile.updated_at).bind(draft_id).bind(hash).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        sqlx::query("UPDATE profile_drafts SET status='applied' WHERE id=?")
            .bind(draft_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let result = AppliedProfileOperation {
            draft_id: draft_id.into(),
            hash: hash.into(),
            profile: profile.clone(),
        };
        sqlx::query("INSERT INTO operation_results(operation_id,kind,result) VALUES(?,?,?)")
            .bind(operation_id)
            .bind("profile.apply")
            .bind(serde_json::to_string(&result).map_err(|e| e.to_string())?)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        event_tx(&mut tx, "profile.applied", &profile.id, &profile).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        self.materialize(&profile)?;
        Ok(profile)
    }

    pub async fn list_profiles(&self) -> Result<Vec<AgentProfile>, String> {
        let rows = sqlx::query_as::<_,(String,i64,String,i64,i64)>("SELECT id,revision,candidate,created_at,updated_at FROM agent_profiles ORDER BY created_at,id").fetch_all(&self.pool).await.map_err(|e|e.to_string())?;
        rows.into_iter().map(decode_profile).collect()
    }
    pub async fn read_profile(&self, agent_id: &str) -> Result<AgentProfile, String> {
        let row = sqlx::query_as::<_, (String, i64, String, i64, i64)>(
            "SELECT id,revision,candidate,created_at,updated_at FROM agent_profiles WHERE id=?",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "profile not found".to_string())?;
        let profile = decode_profile(row)?;
        self.materialize(&profile)?;
        Ok(profile)
    }
    pub async fn list_profile_drafts(
        &self,
        thread_id: Option<&str>,
    ) -> Result<Vec<ProfileDraft>, String> {
        let rows = if let Some(thread) = thread_id {
            sqlx::query_as::<_,(String,String,Option<String>,String,String,String,Option<i64>)>("SELECT id,thread_id,agent_id,candidate,hash,status,base_revision FROM profile_drafts WHERE thread_id=? ORDER BY created_at,id").bind(thread).fetch_all(&self.pool).await.map_err(|e|e.to_string())?
        } else {
            sqlx::query_as::<_,(String,String,Option<String>,String,String,String,Option<i64>)>("SELECT id,thread_id,agent_id,candidate,hash,status,base_revision FROM profile_drafts ORDER BY created_at,id").fetch_all(&self.pool).await.map_err(|e|e.to_string())?
        };
        rows.into_iter().map(decode_draft).collect()
    }

    pub async fn bind_session(&self, binding: SessionBinding) -> Result<SessionBinding, String> {
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        if let Some(prior) = binding_by_thread(&mut tx, &binding.thread_id).await? {
            tx.commit().await.map_err(|e| e.to_string())?;
            return if prior == binding {
                Ok(prior)
            } else {
                Err("session identity binding is immutable".into())
            };
        }
        sqlx::query("INSERT INTO session_bindings(thread_id,agent_id,kind,project_id,cwd,task_run_id,learning_eligible) VALUES(?,?,?,?,?,?,?)")
            .bind(&binding.thread_id).bind(&binding.agent_id).bind(&binding.kind).bind(&binding.project_id).bind(&binding.cwd).bind(&binding.task_run_id).bind(binding.learning_eligible as i64).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        event_tx(&mut tx, "session.bound", &binding.thread_id, &binding).await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(binding)
    }
    pub async fn session_binding(&self, thread_id: &str) -> Result<SessionBinding, String> {
        let mut conn = self.pool.acquire().await.map_err(|e| e.to_string())?;
        binding_by_thread(&mut *conn, thread_id)
            .await?
            .ok_or_else(|| "session binding not found".into())
    }
    pub async fn list_bindings(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<SessionBinding>, String> {
        let rows = if let Some(agent) = agent_id {
            sqlx::query_as::<_,(String,Option<String>,String,Option<String>,Option<String>,Option<String>,i64)>("SELECT thread_id,agent_id,kind,project_id,cwd,task_run_id,learning_eligible FROM session_bindings WHERE agent_id=? ORDER BY thread_id").bind(agent).fetch_all(&self.pool).await.map_err(|e|e.to_string())?
        } else {
            sqlx::query_as::<_,(String,Option<String>,String,Option<String>,Option<String>,Option<String>,i64)>("SELECT thread_id,agent_id,kind,project_id,cwd,task_run_id,learning_eligible FROM session_bindings ORDER BY thread_id").fetch_all(&self.pool).await.map_err(|e|e.to_string())?
        };
        rows.into_iter()
            .map(|r| {
                Ok(SessionBinding {
                    thread_id: r.0,
                    agent_id: r.1,
                    kind: r.2,
                    project_id: r.3,
                    cwd: r.4,
                    task_run_id: r.5,
                    learning_eligible: r.6 != 0,
                })
            })
            .collect()
    }

    fn materialize(&self, profile: &AgentProfile) -> Result<(), String> {
        let dir = self
            .root
            .join("profiles")
            .join(&profile.id)
            .join(profile.revision.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        write_atomic(&dir.join("instructions.md"), &profile.instructions)?;
        write_atomic(&dir.join("soul.md"), &profile.soul)
    }
}

fn write_atomic(path: &std::path::Path, content: &str) -> Result<(), String> {
    let temp = path.with_extension(format!("{}.tmp", id()));
    std::fs::write(&temp, content).map_err(|e| e.to_string())?;
    std::fs::rename(temp, path).map_err(|e| e.to_string())
}
fn decode_profile(r: (String, i64, String, i64, i64)) -> Result<AgentProfile, String> {
    let c: ProfileCandidate = serde_json::from_str(&r.2).map_err(|e| e.to_string())?;
    Ok(AgentProfile {
        id: r.0,
        revision: r.1,
        name: c.name,
        description: c.description,
        instructions: c.instructions,
        soul: c.soul,
        model: c.model,
        effort: c.effort,
        created_at: r.3,
        updated_at: r.4,
    })
}
fn decode_draft(
    r: (
        String,
        String,
        Option<String>,
        String,
        String,
        String,
        Option<i64>,
    ),
) -> Result<ProfileDraft, String> {
    Ok(ProfileDraft {
        id: r.0,
        thread_id: r.1,
        agent_id: r.2,
        candidate: serde_json::from_str(&r.3).map_err(|e| e.to_string())?,
        hash: r.4,
        status: r.5,
        base_revision: r.6,
    })
}
async fn draft_by_id(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
) -> Result<ProfileDraft, String> {
    let r=sqlx::query_as::<_,(String,String,Option<String>,String,String,String,Option<i64>)>("SELECT id,thread_id,agent_id,candidate,hash,status,base_revision FROM profile_drafts WHERE id=?").bind(id).fetch_optional(&mut **tx).await.map_err(|e|e.to_string())?.ok_or_else(||"profile draft not found".to_string())?;
    decode_draft(r)
}
async fn binding_by_thread(
    tx: &mut sqlx::SqliteConnection,
    thread: &str,
) -> Result<Option<SessionBinding>, String> {
    sqlx::query_as::<_,(String,Option<String>,String,Option<String>,Option<String>,Option<String>,i64)>("SELECT thread_id,agent_id,kind,project_id,cwd,task_run_id,learning_eligible FROM session_bindings WHERE thread_id=?").bind(thread).fetch_optional(&mut *tx).await.map_err(|e|e.to_string()).map(|r|r.map(|r|SessionBinding{thread_id:r.0,agent_id:r.1,kind:r.2,project_id:r.3,cwd:r.4,task_run_id:r.5,learning_eligible:r.6!=0}))
}
async fn event_tx<T: Serialize>(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    kind: &str,
    entity: &str,
    payload: &T,
) -> Result<(), String> {
    sqlx::query("INSERT INTO events(kind,entity_id,payload,created_at) VALUES(?,?,?,?)")
        .bind(kind)
        .bind(entity)
        .bind(serde_json::to_string(payload).map_err(|e| e.to_string())?)
        .bind(now())
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
