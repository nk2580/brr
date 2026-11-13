#![recursion_limit = "256"]

//! Brr's owned desktop agent runtime, forked from the Codex CLI app-server.
//! This executable speaks stdio only and owns the ChatGPT login policy.
use clap::Parser;
use codex_arg0::arg0_dispatch_or_else;
use codex_config::LoaderOverrides;
use codex_utils_cli::CliConfigOverrides;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "brr-runtime",
    version = "0.1.0",
    about = "Brr desktop agent runtime"
)]
struct BrrArgs {
    #[command(flatten)]
    config: CliConfigOverrides,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|paths| async move {
        #[cfg(target_os = "linux")]
        let paths = {
            let mut paths = paths;
            if let Some(executable) = paths.codex_self_exe.as_deref() {
                if let Some(alias) = packaged_linux_sandbox_alias(executable)? {
                    paths.codex_linux_sandbox_exe = Some(alias);
                }
            }
            paths
        };

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

/// Finds the package-owned sibling alias used by older bubblewrap versions that
/// cannot set argv[0]. The link must resolve to this exact runtime executable.
#[cfg(target_os = "linux")]
fn packaged_linux_sandbox_alias(executable: &Path) -> anyhow::Result<Option<PathBuf>> {
    let alias = executable.with_file_name("codex-linux-sandbox");
    let metadata = match std::fs::symlink_metadata(&alias) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    anyhow::ensure!(
        metadata.file_type().is_symlink(),
        "packaged codex-linux-sandbox must be a symlink beside brr-runtime"
    );
    anyhow::ensure!(
        alias.canonicalize()? == executable.canonicalize()?,
        "packaged codex-linux-sandbox must target this brr-runtime executable"
    );
    Ok(Some(alias))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::packaged_linux_sandbox_alias;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn uses_only_a_sibling_symlink_to_this_runtime() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let runtime = root.path().join("brr-runtime");
        let alias = root.path().join("codex-linux-sandbox");
        let other = root.path().join("other-runtime");
        fs::write(&runtime, b"runtime")?;
        fs::write(&other, b"other")?;

        assert_eq!(packaged_linux_sandbox_alias(&runtime)?, None);

        symlink(&runtime, &alias)?;
        assert_eq!(packaged_linux_sandbox_alias(&runtime)?, Some(alias.clone()));

        fs::remove_file(&alias)?;
        symlink(&other, &alias)?;
        assert!(packaged_linux_sandbox_alias(&runtime).is_err());

        fs::remove_file(&alias)?;
        fs::write(&alias, b"not a symlink")?;
        assert!(packaged_linux_sandbox_alias(&runtime).is_err());
        Ok(())
    }
}
