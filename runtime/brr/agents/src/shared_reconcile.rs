use super::*;

/// Remove only rows previously managed by Git that are absent from its new
/// HEAD. Node-local tasks, runs, session bindings, leases and events survive.
pub(super) async fn reconcile(
    tx: &mut Transaction<'_, Sqlite>,
    node: &str,
    next: &SharedSnapshot,
) -> Result<(), String> {
    let mut rows = Vec::new();
    for row in sqlx::query(
        "SELECT origin_node_id,table_name,entity_key,payload FROM fleet_shared_projection",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(err)?
    {
        rows.push(SharedRow {
            origin_node_id: Some(row.try_get("origin_node_id").map_err(err)?),
            table: table_from_name(row.try_get::<&str, _>("table_name").map_err(err)?)?,
            key: row.try_get("entity_key").map_err(err)?,
            data: parse_json(row.try_get::<&str, _>("payload").map_err(err)?)?,
        });
    }
    let old = SharedSnapshot {
        schema_version: 1,
        rows,
    };
    let present = next
        .rows
        .iter()
        .map(|row| (&row.origin_node_id, row.table, &row.key))
        .collect::<HashSet<_>>();
    let mut removed = old
        .rows
        .iter()
        .filter(|row| !present.contains(&(&row.origin_node_id, row.table, &row.key)))
        .collect::<Vec<_>>();
    // Evidence links precede their FK parents; audit/proof history is distinct
    // from an execution binding and may be removed only by an explicit Git diff.
    removed.sort_by_key(|row| match row.table {
        SharedTable::MemoryRecordSources | SharedTable::SkillSources => 0,
        SharedTable::MemorySourceRevisions | SharedTable::ProfileRevisions => 1,
        _ => 2,
    });
    for row in removed {
        let origin = row.origin_node_id.as_deref().unwrap();
        let id = local_reference(&old, node, origin, row.table, &row.key);
        let reference = |field: &str, table| -> Result<String, String> {
            Ok(local_reference(
                &old,
                node,
                origin,
                table,
                row.data[field]
                    .as_str()
                    .ok_or("Invalid removed shared reference")?,
            ))
        };
        match row.table {
            SharedTable::MemoryRecordSources => {
                sqlx::query("DELETE FROM memory_record_sources WHERE memory_id=? AND source_id=?")
                    .bind(reference("memoryId", SharedTable::MemoryRecords)?)
                    .bind(reference("sourceId", SharedTable::MemorySources)?)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::SkillSources => {
                sqlx::query("DELETE FROM skill_sources WHERE skill_id=? AND source_id=?")
                    .bind(reference("skillId", SharedTable::LearnedSkills)?)
                    .bind(reference("sourceId", SharedTable::MemorySources)?)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::MemoryRecords => {
                sqlx::query("DELETE FROM memory_record_sources WHERE memory_id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
                sqlx::query("DELETE FROM memory_fts WHERE memory_id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
                sqlx::query("DELETE FROM memory_records WHERE id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::LearnedSkills => {
                sqlx::query("DELETE FROM skill_sources WHERE skill_id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
                sqlx::query("DELETE FROM learned_skills WHERE id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::MemorySources => {
                sqlx::query("DELETE FROM memory_record_sources WHERE source_id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
                sqlx::query("DELETE FROM skill_sources WHERE source_id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
                sqlx::query("DELETE FROM memory_source_revisions WHERE source_id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
                sqlx::query("DELETE FROM memory_sources WHERE source_id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::MemorySourceRevisions => {
                sqlx::query("DELETE FROM memory_source_revisions WHERE source_id=? AND revision=?")
                    .bind(reference("sourceId", SharedTable::MemorySources)?)
                    .bind(row.data["revision"].as_i64())
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::AgentProfiles => {
                sqlx::query("DELETE FROM agent_profiles WHERE id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::ProfileRevisions => {
                sqlx::query("DELETE FROM profile_revisions WHERE agent_id=? AND revision=?")
                    .bind(reference("agentId", SharedTable::AgentProfiles)?)
                    .bind(row.data["revision"].as_i64())
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::ProfileDrafts if origin == node => {
                sqlx::query("DELETE FROM profile_drafts WHERE id=?")
                    .bind(&id)
                    .execute(&mut **tx)
                    .await
                    .map_err(err)?;
            }
            SharedTable::ProfileReviewReceipts if origin == node => {
                sqlx::query("DELETE FROM operation_results WHERE operation_id=? AND kind IN ('profile.approve','profile.apply')").bind(&id).execute(&mut **tx).await.map_err(err)?;
            }
            SharedTable::TaskDefinitions | SharedTable::WorkflowDefinitions => {
                // Keep the owner's execution snapshot but stop re-exporting a
                // deleted shared definition on the next unrelated SQL write.
                sqlx::query("INSERT OR IGNORE INTO fleet_shared_tombstones(origin_node_id,table_name,entity_key) VALUES(?,?,?)").bind(origin).bind(row.table.name()).bind(&row.key).execute(&mut **tx).await.map_err(err)?;
            }
            _ => (),
        }
    }
    Ok(())
}
