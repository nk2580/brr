//! Brr's host-owned chat database and imported originals are never agent-readable.
//! Only the current thread's explicitly staged originals receive read access.
use codex_protocol::models::{ManagedFileSystemPermissions, PermissionProfile};
use codex_protocol::permissions::{FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry};
use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Path;

pub(super) fn scoped_chat_profile(
    mut profile: PermissionProfile,
    home: &Path,
    service_data_root: &Path,
    workspace_cwd: &Path,
    thread_id: &str,
) -> Result<PermissionProfile, String> {
    if thread_id.is_empty()
        || !thread_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("Invalid Brr thread identity".into());
    }
    let service_root = AbsolutePathBuf::try_from(service_data_root).map_err(|e| e.to_string())?;
    let private = AbsolutePathBuf::try_from(home.join("agents/chat")).map_err(|e| e.to_string())?;
    let own = AbsolutePathBuf::try_from(private.as_path().join("runtime").join(thread_id))
        .map_err(|e| e.to_string())?;
    if !private.as_path().starts_with(service_root.as_path())
        || !own.as_path().starts_with(service_root.as_path())
    {
        return Err("Brr chat paths must be inside the host-owned service data root".into());
    }
    let workspace = AbsolutePathBuf::try_from(workspace_cwd).map_err(|e| e.to_string())?;
    let workspace_access = if workspace.as_path().starts_with(service_root.as_path())
        && workspace != service_root
        && is_host_managed_scratch_workspace(home, &workspace, thread_id)
    {
        let policy = profile.file_system_sandbox_policy();
        if policy.can_write_local_path_with_cwd(workspace.as_path(), workspace_cwd) {
            Some(FileSystemAccessMode::Write)
        } else if policy.can_read_local_path_with_cwd(workspace.as_path(), workspace_cwd) {
            Some(FileSystemAccessMode::Read)
        } else {
            None
        }
    } else {
        None
    };
    let deny = FileSystemPath::from(service_root.clone());
    let allow = FileSystemPath::from(own);
    let PermissionProfile::Managed {
        file_system: ManagedFileSystemPermissions::Restricted { entries, .. },
        ..
    } = &mut profile
    else {
        return Err("Brr chat requires managed filesystem isolation".into());
    };
    let FileSystemPath::Path { path: private_uri } = &deny else {
        unreachable!()
    };
    let tmpdir_inside_service = AbsolutePathBuf::try_from(std::env::temp_dir())
        .is_ok_and(|tmpdir| tmpdir.as_path().starts_with(service_root.as_path()));
    // A history fork inherits the source's raw profile. Never inherit any
    // service-root grant; re-add only its exact current execution workspace and
    // this thread's staged originals below.
    entries.retain(|entry| match &entry.path {
        FileSystemPath::Path { path } if path.starts_with(private_uri) => {
            entry.access == FileSystemAccessMode::Deny && entry.path != deny
        }
        FileSystemPath::Special {
            value: codex_protocol::permissions::FileSystemSpecialPath::ProjectRoots { .. },
        } if workspace.as_path().starts_with(service_root.as_path()) => {
            entry.access == FileSystemAccessMode::Deny
        }
        FileSystemPath::Special {
            value: codex_protocol::permissions::FileSystemSpecialPath::Tmpdir,
        } if tmpdir_inside_service => entry.access == FileSystemAccessMode::Deny,
        _ => true,
    });
    entries.push(FileSystemSandboxEntry::new(
        deny,
        FileSystemAccessMode::Deny,
    ));
    if let Some(access) = workspace_access {
        entries.push(FileSystemSandboxEntry::new(
            FileSystemPath::from(workspace),
            access,
        ));
    }
    entries.push(FileSystemSandboxEntry::new(
        allow,
        FileSystemAccessMode::Read,
    ));
    Ok(profile)
}

