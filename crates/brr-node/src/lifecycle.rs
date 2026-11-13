//! Explicit installation and control of the current user's Brr node service.

use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServicePlatform {
    Launchd,
    Systemd,
}

#[derive(Debug)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[OsString]) -> io::Result<CommandOutput>;
}

pub struct ServiceLifecycle<'a> {
    data_dir: PathBuf,
    daemon: PathBuf,
    home: PathBuf,
    platform: ServicePlatform,
    runner: &'a dyn CommandRunner,
}

impl<'a> ServiceLifecycle<'a> {
    pub fn new(data_dir: &Path, daemon: &Path, runner: &'a dyn CommandRunner) -> io::Result<Self> {
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Install and manage Brr as a regular user, not root",
            ));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        #[cfg(target_os = "macos")]
        let platform = ServicePlatform::Launchd;
        #[cfg(target_os = "linux")]
        let platform = ServicePlatform::Systemd;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Brr service lifecycle supports macOS and Linux",
        ));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Ok(Self {
            data_dir: absolute(data_dir)?,
            daemon: absolute(daemon)?,
            home,
            platform,
            runner,
        })
    }

    #[cfg(test)]
    fn for_test(
        data_dir: &Path,
        daemon: &Path,
        home: &Path,
        platform: ServicePlatform,
        runner: &'a dyn CommandRunner,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            daemon: daemon.into(),
            home: home.into(),
            platform,
            runner,
        }
    }

    pub fn service_label(&self) -> String {
        let standard = match self.platform {
            ServicePlatform::Launchd => self
                .home
                .join("Library/Application Support/dev.brr.desktop"),
            ServicePlatform::Systemd => std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(".local/share"))
                .join("brr"),
        };
        if normalize(&self.data_dir) == normalize(&standard) {
            return match self.platform {
                ServicePlatform::Launchd => "dev.brr.desktop.brrd".into(),
                ServicePlatform::Systemd => "brrd".into(),
            };
        }
        let digest = Sha256::digest(normalize(&self.data_dir).to_string_lossy().as_bytes());
        let suffix = digest
            .iter()
            .take(6)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        match self.platform {
            ServicePlatform::Launchd => format!("dev.brr.desktop.brrd.{suffix}"),
            ServicePlatform::Systemd => format!("brrd-{suffix}"),
        }
    }

    pub fn definition_path(&self) -> PathBuf {
        match self.platform {
            ServicePlatform::Launchd => self
                .home
                .join("Library/LaunchAgents")
                .join(format!("{}.plist", self.service_label())),
            ServicePlatform::Systemd => std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(".config"))
                .join("systemd/user")
                .join(format!("{}.service", self.service_label())),
        }
    }

    pub fn install(&self, start: bool) -> io::Result<()> {
        let path = self.definition_path();
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to replace a symlink service definition",
            ));
        }
        let content = match self.platform {
            ServicePlatform::Launchd => self.launchd_definition(),
            ServicePlatform::Systemd => self.systemd_definition(),
        };
        if self.platform == ServicePlatform::Launchd {
            fs::create_dir_all(self.data_dir.join("logs"))?;
        }
        atomic_write(&path, content.as_bytes())?;
        match self.platform {
            ServicePlatform::Launchd => {
                if start {
                    self.start()
                } else {
                    Ok(())
                }
            }
            ServicePlatform::Systemd => {
                self.checked("systemctl", &["--user", "daemon-reload"])?;
                self.checked(
                    "systemctl",
                    &[
                        "--user",
                        "enable",
                        &format!("{}.service", self.service_label()),
                    ],
                )?;
                if start {
                    self.start()
                } else {
                    Ok(())
                }
            }
        }
    }

    pub fn start(&self) -> io::Result<()> {
        match self.platform {
            ServicePlatform::Systemd => self.checked(
                "systemctl",
                &[
                    "--user",
                    "start",
                    &format!("{}.service", self.service_label()),
                ],
            ),
            ServicePlatform::Launchd => {
                let domain = self.launch_domain()?;
                let target = format!("{domain}/{}", self.service_label());
                let loaded = self
                    .runner
                    .run("launchctl", &["print".into(), target.clone().into()])?;
                if loaded.status == 0 {
                    // kickstart without -k leaves a currently running daemon alone.
                    self.checked("launchctl", &["kickstart", &target])
                } else {
                    // The plist uses RunAtLoad, so bootstrapping starts the agent.
                    self.checked(
                        "launchctl",
                        &[
                            "bootstrap",
                            &domain,
                            &self.definition_path().to_string_lossy(),
                        ],
                    )
                }
            }
        }
    }

    pub fn stop(&self) -> io::Result<()> {
        match self.platform {
            ServicePlatform::Systemd => self.checked(
                "systemctl",
                &[
                    "--user",
                    "stop",
                    &format!("{}.service", self.service_label()),
                ],
            ),
            ServicePlatform::Launchd => {
                let target = format!("{}/{}", self.launch_domain()?, self.service_label());
                let output = self
                    .runner
                    .run("launchctl", &["bootout".into(), target.into()])?;
                if output.status == 0 || output.status == 3 {
                    Ok(())
                } else {
                    Err(command_error("launchctl", &output))
                }
            }
        }
    }

    pub fn uninstall(&self) -> io::Result<()> {
        match self.platform {
            ServicePlatform::Systemd => {
                let unit = format!("{}.service", self.service_label());
                let _ = self.runner.run(
                    "systemctl",
                    &["--user".into(), "stop".into(), unit.clone().into()],
                );
                let _ = self.runner.run(
                    "systemctl",
                    &["--user".into(), "disable".into(), unit.into()],
                );
            }
            ServicePlatform::Launchd => {
                let _ = self.stop();
            }
        }
        let path = self.definition_path();
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to remove a symlink service definition",
            ));
        }
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if self.platform == ServicePlatform::Systemd {
            self.checked("systemctl", &["--user", "daemon-reload"])?;
        }
        Ok(())
    }

    pub fn status(&self) -> io::Result<CommandOutput> {
        match self.platform {
            ServicePlatform::Systemd => self.runner.run(
                "systemctl",
                &[
                    "--user".into(),
                    "status".into(),
                    format!("{}.service", self.service_label()).into(),
                    "--no-pager".into(),
                ],
            ),
            ServicePlatform::Launchd => self.runner.run(
                "launchctl",
                &[
                    "print".into(),
                    format!("{}/{}", self.launch_domain()?, self.service_label()).into(),
                ],
            ),
        }
    }

    pub fn linger_notice(&self) -> Option<&'static str> {
        (self.platform == ServicePlatform::Systemd).then_some("For Brr to keep running after logout, user lingering must be enabled by an administrator. brrctl does not change linger settings.")
    }

    fn checked(&self, program: &str, args: &[&str]) -> io::Result<()> {
        let args = args.iter().map(OsString::from).collect::<Vec<_>>();
        let output = self.runner.run(program, &args)?;
        if output.status == 0 {
            Ok(())
        } else {
            Err(command_error(program, &output))
        }
    }

    fn systemd_definition(&self) -> String {
        format!("[Unit]\nDescription=Brr per-user execution node\n\n[Service]\nType=simple\nExecStart={}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            [self.daemon.clone().into_os_string(), OsString::from("--data-dir"), self.data_dir.clone().into_os_string()]
                .iter().map(|arg| systemd_quote(&arg.to_string_lossy())).collect::<Vec<_>>().join(" "))
    }

    fn launchd_definition(&self) -> String {
        let args = [
            self.daemon.as_os_str().to_string_lossy().into_owned(),
            "--data-dir".into(),
            self.data_dir.to_string_lossy().into_owned(),
        ];
        let arguments = args
            .iter()
            .map(|arg| format!("    <string>{}</string>\n", xml_escape(arg)))
            .collect::<String>();
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{}</string>\n  <key>ProgramArguments</key><array>\n{}  </array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>StandardOutPath</key><string>{}</string>\n  <key>StandardErrorPath</key><string>{}</string>\n</dict>\n</plist>\n", xml_escape(&self.service_label()), arguments, xml_escape(&self.data_dir.join("logs/brrd.out.log").to_string_lossy()), xml_escape(&self.data_dir.join("logs/brrd.err.log").to_string_lossy()))
    }

    #[cfg(target_os = "macos")]
    fn launch_domain(&self) -> io::Result<String> {
        Ok(format!("gui/{}", unsafe { libc::geteuid() }))
    }
    #[cfg(not(target_os = "macos"))]
    fn launch_domain(&self) -> io::Result<String> {
        Ok("gui/501".into())
    }
}

