use std::sync::Arc;

use crate::oauth_private_key_jwt::OAuthPrivateKeyJwt;
use base64::Engine;
use http::HeaderValue;
use http::Method;
use http::Request;
use http::header::AUTHORIZATION;
use http::header::CONTENT_LENGTH;
use http::header::CONTENT_TYPE;
use rmcp::transport::auth::OAuthHttpClient;
use rmcp::transport::auth::OAuthHttpClientFuture;
use rmcp::transport::auth::OAuthHttpRedirectPolicy;
use rmcp::transport::auth::OAuthHttpRequest;
use url::Url;

/// Client authentication applied to authorization-code and refresh-token exchanges.
///
/// Private-key JWT needs a registered signing key and is intentionally handled by a
/// separate signer implementation rather than being approximated here.
pub(crate) enum OAuthTokenEndpointAuth {
    Discovered,
    Public,
    ClientSecretBasic(String),
    ClientSecretPost(String),
    PrivateKeyJwt(Arc<OAuthPrivateKeyJwt>),
}

/// Applies a validated server registration only to token requests for its exact endpoint.
pub(crate) struct OAuthTokenHttpClient {
    inner: Arc<dyn OAuthHttpClient>,
    token_endpoint: Url,
    client_id: String,
    auth: OAuthTokenEndpointAuth,
    resource: Option<String>,
}

impl OAuthTokenHttpClient {
    pub(crate) fn for_discovered_client(
        inner: Arc<dyn OAuthHttpClient>,
        token_endpoint: Url,
        resource: Option<String>,
    ) -> anyhow::Result<Self> {
        Self::new(
            inner,
            token_endpoint,
            String::new(),
            OAuthTokenEndpointAuth::Discovered,
            resource,
        )
    }
    pub(crate) fn new(
        inner: Arc<dyn OAuthHttpClient>,
        token_endpoint: Url,
        client_id: String,
        auth: OAuthTokenEndpointAuth,
        resource: Option<String>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            token_endpoint.scheme() == "https"
                || (token_endpoint.scheme() == "http"
                    && matches!(
                        token_endpoint.host_str(),
                        Some("localhost" | "127.0.0.1" | "[::1]")
                    )),
            "OAuth token endpoint must use HTTPS or loopback HTTP"
        );
        anyhow::ensure!(
            token_endpoint.username().is_empty()
                && token_endpoint.password().is_none()
                && token_endpoint.fragment().is_none(),
            "OAuth token endpoint must not contain user info or a fragment"
        );
        anyhow::ensure!(
            !client_id.is_empty() || matches!(auth, OAuthTokenEndpointAuth::Discovered),
            "OAuth client ID must not be empty"
        );
        if let OAuthTokenEndpointAuth::PrivateKeyJwt(signer) = &auth {
            anyhow::ensure!(
                signer.client_id() == client_id && signer.audience() == token_endpoint.as_str(),
                "OAuth signing key must be bound to the configured client and exact token endpoint"
            );
            signer.assertion()?;
        }
        Ok(Self {
            inner,
            token_endpoint,
            client_id,
            auth,
            resource,
        })
    }

    fn rewrite_token_request(
        &self,
        mut request: Request<Vec<u8>>,
    ) -> anyhow::Result<(Request<Vec<u8>>, bool)> {
        if request.method() != Method::POST
            || request.uri().to_string() != self.token_endpoint.as_str()
            || !is_form_request(&request)
            || !has_supported_grant(&request)
        {
            return Ok((request, false));
        }

        let mut pairs = url::form_urlencoded::parse(request.body())
            .filter(|(key, _)| {
                !(key == "resource" && self.resource.is_some())
                    && (matches!(self.auth, OAuthTokenEndpointAuth::Discovered)
                        || !matches!(
                            key.as_ref(),
                            "client_id"
                                | "client_secret"
                                | "client_assertion"
                                | "client_assertion_type"
                        ))
            })
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        if !matches!(self.auth, OAuthTokenEndpointAuth::Discovered) {
            pairs.push(("client_id".to_string(), self.client_id.clone()));
        }
        pairs.extend(
            self.resource
                .iter()
                .map(|resource| ("resource".to_string(), resource.clone())),
        );
        if !matches!(self.auth, OAuthTokenEndpointAuth::Discovered) {
            request.headers_mut().remove(AUTHORIZATION);
        }
        match &self.auth {
            OAuthTokenEndpointAuth::Public | OAuthTokenEndpointAuth::Discovered => {}
            OAuthTokenEndpointAuth::PrivateKeyJwt(signer) => {
                pairs.push((
                    "client_assertion_type".into(),
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer".into(),
                ));
                pairs.push(("client_assertion".into(), signer.assertion()?));
            }
            OAuthTokenEndpointAuth::ClientSecretBasic(secret) => {
                let credentials = format!(
                    "{}:{}",
                    form_encode_component(&self.client_id),
                    form_encode_component(secret)
                );
                let value = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(credentials)
                );
                request.headers_mut().insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&value).expect("ASCII header"),
                );
            }
            OAuthTokenEndpointAuth::ClientSecretPost(secret) => {
                pairs.push(("client_secret".to_string(), secret.clone()));
            }
        }
        *request.body_mut() = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish()
            .into_bytes();
        let body_length = request.body().len().to_string();
        request.headers_mut().remove(CONTENT_LENGTH);
        request.headers_mut().insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&body_length).expect("decimal length header"),
        );
        Ok((request, true))
    }
}

