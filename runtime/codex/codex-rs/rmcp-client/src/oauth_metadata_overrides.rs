use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use http::Method;
use rmcp::transport::auth::{
    AuthorizationMetadata, OAuthHttpClient, OAuthHttpClientFuture, OAuthHttpRedirectPolicy,
    OAuthHttpRequest,
};
use url::Url;

/// Validated endpoint and scope overrides supplied by a portable OAuth declaration.
///
/// The issuer is deliberately not an override: a discovered issuer remains bound to
/// the server that published it, and explicit endpoints do not invent issuer claims.
pub(crate) struct OAuthMetadataOverrides {
    authorization_server_base: Option<Url>,
    authorization_url: Option<Url>,
    token_url: Option<Url>,
    registration_url: Option<Url>,
    resource: Option<Url>,
    default_scopes: Option<Vec<String>>,
    oidc_enabled: Option<bool>,
    oidc_configuration_url: Option<Url>,
    oidc_userinfo_endpoint: Option<Url>,
    oidc_scopes_supported: Option<Vec<String>>,
}

impl OAuthMetadataOverrides {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_raw(
        authorization_server_base: Option<&str>,
        authorization_url: Option<&str>,
        token_url: Option<&str>,
        registration_url: Option<&str>,
        resource: Option<&str>,
        default_scopes: Option<Vec<String>>,
        oidc_enabled: Option<bool>,
        oidc_configuration_url: Option<&str>,
        oidc_userinfo_endpoint: Option<&str>,
        oidc_scopes_supported: Option<Vec<String>>,
    ) -> Result<Self> {
        Ok(Self {
            authorization_server_base: optional_endpoint(
                authorization_server_base,
                "authorization server base",
            )?,
            authorization_url: optional_endpoint(authorization_url, "authorization URL")?,
            token_url: optional_endpoint(token_url, "token URL")?,
            registration_url: optional_endpoint(registration_url, "registration URL")?,
            resource: optional_endpoint(resource, "resource")?,
            default_scopes,
            oidc_enabled,
            oidc_configuration_url: optional_endpoint(
                oidc_configuration_url,
                "OIDC configuration URL",
            )?,
            oidc_userinfo_endpoint: optional_endpoint(
                oidc_userinfo_endpoint,
                "OIDC userinfo endpoint",
            )?,
            oidc_scopes_supported,
        })
    }

    /// Applies endpoint overrides without changing the issuer or other discovered data.
    /// Without discovery, callers must explicitly supply both authorization and token URLs.
    pub(crate) fn apply_to_metadata(
        &self,
        metadata: Option<AuthorizationMetadata>,
    ) -> Result<AuthorizationMetadata> {
        ensure!(
            metadata.is_some() || (self.authorization_url.is_some() && self.token_url.is_some()),
            "OAuth discovery is unavailable and both authorizationUrl and tokenUrl are required"
        );
        let mut metadata = metadata.unwrap_or_default();
        if let Some(url) = &self.authorization_url {
            metadata.authorization_endpoint = url.to_string();
        }
        if let Some(url) = &self.token_url {
            metadata.token_endpoint = url.to_string();
        }
        if let Some(url) = &self.registration_url {
            metadata.registration_endpoint = Some(url.to_string());
        }
        if let Some(scopes) = &self.oidc_scopes_supported {
            metadata.scopes_supported = Some(scopes.clone());
        }
        if let Some(userinfo_endpoint) = &self.oidc_userinfo_endpoint {
            metadata.additional_fields.insert(
                "userinfo_endpoint".to_string(),
                serde_json::Value::String(userinfo_endpoint.to_string()),
            );
        }
        validate_endpoint(&metadata.authorization_endpoint, "authorization endpoint")?;
        validate_endpoint(&metadata.token_endpoint, "token endpoint")?;
        if let Some(url) = &metadata.registration_endpoint {
            validate_endpoint(url, "registration endpoint")?;
        }
        Ok(metadata)
    }