pub struct SystemCommandRunner;
impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[OsString]) -> io::Result<CommandOutput> {
        let output = Command::new(program).args(args).output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn command_error(program: &str, output: &CommandOutput) -> io::Error {
    io::Error::other(format!(
        "{program} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn systemd_quote(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '$' => escaped.push_str("$$"),
            '`' => escaped.push_str("\\x60"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                escaped.push_str(&format!("\\x{:02x}", control as u32))
            }
            other => escaped.push(other),
        }
    }
    format!("\"{escaped}\"")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "service definition has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".brr-service-{}-{nonce}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    use std::io::Write;
    file.write_all(content)?;
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(temp);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, sync::Mutex};

    #[derive(Default)]
    struct FakeRunner(Mutex<Vec<(String, Vec<OsString>)>>);

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[OsString]) -> std::io::Result<CommandOutput> {
            self.0
                .lock()
                .unwrap()
                .push((program.to_owned(), args.to_vec()));
            Ok(CommandOutput {
                status: 0,
                stdout: b"active".to_vec(),
                stderr: vec![],
            })
        }
    }

    #[test]
    fn systemd_install_escapes_paths_and_enables_only_the_user_unit() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home with spaces");
        let data = home.join("data with spaces");
        let daemon = home.join("Applications/Brr Node/brrd");
        let runner = FakeRunner::default();
        let lifecycle =
            ServiceLifecycle::for_test(&data, &daemon, &home, ServicePlatform::Systemd, &runner);

        lifecycle.install(false).unwrap();

        let unit = std::fs::read_to_string(lifecycle.definition_path()).unwrap();
        assert!(unit.contains("ExecStart=\""));
        assert!(unit.contains("Applications/Brr Node/brrd\""));
        assert!(unit.contains("--data-dir\" \""));
        assert!(unit.contains("data with spaces\""));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(
            !unit.contains("After=default.target"),
            "The enabled service must not order itself after the target that waits for it"
        );
        assert!(!unit.contains("sudo"));
        let calls = runner.0.lock().unwrap();
        assert_eq!(
            calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
            ["systemctl", "systemctl"]
        );
        assert!(calls
            .iter()
            .all(|(_, args)| args.first().is_some_and(|arg| arg == "--user")));
    }

    #[test]
    fn alternate_roots_get_distinct_service_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let daemon = home.join("bin/brrd");
        let runner = FakeRunner::default();
        let first = ServiceLifecycle::for_test(
            &home.join("qa-a"),
            &daemon,
            &home,
            ServicePlatform::Systemd,
            &runner,
        );
        let second = ServiceLifecycle::for_test(
            &home.join("qa-b"),
            &daemon,
            &home,
            ServicePlatform::Systemd,
            &runner,
        );
        assert_ne!(first.service_label(), second.service_label());
        assert!(first.service_label().starts_with("brrd-"));
        assert_ne!(first.definition_path(), second.definition_path());
    }

    #[test]
    fn launchd_uses_argument_array_and_xml_escapes_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home & one");
        let data = home.join("Library/Application Support/dev.brr.desktop");
        let daemon = home.join("Apps/Agent <Node>/brrd");
        let runner = FakeRunner::default();
        let lifecycle =
            ServiceLifecycle::for_test(&data, &daemon, &home, ServicePlatform::Launchd, &runner);
        lifecycle.install(false).unwrap();
        let plist = fs::read_to_string(lifecycle.definition_path()).unwrap();
        assert!(plist.contains("<key>ProgramArguments</key><array>"));
        assert!(plist.contains("Agent &lt;Node&gt;"));
        assert!(plist.contains("home &amp; one"));
        assert!(plist.contains("<string>--data-dir</string>"));
        assert!(data.join("logs").is_dir());
        assert_eq!(runner.0.lock().unwrap().len(), 0);
    }

    #[test]
    fn launchd_start_and_stop_are_scoped_to_the_user_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let data = home.join("qa-data");
        let runner = FakeRunner::default();
        let lifecycle = ServiceLifecycle::for_test(
            &data,
            &home.join("bin/brrd"),
            &home,
            ServicePlatform::Launchd,
            &runner,
        );
        lifecycle.install(true).unwrap();
        lifecycle.stop().unwrap();

        let calls = runner.0.lock().unwrap();
        assert!(calls.iter().all(|(program, _)| program == "launchctl"));
        assert!(calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "kickstart")));
        assert!(calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "bootout")));
        assert!(calls.iter().all(|(_, args)| {
            !args.iter().any(|arg| arg == "sh" || arg == "bash")
                && !args
                    .iter()
                    .any(|arg| arg.to_string_lossy().contains("sudo"))
        }));
    }

    #[test]
    fn starting_an_already_bootstrapped_agent_does_not_boot_it_out() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let runner = FakeRunner::default();
        let lifecycle = ServiceLifecycle::for_test(
            &home.join("qa-data"),
            &home.join("bin/brrd"),
            &home,
            ServicePlatform::Launchd,
            &runner,
        );
        lifecycle.start().unwrap();
        let calls = runner.0.lock().unwrap();
        assert_eq!(calls.first().unwrap().1.first().unwrap(), "print");
        assert!(!calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "bootout")));
        assert!(!calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "bootstrap")));
        let kickstart = calls
            .iter()
            .find(|(_, args)| args.first().is_some_and(|arg| arg == "kickstart"))
            .unwrap();
        assert_eq!(kickstart.1.len(), 2);
        assert!(!kickstart.1.iter().any(|arg| arg == "-k"));
    }

    #[test]
    fn starting_an_unloaded_agent_bootstraps_its_user_plist() {
        struct UnloadedRunner(Mutex<Vec<(String, Vec<OsString>)>>);
        impl CommandRunner for UnloadedRunner {
            fn run(&self, program: &str, args: &[OsString]) -> io::Result<CommandOutput> {
                self.0.lock().unwrap().push((program.into(), args.to_vec()));
                Ok(CommandOutput {
                    status: if args.first().is_some_and(|arg| arg == "print") {
                        113
                    } else {
                        0
                    },
                    stdout: vec![],
                    stderr: vec![],
                })
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home with spaces");
        let runner = UnloadedRunner(Mutex::new(Vec::new()));
        let lifecycle = ServiceLifecycle::for_test(
            &home.join("qa data"),
            &home.join("Brr Node/brrd"),
            &home,
            ServicePlatform::Launchd,
            &runner,
        );
        lifecycle.start().unwrap();
        let calls = runner.0.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|call| call.1[0].to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["print", "bootstrap"]
        );
        assert!(calls[1]
            .1
            .iter()
            .any(|arg| arg == &lifecycle.definition_path().as_os_str()));
    }

    #[test]
    fn user_service_actions_never_remove_data_and_print_linger_guidance() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let data = home.join("qa-data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("keep.db"), b"keep").unwrap();
        let runner = FakeRunner::default();
        let lifecycle = ServiceLifecycle::for_test(
            &data,
            &home.join("bin/brrd"),
            &home,
            ServicePlatform::Systemd,
            &runner,
        );

        lifecycle.install(true).unwrap();
        assert!(lifecycle
            .linger_notice()
            .unwrap()
            .contains("does not change linger"));
        lifecycle.start().unwrap();
        assert_eq!(lifecycle.status().unwrap().status, 0);
        lifecycle.stop().unwrap();
        lifecycle.uninstall().unwrap();

        assert_eq!(fs::read(data.join("keep.db")).unwrap(), b"keep");
        assert!(!lifecycle.definition_path().exists());
        let calls = runner.0.lock().unwrap();
        assert!(calls.iter().all(|(program, args)| program == "systemctl"
            && args.first().is_some_and(|arg| arg == "--user")));
        assert!(calls
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "start")));
        assert!(calls
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "disable")));
    }

    #[test]
    fn install_refuses_service_definition_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let runner = FakeRunner::default();
        let lifecycle = ServiceLifecycle::for_test(
            &home.join("qa"),
            &home.join("bin/brrd"),
            &home,
            ServicePlatform::Systemd,
            &runner,
        );
        fs::create_dir_all(lifecycle.definition_path().parent().unwrap()).unwrap();
        let target = tmp.path().join("outside");
        fs::write(&target, b"preserve").unwrap();
        std::os::unix::fs::symlink(&target, lifecycle.definition_path()).unwrap();
        assert_eq!(
            lifecycle.install(false).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read(target).unwrap(), b"preserve");
    }
}
