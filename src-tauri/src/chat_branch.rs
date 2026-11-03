//! Read-only capture and safe materialization of a chat branch's working tree.
//!
//! The caller must hold the source conversation/worktree lock and ensure the
//! source is idle. Capture never changes the Git index or source files. Call
//! `verify_unchanged` immediately before asking Worktrunk to create the branch.

#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
use std::{
    collections::{BTreeSet, HashSet},
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

struct Entry {
    relative: PathBuf,
    staged: Option<PathBuf>,
    symlink: Option<PathBuf>,
    mode: u32,
    digest: Vec<u8>,
}

/// A frozen view of tracked changes, deletions, and non-ignored untracked files.
pub struct Snapshot {
    source: PathBuf,
    head: String,
    status: Vec<u8>,
    staging: tempfile::TempDir,
    entries: Vec<Entry>,
}

impl Snapshot {
    pub fn capture(source_root: &Path, staging_parent: &Path) -> Result<Self, String> {
        let source = fs::canonicalize(source_root).map_err(err)?;
        let staging_root = fs::canonicalize(staging_parent).map_err(err)?;
        if staging_root.starts_with(&source) {
            return Err("Snapshot staging must be outside the source worktree".into());
        }
        let head = git(&source, &["rev-parse", "HEAD"])?;
        let head = String::from_utf8(head).map_err(err)?.trim().to_owned();
        let status = git(
            &source,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        let conflicts = git(&source, &["ls-files", "-u", "-z"])?;
        if !conflicts.is_empty() {
            return Err("Cannot branch a worktree with unresolved merge conflicts".into());
        }
        let gitlinks = git(&source, &["ls-files", "-s", "-z"])?;
        if gitlinks
            .split(|b| *b == 0)
            .filter(|item| !item.is_empty())
            .any(|item| item.starts_with(b"160000 "))
        {
            return Err("Cannot snapshot a repository containing nested Git submodules".into());
        }

        let mut paths = BTreeSet::new();
        for path in nul_paths(&git(
            &source,
            &["diff", "--name-only", "--no-renames", "-z", "HEAD"],
        )?)? {
            paths.insert(path);
        }
        for path in nul_paths(&git(
            &source,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?)? {
            paths.insert(path);
        }

        let staging = tempfile::Builder::new()
            .prefix("brr-chat-branch-")
            .tempdir_in(staging_parent)
            .map_err(err)?;
        let mut entries = Vec::new();
        for (index, relative) in paths.into_iter().enumerate() {
            validate_relative(&relative)?;
            let absolute = source.join(&relative);
            if has_nested_git(&source, &relative) {
                return Err(format!(
                    "Cannot snapshot nested Git repository: {}",
                    relative.display()
                ));
            }
            match fs::symlink_metadata(&absolute) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    entries.push(Entry {
                        relative,
                        staged: None,
                        symlink: None,
                        mode: 0,
                        digest: Vec::new(),
                    });
                }
                Err(error) => {
                    return Err(format!("Cannot inspect {}: {error}", relative.display()))
                }
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = fs::read_link(&absolute).map_err(err)?;
                    validate_symlink(&source, &relative, &target)?;
                    entries.push(Entry {
                        relative,
                        staged: None,
                        symlink: Some(target.clone()),
                        mode: 0,
                        digest: path_bytes(target.as_os_str()),
                    });
                }
                Ok(metadata) if metadata.is_file() => {
                    let bytes = fs::read(&absolute).map_err(err)?;
                    let staged_path = staging.path().join(index.to_string());
                    fs::write(&staged_path, &bytes).map_err(err)?;
                    let mode = safe_mode_bits(&metadata);
                    let digest = digest(&bytes);
                    entries.push(Entry {
                        relative,
                        staged: Some(staged_path),
                        symlink: None,
                        mode,
                        digest,
                    });
                }
                Ok(_) => {
                    return Err(format!(
                        "Cannot snapshot special file: {}",
                        relative.display()
                    ))
                }
            }
        }
        let snapshot = Self {
            source,
            head,
            status,
            staging,
            entries,
        };
        snapshot.verify_unchanged(source_root)?;
        Ok(snapshot)
    }

    pub fn head(&self) -> &str {
        &self.head
    }

    pub fn verify_unchanged(&self, source_root: &Path) -> Result<(), String> {
        let root = fs::canonicalize(source_root).map_err(err)?;
        if root != self.source {
            return Err("The source worktree path changed after snapshot capture".into());
        }
        let current_head = git(&root, &["rev-parse", "HEAD"])?;
        if String::from_utf8_lossy(&current_head).trim() != self.head {
            return Err("The source worktree HEAD changed after snapshot capture".into());
        }
        let status = git(
            &root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        if status != self.status {
            return Err("The source worktree changed after snapshot capture".into());
        }
        for entry in &self.entries {
            let path = root.join(&entry.relative);
            match (
                entry.staged.as_ref(),
                entry.symlink.as_ref(),
                fs::symlink_metadata(&path),
            ) {
                (None, None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                (Some(staged), None, Ok(metadata))
                    if metadata.is_file() && !metadata.file_type().is_symlink() =>
                {
                    let bytes = fs::read(path).map_err(err)?;
                    if digest(&bytes) != entry.digest
                        || safe_mode_bits(&metadata) != entry.mode
                        || digest(&fs::read(staged).map_err(err)?) != entry.digest
                    {
                        return Err(format!(
                            "Source file changed after snapshot capture: {}",
                            entry.relative.display()
                        ));
                    }
                }
                (None, Some(target), Ok(metadata))
                    if metadata.file_type().is_symlink()
                        && fs::read_link(path).map_err(err)? == *target => {}
                _ => {
                    return Err(format!(
                        "Source file changed after snapshot capture: {}",
                        entry.relative.display()
                    ))
                }
            }
        }
        Ok(())
    }

    pub fn materialize(&self, destination: &Path) -> Result<(), String> {
        let destination = fs::canonicalize(destination).map_err(err)?;
        for entry in &self.entries {
            validate_relative(&entry.relative)?;
            let target = destination.join(&entry.relative);
            if !target.starts_with(&destination) {
                return Err("Snapshot path escapes the destination worktree".into());
            }
            ensure_safe_parent(&destination, &target)?;
            match (&entry.staged, &entry.symlink) {
                (None, None) => {
                    if target.exists() || fs::symlink_metadata(&target).is_ok() {
                        fs::remove_file(&target).map_err(err)?;
                    }
                }
                (Some(staged), None) => {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(err)?;
                    }
                    if fs::symlink_metadata(&target)
                        .map(|metadata| metadata.file_type().is_symlink())
                        .unwrap_or(false)
                    {
                        fs::remove_file(&target).map_err(err)?;
                    }
                    fs::copy(staged, &target).map_err(err)?;
                    set_mode_bits(&target, entry.mode)?;
                }
                (None, Some(link)) => {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(err)?;
                    }
                    #[cfg(unix)]
                    if fs::symlink_metadata(&target).is_ok() {
                        fs::remove_file(&target).map_err(err)?;
                    }
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(link, &target).map_err(err)?;
                    #[cfg(not(unix))]
                    return Err("Snapshot symlinks are not supported on this platform".into());
                }
                _ => unreachable!(),
            }
        }
        let _keep_staging_alive = &self.staging;
        Ok(())
    }
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(err)?;
    if !output.status.success() {
        return Err(format!(
            "Git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn nul_paths(bytes: &[u8]) -> Result<Vec<PathBuf>, String> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let path = path_from_bytes(part)?;
            validate_relative(&path)?;
            Ok(path)
        })
        .collect()
}

fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        Ok(PathBuf::from(OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(bytes.to_vec())
            .map(PathBuf::from)
            .map_err(err)
    }
}

fn path_bytes(path: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        path.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}

fn set_mode_bits(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).map_err(err)?.permissions();
        // Preserve ordinary rwx permissions while dropping setuid, setgid, and sticky bits.
        permissions.set_mode((permissions.mode() & !0o777) | (mode & 0o777));
        fs::set_permissions(path, permissions).map_err(err)?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn safe_mode_bits(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

fn ensure_safe_parent(root: &Path, target: &Path) -> Result<(), String> {
    let mut existing = target
        .parent()
        .ok_or_else(|| "Snapshot target has no parent".to_string())?;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| "Snapshot target has no existing ancestor".to_string())?;
    }
    if !fs::canonicalize(existing).map_err(err)?.starts_with(root) {
        return Err(format!(
            "Snapshot path would escape the destination worktree: {}",
            target.display()
        ));
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<(), String> {
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("Unsafe repository path: {}", path.display()));
    }
    Ok(())
}

