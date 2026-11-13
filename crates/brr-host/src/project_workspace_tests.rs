use super::*;

#[test]
fn session_snapshot_round_trips_and_missing_metadata_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let canonical = fs::canonicalize(temp.path()).unwrap();
    let manager = WorktreeManager::new(temp.path().to_path_buf());
    let snapshot = SessionWorkspace {
        cwd: canonical.to_string_lossy().into_owned(),
        roots: vec![canonical.to_string_lossy().into_owned()],
        folders: vec![ResolvedFolder {
            source: canonical.to_string_lossy().into_owned(),
            path: canonical.to_string_lossy().into_owned(),
        }],
        worktrees: vec![],
        scope_project_id: Some("project-1".into()),
        project_revision: Some(3),
    };
    manager.save_session("thread-1", &snapshot).unwrap();
    assert_eq!(manager.get_session("thread-1").unwrap(), Some(snapshot));
    fs::remove_file(temp.path().join("worktrees/thread-1.json")).unwrap();
    assert!(manager
        .get_session("thread-1")
        .unwrap_err()
        .contains("missing"));
}

#[tokio::test]
async fn folderless_session_gets_private_scratch_directory() {
    let temp = tempfile::tempdir().unwrap();
    let manager = WorktreeManager::new(temp.path().to_path_buf());
    let session = manager.prepare_session(None).await.unwrap();
    assert!(session.folders.is_empty());
    assert!(Path::new(&session.cwd).is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&session.cwd).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[tokio::test]
#[cfg(unix)]
async fn saved_shared_folder_cannot_be_redirected_by_replacing_it_with_a_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let primary = base.join("primary");
    fs::create_dir_all(&primary).unwrap();
    let folder = base.join("selected");
    let outside = base.join("elsewhere");
    fs::create_dir_all(&folder).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let manager = WorktreeManager::new(base.join("engine"));
    let project = crate::projects::ProjectRecord {
        id: "project-1".into(),
        name: "Shared".into(),
        folders: vec![
            primary.to_string_lossy().into_owned(),
            folder.to_string_lossy().into_owned(),
        ],
        primary_folder: Some(primary.to_string_lossy().into_owned()),
        revision: 1,
        model: None,
        effort: None,
        permission: None,
        hidden: false,
    };
    let snapshot = manager.prepare_session(Some(&project)).await.unwrap();
    manager.save_session("thread-1", &snapshot).unwrap();
    fs::remove_dir(&folder).unwrap();
    std::os::unix::fs::symlink(&outside, &folder).unwrap();
    assert!(
        manager.get_session("thread-1").is_err(),
        "a resumed chat must retain its original canonical roots"
    );
}

fn fixture_git(root: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn roots_from_two_checkouts_of_one_repository_share_one_new_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let repo = base.join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/code.txt"), "source").unwrap();
    fixture_git(&repo, &["init", "-b", "main"]);
    fixture_git(&repo, &["add", "."]);
    fixture_git(
        &repo,
        &[
            "-c",
            "user.name=Brr Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "test: source",
        ],
    );
    let checkout = base.join("second-checkout");
    fixture_git(
        &repo,
        &["worktree", "add", "--detach", checkout.to_str().unwrap()],
    );
    let shared = base.join("notes");
    fs::create_dir_all(&shared).unwrap();
    let primary = repo.join("src").to_string_lossy().into_owned();
    let second = checkout.join("src").to_string_lossy().into_owned();
    let project = crate::projects::ProjectRecord {
        id: "project-1".into(),
        name: "Combined".into(),
        folders: vec![
            primary.clone(),
            second,
            shared.to_string_lossy().into_owned(),
        ],
        primary_folder: Some(primary),
        revision: 1,
        model: None,
        effort: None,
        permission: None,
        hidden: false,
    };
    let manager = WorktreeManager::with_config(
        base.join("engine"),
        UserConfig::default(),
        base.join("approvals.toml"),
    );
    let session = manager.prepare_session(Some(&project)).await.unwrap();
    assert_eq!(session.worktrees.len(), 1);
    assert_eq!(session.folders[0].path, session.folders[1].path);
    assert_eq!(session.folders[2].path, shared.to_string_lossy());
    assert_eq!(
        fs::read_to_string(Path::new(&session.cwd).join("code.txt")).unwrap(),
        "source"
    );
}