/// In-root CWD grants are limited to scratch directories recorded by Host for
/// this exact thread. Arbitrary private paths in a thread request cannot become
/// tool-readable merely by being selected as the runtime CWD.
fn is_host_managed_scratch_workspace(
    home: &Path,
    workspace: &AbsolutePathBuf,
    thread_id: &str,
) -> bool {
    let Ok(scratch_root) = AbsolutePathBuf::try_from(home.join("session-workspaces")) else {
        return false;
    };
    if workspace.as_path().parent() != Some(scratch_root.as_path()) {
        return false;
    }
    let marker = home.join("worktree-sessions").join(thread_id);
    let metadata = home.join("worktrees").join(format!("{thread_id}.json"));
    for path in [&marker, &metadata] {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            _ => return false,
        }
    }
    let Ok(bytes) = std::fs::read(metadata) else {
        return false;
    };
    let Ok(snapshot) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let Some(saved_cwd) = snapshot.get("cwd").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Ok(saved_cwd) = AbsolutePathBuf::try_from(Path::new(saved_cwd)) else {
        return false;
    };
    if saved_cwd != *workspace {
        return false;
    }
    snapshot
        .get("roots")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|roots| {
            roots.iter().any(|root| {
                root.as_str()
                    .and_then(|root| AbsolutePathBuf::try_from(Path::new(root)).ok())
                    .is_some_and(|root| root == *workspace)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn denies_other_conversations_and_retains_workspace_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let service_root = dir.path().join("data");
        let home = service_root.join("engine");
        let cwd = home.join("session-workspaces/current");
        std::fs::create_dir_all(&cwd).unwrap();
        save_scratch_metadata(&home, "source", &cwd);
        save_scratch_metadata(&home, "fork", &cwd);
        for base in [
            PermissionProfile::read_only(),
            PermissionProfile::workspace_write(),
        ] {
            let expected = base
                .file_system_sandbox_policy()
                .can_write_local_path_with_cwd(&cwd.join("source.txt"), &cwd);
            let first = scoped_chat_profile(base, &home, &service_root, &cwd, "source").unwrap();
            let fork = scoped_chat_profile(first, &home, &service_root, &cwd, "fork").unwrap();
            let fs = fork.file_system_sandbox_policy();
            assert!(!fs.can_read_local_path_with_cwd(&home.join("agents/chat/chat.sqlite3"), &cwd));
            assert!(!fs.can_read_local_path_with_cwd(
                &home.join("agents/chat/runtime/source/private.txt"),
                &cwd
            ));
            assert!(fs.can_read_local_path_with_cwd(
                &home.join("agents/chat/runtime/fork/selected.txt"),
                &cwd
            ));
            assert!(!fs.can_write_local_path_with_cwd(
                &home.join("agents/chat/runtime/fork/selected.txt"),
                &cwd
            ));
            assert_eq!(
                fs.can_write_local_path_with_cwd(&cwd.join("source.txt"), &cwd),
                expected
            );
        }
    }

    fn save_scratch_metadata(home: &Path, thread_id: &str, cwd: &Path) {
        std::fs::create_dir_all(home.join("worktree-sessions")).unwrap();
        std::fs::create_dir_all(home.join("worktrees")).unwrap();
        std::fs::write(home.join("worktree-sessions").join(thread_id), b"").unwrap();
        std::fs::write(
            home.join("worktrees").join(format!("{thread_id}.json")),
            serde_json::json!({ "cwd": cwd, "roots": [cwd] }).to_string(),
        )
        .unwrap();
    }
    #[test]
    fn rejects_unmanaged_profiles_and_path_traversal() {
        assert!(
            scoped_chat_profile(
                PermissionProfile::Disabled,
                Path::new("/tmp/home"),
                Path::new("/tmp/data"),
                Path::new("/tmp/workspace"),
                "thread"
            )
            .is_err()
        );
        assert!(
            scoped_chat_profile(
                PermissionProfile::read_only(),
                Path::new("/tmp/data/home"),
                Path::new("/tmp/data"),
                Path::new("/tmp/workspace"),
                "../other"
            )
            .is_err()
        );
    }

    #[test]
    fn protects_the_service_root_and_reopens_only_this_threads_workspace_and_originals() {
        let temp = tempfile::tempdir().unwrap();
        let service_root = temp.path().join("data");
        let home = service_root.join("engine");
        let cwd = home.join("session-workspaces/current-thread");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(home.join("worktree-sessions")).unwrap();
        std::fs::create_dir_all(home.join("worktrees")).unwrap();
        std::fs::write(home.join("worktree-sessions/current-thread"), b"").unwrap();
        std::fs::write(
            home.join("worktrees/current-thread.json"),
            serde_json::json!({ "cwd": cwd, "roots": [cwd] }).to_string(),
        )
        .unwrap();
        let profile = scoped_chat_profile(
            PermissionProfile::workspace_write(),
            &home,
            &service_root,
            &cwd,
            "current-thread",
        )
        .unwrap();
        let policy = profile.file_system_sandbox_policy();
        assert!(policy.can_write_local_path_with_cwd(&cwd.join("file.txt"), &cwd));
        assert!(policy.can_read_local_path_with_cwd(
            &home.join("agents/chat/runtime/current-thread/original.txt"),
            &cwd
        ));
        for private in [
            service_root.join("node/auth.sqlite"),
            service_root.join("node/peers.sqlite"),
            service_root.join("projects.sqlite"),
            service_root.join(".brr-fleet/origin.git/objects/private"),
            home.join("agents/chat/chat.sqlite3"),
            home.join("session-workspaces/other-thread/secret.txt"),
            home.join("agents/chat/runtime/other-thread/original.txt"),
        ] {
            assert!(
                !policy.can_read_local_path_with_cwd(&private, &cwd),
                "unexpected read grant for {}",
                private.display()
            );
        }
        assert!(!policy.can_write_local_path_with_cwd(
            &home.join("agents/chat/runtime/current-thread/original.txt"),
            &cwd
        ));
    }

    #[test]
    fn a_private_directory_cannot_be_reopened_by_requesting_it_as_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let service_root = temp.path().join("data");
        let home = service_root.join("engine");
        let node = service_root.join("node");
        let chats = home.join("agents/chat");
        std::fs::create_dir_all(&node).unwrap();
        std::fs::create_dir_all(&chats).unwrap();
        let unmanaged_scratch = home.join("session-workspaces/unmanaged");
        std::fs::create_dir_all(&unmanaged_scratch).unwrap();
        let auth_db = node.join("auth.sqlite");
        std::fs::write(&auth_db, "private credential database").unwrap();

        for cwd in [&node, &chats, &service_root, &home, &unmanaged_scratch] {
            let profile = scoped_chat_profile(
                PermissionProfile::workspace_write(),
                &home,
                &service_root,
                cwd,
                "hostile-cwd",
            )
            .unwrap();
            let policy = profile.file_system_sandbox_policy();
            assert!(
                !policy.can_read_local_path_with_cwd(&auth_db, cwd),
                "private auth store became readable with cwd {}",
                cwd.display()
            );
            assert!(
                !policy.can_write_local_path_with_cwd(&node.join("tool-created"), cwd),
                "private service directory became writable with cwd {}",
                cwd.display()
            );
        }
    }
}
