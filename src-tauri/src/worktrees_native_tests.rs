use super::*;
use std::path::Path;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn committed_repo(base: &Path) -> (PathBuf, PathBuf) {
    let root = base.join("source");
    let selected = root.join("nested");
    fs::create_dir_all(&selected).unwrap();
    git(&root, &["init", "-b", "main"]);
    fs::write(selected.join(".keep"), "tracked").unwrap();
    git(&root, &["add", "."]);
    git(
        &root,
        &[
            "-c",
            "user.name=Brr Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "test: fixture",
        ],
    );
    (root, selected)
}

#[tokio::test]
async fn native_creation_needs_no_installed_worktrunk_executable() {
    let temp = tempfile::tempdir().unwrap();
    let (root, selected) = committed_repo(temp.path());
    fs::write(root.join("other-agent.txt"), "keep exactly").unwrap();
    let manager = WorktreeManager::with_config(
        temp.path().join("native-home"),
        UserConfig::default(),
        temp.path().join("approvals.toml"),
    );
    let workspace = manager.prepare(selected.to_str().unwrap()).await.unwrap();
    assert_ne!(Path::new(&workspace.path), root.canonicalize().unwrap());
    assert!(Path::new(&workspace.path).join(".git").is_file());
    assert_eq!(
        Path::new(&workspace.cwd),
        Path::new(&workspace.path).join("nested")
    );
    assert_eq!(
        workspace.project_path,
        selected.canonicalize().unwrap().to_string_lossy()
    );
    fs::write(
        Path::new(&workspace.cwd).join("only-this-session.txt"),
        "isolated",
    )
    .unwrap();
    assert!(!selected.join("only-this-session.txt").exists());
    assert_eq!(
        fs::read_to_string(root.join("other-agent.txt")).unwrap(),
        "keep exactly"
    );
    let second = manager.prepare(selected.to_str().unwrap()).await.unwrap();
    assert_ne!(workspace.path, second.path);
    manager.validate(&workspace).await.unwrap();
    git(
        Path::new(&workspace.path),
        &["switch", "-c", "unexpected-branch"],
    );
    assert!(
        manager.validate(&workspace).await.is_err(),
        "a manually changed branch must not pass saved-session validation"
    );
}

#[test]
fn saved_workspace_metadata_remains_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let manager = WorktreeManager::new(temp.path().to_path_buf());
    assert!(manager.get("thread-1").unwrap().is_none());
    let workspace = Workspace {
        project_path: "/source".into(),
        path: "/tree".into(),
        branch: "codex/brr-a".into(),
        cwd: "/tree".into(),
    };
    manager.save("thread-1", &workspace).unwrap();
    assert_eq!(manager.get("thread-1").unwrap().unwrap().path, "/tree");
    assert!(manager.save("../escape", &workspace).is_err());
    fs::write(temp.path().join("worktrees/thread-1.json"), "broken").unwrap();
    assert!(manager.get("thread-1").unwrap_err().contains("corrupt"));
    fs::remove_dir_all(temp.path().join("worktrees")).unwrap();
    assert!(manager.get("thread-1").unwrap_err().contains("backup"));
}
