#[cfg(unix)]
use {
    brr_node::{
        local::{Dispatch, LocalServer},
        paths::{default_data_dir, owned_browser_path, owned_runtime_path},
        service::{Service, ServiceConfig},
        settings::Settings,
    },
    brr_remote_protocol::ClientRole,
    clap::Parser,
    std::{path::PathBuf, sync::Arc},
    tokio::sync::watch,
};

#[cfg(unix)]
#[derive(Parser)]
#[command(name = "brrd", version, about = "Brr per-user execution service")]
struct Arguments {
    #[arg(long, default_value_os_t = default_data_dir())]
    data_dir: PathBuf,
    #[arg(long)]
    name: Option<String>,
    /// Bind only to your LAN/VPN interface when enabling remote access.
    #[arg(long)]
    listen: Option<std::net::SocketAddr>,
    #[arg(long)]
    advertise_host: Vec<String>,
    #[arg(long)]
    endpoint: Option<String>,
    #[cfg(debug_assertions)]
    #[arg(long, hide = true)]
    runtime_path: Option<PathBuf>,
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if unsafe { libc::geteuid() } == 0 {
        return Err("Run Brr as a regular user, not root".into());
    }
    unsafe {
        libc::umask(0o077);
    }
    let arguments = Arguments::parse();
    let node_dir = arguments.data_dir.join("node");
    let mut settings = Settings::load(&node_dir)?;
    if let Some(listen) = arguments.listen {
        settings.listen = listen;
    }
    if let Some(name) = arguments.name {
        settings.name = name;
    }
    if !arguments.advertise_host.is_empty() {
        settings.endpoint_hosts = arguments.advertise_host;
    }
    if let Some(endpoint) = arguments.endpoint {
        settings.public_endpoint = Some(endpoint);
    }
    settings.validate()?;
    let runtime_path = owned_runtime_path();
    #[cfg(debug_assertions)]
    let runtime_path = arguments.runtime_path.unwrap_or(runtime_path);
    let service = Service::open(ServiceConfig {
        data_dir: arguments.data_dir,
        runtime_path,
        name: settings.name.clone(),
        endpoint_hosts: settings.endpoint_hosts.clone(),
    })
    .await?;
    let browser = owned_browser_path();
    if browser.is_file() {
        service.configure_browser(browser).await;
    }
    settings.save(&node_dir)?;
    let listener = tokio::net::TcpListener::bind(settings.listen).await?;
    service
        .set_endpoint(settings.endpoint(listener.local_addr()?))
        .await?;
    let local = LocalServer::bind(service.instance())?;
    let dispatch: Dispatch = {
        let service = service.clone();
        Arc::new(move |request| {
            let service = service.clone();
            Box::pin(async move {
                service
                    .handle(request, ClientRole::Desktop, "local-owner")
                    .await
            })
        })
    };
    let (stop, shutdown) = watch::channel(false);
    let mut local_task = tokio::spawn(local.run(dispatch, service.events(), shutdown.clone()));
    let peer_task = tokio::spawn(service.clone().run_peer_monitor(shutdown.clone()));
    let sync_task = tokio::spawn(service.clone().run_catalog_sync(shutdown.clone()));
    let history_task = tokio::spawn(service.clone().run_history_projection(shutdown.clone()));
    let mut network_task = tokio::spawn(brr_node::network::serve(
        listener,
        service.network_api(),
        shutdown,
    ));
    let startup = {
        let service = service.clone();
        tokio::spawn(async move {
            let _ = service.start_runtime().await;
        })
    };
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = termination.recv() => {},
        result = &mut local_task => { result?; },
        result = &mut network_task => { result??; },
    }
    let _ = stop.send(true);
    startup.abort();
    let _ = startup.await;
    history_task.abort();
    let _ = history_task.await;
    sync_task.abort();
    let _ = sync_task.await;
    peer_task.await?;
    service.shutdown().await;
    if !local_task.is_finished() {
        local_task.await?;
    }
    if !network_task.is_finished() {
        network_task.await??;
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("Brr fleet services currently support macOS and Linux");
    std::process::exit(1);
}
