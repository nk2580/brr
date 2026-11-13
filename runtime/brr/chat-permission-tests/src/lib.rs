#![allow(dead_code)]

#[path = "../../../codex/codex-rs/app-server/src/request_processors/brr_chat_permissions.rs"]
mod brr_chat_permissions;

#[cfg(all(test, target_os = "macos"))]
mod seatbelt_tests {
    use crate::brr_chat_permissions::scoped_chat_profile;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use codex_sandboxing::seatbelt::{
        CreateSeatbeltCommandArgsParams, MACOS_PATH_TO_SEATBELT_EXECUTABLE,
        create_seatbelt_command_args,
    };
    use codex_utils_absolute_path::AbsolutePathBuf;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    struct ServiceRootEnv(Option<OsString>);

    impl Drop for ServiceRootEnv {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var("BRR_SERVICE_DATA_DIR", value),
                    None => std::env::remove_var("BRR_SERVICE_DATA_DIR"),
                }
            }
        }
    }

    fn run(profile: &PermissionProfile, cwd: &Path, command: Vec<String>) -> std::process::Output {
        let filesystem = profile.file_system_sandbox_policy();
        let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
            command,
            file_system_sandbox_policy: &filesystem,
            network_sandbox_policy: profile.network_sandbox_policy(),
            sandbox_policy_cwd: cwd,
            enforce_managed_network: false,
            managed_network: None,
            environment_id: None,
            network: None,
            extra_allow_unix_sockets: &[],
        })
        .expect("Seatbelt command args should be generated from the runtime policy");
        Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
            .args(&args)
            .output()
            .expect("sandbox-exec should launch")
    }

    fn cat(path: &Path) -> Vec<String> {
        vec!["/bin/cat".into(), path.to_string_lossy().into_owned()]
    }

    #[test]
    fn seatbelt_allows_only_selected_chat_staging_as_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let service_root = temp.path().join("brr-data");
        fs::create_dir_all(&service_root).unwrap();
        let service_root = service_root.canonicalize().unwrap();
        let home = service_root.join("engine");
        let workspace = home.join("session-workspaces/selected-thread");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(home.join("worktree-sessions")).unwrap();
        fs::create_dir_all(home.join("worktrees")).unwrap();
        fs::write(home.join("worktree-sessions/selected-thread"), b"").unwrap();
        fs::write(
            home.join("worktrees/selected-thread.json"),
            serde_json::json!({ "cwd": workspace, "roots": [workspace] }).to_string(),
        )
        .unwrap();

        let private = home.join("agents/chat");
        let own = private.join("runtime/selected-thread");
        let foreign = private.join("runtime/foreign-thread");
        fs::create_dir_all(&own).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        let selected = own.join("selection.txt");
        let foreign_file = foreign.join("selection.txt");
        let database = private.join("chat.sqlite3");
        let node_directory = service_root.join("node");
        let fleet_repo = service_root.join(".brr-fleet/origin.git");
        fs::create_dir_all(&node_directory).unwrap();
        fs::create_dir_all(&fleet_repo).unwrap();
        let auth = node_directory.join("auth.sqlite");
        let peers = node_directory.join("peers.sqlite");
        let project_store = service_root.join("projects.sqlite");
        let git_secret = fleet_repo.join("config");
        fs::write(&auth, "TLS private key").unwrap();
        fs::write(&peers, "fleet bearer credential").unwrap();
        fs::write(&project_store, "private project store").unwrap();
        fs::write(&git_secret, "private Git object data").unwrap();
        let ipc = node_directory.join("brrd.sock");
        #[cfg(unix)]
        let _listener = std::os::unix::net::UnixListener::bind(&ipc).unwrap();
        fs::write(&selected, "selected attachment\n").unwrap();
        fs::write(&foreign_file, "foreign attachment\n").unwrap();
        fs::write(&database, "private chat database\n").unwrap();

        let writable_roots = [AbsolutePathBuf::try_from(workspace.as_path()).unwrap()];
        let profile = scoped_chat_profile(
            PermissionProfile::workspace_write_with(
                &writable_roots,
                NetworkSandboxPolicy::Enabled,
                false,
                false,
            ),
            &home,
            &service_root,
            &workspace,
            "selected-thread",
        )
        .unwrap();
        let _restore_service_root = ServiceRootEnv(std::env::var_os("BRR_SERVICE_DATA_DIR"));
        unsafe { std::env::set_var("BRR_SERVICE_DATA_DIR", &service_root) };

        let allowed = run(&profile, &workspace, cat(&selected));
        assert!(allowed.status.success(), "allowed read failed: {allowed:?}");
        assert_eq!(
            String::from_utf8_lossy(&allowed.stdout),
            "selected attachment\n"
        );
        let read_only_profile = scoped_chat_profile(
            PermissionProfile::read_only(),
            &home,
            &service_root,
            &workspace,
            "selected-thread",
        )
        .unwrap();
        let read_only_selected = run(&read_only_profile, &workspace, cat(&selected));
        assert!(
            read_only_selected.status.success(),
            "selected original was unavailable in read-only mode: {read_only_selected:?}"
        );
        let read_only_auth = run(&read_only_profile, &workspace, cat(&auth));
        assert!(
            !read_only_auth.status.success(),
            "full-disk read profile reopened the private service root"
        );
        let workspace_write = run(
            &profile,
            &workspace,
            vec![
                "/usr/bin/touch".into(),
                workspace.join("workspace-write.txt").display().to_string(),
            ],
        );
        assert!(
            workspace_write.status.success(),
            "current workspace write failed: {workspace_write:?}"
        );

        for denied_path in [
            &foreign_file,
            &database,
            &auth,
            &peers,
            &project_store,
            &git_secret,
        ] {
            let denied = run(&profile, &workspace, cat(denied_path));
            assert!(
                !denied.status.success(),
                "unselected private path was readable: {}",
                denied_path.display()
            );
        }
        let private_write = run(
            &profile,
            &workspace,
            vec![
                "/usr/bin/touch".into(),
                auth.with_extension("tool-write").display().to_string(),
            ],
        );
        assert!(
            !private_write.status.success(),
            "tool wrote into Brr service data"
        );

        let hostile_workspace_roots =
            [AbsolutePathBuf::try_from(node_directory.as_path()).unwrap()];
        let hostile_cwd_profile = scoped_chat_profile(
            PermissionProfile::workspace_write_with(
                &hostile_workspace_roots,
                NetworkSandboxPolicy::Enabled,
                false,
                false,
            ),
            &home,
            &service_root,
            &node_directory,
            "selected-thread",
        )
        .unwrap();
        let hostile_auth_read = run(&hostile_cwd_profile, &node_directory, cat(&auth));
        assert!(
            !hostile_auth_read.status.success(),
            "requesting the node store as CWD exposed auth material"
        );
        let hostile_socket_connect = run(
            &hostile_cwd_profile,
            &node_directory,
            vec![
                "/usr/bin/python3".into(),
                "-c".into(),
                "import socket,sys; socket.socket(socket.AF_UNIX).connect(sys.argv[1])".into(),
                ipc.to_string_lossy().into_owned(),
            ],
        );
        assert!(
            !hostile_socket_connect.status.success(),
            "requesting the node store as CWD exposed Brr's IPC socket"
        );

        #[cfg(unix)]
        {
            let connect = run(
                &profile,
                &workspace,
                vec![
                    "/usr/bin/python3".into(),
                    "-c".into(),
                    "import socket,sys; socket.socket(socket.AF_UNIX).connect(sys.argv[1])".into(),
                    ipc.to_string_lossy().into_owned(),
                ],
            );
            assert!(
                !connect.status.success(),
                "Brr IPC socket remained reachable"
            );
        }

        let write_path = own.join("agent-write.txt");
        let write_attempt = run(
            &profile,
            &workspace,
            vec!["/usr/bin/touch".into(), write_path.display().to_string()],
        );
        assert!(
            !write_attempt.status.success(),
            "agent unexpectedly wrote into imported attachment staging: {write_attempt:?}"
        );
        assert!(!write_path.exists());
    }
}
