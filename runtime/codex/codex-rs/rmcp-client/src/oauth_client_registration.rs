use std::sync::Arc;

use anyhow::Result;
use anyhow::bail;
use rmcp::transport::AuthorizationManager;
use rmcp::transport::AuthorizationRequest;
use rmcp::transport::AuthorizationSession;
use rmcp::transport::auth::OAuthHttpClient;
use rmcp::transport::auth::OAuthState;
use url::Url;

use crate::oauth::validate_authorization_server_endpoints;
use crate::oauth_callback::McpOAuthCallbackMode;
use crate::oauth_callback::append_callback_id_to_redirect_uri;
use crate::oauth_callback::callback_mode;
use crate::oauth_callback::validate_callback_redirect;
use crate::oauth_metadata_overrides::OAuthMetadataOverrides;
use crate::oauth_token_http_client::OAuthTokenHttpClient;

/// OAuth client-registration strategy for one interactive HTTP MCP login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpOAuthClientRegistration {
    /// Prefer a supported native CIMD and otherwise use advertised DCR.
    #[default]
    Auto,
    /// Require a ChatGPT-hosted Codex public native Client ID Metadata Document.
    Cimd,
    /// Require the authorization server's Dynamic Client Registration endpoint.
    Dcr,
}

/// OAuth state prepared from one authorization-server metadata resolution.
pub(crate) struct PreparedOAuthLogin {
    pub(crate) oauth_state: OAuthState,
    pub(crate) authorization_server_issuer: Option<String>,
    pub(crate) redirect_uri: String,
}

pub(crate) async fn start_authorization(
    server_url: &str,
    http_client: Arc<dyn OAuthHttpClient>,
    scopes: &[&str],
    redirect_uri: &str,
    callback_id: &str,
    client_registration: McpOAuthClientRegistration,
    client_metadata_url: Option<&str>,
    metadata_overrides: &OAuthMetadataOverrides,
    preserve_empty_scopes: bool,
) -> Result<PreparedOAuthLogin> {
    let resource_url = Url::parse(server_url)?;
    let discovery_base = metadata_overrides
        .metadata_discovery_base(&resource_url)
        .to_string();
    let mut discovery_manager =
        AuthorizationManager::new_with_oauth_http_client(discovery_base, Arc::clone(&http_client))
            .await?;
    discovery_manager.set_allow_missing_issuer(true);
    let discovered_metadata = match discovery_manager.resolve_metadata().await {
        Ok(resolved) => Some(resolved.metadata),
        Err(error) => {
            if metadata_overrides.apply_to_metadata(None).is_err() {
                return Err(error.into());
            }
            None
        }
    };
    let mut metadata = metadata_overrides.apply_to_metadata(discovered_metadata)?;
    validate_authorization_server_endpoints(&metadata)?;
    let authorization_server_issuer = metadata.issuer.clone();
    let callback_mode = callback_mode(&metadata)?;

    let cimd_advertised = metadata
        .additional_fields
        .get("client_id_metadata_document_supported")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let public_client_auth_supported = metadata
        .additional_fields
        .get("token_endpoint_auth_methods_supported")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|methods| methods.iter().any(|method| method.as_str() == Some("none")));

    let uses_shared_callback = callback_mode == McpOAuthCallbackMode::IssuerBound;
    let redirect_uri = if uses_shared_callback {
        redirect_uri.to_string()
    } else {
        append_callback_id_to_redirect_uri(redirect_uri, callback_id)?
    };
    let parsed_redirect_uri = Url::parse(&redirect_uri)?;
    let expected_callback_path = if uses_shared_callback {
        "/callback".to_string()
    } else {
        format!("/callback/{callback_id}")
    };
    let native_redirect_supported = parsed_redirect_uri.scheme() == "http"
        && matches!(
            parsed_redirect_uri.host_str(),
            Some("127.0.0.1" | "localhost")
        )
        && parsed_redirect_uri.port().is_some_and(|port| port > 0)
        && parsed_redirect_uri.path() == expected_callback_path
        && parsed_redirect_uri.query().is_none()
        && parsed_redirect_uri.fragment().is_none()
        && parsed_redirect_uri.username().is_empty()
        && parsed_redirect_uri.password().is_none();
    let client_metadata_url = client_metadata_url
        .map(|value| {
            let parsed = Url::parse(value)?;
            if parsed.scheme() != "https"
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                bail!("MCP OAuth client metadata URL must be HTTPS without credentials, query, or fragment");
            }
            Ok::<_, anyhow::Error>(parsed.to_string())
        })
        .transpose()?;
    validate_callback_redirect(&redirect_uri, callback_id, callback_mode)?;
    // MCP 2026-07-28 priority: pre-registered clients never reach this path; offer
    // advertised CIMD here and otherwise let rmcp fall back to DCR.
    // https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/client-registration
    let offer_cimd = match client_registration {
        McpOAuthClientRegistration::Auto => {
            client_metadata_url.is_some()
                && cimd_advertised
                && native_redirect_supported
                && public_client_auth_supported
        }
        McpOAuthClientRegistration::Cimd => {
            if !cimd_advertised || !public_client_auth_supported {
                bail!(
                    "MCP authorization server does not advertise CIMD with token endpoint auth method `none`"
                );
            }
            if !native_redirect_supported {
                bail!(
                    "MCP OAuth CIMD requires an ephemeral loopback callback at `{expected_callback_path}`"
                );
            }
            if client_metadata_url.is_none() {
                bail!("MCP OAuth CIMD requires a host-owned client metadata document URL");
            }
            true
        }
        McpOAuthClientRegistration::Dcr => false,
    };

    let resource_url = Url::parse(server_url)?;
    let auth_http_client =
        if let Some(resource) = metadata_overrides.effective_resource(&resource_url) {
            let token_endpoint = Url::parse(&metadata.token_endpoint)?;
            Arc::new(OAuthTokenHttpClient::for_discovered_client(
                http_client,
                token_endpoint,
                Some(resource.as_str().to_string()),
            )?) as Arc<dyn OAuthHttpClient>
        } else {
            http_client
        };
    if preserve_empty_scopes && scopes.is_empty() {
        metadata.scopes_supported = None;
    }
    let mut auth_manager = AuthorizationManager::new_with_oauth_http_client(
        metadata_overrides
            .metadata_discovery_base(&resource_url)
            .to_string(),
        auth_http_client,
    )
    .await?;
    auth_manager.set_allow_missing_issuer(true);
    auth_manager.set_metadata(metadata);
    let mut request = AuthorizationRequest::new(redirect_uri.clone())
        .with_scopes(scopes.iter().copied())
        .with_client_name("Codex");
    if offer_cimd {
        // CIMD is an active IETF Internet-Draft: this HTTPS client identifier resolves
        // to its self-referential JSON metadata document.
        // https://datatracker.ietf.org/doc/draft-ietf-oauth-client-id-metadata-document/
        request = request.with_client_metadata_url(
            client_metadata_url.expect("CIMD offer requires host-owned metadata URL"),
        );
    }
    let session = AuthorizationSession::new(auth_manager, request)
        .await
        .map_err(|(_auth_manager, error)| error)?;

    Ok(PreparedOAuthLogin {
        oauth_state: OAuthState::Session(session),
        authorization_server_issuer,
        redirect_uri,
    })
}

#[cfg(test)]
#[path = "oauth_client_registration_tests.rs"]
mod tests;
