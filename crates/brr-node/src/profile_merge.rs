//! Prepare a reviewed profile merge without changing either writer's state.
use super::{conflict_revision, decode_record, err, hex, row_record, ProfileMergeReview};
use brr_agent_core::{shared_entity_id, ProfileCandidate, SharedRow, SharedTable};
use brr_fleet::{Conflict, Record};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) struct ProfileMerge {
    pub review: ProfileMergeReview,
    head: Record,
    agent_id: String,
    revision: i64,
    matching_revisions: Vec<Record>,
}

impl ProfileMerge {
    pub fn prepare(
        conflicts: &[Conflict],
        id: &str,
        expected_revision: &str,
        candidate: ProfileCandidate,
    ) -> Result<Self, String> {
        let conflict = conflicts
            .iter()
            .find(|conflict| conflict.id == id)
            .ok_or("The profile conflict has already changed")?;
        if conflict_revision(conflict) != expected_revision {
            return Err("The profile conflict changed. Refresh before reviewing it.".into());
        }
        let current = profile_side(conflict.current.as_ref())?;
        let incoming = profile_side(conflict.incoming.as_ref())?;
        if current.0 != incoming.0 {
            return Err("The profile conflict has incompatible identities".into());
        }
        let (agent_id, current_revision) = current;
        let prefer_current = current_revision >= incoming.1;
        let (head, revision) = if prefer_current {
            (conflict.current.as_ref().unwrap(), current_revision)
        } else {
            (conflict.incoming.as_ref().unwrap(), incoming.1)
        };
        let mut revisions = Vec::new();
        let mut matching_revisions = Vec::new();
        for conflict in conflicts {
            let sides = [&conflict.current, &conflict.incoming];
            let mut is_related = false;
            for record in sides.iter().filter_map(|side| side.as_ref()) {
                if record.origin_node_id != head.origin_node_id || record.kind != head.kind {
                    continue;
                }
                let envelope = decode_record(record)?;
                is_related |= envelope.table == SharedTable::ProfileRevisions
                    && envelope.data["agentId"] == agent_id;
            }
            if is_related {
                let record = sides[usize::from(!prefer_current)]
                    .as_ref()
                    .filter(|record| !record.deleted)
                    .ok_or("The selected side has no reviewed profile revision")?;
                matching_revisions.push(record.clone());
                revisions.push(conflict_revision(conflict));
            }
        }
        revisions.sort_unstable();
        let review_hash = hex(&Sha256::digest(
            serde_json::to_vec(&json!([
                "brr-profile-merge-v1",
                expected_revision,
                revisions,
                candidate
            ]))
            .map_err(err)?,
        ));
        Ok(Self {
            review: ProfileMergeReview {
                candidate,
                review_hash,
            },
            head: head.clone(),
            agent_id,
            revision,
            matching_revisions,
        })
    }

    pub fn records(self, reviewing_node: &str) -> Result<(Vec<Record>, Vec<Record>), String> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("Profile revision overflow")?;
        let original = decode_record(&self.head)?;
        let original_node = self.head.origin_node_id.clone();
        let agent_id = self.agent_id;
        let agent_ref = shared_entity_id(&original_node, SharedTable::AgentProfiles, &agent_id);
        let ProfileMergeReview {
            candidate,
            review_hash,
        } = self.review;
        // Same typed serde serialization used by AgentStore's profile reviews.
        let candidate_hash = hex(&Sha256::digest(
            serde_json::to_vec(&candidate).map_err(err)?,
        ));
        let draft_id = format!("merge-{review_hash}");
        let approve_id = format!("review-{review_hash}");
        let apply_id = format!("apply-{review_hash}");
        let draft_ref = shared_entity_id(reviewing_node, SharedTable::ProfileDrafts, &draft_id);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let created_at = original.data["createdAt"]
            .as_u64()
            .ok_or("Profile creation time is invalid")?;
        let updated_at = timestamp.max(original.data["updatedAt"].as_u64().unwrap_or(created_at));
        let candidate = serde_json::to_value(candidate).map_err(err)?;
        let row = |node: &str, table, key: &str, data| {
            row_record(&SharedRow {
                origin_node_id: Some(node.into()),
                table,
                key: key.into(),
                data,
            })
        };
        let mut head = row(
            &original_node,
            SharedTable::AgentProfiles,
            &agent_id,
            json!({
                "id": agent_id, "revision":revision, "candidate":candidate, "createdAt":created_at,
                "updatedAt":updated_at,
            }),
        )?;
        head.base_revision = Some(self.head.revision);
        let mut resolutions = vec![head];
        resolutions.extend(self.matching_revisions);
        let mut receipt = json!({
            "id":draft_id, "threadId":format!("fleet-review:{review_hash}"), "agentId":agent_ref,
            "candidate":candidate, "hash":candidate_hash, "status":"approved", "baseRevision":self.revision,
        });
        let approval = row(
            reviewing_node,
            SharedTable::ProfileReviewReceipts,
            &approve_id,
            json!({
                "operationId":approve_id, "kind":"profile.approve", "result":receipt,
            }),
        )?;
        receipt["createdAt"] = json!(updated_at);
        receipt["status"] = json!("applied");
        let draft = row(
            reviewing_node,
            SharedTable::ProfileDrafts,
            &draft_id,
            receipt,
        )?;
        let published_revision = row(
            &original_node,
            SharedTable::ProfileRevisions,
            &format!("{agent_id}@{revision}"),
            json!({
                "agentId":agent_id, "revision":revision, "candidate":candidate, "createdAt":created_at,
                "updatedAt":updated_at, "draftId":draft_ref, "hash":candidate_hash,
            }),
        )?;
        let mut applied: Value = candidate;
        applied["id"] = json!(agent_ref);
        applied["revision"] = json!(revision);
        applied["createdAt"] = json!(created_at);
        applied["updatedAt"] = json!(updated_at);
        let application = row(
            reviewing_node,
            SharedTable::ProfileReviewReceipts,
            &apply_id,
            json!({
                "operationId":apply_id, "kind":"profile.apply",
                "result":{"draftId":draft_id, "hash":candidate_hash, "profile":applied},
            }),
        )?;
        Ok((
            resolutions,
            vec![published_revision, draft, approval, application],
        ))
    }
}

fn profile_side(record: Option<&Record>) -> Result<(String, i64), String> {
    let record = record
        .filter(|record| !record.deleted)
        .ok_or("Composing a profile requires two live variants")?;
    let envelope = decode_record(record)?;
    if envelope.table != SharedTable::AgentProfiles {
        return Err("This conflict is not a profile head".into());
    }
    let revision = envelope.data["revision"]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or("Invalid profile revision in the conflict")?;
    Ok((envelope.key, revision))
}
