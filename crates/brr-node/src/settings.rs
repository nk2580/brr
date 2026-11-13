//! Non-secret, per-user listener configuration. Credentials live in auth.sqlite.
use crate::instance::private_directory;
use brr_remote_protocol::ApiError;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub listen: SocketAddr,
    pub endpoint_hosts: Vec<String>,
    pub public_endpoint: Option<String>,
    pub name: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8787".parse().unwrap(),
            endpoint_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            public_endpoint: None,
            name: std::env::var("HOSTNAME").unwrap_or_else(|_| "Brr node".into()),
        }
    }
}
impl Settings {
    pub fn load(directory: &Path) -> Result<Self, ApiError> {
        let path = directory.join("service.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(io)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ApiError::new(
                "unsafe_settings",
                "Service settings must be a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(ApiError::new(
                    "unsafe_settings",
                    "Service settings must belong to the current user",
                ));
            }
        }
        let value: Self = serde_json::from_slice(&std::fs::read(path).map_err(io)?).map_err(io)?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.endpoint_hosts.is_empty() || self.name.trim().is_empty() {
            return Err(ApiError::new(
                "invalid_settings",
                "Name and endpoint hosts are required",
            ));
        }
        if let Some(endpoint) = &self.public_endpoint {
            let url = endpoint_url(endpoint)?;
            if !self
                .endpoint_hosts
                .iter()
                .any(|host| url.host_str() == Some(host.trim_matches(['[', ']'])))
            {
                return Err(ApiError::new(
                    "invalid_endpoint",
                    "The advertised host must be listed in endpointHosts for TLS verification",
                ));
            }
        }
        Ok(())
    }
    pub fn save(&self, directory: &Path) -> Result<(), ApiError> {
        self.validate()?;
        private_directory(directory)?;
        let temporary = directory.join(format!(".service-{}.json", uuid::Uuid::new_v4()));
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary).map_err(io)?;
        file.write_all(&serde_json::to_vec_pretty(self).map_err(io)?)
            .map_err(io)?;
        file.sync_all().map_err(io)?;
        std::fs::rename(&temporary, directory.join("service.json")).map_err(io)
    }
    pub fn endpoint(&self, bound: SocketAddr) -> String {
        self.public_endpoint
            .clone()
            .unwrap_or_else(|| format!("https://localhost:{}", bound.port()))
    }
}
pub fn endpoint_url(endpoint: &str) -> Result<url::Url, ApiError> {
    let url = url::Url::parse(endpoint).map_err(io)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::new(
            "invalid_endpoint",
            "Use a root HTTPS endpoint without credentials or query parameters",
        ));
    }
    Ok(url)
}
fn io(error: impl std::fmt::Display) -> ApiError {
    ApiError::new("settings_io", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_preserve_explicit_lan_endpoint_and_reject_mismatched_tls_host() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.endpoint_hosts.push("machine.local".into());
        settings.public_endpoint = Some("https://machine.local:8787".into());
        settings.save(directory.path()).unwrap();
        assert_eq!(
            Settings::load(directory.path())
                .unwrap()
                .endpoint("0.0.0.0:8787".parse().unwrap()),
            "https://machine.local:8787"
        );
        settings.public_endpoint = Some("https://another.local:8787".into());
        assert!(settings.validate().is_err());
    }
}
