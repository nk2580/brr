//! Validate the semantic document before durable intent is accepted.
use serde_json::Value;

pub(super) fn validate_document(document: &Value) -> Result<(), String> {
    use std::collections::HashSet;
    let text = document["text"]
        .as_str()
        .ok_or("Invalid message document")?;
    if document["version"] != 1 || text.len() > 2_000_000 {
        return Err("Invalid or oversized message document".into());
    }
    let mut boundaries = HashSet::from([0usize]);
    let mut offset = 0;
    for character in text.chars() {
        offset += character.len_utf16();
        boundaries.insert(offset);
    }
    let range = |entry: &Value| -> Result<(), String> {
        let start = entry["start"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("Invalid semantic range")?;
        let end = entry["end"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("Invalid semantic range")?;
        if start >= end || !boundaries.contains(&start) || !boundaries.contains(&end) {
            return Err(
                "Semantic ranges must follow complete characters inside the message".into(),
            );
        }
        Ok(())
    };
    for field in ["references", "quotes"] {
        let entries = document[field]
            .as_array()
            .ok_or("Invalid semantic document entries")?;
        if entries.len() > 128 {
            return Err("Too many message references".into());
        }
        for entry in entries {
            range(entry)?;
            if field == "references" {
                if !matches!(
                    entry["type"].as_str(),
                    Some("agent" | "file" | "folder" | "skill")
                ) || entry["id"]
                    .as_str()
                    .is_none_or(|v| v.is_empty() || v.len() > 4096)
                    || entry["name"]
                        .as_str()
                        .is_none_or(|v| v.is_empty() || v.len() > 4096)
                {
                    return Err("Invalid message reference".into());
                }
            } else if entry["text"].as_str().is_none() {
                return Err("Invalid quote".into());
            }
        }
    }
    if let Some(selection) = document.get("selection") {
        for field in ["anchor", "focus"] {
            if selection[field]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .is_none_or(|v| !boundaries.contains(&v))
            {
                return Err("Invalid composer selection".into());
            }
        }
    }
    Ok(())
}

pub(super) fn workspace_reference(
    cwd: &std::path::Path,
    roots: &[String],
    id: &str,
    folder: bool,
) -> Result<std::path::PathBuf, String> {
    let id = std::path::Path::new(id);
    let path = if id.is_absolute() {
        id.to_path_buf()
    } else {
        cwd.join(id)
    };
    let actual = std::fs::canonicalize(&path)
        .map_err(|_| format!("Referenced file is unavailable: {}", path.display()))?;
    if !roots.iter().any(|root| actual.starts_with(root)) {
        return Err("Reference is outside this conversation's saved folders".into());
    }
    let metadata = std::fs::metadata(&actual).map_err(|e| e.to_string())?;
    if (folder && !metadata.is_dir()) || (!folder && !metadata.is_file()) {
        return Err("Reference type no longer matches the selected file or folder".into());
    }
    Ok(actual)
}

/// Resolve a semantic reference against a saved session. Absolute IDs may have been selected
/// from the original project before WorkTrunk created the isolated session folders, so map only
/// through the host-owned source-to-session-folder snapshot before applying the usual root check.
pub(super) fn session_workspace_reference(
    workspace: &crate::worktrees::SessionWorkspace,
    id: &str,
    folder: bool,
) -> Result<std::path::PathBuf, String> {
    let id_path = std::path::Path::new(id);
    if !id_path.is_absolute() || workspace.roots.iter().any(|root| id_path.starts_with(root)) {
        return workspace_reference(
            std::path::Path::new(&workspace.cwd),
            &workspace.roots,
            id,
            folder,
        );
    }

    let matches: Vec<_> = workspace
        .folders
        .iter()
        .filter_map(|mapping| {
            id_path
                .strip_prefix(std::path::Path::new(&mapping.source))
                .ok()
                .map(|suffix| (mapping, suffix))
        })
        .collect();
    if matches.is_empty() {
        return workspace_reference(
            std::path::Path::new(&workspace.cwd),
            &workspace.roots,
            id,
            folder,
        );
    }

    let mut resolved = Vec::new();
    for (mapping, suffix) in &matches {
        if suffix.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return Err("Reference escapes its saved source folder".into());
        }
        let mapped = std::path::Path::new(&mapping.path).join(suffix);
        let mapped = mapped.to_str().ok_or("Reference path is not valid UTF-8")?;
        match workspace_reference(
            std::path::Path::new(&workspace.cwd),
            &workspace.roots,
            mapped,
            folder,
        ) {
            Ok(path) => resolved.push(path),
            Err(error) if matches.len() == 1 => return Err(error),
            Err(_) => return Err("Reference matches ambiguous saved source folders".into()),
        }
    }
    let first = resolved
        .first()
        .ok_or("Reference is unavailable in the saved session")?;
    if resolved.iter().any(|path| path != first) {
        return Err("Reference matches ambiguous saved source folders".into());
    }
    Ok(first.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_semantic_ranges_in_utf16_without_splitting_surrogates() {
        let mut doc = json!({"version":1,"text":"🦀 @src/main.rs","references":[{"id":"src/main.rs","name":"src/main.rs","type":"file","start":3,"end":15}],"quotes":[]});
        assert!(validate_document(&doc).is_ok());
        doc["references"][0]["start"] = json!(1);
        assert!(
            validate_document(&doc).is_err(),
            "a reference must not split a surrogate pair"
        );
        doc["references"][0]["start"] = json!(3);
        doc["references"][0]["end"] = json!(100);
        assert!(
            validate_document(&doc).is_err(),
            "a reference must be inside its text"
        );
    }
    #[test]
    fn resolves_actual_workspace_paths_and_rejects_escaping_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("same.txt"), "worktree bytes").unwrap();
        let roots = vec![root.to_string_lossy().into_owned()];
        assert_eq!(
            workspace_reference(&root, &roots, "same.txt", false).unwrap(),
            root.join("same.txt")
        );
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("secret.txt"), "other chat").unwrap();
        assert!(workspace_reference(
            &root,
            &roots,
            &other.path().join("secret.txt").to_string_lossy(),
            false
        )
        .is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(other.path().join("secret.txt"), root.join("escape.txt"))
                .unwrap();
            assert!(workspace_reference(&root, &roots, "escape.txt", false).is_err());
        }
    }

    #[test]
    fn maps_source_absolute_ids_only_into_saved_roots_and_rejects_ambiguous_mappings() {
        use crate::worktrees::{ResolvedFolder, SessionWorkspace};
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let source = base.join("source");
        let nested_source = source.join("nested");
        let root_a = base.join("session-a");
        let root_b = base.join("session-b");
        std::fs::create_dir_all(&nested_source).unwrap();
        std::fs::create_dir_all(root_a.join("nested")).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        std::fs::write(root_a.join("nested/file.txt"), "a").unwrap();
        std::fs::write(root_b.join("file.txt"), "b").unwrap();
        let workspace = SessionWorkspace {
            cwd: root_a.to_string_lossy().into_owned(),
            roots: vec![
                root_a.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
            folders: vec![
                ResolvedFolder {
                    source: source.to_string_lossy().into_owned(),
                    path: root_a.to_string_lossy().into_owned(),
                },
                ResolvedFolder {
                    source: nested_source.to_string_lossy().into_owned(),
                    path: root_b.to_string_lossy().into_owned(),
                },
            ],
            worktrees: vec![],
            scope_project_id: None,
            project_revision: None,
        };
        assert!(session_workspace_reference(
            &workspace,
            &nested_source.join("file.txt").to_string_lossy(),
            false
        )
        .unwrap_err()
        .contains("ambiguous"));
        assert!(session_workspace_reference(
            &workspace,
            &source.join("../outside.txt").to_string_lossy(),
            false
        )
        .is_err());
        assert_eq!(
            session_workspace_reference(
                &workspace,
                &root_a.join("nested/file.txt").to_string_lossy(),
                false
            )
            .unwrap(),
            root_a.join("nested/file.txt")
        );
    }
}
