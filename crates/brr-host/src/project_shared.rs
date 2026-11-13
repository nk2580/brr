use super::*;
use brr_agent_core::{shared_local_id, SharedRow, SharedSnapshot, SharedTable};
use serde_json::json;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashSet;

pub(super) async fn init(pool: &SqlitePool) -> Result<(), String> {
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_project_identity(local_id TEXT PRIMARY KEY NOT NULL,origin_node_id TEXT NOT NULL,entity_key TEXT NOT NULL,UNIQUE(origin_node_id,entity_key))")
        .execute(pool)
        .await
        .map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_project_metadata(local_id TEXT PRIMARY KEY NOT NULL REFERENCES project_records(id) ON DELETE CASCADE,shared_revision INTEGER NOT NULL CHECK(shared_revision > 0))")
        .execute(pool)
        .await
        .map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_project_tombstones(local_id TEXT PRIMARY KEY NOT NULL REFERENCES project_records(id) ON DELETE CASCADE)")
        .execute(pool)
        .await
        .map_err(err)?;
    sqlx::query("CREATE TABLE IF NOT EXISTS fleet_project_state(id INTEGER PRIMARY KEY CHECK(id=1),node_id TEXT NOT NULL,revision TEXT NOT NULL)")
        .execute(pool)
        .await
        .map_err(err)?;
    // Projects that predate fleet metadata start at shared revision one.
    sqlx::query("INSERT OR IGNORE INTO fleet_project_metadata(local_id,shared_revision) SELECT id,1 FROM project_records")
        .execute(pool)
        .await
        .map_err(err)?;
    Ok(())
}

async fn rows(tx: &mut Transaction<'_, Sqlite>, node: &str) -> Result<Vec<SharedRow>, String> {
    let mut result = Vec::new();
    for row in sqlx::query("SELECT p.id,p.name,COALESCE(m.shared_revision,1) AS shared_revision,p.model,p.effort,p.permission,p.hidden,i.origin_node_id,i.entity_key FROM project_records p LEFT JOIN fleet_project_identity i ON i.local_id=p.id LEFT JOIN fleet_project_metadata m ON m.local_id=p.id LEFT JOIN fleet_project_tombstones t ON t.local_id=p.id WHERE t.local_id IS NULL ORDER BY p.id")
        .fetch_all(&mut **tx)
        .await
        .map_err(err)?
    {
        let id: String = row.try_get("id").map_err(err)?;
        let origin = row
            .try_get::<Option<String>, _>("origin_node_id")
            .map_err(err)?
            .unwrap_or_else(|| node.into());
        let key = row
            .try_get::<Option<String>, _>("entity_key")
            .map_err(err)?
            .unwrap_or(id);
        result.push(SharedRow {
            origin_node_id: Some(origin),
            table: SharedTable::Projects,
            key: key.clone(),
            data: json!({
                "id": key,
                "name": row.try_get::<String, _>("name").map_err(err)?,
                "revision": row.try_get::<i64, _>("shared_revision").map_err(err)?,
                "model": row.try_get::<Option<String>, _>("model").map_err(err)?,
                "effort": row.try_get::<Option<String>, _>("effort").map_err(err)?,
                "permission": row.try_get::<Option<String>, _>("permission").map_err(err)?,
                "hidden": row.try_get::<i64, _>("hidden").map_err(err)? != 0
            }),
        });
    }
    Ok(result)
}

impl ProjectStore {
    pub(super) async fn before_shared_commit(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<(), String> {
        let hook = self
            .shared_hook
            .read()
            .expect("project shared hook")
            .clone();
        if let Some((node, hook)) = hook {
            let candidate = rows(tx, &node).await?;
            for row in &candidate {
                if row.origin_node_id.as_deref() == Some(node.as_str()) {
                    sqlx::query("INSERT OR IGNORE INTO fleet_project_identity(local_id,origin_node_id,entity_key) VALUES(?,?,?)")
                        .bind(&row.key)
                        .bind(&node)
                        .bind(&row.key)
                        .execute(&mut **tx)
                        .await
                        .map_err(err)?;
                }
            }
            let revision = hook(&candidate)?;
            if revision.is_empty() {
                return Err("Project shared commit has no Git revision".into());
            }
            sqlx::query("INSERT INTO fleet_project_state(id,node_id,revision) VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision")
                .bind(&node)
                .bind(revision)
                .execute(&mut **tx)
                .await
                .map_err(err)?;
        }
        Ok(())
    }

    pub async fn shared_snapshot(&self, node: &str) -> Result<Vec<SharedRow>, String> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let snapshot = rows(&mut tx, node).await?;
        tx.rollback().await.map_err(err)?;
        Ok(snapshot)
    }

