//! Assertions for provider-registered private-key JWT OAuth clients.
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

pub(crate) struct OAuthPrivateKeyJwt {
    client_id: String,
    audience: String,
    header: Header,
    key: EncodingKey,
}

impl OAuthPrivateKeyJwt {
    pub(crate) fn client_id(&self) -> &str {
        &self.client_id
    }
    pub(crate) fn audience(&self) -> &str {
        &self.audience
    }
    pub(crate) fn new(
        client_id: String,
        token_endpoint: &Url,
        algorithm: &str,
        private_pem: &str,
        key_id: Option<String>,
    ) -> anyhow::Result<Self> {
        let algorithm: Algorithm = algorithm.parse()?;
        let key = match algorithm {
            Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512 => EncodingKey::from_rsa_pem(private_pem.as_bytes()),
            Algorithm::ES256 | Algorithm::ES384 => EncodingKey::from_ec_pem(private_pem.as_bytes()),
            Algorithm::EdDSA => EncodingKey::from_ed_pem(private_pem.as_bytes()),
            _ => anyhow::bail!("Private-key JWT requires an asymmetric signing algorithm"),
        }
        .map_err(|_| anyhow::anyhow!("Could not parse the registered OAuth private key"))?;
        anyhow::ensure!(!client_id.trim().is_empty(), "OAuth client ID is required");
        anyhow::ensure!(
            token_endpoint.scheme() == "https"
                || (token_endpoint.scheme() == "http"
                    && matches!(
                        token_endpoint.host_str(),
                        Some("localhost" | "127.0.0.1" | "[::1]")
                    )),
            "OAuth token endpoint requires HTTPS or loopback HTTP"
        );
        anyhow::ensure!(
            token_endpoint.username().is_empty()
                && token_endpoint.password().is_none()
                && token_endpoint.fragment().is_none(),
            "Invalid OAuth token endpoint"
        );
        let mut header = Header::new(algorithm);
        header.kid = key_id;
        Ok(Self {
            client_id,
            audience: token_endpoint.as_str().to_string(),
            header,
            key,
        })
    }

    pub(crate) fn assertion(&self) -> anyhow::Result<String> {
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            sub: &'a str,
            aud: &'a str,
            iat: u64,
            exp: u64,
            jti: String,
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let claims = Claims {
            iss: &self.client_id,
            sub: &self.client_id,
            aud: &self.audience,
            iat: now,
            exp: now
                .checked_add(60)
                .ok_or_else(|| anyhow::anyhow!("Invalid system clock"))?,
            jti: uuid::Uuid::new_v4().to_string(),
        };
        jsonwebtoken::encode(&self.header, &claims, &self.key)
            .map_err(|_| anyhow::anyhow!("Could not sign the registered OAuth client assertion"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{DecodingKey, Validation, decode};
    use serde::Deserialize;

    // Generated solely for these tests; never a provider or user key.
    const PRIVATE_KEY: &str = include_str!("../tests/fixtures/brr-test-rsa.private.fixture");
    const PUBLIC_KEY: &str = include_str!("../tests/fixtures/brr-test-rsa.public.fixture");

    #[derive(Debug, Deserialize)]
    struct Claims {
        iss: String,
        sub: String,
        aud: String,
        iat: u64,
        exp: u64,
        jti: String,
    }

    #[test]
    fn signs_short_lived_unique_assertions_for_the_registered_client_and_exact_endpoint() {
        let endpoint = Url::parse("https://issuer.test/oauth/token").unwrap();
        let signer = OAuthPrivateKeyJwt::new(
            "registered-client".into(),
            &endpoint,
            "RS256",
            PRIVATE_KEY,
            Some("registered-key-id".into()),
        )
        .unwrap();
        let first = signer.assertion().unwrap();
        let second = signer.assertion().unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[endpoint.as_str()]);
        validation.set_issuer(&["registered-client"]);
        let key = DecodingKey::from_rsa_pem(PUBLIC_KEY.as_bytes()).unwrap();
        let decoded = decode::<Claims>(&first, &key, &validation).unwrap();
        assert_eq!(decoded.header.kid.as_deref(), Some("registered-key-id"));
        assert_eq!(decoded.claims.iss, "registered-client");
        assert_eq!(decoded.claims.sub, "registered-client");
        assert_eq!(decoded.claims.aud, endpoint.as_str());
        assert_eq!(decoded.claims.exp - decoded.claims.iat, 60);
        assert_ne!(
            decoded.claims.jti,
            decode::<Claims>(&second, &key, &validation)
                .unwrap()
                .claims
                .jti
        );
    }

    #[test]
    fn refuses_symmetric_algorithms_invalid_keys_and_remote_plain_http() {
        let endpoint = Url::parse("https://issuer.test/token").unwrap();
        assert!(
            OAuthPrivateKeyJwt::new("client".into(), &endpoint, "HS256", PRIVATE_KEY, None)
                .is_err()
        );
        assert!(
            OAuthPrivateKeyJwt::new("client".into(), &endpoint, "RS256", "not a key", None)
                .is_err()
        );
        assert!(
            OAuthPrivateKeyJwt::new(
                "client".into(),
                &Url::parse("http://issuer.test/token").unwrap(),
                "RS256",
                PRIVATE_KEY,
                None
            )
            .is_err()
        );
    }
}