fn repository_with_hook(base: &Path, name: &str, command: &str) -> PathBuf {
    let root = base.join(name);
    fs::create_dir_all(root.join(".config")).unwrap();
    fs::write(
        root.join(".config/wt.toml"),
        format!("pre-start = {command:?}\n"),
    )
    .unwrap();
    fixture_git(&root, &["init", "-b", "main"]);
    fixture_git(&root, &["add", "."]);
    fixture_git(
        &root,
        &[
            "-c",
            "user.name=Brr Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "test: hook fixture",
        ],
    );
    root
}

fn repository_worktrees(root: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(str::to_owned))
        .collect()
}

fn project_for_folders(folders: &[PathBuf]) -> crate::projects::ProjectRecord {
    let folders: Vec<String> = folders
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    crate::projects::ProjectRecord {
        id: "multi-repo".into(),
        name: "Two repositories".into(),
        primary_folder: folders.first().cloned(),
        folders,
        revision: 1,
        model: None,
        effort: None,
        permission: None,
        hidden: false,
    }
}

#[tokio::test]
async fn multiple_repositories_review_all_hooks_before_creating_any_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let one = repository_with_hook(&base, "one", "touch one-ready");
    let two = repository_with_hook(&base, "two", "touch two-ready");
    let manager = WorktreeManager::with_config(
        base.join("engine"),
        UserConfig::default(),
        base.join("approvals.toml"),
    );
    let project = project_for_folders(&[one.clone(), two.clone()]);
    manager
        .approve(one.to_str().unwrap(), vec!["touch one-ready".into()])
        .await
        .unwrap();
    let commands = manager.inspect_session(Some(&project)).await.unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands.iter().filter(|command| !command.approved).count(),
        1
    );
    assert!(manager
        .prepare_session(Some(&project))
        .await
        .unwrap_err()
        .contains("Unapproved"));
    assert_eq!(repository_worktrees(&one).len(), 1);
    assert_eq!(repository_worktrees(&two).len(), 1);
    manager
        .approve(two.to_str().unwrap(), vec!["touch two-ready".into()])
        .await
        .unwrap();
    let session = manager.prepare_session(Some(&project)).await.unwrap();
    assert_eq!(session.worktrees.len(), 2);
    assert!(Path::new(&session.folders[0].path)
        .join("one-ready")
        .is_file());
    assert!(Path::new(&session.folders[1].path)
        .join("two-ready")
        .is_file());
}

#[tokio::test]
async fn setup_failure_reports_every_retained_worktree_across_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let one = repository_with_hook(&base, "one", "touch one-ready");
    let two = repository_with_hook(&base, "two", "printf setup-failed; exit 1");
    let manager = WorktreeManager::with_config(
        base.join("engine"),
        UserConfig::default(),
        base.join("approvals.toml"),
    );
    manager
        .approve(one.to_str().unwrap(), vec!["touch one-ready".into()])
        .await
        .unwrap();
    manager
        .approve(
            two.to_str().unwrap(),
            vec!["printf setup-failed; exit 1".into()],
        )
        .await
        .unwrap();
    let error = manager
        .prepare_session(Some(&project_for_folders(&[one.clone(), two.clone()])))
        .await
        .unwrap_err();
    assert!(error.contains("setup-failed"), "{error}");
    let retained: Vec<String> = [&one, &two]
        .into_iter()
        .flat_map(|root| {
            repository_worktrees(root)
                .into_iter()
                .filter(move |path| Path::new(path) != root)
        })
        .collect();
    assert!(!retained.is_empty());
    for path in retained {
        assert!(Path::new(&path).is_dir());
        assert!(
            error.contains(&path),
            "missing retained path {path}: {error}"
        );
    }
}
