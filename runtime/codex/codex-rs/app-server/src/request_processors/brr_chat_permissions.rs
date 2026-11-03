//! Brr's host-owned chat database and imported originals are never agent-readable.
//! Only the current thread's explicitly staged originals receive read access.
use codex_protocol::models::{ManagedFileSystemPermissions, PermissionProfile};
use codex_protocol::permissions::{FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry};
use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Path;

pub(super) fn scoped_chat_profile(
    mut profile: PermissionProfile,
    home: &Path,
    thread_id: &str,
) -> Result<PermissionProfile, String> {
    if thread_id.is_empty()
        || !thread_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("Invalid Brr thread identity".into());
    }
    let private = AbsolutePathBuf::try_from(home.join("agents/chat")).map_err(|e| e.to_string())?;
    let own = AbsolutePathBuf::try_from(private.as_path().join("runtime").join(thread_id))
        .map_err(|e| e.to_string())?;
    let deny = FileSystemPath::from(private);
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
    // A history fork inherits the source's raw profile. Never inherit its grant.
    entries.retain(|entry| match &entry.path {
        FileSystemPath::Path { path } if path.starts_with(private_uri) => {
            entry.access == FileSystemAccessMode::Deny && entry.path != deny
        }
        _ => true,
    });
    entries.push(FileSystemSandboxEntry::new(
        deny,
        FileSystemAccessMode::Deny,
    ));
    entries.push(FileSystemSandboxEntry::new(
        allow,
        FileSystemAccessMode::Read,
    ));
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn denies_other_conversations_and_retains_workspace_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let cwd = home.join("workspace");
        std::fs::create_dir(&cwd).unwrap();
        for base in [
            PermissionProfile::read_only(),
            PermissionProfile::workspace_write(),
        ] {
            let expected = base
                .file_system_sandbox_policy()
                .can_write_local_path_with_cwd(&cwd.join("source.txt"), &cwd);
            let first = scoped_chat_profile(base, home, "source").unwrap();
            let fork = scoped_chat_profile(first, home, "fork").unwrap();
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
    #[test]
    fn rejects_unmanaged_profiles_and_path_traversal() {
        assert!(
            scoped_chat_profile(PermissionProfile::Disabled, Path::new("/tmp"), "thread").is_err()
        );
        assert!(scoped_chat_profile(
            PermissionProfile::read_only(),
            Path::new("/tmp"),
            "../other"
        )
        .is_err());
    }
}
