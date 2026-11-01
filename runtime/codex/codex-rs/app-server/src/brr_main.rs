#![recursion_limit = "256"]

//! Brr's owned desktop agent runtime, forked from the Codex CLI app-server.
//! This executable speaks stdio only and owns the ChatGPT login policy.
use clap::Parser;
use codex_arg0::arg0_dispatch_or_else;
use codex_config::LoaderOverrides;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Parser)]
#[command(name = "brr-runtime", version = "0.1.0", about = "Brr desktop agent runtime")]
struct BrrArgs {
    #[command(flatten)]
    config: CliConfigOverrides,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|paths| async move {
        let mut args = BrrArgs::parse();
        brr_runtime_policy::enforce_startup_overrides(&mut args.config.raw_overrides);
        codex_app_server::run_main(
            paths,
            args.config,
            LoaderOverrides::default(),
            /* strict_config */ false,
            /* default_analytics_enabled */ false,
        )
        .await?;
        Ok(())
    })
}