fn validate_symlink(root: &Path, path: &Path, target: &Path) -> Result<(), String> {
    let mut visited = HashSet::new();
    check_symlink_target(root, path, target, &mut visited)?;
    Ok(())
}

fn check_symlink_target(
    root: &Path,
    path: &Path,
    target: &Path,
    visited: &mut HashSet<PathBuf>,
) -> Result<PathBuf, String> {
    if target.is_absolute() {
        return Err(format!(
            "Refusing absolute symlink in snapshot: {}",
            path.display()
        ));
    }
    if !visited.insert(path.to_path_buf()) {
        return Err(format!(
            "Refusing cyclic symlink in snapshot: {}",
            path.display()
        ));
    }
    let mut resolved = path.parent().unwrap_or(Path::new("")).to_path_buf();
    for part in target.components() {
        match part {
            Component::ParentDir => {
                if !resolved.pop() {
                    return Err(format!(
                        "Refusing escaping symlink in snapshot: {}",
                        path.display()
                    ));
                }
            }
            Component::Normal(name) => {
                resolved.push(name);
                let absolute = root.join(&resolved);
                match fs::symlink_metadata(&absolute) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        let nested_target = fs::read_link(&absolute).map_err(err)?;
                        resolved = check_symlink_target(root, &resolved, &nested_target, visited)?;
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("Cannot inspect symlink target: {error}")),
                }
            }
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "Unsafe symlink target in snapshot: {}",
                    path.display()
                ))
            }
        }
    }
    visited.remove(path);
    Ok(resolved)
}