    pub async fn materialize_shared_snapshot(
        &self,
        node: &str,
        rows: &[SharedRow],
        revision: &str,
    ) -> Result<(), String> {
        SharedSnapshot {
            schema_version: 1,
            rows: rows.to_vec(),
        }
        .validate(true)?;
        if revision.is_empty() || node.is_empty() {
            return Err("Project materialization requires node and Git revision".into());
        }
        let mut tx = self.pool.begin().await.map_err(err)?;
        if let Some(existing) =
            sqlx::query_scalar::<_, String>("SELECT node_id FROM fleet_project_state WHERE id=1")
                .fetch_optional(&mut *tx)
                .await
                .map_err(err)?
        {
            if existing != node {
                return Err("Project store belongs to another node identity".into());
            }
        }

        let incoming = rows
            .iter()
            .map(|row| {
                if row.table != SharedTable::Projects {
                    return Err("Unexpected shared project table".to_owned());
                }
                Ok((
                    row.origin_node_id.as_deref().unwrap_or_default().to_owned(),
                    row.key.clone(),
                ))
            })
            .collect::<Result<HashSet<_>, String>>()?;

        // Git tombstones remove rows from the catalog snapshot. Hide those
        // projects locally while retaining folders, memberships, and the
        // local record so existing execution references remain stable.
        let previously_projected = sqlx::query_as::<_, (String, String, String)>(
            "SELECT local_id,origin_node_id,entity_key FROM fleet_project_identity",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(err)?;
        for (local_id, origin, key) in previously_projected {
            if incoming.contains(&(origin, key)) {
                continue;
            }
            sqlx::query("UPDATE project_records SET hidden=1,revision=revision+CASE WHEN hidden=0 THEN 1 ELSE 0 END WHERE id=?")
                .bind(&local_id)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
            sqlx::query("INSERT OR IGNORE INTO fleet_project_tombstones(local_id) VALUES(?)")
                .bind(local_id)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
        }

        for row in rows {
            if row.table != SharedTable::Projects {
                return Err("Unexpected shared project table".into());
            }
            let origin = row.origin_node_id.as_deref().unwrap();
            let id = if origin == node {
                row.key.clone()
            } else {
                shared_local_id(origin, SharedTable::Projects, &row.key)
            };
            let data = &row.data;
            let name = data["name"].as_str().ok_or("Invalid shared project name")?;
            let shared_revision = data["revision"]
                .as_i64()
                .filter(|value| *value > 0)
                .ok_or("Invalid shared project revision")?;
            let hidden = data["hidden"]
                .as_bool()
                .ok_or("Invalid shared project hidden flag")?;
            let optional = |key: &str| -> Result<Option<&str>, String> {
                if data[key].is_null() {
                    Ok(None)
                } else {
                    data[key]
                        .as_str()
                        .map(Some)
                        .ok_or_else(|| format!("Invalid shared project {key}"))
                }
            };
            let model = optional("model")?;
            let effort = optional("effort")?;
            let permission = optional("permission")?;
            if name.trim().is_empty()
                || name.len() > MAX_NAME_BYTES
                || model.is_some_and(|value| value.len() > MAX_MODEL_BYTES)
                || effort.is_some_and(|value| !ALLOWED_EFFORTS.contains(&value))
                || permission.is_some_and(|value| !matches!(value, "read-only" | "workspace-write"))
                || data["id"].as_str() != Some(row.key.as_str())
            {
                return Err("Invalid shared project metadata".into());
            }

            if let Some((old_origin, old_key)) = sqlx::query_as::<_, (String, String)>(
                "SELECT origin_node_id,entity_key FROM fleet_project_identity WHERE local_id=?",
            )
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(err)?
            {
                if old_origin != origin || old_key != row.key {
                    return Err("Project identity cannot change origin or key".into());
                }
            }

            let existing =
                sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>, i64)>(
                    "SELECT name,model,effort,permission,hidden FROM project_records WHERE id=?",
                )
                .bind(&id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(err)?;
            if let Some((old_name, old_model, old_effort, old_permission, old_hidden)) = existing {
                let metadata_changed = old_name != name
                    || old_model.as_deref() != model
                    || old_effort.as_deref() != effort
                    || old_permission.as_deref() != permission
                    || (old_hidden != 0) != hidden;
                sqlx::query("UPDATE project_records SET name=?,revision=revision+?,model=?,effort=?,permission=?,hidden=? WHERE id=?")
                    .bind(name)
                    .bind(if metadata_changed { 1_i64 } else { 0_i64 })
                    .bind(model)
                    .bind(effort)
                    .bind(permission)
                    .bind(hidden)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
                sqlx::query("INSERT INTO fleet_project_metadata(local_id,shared_revision) VALUES(?,?) ON CONFLICT(local_id) DO UPDATE SET shared_revision=excluded.shared_revision")
                    .bind(&id)
                    .bind(shared_revision)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
            } else {
                sqlx::query("INSERT INTO project_records(id,name,folders,primary_folder,revision,model,effort,permission,hidden) VALUES(?,?,'[]',NULL,1,?,?,?,?)")
                    .bind(&id)
                    .bind(name)
                    .bind(model)
                    .bind(effort)
                    .bind(permission)
                    .bind(hidden)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
                sqlx::query(
                    "INSERT INTO fleet_project_metadata(local_id,shared_revision) VALUES(?,?)",
                )
                .bind(&id)
                .bind(shared_revision)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
            }

            sqlx::query("DELETE FROM fleet_project_tombstones WHERE local_id=?")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
            sqlx::query("INSERT INTO fleet_project_identity(local_id,origin_node_id,entity_key) VALUES(?,?,?) ON CONFLICT(local_id) DO UPDATE SET origin_node_id=excluded.origin_node_id,entity_key=excluded.entity_key")
                .bind(&id)
                .bind(origin)
                .bind(&row.key)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
        }
        sqlx::query("INSERT INTO fleet_project_state(id,node_id,revision) VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision")
            .bind(node)
            .bind(revision)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        tx.commit().await.map_err(err)
    }
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
