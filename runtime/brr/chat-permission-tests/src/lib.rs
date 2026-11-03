#![allow(dead_code)]

#[path = "../../../codex/codex-rs/app-server/src/request_processors/brr_chat_permissions.rs"]
mod brr_chat_permissions;

#[cfg(all(test, target_os = "macos"))]
mod seatbelt_tests {
    use crate::brr_chat_permissions::scoped_chat_profile;
    use codex_protocol::models::PermissionProfile;
    use codex_sandboxing::seatbelt::{
        CreateSeatbeltCommandArgsParams, MACOS_PATH_TO_SEATBELT_EXECUTABLE,
        create_seatbelt_command_args,
    };
    use std::fs;
    use std::path::Path;
    use std::process::Command;

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
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();

        let private = home.join("agents/chat");
        let own = private.join("runtime/selected-thread");
        let foreign = private.join("runtime/foreign-thread");
        fs::create_dir_all(&own).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        let selected = own.join("selection.txt");
        let foreign_file = foreign.join("selection.txt");
        let database = private.join("chat.sqlite3");
        fs::write(&selected, "selected attachment\n").unwrap();
        fs::write(&foreign_file, "foreign attachment\n").unwrap();
        fs::write(&database, "private chat database\n").unwrap();

        let profile = scoped_chat_profile(
            PermissionProfile::workspace_write(),
            &home,
            "selected-thread",
        )
        .unwrap();

        let allowed = run(&profile, &workspace, cat(&selected));
        assert!(allowed.status.success(), "allowed read failed: {allowed:?}");
        assert_eq!(String::from_utf8_lossy(&allowed.stdout), "selected attachment\n");

        for denied_path in [&foreign_file, &database] {
            let denied = run(&profile, &workspace, cat(denied_path));
            assert!(
                !denied.status.success(),
                "unselected private path was readable: {}",
                denied_path.display()
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