    pub(crate) fn can_apply_without_discovery(&self) -> bool {
        self.authorization_url.is_some() && self.token_url.is_some()
    }

    pub(crate) fn authorization_server_base(&self) -> Option<&Url> {
        self.authorization_server_base.as_ref()
    }

    /// Selects the base URL to give the SDK's metadata resolver. A configured
    /// authorization-server base intentionally bypasses discovery at the MCP URL.
    pub(crate) fn metadata_discovery_base(&self, resource_server_url: &Url) -> Url {
        self.authorization_server_base
            .clone()
            .unwrap_or_else(|| resource_server_url.clone())
    }

    pub(crate) fn resource(&self) -> Option<&Url> {
        self.resource.as_ref()
    }

    /// Uses the explicit resource override, or pins the MCP resource when a
    /// separate authorization-server base would otherwise become the resource.
    pub(crate) fn effective_resource(&self, resource_server_url: &Url) -> Option<Url> {
        self.resource.clone().or_else(|| {
            self.authorization_server_base
                .as_ref()
                .map(|_| resource_server_url.clone())
        })
    }

    pub(crate) fn default_scopes(&self) -> Option<&[String]> {
        self.default_scopes.as_deref()
    }

    pub(crate) fn oidc_enabled(&self) -> Option<bool> {
        self.oidc_enabled
    }

    pub(crate) fn oidc_discovery_enabled(&self) -> bool {
        self.oidc_enabled.unwrap_or(true)
    }

    pub(crate) fn oidc_configuration_url(&self) -> Option<&Url> {
        self.oidc_configuration_url.as_ref()
    }

    pub(crate) fn oidc_userinfo_endpoint(&self) -> Option<&Url> {
        self.oidc_userinfo_endpoint.as_ref()
    }

    pub(crate) fn oidc_scopes_supported(&self) -> Option<&[String]> {
        self.oidc_scopes_supported.as_deref()
    }

    /// Redirects SDK OpenID discovery GETs through the configured URL while retaining
    /// the caller-provided HTTP client (and therefore its network policy).
    pub(crate) fn wrap_http_client(
        &self,
        inner: Arc<dyn OAuthHttpClient>,
    ) -> Arc<dyn OAuthHttpClient> {
        if !self.oidc_discovery_enabled() || self.oidc_configuration_url.is_some() {
            return Arc::new(OidcDiscoveryHttpClient {
                inner,
                enabled: self.oidc_discovery_enabled(),
                configuration_url: self.oidc_configuration_url.clone(),
            });
        }
        inner
    }
}

struct OidcDiscoveryHttpClient {
    inner: Arc<dyn OAuthHttpClient>,
    enabled: bool,
    configuration_url: Option<Url>,
}

