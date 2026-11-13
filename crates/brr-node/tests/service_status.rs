#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn service_status_preserves_manager_output_and_exit_code() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping service status CLI test: brrctl refuses service management as root");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("mock-bin");
    fs::create_dir_all(&bin).unwrap();

    #[cfg(target_os = "macos")]
    let (manager, expected_code) = ("launchctl", 113);
    #[cfg(target_os = "linux")]
    let (manager, expected_code) = ("systemctl", 73);

    let manager_path = bin.join(manager);

    let data_dir = temp.path().join("isolated-status-data");
    for code in [0, expected_code] {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' 'manager stdout'\nprintf '%s\\n' 'manager stderr' >&2\nexit {code}\n"
        );
        fs::write(&manager_path, script).unwrap();
        fs::set_permissions(&manager_path, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_brrctl"))
            .env("PATH", &bin)
            .arg("--data-dir")
            .arg(&data_dir)
            .args(["service", "status"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(code));
        assert_eq!(output.stdout, b"manager stdout\n");
        assert_eq!(output.stderr, b"manager stderr\n");
    }
}
