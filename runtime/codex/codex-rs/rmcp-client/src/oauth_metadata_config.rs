use anyhow::Result;
use codex_config::McpServerOAuthConfig;

use crate::oauth_metadata_overrides::OAuthMetadataOverrides;

pub(crate) fn from_config(
    config: Option<&McpServerOAuthConfig>,
    resource: Option<&str>,
) -> Result<OAuthMetadataOverrides> {
    OAuthMetadataOverrides::from_raw(
        config.and_then(|config| config.authorization_server_base.as_deref()),
        config.and_then(|config| config.authorization_url.as_deref()),
        config.and_then(|config| config.token_url.as_deref()),
        config.and_then(|config| config.registration_url.as_deref()),
        resource,
        config.and_then(|config| config.default_scopes.clone()),
        config.and_then(|config| config.oidc_enabled),
        config.and_then(|config| config.oidc_configuration_url.as_deref()),
        config.and_then(|config| config.oidc_userinfo_endpoint.as_deref()),
        config.and_then(|config| config.oidc_scopes_supported.clone()),
    )
}