impl OAuthHttpClient for OidcDiscoveryHttpClient {
    fn execute(&self, mut request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        let route = route_oidc_discovery_request(
            &mut request.request,
            self.configuration_url.as_ref(),
            self.enabled,
        );
        match route {
            OidcDiscoveryRoute::Suppress => {
                return Box::pin(async {
                    Ok(http::Response::builder()
                        .status(http::StatusCode::NOT_FOUND)
                        .body(Vec::new())
                        .expect("valid synthetic discovery response"))
                });
            }
            OidcDiscoveryRoute::Remapped => {
                request.redirect_policy = OAuthHttpRedirectPolicy::Stop;
            }
            OidcDiscoveryRoute::PassThrough => {}
        }
        let remapped = route == OidcDiscoveryRoute::Remapped;
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let response = inner.execute(request).await?;
            if remapped && response.status().is_redirection() {
                return Err(Box::new(std::io::Error::other(
                    "redirects from configured OIDC discovery URLs are not followed",
                )) as _);
            }
            Ok(response)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OidcDiscoveryRoute {
    PassThrough,
    Remapped,
    Suppress,
}

fn route_oidc_discovery_request(
    request: &mut http::Request<Vec<u8>>,
    configuration_url: Option<&Url>,
    enabled: bool,
) -> OidcDiscoveryRoute {
    if !is_oidc_discovery_request(request) {
        return OidcDiscoveryRoute::PassThrough;
    }
    if !enabled {
        return OidcDiscoveryRoute::Suppress;
    }
    let Some(configuration_url) = configuration_url else {
        return OidcDiscoveryRoute::PassThrough;
    };
    let Ok(uri) = configuration_url.as_str().parse() else {
        return OidcDiscoveryRoute::PassThrough;
    };
    for header in [
        http::header::AUTHORIZATION,
        http::header::COOKIE,
        http::header::PROXY_AUTHORIZATION,
        http::header::HOST,
    ] {
        request.headers_mut().remove(header);
    }
    *request.uri_mut() = uri;
    OidcDiscoveryRoute::Remapped
}

fn is_oidc_discovery_request(request: &http::Request<Vec<u8>>) -> bool {
    if request.method() != Method::GET {
        return false;
    }
    let Ok(request_url) = Url::parse(&request.uri().to_string()) else {
        return false;
    };
    let path = request_url.path();
    path == "/.well-known/openid-configuration"
        || path.starts_with("/.well-known/openid-configuration/")
        || path.ends_with("/.well-known/openid-configuration")
}

fn optional_endpoint(value: Option<&str>, label: &str) -> Result<Option<Url>> {
    value
        .map(|value| validate_endpoint(value, label))
        .transpose()
}

fn validate_endpoint(value: &str, label: &str) -> Result<Url> {
    let url = Url::parse(value).with_context(|| format!("invalid OAuth {label}"))?;
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
        "OAuth {label} must use HTTPS or loopback HTTP"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
        "OAuth {label} must not contain user info or a fragment"
    );
    Ok(url)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn endpoint_overrides_preserve_discovered_issuer_and_other_fields() {
        let mut discovered = AuthorizationMetadata::default();
        discovered.authorization_endpoint = "https://discovered.example/authorize".into();
        discovered.token_endpoint = "https://discovered.example/token".into();
        discovered.issuer = Some("https://issuer.example".into());
        discovered
            .additional_fields
            .insert("custom".into(), serde_json::json!("kept"));
        let overrides = overrides(
            Some("https://configured.example/authorize"),
            Some("https://configured.example/token"),
            None,
            None,
            None,
            None,
        );

        let result = overrides.apply_to_metadata(Some(discovered)).unwrap();
        assert_eq!(
            result.authorization_endpoint,
            "https://configured.example/authorize"
        );
        assert_eq!(result.token_endpoint, "https://configured.example/token");
        assert_eq!(result.issuer.as_deref(), Some("https://issuer.example"));
        assert_eq!(
            result.additional_fields.get("custom"),
            Some(&serde_json::json!("kept"))
        );
    }

    #[test]
    fn explicit_endpoint_pair_works_without_discovery() {
        let overrides = overrides(
            Some("https://auth.example/authorize"),
            Some("https://auth.example/token"),
            None,
            None,
            Some(vec!["openid".into()]),
            None,
        );
        let result = overrides.apply_to_metadata(None).unwrap();
        assert_eq!(
            result.authorization_endpoint,
            "https://auth.example/authorize"
        );
        assert_eq!(result.token_endpoint, "https://auth.example/token");
        assert_eq!(
            overrides.default_scopes(),
            Some(["openid".into()].as_slice())
        );
    }

    #[test]
    fn missing_discovery_requires_both_explicit_endpoints() {
        let only_token = overrides(
            None,
            Some("https://auth.example/token"),
            None,
            None,
            None,
            None,
        );
        assert!(only_token.apply_to_metadata(None).is_err());
    }

    #[test]
    fn configured_oidc_url_is_remapped_only_for_oidc_gets_and_other_requests_pass_through() {
        let overrides = overrides(
            None,
            None,
            None,
            Some("https://oidc.example/custom/config"),
            None,
            Some(true),
        );
        let mut oidc = request("https://issuer.example/.well-known/openid-configuration");
        assert_eq!(
            route_oidc_discovery_request(&mut oidc, overrides.oidc_configuration_url(), true),
            OidcDiscoveryRoute::Remapped
        );
        assert_eq!(oidc.uri(), "https://oidc.example/custom/config");

        let mut unrelated =
            request("https://issuer.example/.well-known/oauth-authorization-server");
        assert_eq!(
            route_oidc_discovery_request(&mut unrelated, overrides.oidc_configuration_url(), true),
            OidcDiscoveryRoute::PassThrough
        );
        assert_eq!(
            unrelated.uri(),
            "https://issuer.example/.well-known/oauth-authorization-server"
        );
    }

    #[test]
    fn configured_oidc_url_enables_oidc_when_enable_flag_is_omitted() {
        let overrides = overrides(
            None,
            None,
            None,
            Some("https://oidc.example/custom/config"),
            None,
            None,
        );
        assert!(overrides.oidc_discovery_enabled());
        assert_eq!(
            overrides.oidc_configuration_url().map(Url::as_str),
            Some("https://oidc.example/custom/config")
        );
    }

    #[test]
    fn oidc_userinfo_and_scopes_are_carried_into_sdk_metadata() {
        let overrides = OAuthMetadataOverrides::from_raw(
            None,
            Some("https://auth.example/authorize"),
            Some("https://auth.example/token"),
            None,
            None,
            None,
            None,
            None,
            Some("https://auth.example/userinfo"),
            Some(vec!["openid".to_string(), "profile".to_string()]),
        )
        .unwrap();
        let metadata = overrides.apply_to_metadata(None).unwrap();
        assert_eq!(
            metadata.additional_fields.get("userinfo_endpoint"),
            Some(&serde_json::json!("https://auth.example/userinfo"))
        );
        assert_eq!(
            metadata.scopes_supported.as_deref(),
            Some(["openid".to_string(), "profile".to_string()].as_slice())
        );
    }

    #[test]
    fn explicit_oidc_false_suppresses_all_sdk_oidc_discovery_candidates() {
        let overrides = overrides(None, None, None, None, None, Some(false));
        for uri in [
            "https://issuer.example/.well-known/openid-configuration",
            "https://issuer.example/.well-known/openid-configuration/tenant",
            "https://issuer.example/tenant/.well-known/openid-configuration",
        ] {
            let mut request = request(uri);
            assert_eq!(
                route_oidc_discovery_request(
                    &mut request,
                    overrides.oidc_configuration_url(),
                    overrides.oidc_discovery_enabled(),
                ),
                OidcDiscoveryRoute::Suppress
            );
        }
    }

    #[test]
    fn oidc_path_insertion_and_path_appending_candidates_are_recognized() {
        let configured = Url::parse("https://oidc.example/custom/config").unwrap();
        for uri in [
            "https://issuer.example/.well-known/openid-configuration/tenant",
            "https://issuer.example/tenant/.well-known/openid-configuration",
        ] {
            let mut request = request(uri);
            assert_eq!(
                route_oidc_discovery_request(&mut request, Some(&configured), true),
                OidcDiscoveryRoute::Remapped
            );
            assert_eq!(request.uri(), "https://oidc.example/custom/config");
        }
    }

    #[test]
    fn remapping_custom_oidc_origin_strips_all_request_credentials() {
        let configured = Url::parse("https://oidc.example/custom/config").unwrap();
        let mut request = request("https://issuer.example/.well-known/openid-configuration");
        request.headers_mut().insert(
            http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        request
            .headers_mut()
            .insert(http::header::COOKIE, "session=secret".parse().unwrap());
        request.headers_mut().insert(
            http::header::PROXY_AUTHORIZATION,
            "Basic secret".parse().unwrap(),
        );
        request
            .headers_mut()
            .insert(http::header::HOST, "issuer.example".parse().unwrap());
        assert_eq!(
            route_oidc_discovery_request(&mut request, Some(&configured), true),
            OidcDiscoveryRoute::Remapped
        );
        for header in [
            http::header::AUTHORIZATION,
            http::header::COOKIE,
            http::header::PROXY_AUTHORIZATION,
            http::header::HOST,
        ] {
            assert!(request.headers().get(header).is_none());
        }
    }

    #[test]
    fn configured_authorization_server_base_is_the_discovery_base() {
        let configured = OAuthMetadataOverrides::from_raw(
            Some("https://auth.example/tenant"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let resource_url = Url::parse("https://mcp.example/resource").unwrap();
        assert_eq!(
            configured.metadata_discovery_base(&resource_url).as_str(),
            "https://auth.example/tenant"
        );
        let defaults = self::overrides(None, None, None, None, None, None);
        assert_eq!(
            defaults.metadata_discovery_base(&resource_url),
            resource_url
        );
    }

    #[tokio::test]
    async fn configured_oidc_fetch_uses_the_supplied_client_and_stops_redirects() {
        let overrides = overrides(
            None,
            None,
            None,
            Some("https://oidc.example/custom/config"),
            None,
            None,
        );
        let capture = Arc::new(CaptureClient::default());
        let wrapped = overrides.wrap_http_client(capture.clone());
        let manager = rmcp::transport::auth::AuthorizationManager::new_with_oauth_http_client(
            "https://issuer.example/mcp",
            wrapped,
        )
        .await
        .unwrap();

        let _ = manager.resolve_metadata().await;
        let requests = capture.0.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|(uri, _)| uri == "https://oidc.example/custom/config")
                .count(),
            1,
            "a redirect response must not be followed"
        );
        assert!(
            requests
                .iter()
                .any(|(uri, stopped)| { uri == "https://oidc.example/custom/config" && *stopped })
        );
    }

    #[test]
    fn loopback_http_is_allowed_but_user_info_fragments_and_remote_http_are_rejected() {
        assert!(validate_endpoint("http://localhost:8080/token", "token endpoint").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:8080/token", "token endpoint").is_ok());
        assert!(validate_endpoint("http://auth.example/token", "token endpoint").is_err());
        assert!(validate_endpoint("https://user@auth.example/token", "token endpoint").is_err());
        assert!(
            validate_endpoint("https://auth.example/token#fragment", "token endpoint").is_err()
        );
    }

    fn overrides(
        authorization_url: Option<&str>,
        token_url: Option<&str>,
        registration_url: Option<&str>,
        oidc_configuration_url: Option<&str>,
        default_scopes: Option<Vec<String>>,
        oidc_enabled: Option<bool>,
    ) -> OAuthMetadataOverrides {
        OAuthMetadataOverrides::from_raw(
            None,
            authorization_url,
            token_url,
            registration_url,
            None,
            default_scopes,
            oidc_enabled,
            oidc_configuration_url,
            None,
            None,
        )
        .unwrap()
    }

    #[derive(Default)]
    struct CaptureClient(Mutex<Vec<(String, bool)>>);

    impl OAuthHttpClient for CaptureClient {
        fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
            let uri = request.request.uri().to_string();
            let stop_redirect = request.redirect_policy == OAuthHttpRedirectPolicy::Stop;
            self.0.lock().unwrap().push((uri.clone(), stop_redirect));
            Box::pin(async move {
                let status = if uri == "https://oidc.example/custom/config" {
                    http::StatusCode::FOUND
                } else {
                    http::StatusCode::NOT_FOUND
                };
                Ok(http::Response::builder()
                    .status(status)
                    .body(Vec::new())
                    .unwrap())
            })
        }
    }

    fn request(uri: &str) -> http::Request<Vec<u8>> {
        http::Request::builder()
            .method("GET")
            .uri(uri)
            .body(Vec::new())
            .unwrap()
    }
}
