#[cfg(unix)]
use {
    brr_node::lifecycle::{ServiceLifecycle, SystemCommandRunner},
    brr_node::{local::LocalClient, paths::default_data_dir},
    brr_remote_protocol::{Channel, Request},
    clap::{Parser, Subcommand},
    serde_json::{json, Value},
    std::path::PathBuf,
};

#[cfg(unix)]
#[derive(Parser)]
#[command(
    name = "brrctl",
    version,
    about = "Manage a Brr execution node and fleet"
)]
struct Arguments {
    #[arg(long, global = true, default_value_os_t = default_data_dir())]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Control,
}

#[cfg(unix)]
#[derive(Subcommand)]
enum Control {
    Status,
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Create a single-use phone pairing invitation through same-user IPC.
    Pair {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long, default_value_t = 300)]
        expires_in_seconds: u64,
    },
    Fleet {
        #[command(subcommand)]
        command: FleetCommand,
    },
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    Request {
        channel: String,
        method: String,
        #[arg(long, default_value = "{}")]
        params: String,
        #[arg(long)]
        target_node_id: Option<String>,
        #[arg(long)]
        expected_epoch: Option<String>,
        #[arg(long)]
        expected_revision: Option<String>,
        #[arg(long)]
        operation_id: Option<String>,
    },
}

#[cfg(unix)]
#[derive(Subcommand)]
enum FleetCommand {
    /// Mark this node as the local fleet primary.
    Init,
    /// Create a single-use invitation for a fleet node. Save the JSON for the joining node.
    Invite {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long, default_value_t = 300)]
        expires_in_seconds: u64,
    },
    /// Join a fleet using invitation JSON read from stdin or a file.
    Join {
        #[arg(long)]
        invitation_file: Option<PathBuf>,
    },
    /// Synchronize this node's fleet records with its primary.
    Sync,
    /// Show fleet role and registered nodes.
    Status,
}

#[cfg(unix)]
#[derive(Subcommand)]
enum AccountCommand {
    /// Start the managed ChatGPT device-code login flow.
    Login,
    /// Show managed ChatGPT login status.
    Status,
}

#[cfg(unix)]
#[derive(Subcommand)]
enum ServiceCommand {
    /// Install this user's service definition. Add --start to launch it now.
    Install {
        #[arg(long)]
        start: bool,
    },
    /// Start this user's installed service.
    Start,
    /// Stop this user's service without uninstalling it.
    Stop,
    /// Stop and remove only this user's service definition.
    Uninstall,
    /// Show this user's service manager status.
    Status,
}

#[cfg(unix)]
fn request(channel: Channel, method: &str, params: Value) -> Request {
    let mut request = Request {
        id: uuid::Uuid::new_v4().to_string(),
        channel,
        method: method.into(),
        params,
        operation_id: None,
        expected_epoch: None,
        expected_revision: None,
        target_node_id: None,
    };
    if request.policy().is_ok_and(|policy| policy.mutating) {
        request.operation_id = Some(uuid::Uuid::new_v4().to_string());
    }
    request
}

#[cfg(unix)]
fn fleet_invitation_request(
    name: Option<String>,
    endpoint: Option<String>,
    expires_in_seconds: u64,
    role: &str,
) -> Request {
    request(
        Channel::Fleet,
        "invitation.create",
        json!({"name":name,"endpoint":endpoint,"expiresInSeconds":expires_in_seconds,"role":role}),
    )
}

#[cfg(unix)]
fn fleet_join_request(invitation: Value) -> Request {
    request(Channel::Fleet, "join", json!({"invitation":invitation}))
}

#[cfg(unix)]
fn parse_invitation(input: &str) -> Result<Value, String> {
    let invitation: Value = serde_json::from_str(input).map_err(|error| error.to_string())?;
    if !invitation.is_object() {
        return Err("Invitation must be a JSON object".into());
    }
    for field in [
        "endpoint",
        "nodeId",
        "certificatePem",
        "fingerprint",
        "code",
    ] {
        if invitation[field].as_str().is_none_or(str::is_empty) {
            return Err(format!("Invitation is missing {field}"));
        }
    }
    Ok(invitation)
}

#[cfg(unix)]
fn parse_invitation_reader(
    mut reader: impl std::io::Read,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut input = String::new();
    reader.read_to_string(&mut input)?;
    Ok(parse_invitation(&input)?)
}