impl OAuthHttpClient for OAuthTokenHttpClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        let mut request = request;
        let (rewritten, is_token_exchange) = match self.rewrite_token_request(request.request) {
            Ok(rewritten) => rewritten,
            Err(error) => {
                return Box::pin(async move {
                    Err(Box::new(std::io::Error::other(error.to_string()))
                        as rmcp::transport::auth::OAuthHttpClientError)
                });
            }
        };
        request.request = rewritten;
        request.redirect_policy = token_redirect_policy(request.redirect_policy, is_token_exchange);
        self.inner.execute(request)
    }
}

fn token_redirect_policy(
    requested: OAuthHttpRedirectPolicy,
    is_token_exchange: bool,
) -> OAuthHttpRedirectPolicy {
    if is_token_exchange {
        OAuthHttpRedirectPolicy::Stop
    } else {
        requested
    }
}

fn is_form_request(request: &Request<Vec<u8>>) -> bool {
    request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| {
            mime.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
}

fn has_supported_grant(request: &Request<Vec<u8>>) -> bool {
    url::form_urlencoded::parse(request.body()).any(|(key, value)| {
        key == "grant_type" && matches!(value.as_ref(), "authorization_code" | "refresh_token")
    })
}

fn form_encode_component(value: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("", value)
        .finish()
        .strip_prefix('=')
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::transport::auth::OAuthHttpClientError;

    struct TestClient;

    #[test]
    fn private_key_jwt_signs_code_and_refresh_requests_without_changing_the_grant() {
        let endpoint = Url::parse("https://issuer.example/token").unwrap();
        let signer = OAuthPrivateKeyJwt::new(
            "client id".into(),
            &endpoint,
            "RS256",
            include_str!("../tests/fixtures/brr-test-rsa.private.fixture"),
            Some("registered-key".into()),
        )
        .unwrap();
        let client = adapter(
            Arc::new(TestClient),
            OAuthTokenEndpointAuth::PrivateKeyJwt(Arc::new(signer)),
            None,
        );
        for body in [
            "grant_type=authorization_code&code=opaque&code_verifier=pkce&client_secret=old",
            "grant_type=refresh_token&refresh_token=opaque-refresh&client_assertion=old",
        ] {
            let (sent, matched) = client
                .rewrite_token_request(request(endpoint.as_str(), body))
                .unwrap();
            assert!(matched);
            let fields = url::form_urlencoded::parse(sent.body()).collect::<Vec<_>>();
            let assertion = fields
                .iter()
                .find(|(key, _)| key == "client_assertion")
                .unwrap()
                .1
                .as_ref();
            assert_eq!(
                fields
                    .iter()
                    .filter(|(key, _)| key == "client_assertion")
                    .count(),
                1
            );
            assert!(
                fields
                    .iter()
                    .any(|(key, value)| key == "client_assertion_type"
                        && value == "urn:ietf:params:oauth:client-assertion-type:jwt-bearer")
            );
            assert!(!fields.iter().any(|(key, _)| key == "client_secret"));
            assert!(!sent.headers().contains_key(AUTHORIZATION));
            let key = jsonwebtoken::DecodingKey::from_rsa_pem(include_bytes!(
                "../tests/fixtures/brr-test-rsa.public.fixture"
            ))
            .unwrap();
            let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
            validation.set_audience(&[endpoint.as_str()]);
            validation.set_issuer(&["client id"]);
            let decoded =
                jsonwebtoken::decode::<serde_json::Value>(assertion, &key, &validation).unwrap();
            assert_eq!(decoded.header.kid.as_deref(), Some("registered-key"));
            let original = url::form_urlencoded::parse(body.as_bytes())
                .filter(|(key, _)| {
                    matches!(
                        key.as_ref(),
                        "grant_type" | "code" | "code_verifier" | "refresh_token"
                    )
                })
                .collect::<Vec<_>>();
            assert!(original.iter().all(|entry| fields.contains(entry)));
        }
    }

    #[test]
    fn discovered_registration_keeps_sdk_credentials_when_pinning_resource() {
        let client = OAuthTokenHttpClient::for_discovered_client(
            Arc::new(TestClient),
            Url::parse("https://issuer.example/token").unwrap(),
            Some("https://resource.test".into()),
        )
        .unwrap();
        let (sent, matched) = client.rewrite_token_request(request("https://issuer.example/token", "grant_type=authorization_code&code=abc&client_id=registered-id&client_secret=registered-secret")).unwrap();
        assert!(matched);
        assert_eq!(
            sent.headers().get(AUTHORIZATION).unwrap(),
            "Bearer caller-value"
        );
        let fields = url::form_urlencoded::parse(sent.body()).collect::<Vec<_>>();
        assert!(fields.contains(&("client_id".into(), "registered-id".into())));
        assert!(fields.contains(&("client_secret".into(), "registered-secret".into())));
        assert!(fields.contains(&("resource".into(), "https://resource.test".into())));
    }

    #[test]
    fn preserves_discovered_resource_when_no_override_is_configured() {
        let adapter = adapter(Arc::new(TestClient), OAuthTokenEndpointAuth::Public, None);
        let (sent, matched) = adapter.rewrite_token_request(request("https://issuer.example/token", "grant_type=refresh_token&refresh_token=opaque&resource=https%3A%2F%2Fdiscovered.test")).unwrap();
        assert!(matched);
        let fields = url::form_urlencoded::parse(sent.body()).collect::<Vec<_>>();
        assert!(fields.contains(&("resource".into(), "https://discovered.test".into())));
    }

    #[test]
    fn supports_local_oauth_fixtures_without_accepting_plain_http_providers() {
        for endpoint in [
            "http://127.0.0.1:31415/token",
            "http://localhost:31415/token",
            "http://[::1]:31415/token",
        ] {
            assert!(
                OAuthTokenHttpClient::new(
                    Arc::new(TestClient),
                    Url::parse(endpoint).unwrap(),
                    "client".into(),
                    OAuthTokenEndpointAuth::Public,
                    None
                )
                .is_ok()
            );
        }
        assert!(
            OAuthTokenHttpClient::new(
                Arc::new(TestClient),
                Url::parse("http://issuer.example/token").unwrap(),
                "client".into(),
                OAuthTokenEndpointAuth::Public,
                None
            )
            .is_err()
        );
    }

    impl OAuthHttpClient for TestClient {
        fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
            Box::pin(async move {
                let _ = request;
                Err(Box::new(std::io::Error::other("test client not called"))
                    as OAuthHttpClientError)
            })
        }
    }

    fn request(uri: &str, body: &str) -> Request<Vec<u8>> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(
                CONTENT_TYPE,
                "application/x-www-form-urlencoded; charset=utf-8",
            )
            .header(AUTHORIZATION, "Bearer caller-value")
            .header(CONTENT_LENGTH, "999")
            .body(body.as_bytes().to_vec())
            .unwrap()
    }

    fn adapter(
        inner: Arc<TestClient>,
        auth: OAuthTokenEndpointAuth,
        resource: Option<&str>,
    ) -> OAuthTokenHttpClient {
        OAuthTokenHttpClient::new(
            inner,
            Url::parse("https://issuer.example/token").unwrap(),
            "client id".to_string(),
            auth,
            resource.map(str::to_string),
        )
        .unwrap()
    }

    #[test]
    fn basic_auth_replaces_conflicting_credentials_and_pins_resource() {
        let inner = Arc::new(TestClient);
        let adapter = adapter(
            inner.clone(),
            OAuthTokenEndpointAuth::ClientSecretBasic("s ecret".to_string()),
            Some("https://api.example/mcp"),
        );
        let (sent, matched) = adapter.rewrite_token_request(request(
                "https://issuer.example/token",
                "grant_type=authorization_code&code=abc&code_verifier=verifier&client_secret=old&client_assertion=old&resource=old&resource=old2",
            )).unwrap();

        assert!(matched);
        assert_eq!(
            sent.headers().get(AUTHORIZATION).unwrap(),
            "Basic Y2xpZW50K2lkOnMrZWNyZXQ="
        );
        let fields = url::form_urlencoded::parse(sent.body()).collect::<Vec<_>>();
        assert!(fields.contains(&("code".into(), "abc".into())));
        assert!(fields.contains(&("code_verifier".into(), "verifier".into())));
        assert_eq!(
            fields.iter().filter(|(key, _)| key == "resource").count(),
            1
        );
        assert_eq!(fields[fields.len() - 1].1, "https://api.example/mcp");
        assert!(
            !fields
                .iter()
                .any(|(key, _)| { matches!(key.as_ref(), "client_secret" | "client_assertion") })
        );
        assert_eq!(
            sent.headers().get(CONTENT_LENGTH).unwrap(),
            sent.body().len().to_string().as_str()
        );
    }

    #[test]
    fn public_auth_does_not_add_credentials_to_other_urls_or_grants() {
        let inner = Arc::new(TestClient);
        let adapter = adapter(inner.clone(), OAuthTokenEndpointAuth::Public, None);
        let (other_url, matched_other_url) = adapter
            .rewrite_token_request(request(
                "https://issuer.example/token?other=1",
                "grant_type=authorization_code&code=abc",
            ))
            .unwrap();
        let (other_grant, matched_other_grant) = adapter
            .rewrite_token_request(request(
                "https://issuer.example/token",
                "grant_type=client_credentials",
            ))
            .unwrap();

        assert!(!matched_other_url);
        assert!(!matched_other_grant);
        assert_eq!(
            other_url.headers().get(AUTHORIZATION).unwrap(),
            "Bearer caller-value"
        );
        assert_eq!(
            other_grant.headers().get(AUTHORIZATION).unwrap(),
            "Bearer caller-value"
        );
    }

    #[test]
    fn only_rewritten_token_requests_stop_redirects() {
        assert_eq!(
            token_redirect_policy(OAuthHttpRedirectPolicy::Follow, true),
            OAuthHttpRedirectPolicy::Stop
        );
        assert_eq!(
            token_redirect_policy(OAuthHttpRedirectPolicy::Follow, false),
            OAuthHttpRedirectPolicy::Follow
        );
    }

    #[test]
    fn post_auth_injects_client_id_and_secret_once_and_preserves_refresh_token() {
        let inner = Arc::new(TestClient);
        let adapter = adapter(
            inner.clone(),
            OAuthTokenEndpointAuth::ClientSecretPost("secret".to_string()),
            None,
        );
        let (sent, matched) = adapter
            .rewrite_token_request(request(
                "https://issuer.example/token",
                "grant_type=refresh_token&refresh_token=refresh&client_secret=old&client_id=old",
            ))
            .unwrap();

        assert!(matched);
        let fields = url::form_urlencoded::parse(sent.body()).collect::<Vec<_>>();
        assert!(fields.contains(&("refresh_token".into(), "refresh".into())));
        assert_eq!(
            fields.iter().filter(|(key, _)| key == "client_id").count(),
            1
        );
        assert_eq!(
            fields
                .iter()
                .filter(|(key, _)| key == "client_secret")
                .count(),
            1
        );
        assert!(!sent.headers().contains_key(AUTHORIZATION));
    }
}
