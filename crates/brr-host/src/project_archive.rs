use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use tokio::sync::Mutex;

static ARCHIVE_BATCH: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveReport {
    pub archived_ids: Vec<String>,
    pub skipped: Vec<ArchiveSkip>,
    pub failed: Vec<ArchiveFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveSkip {
    pub thread_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveFailure {
    pub thread_id: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindingInfo {
    pub thread_id: String,
    pub project_id: Option<String>,
    pub kind: String,
    pub cwd: Option<String>,
}

pub(crate) trait ArchiveBackend {
    async fn list(&self, archived: bool, cursor: Option<&str>) -> Result<Value, String>;
    async fn list_descendants(
        &self,
        ancestor_thread_id: &str,
        archived: bool,
        cursor: Option<&str>,
    ) -> Result<Value, String>;
    async fn read(&self, thread_id: &str) -> Result<Value, String>;
    async fn archive(&self, thread_id: &str) -> Result<(), String>;
    async fn bindings(&self) -> Result<Vec<BindingInfo>, String>;
    async fn membership(&self, thread_id: &str) -> Result<Option<String>, String>;
}

pub(crate) async fn archive_project_chats(
    backend: &impl ArchiveBackend,
    project_id: &str,
) -> Result<ArchiveReport, String> {
    let _batch = ARCHIVE_BATCH.lock().await;
    let project = project_id;
    let bindings = backend.bindings().await?;
    let (threads, archived) = list_all(backend).await?;
    let by_id = bindings
        .into_iter()
        .map(|binding| (binding.thread_id.clone(), binding))
        .collect::<HashMap<_, _>>();
    let mut report = ArchiveReport {
        archived_ids: vec![],
        skipped: vec![],
        failed: vec![],
    };

    let mut roots = Vec::new();
    for (id, thread) in &threads {
        if !thread["parentThreadId"].is_null() {
            continue;
        }
        let binding = by_id.get(id);
        let belongs_to_project = match backend.membership(id).await {
            Ok(Some(path)) => path == project,
            // Unknown membership cannot safely be attributed to this deletion request.
            Ok(None) => false,
            Err(error) => {
                return Err(format!(
                    "cannot determine project membership for thread {id}: {error}"
                ));
            }
        };
        if !belongs_to_project {
            continue;
        }
        if archived.contains(id) {
            continue;
        }
        match root_reason(thread) {
            Some(reason) => {
                report.skipped.push(ArchiveSkip {
                    thread_id: id.clone(),
                    reason,
                });
                continue;
            }
            None => {}
        }
        if binding.is_some_and(|b| scheduler_kind(&b.kind)) {
            report.skipped.push(ArchiveSkip {
                thread_id: id.clone(),
                reason: "scheduler-owned thread".into(),
            });
            continue;
        }
        if binding.is_some_and(|b| !conversational_kind(&b.kind)) {
            report.skipped.push(ArchiveSkip {
                thread_id: id.clone(),
                reason: "host-owned thread kind is not conversational".into(),
            });
            continue;
        }
        roots.push(id.clone());
    }

    for root in roots {
        let (fresh_threads, _) = match list_threads(backend, Some(&root)).await {
            Ok(result) => result,
            Err(error) => {
                report.skipped.push(ArchiveSkip {
                    thread_id: root,
                    reason: format!("cannot refresh cascade membership: {error}"),
                });
                continue;
            }
        };
        let mut fresh_children: HashMap<String, Vec<String>> = HashMap::new();
        for (id, thread) in &fresh_threads {
            if let Some(parent) = thread["parentThreadId"].as_str() {
                fresh_children
                    .entry(parent.to_owned())
                    .or_default()
                    .push(id.clone());
            }
        }
        let descendants = match descendant_closure(&root, &fresh_children) {
            Ok(ids) if ids.len() == fresh_threads.len() => ids,
            Ok(_) => {
                report.skipped.push(ArchiveSkip {
                    thread_id: root,
                    reason: "cascade listing returned unrelated or incomplete parent metadata"
                        .into(),
                });
                continue;
            }
            Err(reason) => {
                report.skipped.push(ArchiveSkip {
                    thread_id: root,
                    reason,
                });
                continue;
            }
        };
        let current_bindings = match backend.bindings().await {
            Ok(bindings) => bindings
                .into_iter()
                .map(|binding| (binding.thread_id.clone(), binding))
                .collect::<HashMap<_, _>>(),
            Err(error) => {
                report.skipped.push(ArchiveSkip {
                    thread_id: root,
                    reason: format!("cannot recheck host bindings: {error}"),
                });
                continue;
            }
        };

        let mut fresh_error = None;
        for id in &descendants {
            let Some(thread) = fresh_threads.get(id) else {
                fresh_error = Some("descendant metadata is missing".to_string());
                break;
            };
            if let Some(reason) = unsafe_descendant(thread) {
                fresh_error = Some(reason);
                break;
            }
            if by_id.contains_key(id) && !current_bindings.contains_key(id) {
                fresh_error = Some(format!("host binding disappeared for {id}"));
                break;
            }
            let binding = current_bindings.get(id);
            if binding.is_some_and(|b| scheduler_kind(&b.kind)) {
                fresh_error = Some("cascade contains a scheduler-owned thread".into());
                break;
            }
            if binding.is_some_and(|b| !conversational_kind(&b.kind)) {
                fresh_error =
                    Some("cascade contains a non-conversational host-owned thread".into());
                break;
            }
            match backend.membership(id).await {
                Ok(Some(path)) if path == project => {}
                // Unassigned runtime descendants belong to their verified parent cascade.
                // An explicitly moved descendant remains protected by its own membership.
                Ok(None) => {}
                Ok(_) => {
                    fresh_error = Some(
                        "cascade contains a thread outside this project or with unknown membership"
                            .into(),
                    );
                    break;
                }
                Err(error) => {
                    fresh_error = Some(error);
                    break;
                }
            }
        }
        if let Some(reason) = fresh_error {
            report.skipped.push(ArchiveSkip {
                thread_id: root,
                reason,
            });
            continue;
        }

        let mut fresh_error = None;
        for id in std::iter::once(&root).chain(descendants.iter()) {
            if by_id.contains_key(id) && !current_bindings.contains_key(id) {
                fresh_error = Some(format!("host binding disappeared for {id}"));
                break;
            }
            match backend.read(id).await {
                Ok(value) => {
                    let Some(thread) = value.get("thread") else {
                        fresh_error = Some(format!("fresh metadata for {id} is malformed"));
                        break;
                    };
                    if thread["id"].as_str() != Some(id.as_str()) {
                        fresh_error = Some(format!("fresh metadata identity mismatch for {id}"));
                        break;
                    }
                    if !valid_parent_metadata(thread) {
                        fresh_error = Some(format!("fresh parent metadata for {id} is malformed"));
                        break;
                    }
                    if id != &root
                        && fresh_threads.get(id).is_none_or(|listed| {
                            listed["parentThreadId"] != thread["parentThreadId"]
                        })
                    {
                        fresh_error = Some(format!(
                            "thread parent changed during cascade check for {id}"
                        ));
                        break;
                    }
                    if let Some(reason) = unsafe_descendant(thread) {
                        fresh_error = Some(reason);
                        break;
                    }
                    if id == &root {
                        if let Some(reason) = root_reason(thread) {
                            fresh_error = Some(reason);
                            break;
                        }
                        if !thread["parentThreadId"].is_null() {
                            fresh_error = Some("root thread acquired a parent".into());
                            break;
                        }
                    }
                    let binding = current_bindings.get(id);
                    if binding.is_some_and(|b| scheduler_kind(&b.kind)) {
                        fresh_error = Some("cascade contains a scheduler-owned thread".into());
                        break;
                    }
                    if binding.is_some_and(|b| !conversational_kind(&b.kind)) {
                        fresh_error =
                            Some("cascade contains a non-conversational host-owned thread".into());
                        break;
                    }
                    match backend.membership(id).await {
                        Ok(Some(path)) if path == project => {}
                        Ok(None) if id != &root => {}
                        Ok(_) => {
                            fresh_error =
                                Some("cascade membership changed or became unknown".into());
                            break;
                        }
                        Err(error) => {
                            fresh_error = Some(error);
                            break;
                        }
                    }
                }
                Err(error) => {
                    fresh_error = Some(format!("cannot recheck thread {id}: {error}"));
                    break;
                }
            }
        }
        if let Some(reason) = fresh_error {
            report.skipped.push(ArchiveSkip {
                thread_id: root,
                reason,
            });
            continue;
        }
        match backend.archive(&root).await {
            Ok(()) => report
                .archived_ids
                .extend(std::iter::once(root).chain(descendants)),
            Err(error) => report.failed.push(ArchiveFailure {
                thread_id: root,
                error,
            }),
        }
    }
    Ok(report)
}

async fn list_all(
    backend: &impl ArchiveBackend,
) -> Result<(HashMap<String, Value>, HashSet<String>), String> {
    list_threads(backend, None).await
}

async fn list_threads(
    backend: &impl ArchiveBackend,
    ancestor_thread_id: Option<&str>,
) -> Result<(HashMap<String, Value>, HashSet<String>), String> {
    let mut all = HashMap::new();
    let mut archived_ids = HashSet::new();
    for archived in [false, true] {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        loop {
            let page = match ancestor_thread_id {
                Some(ancestor) => {
                    backend
                        .list_descendants(ancestor, archived, cursor.as_deref())
                        .await?
                }
                None => backend.list(archived, cursor.as_deref()).await?,
            };
            let data = page["data"]
                .as_array()
                .ok_or("thread/list returned malformed data")?;
            for thread in data {
                let id = thread["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("thread/list returned a thread without an ID")?;
                if !valid_parent_metadata(thread) {
                    return Err(format!(
                        "thread/list returned malformed parent metadata for {id}"
                    ));
                }
                if archived {
                    archived_ids.insert(id.to_owned());
                }
                if all.insert(id.to_owned(), thread.clone()).is_some() {
                    return Err(format!("thread/list returned duplicate thread ID {id}"));
                }
            }
            match page.get("nextCursor") {
                Some(Value::Null) => break,
                Some(Value::String(next)) if !next.is_empty() => {
                    if !seen_cursors.insert(next.clone()) {
                        return Err("thread/list pagination cursor repeated".into());
                    }
                    cursor = Some(next.clone());
                }
                _ => return Err("thread/list returned malformed nextCursor".into()),
            }
        }
    }
    Ok((all, archived_ids))
}

fn valid_parent_metadata(thread: &Value) -> bool {
    matches!(
        thread.get("parentThreadId"),
        Some(Value::Null | Value::String(_))
    )
}

fn root_reason(thread: &Value) -> Option<String> {
    if thread["ephemeral"] != false {
        return Some("ephemeral or unknown thread metadata".into());
    }
    if !matches!(
        thread["source"].as_str(),
        Some("cli" | "vscode" | "exec" | "appServer")
    ) {
        return Some("unknown or non-conversational thread source".into());
    }
    match thread["status"]["type"].as_str() {
        Some("idle" | "notLoaded") => None,
        Some("active") => Some("thread is active".into()),
        Some("systemError") => Some("thread has a system error".into()),
        _ => Some("unknown thread status".into()),
    }
}

fn unsafe_descendant(thread: &Value) -> Option<String> {
    if thread["ephemeral"] != false {
        return Some("cascade contains an ephemeral or unknown thread".into());
    }
    match thread["status"]["type"].as_str() {
        Some("idle" | "notLoaded") => {}
        Some("active") => return Some("cascade contains an active thread".into()),
        Some("systemError") => return Some("cascade contains a thread with a system error".into()),
        _ => return Some("cascade contains a thread with unknown status".into()),
    }
    if !thread["parentThreadId"].is_null() && !thread["parentThreadId"].is_string() {
        return Some("cascade parent metadata is malformed".into());
    }
    None
}

fn scheduler_kind(kind: &str) -> bool {
    matches!(kind, "task" | "learning" | "scheduled_task" | "scheduler")
}

fn conversational_kind(kind: &str) -> bool {
    matches!(kind, "baseline" | "agent" | "interview")
}

fn descendant_closure(
    root: &str,
    children: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut seen = HashSet::from([root.to_owned()]);
    let mut pending = vec![root.to_owned()];
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten() {
            if !seen.insert(child.clone()) {
                return Err("thread parent graph contains a cycle or repeated child".into());
            }
            result.push(child.clone());
            pending.push(child.clone());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};

    struct Fake {
        pages: HashMap<(bool, Option<String>), Value>,
        threads: HashMap<String, Value>,
        bindings_data: Vec<BindingInfo>,
        binding_snapshots: Vec<Vec<BindingInfo>>,
        binding_call_count: Arc<StdMutex<usize>>,
        worktrees: HashMap<String, String>,
        membership_errors: HashMap<String, String>,
        archive_errors: HashMap<String, String>,
        archived: Arc<StdMutex<Vec<String>>>,
        list_count: Arc<StdMutex<usize>>,
    }
    impl ArchiveBackend for Fake {
        async fn list(&self, archived: bool, cursor: Option<&str>) -> Result<Value, String> {
            *self.list_count.lock().unwrap() += 1;
            self.pages
                .get(&(archived, cursor.map(str::to_owned)))
                .cloned()
                .ok_or("unexpected page".into())
        }
        async fn list_descendants(
            &self,
            ancestor_thread_id: &str,
            archived: bool,
            cursor: Option<&str>,
        ) -> Result<Value, String> {
            if archived {
                return Ok(json!({"data":[],"nextCursor":null}));
            }
            let offset = cursor
                .unwrap_or("0")
                .parse::<usize>()
                .map_err(|e| e.to_string())?;
            let mut descendants = self
                .threads
                .values()
                .filter(|thread| is_descendant(thread, ancestor_thread_id, &self.threads))
                .cloned()
                .collect::<Vec<_>>();
            descendants.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
            let page = descendants
                .iter()
                .skip(offset)
                .take(100)
                .cloned()
                .collect::<Vec<_>>();
            let next = (offset + page.len() < descendants.len())
                .then(|| (offset + page.len()).to_string());
            Ok(json!({"data":page,"nextCursor":next}))
        }
        async fn read(&self, thread_id: &str) -> Result<Value, String> {
            Ok(json!({"thread":self.threads.get(thread_id).ok_or("missing")?}))
        }
        async fn archive(&self, thread_id: &str) -> Result<(), String> {
            if let Some(error) = self.archive_errors.get(thread_id) {
                return Err(error.clone());
            }
            self.archived.lock().unwrap().push(thread_id.to_owned());
            Ok(())
        }
        async fn bindings(&self) -> Result<Vec<BindingInfo>, String> {
            let mut count = self.binding_call_count.lock().unwrap();
            let snapshot = self.binding_snapshots.get(*count).cloned();
            *count += 1;
            Ok(snapshot.unwrap_or_else(|| self.bindings_data.clone()))
        }
        async fn membership(&self, thread_id: &str) -> Result<Option<String>, String> {
            if let Some(error) = self.membership_errors.get(thread_id) {
                return Err(error.clone());
            }
            Ok(self
                .threads
                .get(thread_id)
                .and_then(|t| t["projectId"].as_str())
                .map(str::to_owned))
        }
    }

    fn thread(id: &str, cwd: &str) -> Value {
        json!({"id":id,"parentThreadId":null,"ephemeral":false,"source":"cli","status":{"type":"idle"},"cwd":cwd,"projectId":cwd})
    }
    fn is_descendant(thread: &Value, ancestor: &str, all: &HashMap<String, Value>) -> bool {
        let mut parent = thread["parentThreadId"].as_str();
        let mut visited = HashSet::new();
        while let Some(id) = parent {
            if id == ancestor {
                return true;
            }
            if !visited.insert(id) {
                return false;
            }
            parent = all
                .get(id)
                .and_then(|parent| parent["parentThreadId"].as_str());
        }
        false
    }
    fn fake(pages: Vec<((bool, Option<String>), Value)>, threads: Vec<Value>) -> Fake {
        Fake {
            pages: pages.into_iter().collect(),
            threads: threads
                .into_iter()
                .map(|t| (t["id"].as_str().unwrap().to_owned(), t))
                .collect(),
            bindings_data: vec![],
            binding_snapshots: vec![],
            binding_call_count: Arc::new(StdMutex::new(0)),
            worktrees: HashMap::new(),
            membership_errors: HashMap::new(),
            archive_errors: HashMap::new(),
            archived: Arc::new(StdMutex::new(vec![])),
            list_count: Arc::new(StdMutex::new(0)),
        }
    }

    #[tokio::test]
    async fn archive_follows_member_roots_with_unassigned_runtime_children() {
        let mut root = thread("root", "/worktree/source");
        root["projectId"] = json!("bucket-a");
        let mut child = thread("child", "/somewhere/else");
        child["parentThreadId"] = json!("root");
        child["projectId"] = Value::Null;
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":[root.clone(),child.clone()],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![root, child],
        );
        let result = archive_project_chats(&backend, "bucket-a").await.unwrap();
        assert_eq!(result.archived_ids.len(), 2);
        assert!(result.skipped.is_empty());
    }

    #[tokio::test]
    async fn loads_both_complete_thread_lists_before_archiving_any_project_root() {
        let root = thread("root", "/project");
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":[root.clone()],"nextCursor":"next"}),
                ),
                (
                    (false, Some("next".into())),
                    json!({"data":[],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![root],
        );
        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert_eq!(report.archived_ids, ["root"]);
        assert_eq!(*backend.list_count.lock().unwrap(), 3);
        assert_eq!(*backend.archived.lock().unwrap(), ["root"]);
    }

    #[tokio::test]
    async fn archives_roots_across_more_than_one_hundred_rows() {
        let first = (0..100)
            .map(|index| thread(&format!("root-{index}"), "/project"))
            .collect::<Vec<_>>();
        let last = thread("root-100", "/project");
        let all_threads = first
            .iter()
            .cloned()
            .chain(std::iter::once(last.clone()))
            .collect::<Vec<_>>();
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":first,"nextCursor":"after-100"}),
                ),
                (
                    (false, Some("after-100".into())),
                    json!({"data":[last],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            all_threads,
        );
        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert_eq!(report.archived_ids.len(), 101);
        assert_eq!(backend.archived.lock().unwrap().len(), 101);
    }

    #[tokio::test]
    async fn unrelated_archived_active_and_scheduler_roots_do_not_block_this_project() {
        let selected = thread("selected", "/project");
        let archived = thread("archived", "/elsewhere");
        let mut already_archived = thread("already-archived", "/project");
        already_archived["status"] = json!({"type":"active","activeFlags":[]});
        let mut active = thread("active", "/elsewhere");
        active["status"] = json!({"type":"active","activeFlags":[]});
        let scheduler = thread("scheduler", "/elsewhere");
        let backend = Fake {
            pages: HashMap::from([
                (
                    (false, None),
                    json!({"data":[selected.clone(), active.clone(), scheduler.clone()],"nextCursor":null}),
                ),
                (
                    (true, None),
                    json!({"data":[archived.clone(), already_archived.clone()],"nextCursor":null}),
                ),
            ]),
            threads: HashMap::from([
                ("selected".into(), selected),
                ("active".into(), active),
                ("scheduler".into(), scheduler),
                ("archived".into(), archived),
                ("already-archived".into(), already_archived),
            ]),
            bindings_data: vec![BindingInfo {
                thread_id: "scheduler".into(),
                project_id: Some("/elsewhere".into()),
                kind: "learning".into(),
                cwd: None,
            }],
            binding_snapshots: vec![],
            binding_call_count: Arc::new(StdMutex::new(0)),
            worktrees: HashMap::new(),
            membership_errors: HashMap::new(),
            archive_errors: HashMap::new(),
            archived: Arc::new(StdMutex::new(vec![])),
            list_count: Arc::new(StdMutex::new(0)),
        };

        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert_eq!(report.archived_ids, ["selected"]);
        assert!(report.skipped.is_empty());
        assert!(report.failed.is_empty());
    }

    #[tokio::test]
    async fn aborts_preflight_before_mutation_when_selected_worktree_metadata_is_corrupt() {
        let valid = thread("valid", "/project");
        let corrupt = thread("corrupt", "/project/worktrees/unknown");
        let backend = Fake {
            membership_errors: HashMap::from([(
                "corrupt".into(),
                "Saved Worktree metadata is corrupt".into(),
            )]),
            ..fake(
                vec![
                    (
                        (false, None),
                        json!({"data":[valid.clone(), corrupt.clone() ],"nextCursor":null}),
                    ),
                    ((true, None), json!({"data":[],"nextCursor":null})),
                ],
                vec![valid, corrupt],
            )
        };

        let error = archive_project_chats(&backend, "/project")
            .await
            .unwrap_err();
        assert!(error.contains("corrupt"));
        assert!(backend.archived.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn holds_archive_when_a_new_active_descendant_appears_before_the_cascade_check() {
        let root = thread("root", "/project");
        let mut spawned = thread("spawned", "/project");
        spawned["parentThreadId"] = json!("root");
        spawned["status"] = json!({"type":"active","activeFlags":[]});
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":[root.clone() ],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![root, spawned],
        );

        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert!(backend.archived.lock().unwrap().is_empty());
        assert_eq!(report.skipped[0].thread_id, "root");
        assert!(report.skipped[0].reason.contains("active"));
    }

    #[tokio::test]
    async fn fails_closed_when_thread_listing_omits_parent_metadata() {
        let mut malformed = thread("malformed", "/project");
        malformed.as_object_mut().unwrap().remove("parentThreadId");
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":[malformed.clone()],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![malformed],
        );
        assert!(archive_project_chats(&backend, "/project").await.is_err());
        assert!(backend.archived.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn fails_closed_when_thread_listing_omits_its_cursor_field() {
        let root = thread("root", "/project");
        let backend = fake(
            vec![
                ((false, None), json!({"data":[root.clone()]})),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![root],
        );
        assert!(archive_project_chats(&backend, "/project").await.is_err());
        assert!(backend.archived.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn does_not_archive_anything_when_a_complete_listing_is_unavailable() {
        let root = thread("root", "/project");
        let backend = fake(
            vec![(
                (false, None),
                json!({"data":[root.clone()],"nextCursor":null}),
            )],
            vec![root],
        );
        assert!(archive_project_chats(&backend, "/project").await.is_err());
        assert!(backend.archived.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn skips_a_whole_cascade_with_a_scheduler_owned_or_active_descendant() {
        let root = thread("root", "/project");
        let mut child = thread("child", "/project");
        child["parentThreadId"] = json!("root");
        let mut active = thread("active", "/project");
        active["parentThreadId"] = json!("child");
        active["status"] = json!({"type":"active","activeFlags":[]});
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":[root.clone(), child.clone(), active.clone()],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![root, child, active],
        );
        // An active grandchild is sufficient to hold the cascade, even if no scheduler binding exists.
        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert!(backend.archived.lock().unwrap().is_empty());
        assert_eq!(report.skipped[0].thread_id, "root");
        assert!(report.skipped[0].reason.contains("active"));

        let mut only_child = thread("child", "/project");
        only_child["parentThreadId"] = json!("root");
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":[thread("root", "/project"), only_child.clone()],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![thread("root", "/project"), only_child],
        );
        let backend = Fake {
            bindings_data: vec![BindingInfo {
                thread_id: "child".into(),
                project_id: Some("/project".into()),
                kind: "learning".into(),
                cwd: None,
            }],
            ..backend
        };
        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert!(backend.archived.lock().unwrap().is_empty());
        assert!(report.skipped[0].reason.contains("scheduler-owned"));
    }

    #[tokio::test]
    async fn matches_current_bucket_membership_and_reports_each_archive_failure() {
        let mut worktree = thread("worktree", "/project/worktrees/abc");
        worktree["projectId"] = json!("/project");
        let sibling = thread("sibling", "/project-other");
        let later = thread("later", "/project");
        let backend = fake(
            vec![
                (
                    (false, None),
                    json!({"data":[worktree.clone(), sibling, later.clone()],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ],
            vec![worktree.clone(), thread("sibling", "/project-other"), later],
        );
        let backend = Fake {
            worktrees: HashMap::from([("worktree".into(), "/project".into())]),
            membership_errors: HashMap::new(),
            archive_errors: HashMap::from([("later".into(), "runtime rejected archive".into())]),
            ..backend
        };
        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert_eq!(report.archived_ids, ["worktree"]);
        assert_eq!(backend.archived.lock().unwrap().as_slice(), ["worktree"]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].thread_id, "later");
        assert_eq!(report.failed[0].error, "runtime rejected archive");
    }

    #[tokio::test]
    async fn refuses_to_fall_back_to_cwd_when_a_saved_binding_disappears() {
        let root = thread("root", "/project");
        let binding = BindingInfo {
            thread_id: "root".into(),
            project_id: Some("/project".into()),
            kind: "baseline".into(),
            cwd: Some("/project".into()),
        };
        let backend = Fake {
            pages: HashMap::from([
                (
                    (false, None),
                    json!({"data":[root.clone()],"nextCursor":null}),
                ),
                ((true, None), json!({"data":[],"nextCursor":null})),
            ]),
            threads: HashMap::from([("root".into(), root)]),
            bindings_data: vec![binding.clone()],
            binding_snapshots: vec![vec![binding], vec![]],
            binding_call_count: Arc::new(StdMutex::new(0)),
            worktrees: HashMap::new(),
            membership_errors: HashMap::new(),
            archive_errors: HashMap::new(),
            archived: Arc::new(StdMutex::new(vec![])),
            list_count: Arc::new(StdMutex::new(0)),
        };
        let report = archive_project_chats(&backend, "/project").await.unwrap();
        assert!(backend.archived.lock().unwrap().is_empty());
        assert!(report.skipped[0].reason.contains("binding disappeared"));
    }
}