#[cfg(unix)]
fn read_invitation(path: Option<&std::path::Path>) -> Result<Value, Box<dyn std::error::Error>> {
    match path {
        Some(path) if path != std::path::Path::new("-") => {
            parse_invitation_reader(std::fs::File::open(path)?)
        }
        _ => parse_invitation_reader(std::io::stdin().lock()),
    }
}

#[cfg(unix)]
async fn stop_unmanaged_service(
    stop_manager_unit: impl FnOnce() -> std::io::Result<()>,
    local: LocalClient,
) -> Result<(), Box<dyn std::error::Error>> {
    // Stop the manager first so KeepAlive/Restart policies cannot relaunch brrd.
    let manager_stop = stop_manager_unit();
    let local_stop = local.stop().await;
    match (manager_stop, local_stop) {
        (_, Ok(())) | (Ok(()), Err(_)) => Ok(()),
        (Err(manager_error), Err(_)) => Err(manager_error.into()),
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let request = match arguments.command {
        Control::Service { command } => {
            let runner = SystemCommandRunner;
            let daemon = std::env::current_exe()?.with_file_name("brrd");
            let lifecycle = ServiceLifecycle::new(&arguments.data_dir, &daemon, &runner)?;
            match command {
                ServiceCommand::Install { start } => {
                    lifecycle.install(start)?;
                    println!("Installed {}", lifecycle.definition_path().display());
                    if let Some(notice) = lifecycle.linger_notice() {
                        eprintln!("{notice}");
                    }
                }
                ServiceCommand::Start => lifecycle.start()?,
                ServiceCommand::Stop => {
                    stop_unmanaged_service(
                        || lifecycle.stop(),
                        LocalClient::new(arguments.data_dir.join("node/brrd.sock")),
                    )
                    .await?;
                }
                ServiceCommand::Uninstall => lifecycle.uninstall()?,
                ServiceCommand::Status => {
                    let output = lifecycle.status()?;
                    print!("{}", String::from_utf8_lossy(&output.stdout));
                    if !output.stderr.is_empty() {
                        eprint!("{}", String::from_utf8_lossy(&output.stderr));
                    }
                    if output.status != 0 {
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                        let _ = std::io::stderr().flush();
                        std::process::exit(output.status);
                    }
                }
            }
            return Ok(());
        }
        Control::Pair {
            name,
            endpoint,
            expires_in_seconds,
        } => fleet_invitation_request(name, endpoint, expires_in_seconds, "mobile"),
        Control::Fleet { command } => match command {
            FleetCommand::Init => request(Channel::Fleet, "init", json!({})),
            FleetCommand::Invite {
                name,
                endpoint,
                expires_in_seconds,
            } => fleet_invitation_request(name, endpoint, expires_in_seconds, "node"),
            FleetCommand::Join { invitation_file } => {
                let invitation = read_invitation(invitation_file.as_deref())?;
                fleet_join_request(invitation)
            }
            FleetCommand::Sync => request(Channel::Sync, "synchronize", json!({})),
            FleetCommand::Status => request(Channel::Fleet, "status", json!({})),
        },
        Control::Account { command } => match command {
            AccountCommand::Login => request(Channel::System, "login.start", json!({})),
            AccountCommand::Status => request(Channel::System, "login.status", json!({})),
        },
        Control::Status => request(Channel::System, "status", json!({})),
        Control::Request {
            channel,
            method,
            params,
            target_node_id,
            expected_epoch,
            expected_revision,
            operation_id,
        } => Request {
            id: uuid::Uuid::new_v4().to_string(),
            channel: serde_json::from_value(Value::String(channel))?,
            method,
            params: serde_json::from_str(&params)?,
            target_node_id,
            expected_epoch,
            expected_revision,
            operation_id: Some(operation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string())),
        },
    };
    let reply = LocalClient::new(arguments.data_dir.join("node/brrd.sock"))
        .request(request)
        .await?;
    if let Some(error) = reply.error {
        return Err(error.into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&reply.result.unwrap_or(Value::Null))?
    );
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("Brr fleet services currently support macOS and Linux");
    std::process::exit(1);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use brr_node::{events::EventHub, instance::InstanceLock, local::LocalServer};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn fleet_and_local_account_commands_have_first_class_cli_routes() {
        for args in [
            vec!["brrctl", "fleet", "init"],
            vec!["brrctl", "fleet", "invite"],
            vec!["brrctl", "fleet", "join"],
            vec![
                "brrctl",
                "fleet",
                "join",
                "--invitation-file",
                "invite.json",
            ],
            vec!["brrctl", "fleet", "sync"],
            vec!["brrctl", "fleet", "status"],
            vec!["brrctl", "account", "login"],
            vec!["brrctl", "account", "status"],
        ] {
            assert!(Arguments::try_parse_from(args).is_ok());
        }
        assert!(Arguments::try_parse_from([
            "brrctl",
            "fleet",
            "join",
            "--invitation",
            "{\"code\":\"secret\"}"
        ])
        .is_err());
    }

    #[test]
    fn fleet_account_and_sync_commands_dispatch_to_scoped_request_contracts() {
        let init = request(Channel::Fleet, "init", json!({}));
        let invite = fleet_invitation_request(Some("Worker".into()), None, 180, "node");
        let sync = request(Channel::Sync, "synchronize", json!({}));
        let login = request(Channel::System, "login.start", json!({}));
        let login_status = request(Channel::System, "login.status", json!({}));
        let status = request(Channel::Fleet, "status", json!({}));
        let invitation = json!({"endpoint":"https://primary.example","nodeId":"primary","certificatePem":"pem","fingerprint":"fingerprint","code":"single-use"});
        let join = fleet_join_request(invitation.clone());
        assert_eq!(
            (init.channel, init.method.as_str()),
            (Channel::Fleet, "init")
        );
        assert_eq!(invite.params["role"], "node");
        assert_eq!(invite.params["name"], "Worker");
        assert_eq!(invite.params["expiresInSeconds"], 180);
        assert_eq!(sync.method, "synchronize");
        assert!(sync.operation_id.is_some());
        assert_eq!(login.method, "login.start");
        assert!(login.operation_id.is_some());
        assert_eq!(login_status.method, "login.status");
        assert!(login_status.operation_id.is_none());
        assert_eq!(
            (status.channel, status.method.as_str()),
            (Channel::Fleet, "status")
        );
        assert_eq!(
            (join.channel, join.method.as_str()),
            (Channel::Fleet, "join")
        );
        assert_eq!(join.params["invitation"], invitation);
        assert!(join.operation_id.is_some());
        for request in [init, invite, join, sync, login, login_status, status] {
            request
                .validate(brr_remote_protocol::ClientRole::Desktop)
                .unwrap();
        }
    }

    #[test]
    fn invitation_reader_accepts_file_json_and_rejects_incomplete_invites() {
        let invitation = json!({
            "endpoint":"https://node.example:9443",
            "nodeId":"node-primary",
            "certificatePem":"-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----",
            "fingerprint":"a".repeat(64),
            "code":"single-use-code"
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invitation.json");
        let encoded = serde_json::to_vec(&invitation).unwrap();
        std::fs::write(&path, &encoded).unwrap();
        assert_eq!(read_invitation(Some(&path)).unwrap(), invitation);
        assert_eq!(
            parse_invitation_reader(std::io::Cursor::new(encoded)).unwrap(),
            invitation
        );
        assert!(parse_invitation("{\"code\":\"secret\"}")
            .unwrap_err()
            .contains("endpoint"));
        assert!(parse_invitation("[]").unwrap_err().contains("JSON object"));
    }

    #[tokio::test]
    async fn service_stop_falls_back_to_the_same_user_socket_when_no_manager_unit_owns_it() {
        let data = tempfile::tempdir().unwrap();
        let store = data.path().join("store.sqlite");
        std::fs::write(&store, b"preserve data").unwrap();
        let lock = InstanceLock::acquire(data.path()).unwrap();
        let server = LocalServer::bind(&lock).unwrap();
        let socket = server.path().to_owned();
        let (_stop, shutdown) = tokio::sync::watch::channel(false);
        let dispatch: brr_node::local::Dispatch = Arc::new(|request| {
            Box::pin(async move {
                brr_remote_protocol::Reply::from_result(request.id, Ok(json!({"ok":true})))
            })
        });
        let daemon = tokio::spawn(server.run(
            dispatch,
            EventHub::new("qa-node".into(), "qa-epoch".into(), 10),
            shutdown,
        ));
        let manager_called = Arc::new(AtomicBool::new(false));
        let manager_was_missing = manager_called.clone();

        stop_unmanaged_service(
            move || {
                manager_was_missing.store(true, Ordering::SeqCst);
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no manager unit owns this daemon",
                ))
            },
            LocalClient::new(socket),
        )
        .await
        .unwrap();

        assert!(manager_called.load(Ordering::SeqCst));
        tokio::time::timeout(std::time::Duration::from_secs(2), daemon)
            .await
            .expect("unmanaged local daemon should exit")
            .unwrap();
        assert_eq!(std::fs::read(store).unwrap(), b"preserve data");
    }
}