fn has_nested_git(root: &Path, relative: &Path) -> bool {
    if !root.join(relative).starts_with(root) {
        return false;
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            break;
        };
        // Do not follow a source symlink while walking names returned by Git.
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            break;
        }
        // The source root is the expected repository. Only reject Git
        // metadata found below it, so an ordinary dirty file is allowed.
        if current.join(".git").exists() {
            return true;
        }
    }
    false
}

fn digest(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}
fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn git_ok(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn repo(dir: &Path) {
        git_ok(dir, &["init", "-q"]);
        git_ok(dir, &["config", "user.email", "test@example.com"]);
        git_ok(dir, &["config", "user.name", "Test"]);
        fs::write(dir.join("tracked.txt"), "original\n").unwrap();
        fs::write(dir.join("gone.txt"), "delete me\n").unwrap();
        fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
        git_ok(dir, &["add", "."]);
        git_ok(dir, &["commit", "-qm", "initial"]);
    }

    #[test]
    fn captures_tracked_edits_deletions_untracked_and_modes_without_touching_source() {
        let source = tempfile::tempdir().unwrap();
        repo(source.path());
        fs::write(source.path().join("tracked.txt"), "edited\n").unwrap();
        fs::remove_file(source.path().join("gone.txt")).unwrap();
        fs::write(source.path().join("new.txt"), "new\n").unwrap();
        fs::set_permissions(
            source.path().join("run.sh"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();
        fs::write(source.path().join("ignored.tmp"), "ignored\n").unwrap();
        fs::write(source.path().join(".gitignore"), "ignored.tmp\n").unwrap();
        let index = fs::read(source.path().join(".git/index")).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let snapshot = Snapshot::capture(source.path(), staging.path()).unwrap();
        let dest = tempfile::tempdir().unwrap();
        repo(dest.path());
        snapshot.materialize(dest.path()).unwrap();
        assert_eq!(
            fs::read(dest.path().join("tracked.txt")).unwrap(),
            b"edited\n"
        );
        assert!(!dest.path().join("gone.txt").exists());
        assert_eq!(fs::read(dest.path().join("new.txt")).unwrap(), b"new\n");
        assert_eq!(
            fs::metadata(dest.path().join("run.sh"))
                .unwrap()
                .permissions()
                .mode()
            & 0o777,
            0o751
        );
        assert!(!dest.path().join("ignored.tmp").exists());
        assert_eq!(fs::read(source.path().join(".git/index")).unwrap(), index);
        snapshot.verify_unchanged(source.path()).unwrap();
    }

    #[test]
    fn rejects_unresolved_conflicts_and_escaping_symlinks() {
        let source = tempfile::tempdir().unwrap();
        repo(source.path());
        let original =
            String::from_utf8(git(source.path(), &["branch", "--show-current"]).unwrap())
                .unwrap()
                .trim()
                .to_owned();
        git_ok(source.path(), &["checkout", "-qb", "other"]);
        fs::write(source.path().join("tracked.txt"), "other\n").unwrap();
        git_ok(source.path(), &["commit", "-qam", "other"]);
        git_ok(source.path(), &["checkout", "-q", &original]);
        fs::write(source.path().join("tracked.txt"), "ours\n").unwrap();
        git_ok(source.path(), &["commit", "-qam", "ours"]);
        let _ = Command::new("git")
            .args(["merge", "other"])
            .current_dir(source.path())
            .output();
        let conflict_staging = tempfile::tempdir().unwrap();
        assert!(Snapshot::capture(source.path(), conflict_staging.path())
            .err()
            .unwrap()
            .contains("conflicts"));

        let source2 = tempfile::tempdir().unwrap();
        repo(source2.path());
        std::os::unix::fs::symlink("../../outside", source2.path().join("escape")).unwrap();
        let source2_staging = tempfile::tempdir().unwrap();
        assert!(Snapshot::capture(source2.path(), source2_staging.path())
            .err()
            .unwrap()
            .contains("escaping symlink"));

        let source2b = tempfile::tempdir().unwrap();
        repo(source2b.path());
        std::os::unix::fs::symlink("../outside-missing", source2b.path().join("inner-link"))
            .unwrap();
        git_ok(source2b.path(), &["add", "inner-link"]);
        git_ok(source2b.path(), &["commit", "-qm", "safe fixture link"]);
        std::os::unix::fs::symlink("inner-link", source2b.path().join("new-link")).unwrap();
        let source2b_staging = tempfile::tempdir().unwrap();
        assert!(Snapshot::capture(source2b.path(), source2b_staging.path())
            .err()
            .unwrap()
            .contains("escaping symlink"));

        let source3 = tempfile::tempdir().unwrap();
        repo(source3.path());
        let nested = source3.path().join("nested-repo");
        fs::create_dir(&nested).unwrap();
        git_ok(&nested, &["init", "-q"]);
        let source3_staging = tempfile::tempdir().unwrap();
        assert!(Snapshot::capture(source3.path(), source3_staging.path())
            .err()
            .unwrap()
            .contains("nested Git repository"));
    }
}
