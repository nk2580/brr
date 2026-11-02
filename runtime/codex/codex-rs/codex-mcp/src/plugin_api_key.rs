//! Host-owned credentials for portable plugin API-key declarations.
use base64::Engine;
use codex_keyring_store::{DefaultKeyringStore, KeyringStore};
use codex_utils_redacted_string::RedactedString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

const SERVICE: &str = "Brr Plugin MCP Credentials";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginApiKeyCredential {
    Token {
        token: RedactedString,
    },
    Basic {
        username: RedactedString,
        password: RedactedString,
    },
    #[serde(rename = "oauth_client_secret", alias = "o_auth_client_secret")]
    OAuthClientSecret {
        secret: RedactedString,
    },
    #[serde(rename = "oauth_private_key_jwt", alias = "o_auth_private_key_jwt")]
    OAuthPrivateKeyJwt {
        #[serde(rename = "privateKey")]
        private_key: RedactedString,
        algorithm: String,
        #[serde(rename = "keyId")]
        key_id: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct PluginApiKeyDeclaration {
    pub url: String,
    pub header_scheme: String,
    pub header_name: Option<String>,
}

impl PluginApiKeyDeclaration {
    fn validate(&self) -> Result<url::Url, String> {
        let url = url::Url::parse(&self.url).map_err(|_| "Invalid API-key server URL")?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("API-key credentials require HTTPS or a loopback HTTP server without URL credentials or fragments".into());
        }
        match (self.header_scheme.as_str(), self.header_name.as_deref()) {
            ("bearer" | "basic", None) => {}
            ("custom_header", Some(name)) if valid_custom_header(name) => {}
            _ => return Err("Invalid API-key header scheme or header name".into()),
        }
        Ok(url)
    }

    fn binding(&self, root: &Path, server: &str) -> Result<String, String> {
        let url = self.validate()?;
        if !root.is_absolute() || server.trim().is_empty() {
            return Err("Invalid plugin credential identity".into());
        }
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let identity = serde_json::to_vec(&(
            root,
            server,
            url.as_str(),
            &self.header_scheme,
            self.header_name
                .as_ref()
                .map(|name| name.to_ascii_lowercase()),
        ))
        .map_err(|_| "Could not bind plugin credentials")?;
        Ok(format!("{:x}", Sha256::digest(identity)))
    }

    fn header(&self, credential: &PluginApiKeyCredential) -> Result<(String, String), String> {
        self.validate()?;
        let (name, value) = match (self.header_scheme.as_str(), credential) {
            ("bearer", PluginApiKeyCredential::Token { token }) => (
                "Authorization".to_string(),
                format!("Bearer {}", token.as_str()),
            ),
            ("custom_header", PluginApiKeyCredential::Token { token }) => (
                self.header_name
                    .clone()
                    .ok_or("Missing custom header name")?,
                token.as_str().to_string(),
            ),
            ("basic", PluginApiKeyCredential::Basic { username, password })
                if !username.contains(':') && !username.is_empty() =>
            {
                let encoded = base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    username.as_str(),
                    password.as_str()
                ));
                ("Authorization".to_string(), format!("Basic {encoded}"))
            }
            _ => {
                return Err(
                    "Credential does not match the plugin's declared authentication scheme".into(),
                );
            }
        };
        let nonempty = match credential {
            PluginApiKeyCredential::Token { token } => !token.trim().is_empty(),
            PluginApiKeyCredential::Basic { username, .. } => !username.is_empty(),
            PluginApiKeyCredential::OAuthClientSecret { .. } => false,
            PluginApiKeyCredential::OAuthPrivateKeyJwt { .. } => false,
        };
        if !nonempty
            || !value
                .bytes()
                .all(|byte| byte == b'\t' || (byte >= 32 && byte != 127))
        {
            return Err(
                "API-key credential is empty or contains invalid HTTP header characters".into(),
            );
        }
        Ok((name, value))
    }
}

fn valid_custom_header(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        && ![
            "host",
            "content-length",
            "transfer-encoding",
            "connection",
            "mcp-protocol-version",
            "mcp-session-id",
            "proxy-authorization",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
}

fn save_in(
    store: &dyn KeyringStore,
    root: &Path,
    server: &str,
    declaration: &PluginApiKeyDeclaration,
    credential: Option<&PluginApiKeyCredential>,
) -> Result<(), String> {
    let binding = declaration.binding(root, server)?;
    match credential {
        Some(credential) => {
            declaration.header(credential)?;
            let value = serde_json::to_string(credential)
                .map_err(|_| "Could not encode plugin credentials")?;
            store.save(SERVICE, &binding, &value).map_err(|_| {
                "Could not save plugin credentials in the system credential store".to_string()
            })
        }
        None => store.delete(SERVICE, &binding).map(|_| ()).map_err(|_| {
            "Could not remove plugin credentials from the system credential store".to_string()
        }),
    }
}

fn load_header_in(
    store: &dyn KeyringStore,
    root: &Path,
    server: &str,
    declaration: &PluginApiKeyDeclaration,
) -> Result<(String, String), String> {
    let binding = declaration.binding(root, server)?;
    let value = store
        .load(SERVICE, &binding)
        .map_err(|_| "Could not access the plugin credential store")?
        .ok_or(
            "This plugin MCP server needs an API-key credential. Configure it in Plugins and MCP.",
        )?;
    let credential: PluginApiKeyCredential = serde_json::from_str(&value)
        .map_err(|_| "Stored plugin credential is invalid; reconnect this server")?;
    declaration.header(&credential)
}

/// Resolves only the credential bound to this exact installed source and declaration.
pub fn plugin_api_key_header(
    root: &Path,
    server: &str,
    declaration: &PluginApiKeyDeclaration,
) -> Result<(String, String), String> {
    load_header_in(&DefaultKeyringStore, root, server, declaration)
}

fn oauth_binding(root: &Path, server: &str, url: &str, auth: &Value) -> Result<String, String> {
    let url = PluginApiKeyDeclaration {
        url: url.into(),
        header_scheme: "bearer".into(),
        header_name: None,
    }
    .validate()?;
    if !root.is_absolute() || server.trim().is_empty() {
        return Err("Invalid plugin credential identity".into());
    }
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    fn compact(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .filter(|(_, value)| !value.is_null())
                    .map(|(key, value)| (key.clone(), compact(value)))
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(compact).collect()),
            value => value.clone(),
        }
    }
    let identity = serde_json::to_vec(&("oauth", root, server, url.as_str(), compact(auth)))
        .map_err(|_| "Could not bind OAuth credentials")?;
    Ok(format!("{:x}", Sha256::digest(identity)))
}

fn load_oauth_secret_in(
    store: &dyn KeyringStore,
    root: &Path,
    server: &str,
    url: &str,
    auth: &Value,
) -> Result<RedactedString, String> {
    let binding = oauth_binding(root, server, url, auth)?;
    let stored = store.load(SERVICE, &binding).map_err(|_| "Could not access the plugin credential store")?
        .ok_or("This plugin MCP server needs its registered OAuth client secret. Configure it in Plugins and MCP.")?;
    match serde_json::from_str::<PluginApiKeyCredential>(&stored)
        .map_err(|_| "Stored plugin credential is invalid; reconnect this server")?
    {
        PluginApiKeyCredential::OAuthClientSecret { secret } if !secret.trim().is_empty() => {
            Ok(secret)
        }
        _ => Err("Stored credential does not match the plugin's OAuth declaration".into()),
    }
}

pub fn plugin_oauth_client_secret(
    root: &Path,
    server: &str,
    url: &str,
    auth: &Value,
) -> Result<RedactedString, String> {
    load_oauth_secret_in(&DefaultKeyringStore, root, server, url, auth)
}

/// Presence only; credential contents never enter a detail response.
pub fn plugin_mcp_credential_configured(
    root: &Path,
    server: &str,
    url: &str,
    auth: &Value,
) -> Result<bool, String> {
    let binding = match auth.get("type").and_then(Value::as_str) {
        Some("api_key") => PluginApiKeyDeclaration {
            url: url.into(),
            header_scheme: auth
                .get("headerScheme")
                .and_then(Value::as_str)
                .ok_or("Missing API-key header scheme")?
                .into(),
            header_name: auth
                .get("headerName")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
        .binding(root, server)?,
        Some("oauth" | "mixed")
            if matches!(
                auth.pointer("/client/tokenEndpointAuthMethod")
                    .and_then(Value::as_str),
                Some("client_secret_basic" | "client_secret_post" | "private_key_jwt")
            ) =>
        {
            oauth_binding(root, server, url, auth)?
        }
        _ => return Ok(false),
    };
    DefaultKeyringStore
        .load(SERVICE, &binding)
        .map(|value| value.is_some())
        .map_err(|_| "Could not access the plugin credential store".into())
}

fn load_oauth_private_key_in(
    store: &dyn KeyringStore,
    root: &Path,
    server: &str,
    url: &str,
    auth: &Value,
) -> Result<(RedactedString, String, Option<String>), String> {
    let binding = oauth_binding(root, server, url, auth)?;
    let stored = store.load(SERVICE, &binding).map_err(|_| "Could not access the plugin credential store")?.ok_or("This plugin needs its provider-registered OAuth signing key. Configure it in Plugins and MCP.")?;
    match serde_json::from_str::<PluginApiKeyCredential>(&stored)
        .map_err(|_| "Stored plugin credential is invalid; reconnect this server")?
    {
        PluginApiKeyCredential::OAuthPrivateKeyJwt {
            private_key,
            algorithm,
            key_id,
        } => Ok((private_key, algorithm, key_id)),
        _ => {
            Err("Stored credential does not match the plugin's private-key JWT declaration".into())
        }
    }
}

pub fn plugin_oauth_private_key_jwt(
    root: &Path,
    server: &str,
    url: &str,
    auth: &Value,
) -> Result<(RedactedString, String, Option<String>), String> {
    load_oauth_private_key_in(&DefaultKeyringStore, root, server, url, auth)
}

fn validate_signing_key(algorithm: &str, pem: &str) -> Result<(), String> {
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    let algorithm: Algorithm = algorithm
        .parse()
        .map_err(|_| "Unsupported OAuth signing algorithm")?;
    let key = match algorithm {
        Algorithm::RS256
        | Algorithm::RS384
        | Algorithm::RS512
        | Algorithm::PS256
        | Algorithm::PS384
        | Algorithm::PS512 => EncodingKey::from_rsa_pem(pem.as_bytes()),
        Algorithm::ES256 | Algorithm::ES384 => EncodingKey::from_ec_pem(pem.as_bytes()),
        Algorithm::EdDSA => EncodingKey::from_ed_pem(pem.as_bytes()),
        _ => return Err("Private-key JWT requires an asymmetric signing algorithm".into()),
    }
    .map_err(|_| "Could not parse the registered OAuth private key")?;
    jsonwebtoken::encode(&Header::new(algorithm), &serde_json::json!({}), &key)
        .map(|_| ())
        .map_err(|_| "The private key cannot sign with the selected algorithm".into())
}

/// Saves a credential after resolving its declaration from the installed portable package.
/// No secret is stored in the package, configuration, or plugin detail response.
pub fn configure_plugin_api_key(
    root: &Path,
    server: &str,
    credential: Option<&PluginApiKeyCredential>,
) -> Result<(), String> {
    let contents = std::fs::read_to_string(root.join("mcp.json"))
        .map_err(|_| "Installed plugin has no readable portable mcp.json")?;
    let manifest: Value =
        serde_json::from_str(&contents).map_err(|_| "Invalid portable MCP manifest")?;
    if let Some(entry) = manifest
        .get("mcpServers")
        .and_then(|servers| servers.get(server))
    {
        if let Some(auth) = entry.pointer("/extensions/com.openai/auth") {
            if matches!(
                auth.get("type").and_then(Value::as_str),
                Some("oauth" | "mixed")
            ) {
                if manifest.get("$schema").and_then(Value::as_str)
                    != Some("https://agent-plugins.org/schemas/1.0.0/mcp.schema.json")
                    || entry.get("type").and_then(Value::as_str) != Some("streamable-http")
                {
                    return Err("Unsupported portable OAuth server declaration".into());
                }
                if auth.pointer("/client/mode").and_then(Value::as_str) != Some("provided")
                    || !matches!(
                        auth.pointer("/client/tokenEndpointAuthMethod")
                            .and_then(Value::as_str),
                        Some("client_secret_basic" | "client_secret_post" | "private_key_jwt")
                    )
                {
                    return Err(
                        "This OAuth declaration does not require host-owned credentials".into(),
                    );
                }
                if !auth
                    .pointer("/client/clientId")
                    .and_then(Value::as_str)
                    .is_some_and(|client_id| !client_id.trim().is_empty())
                {
                    return Err("The registered OAuth client ID is missing".into());
                }
                let url = entry
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("Missing server URL")?;
                let binding = oauth_binding(root, server, url, auth)?;
                let private_key_method = auth
                    .pointer("/client/tokenEndpointAuthMethod")
                    .and_then(Value::as_str)
                    == Some("private_key_jwt");
                return match credential {
                    Some(credential @ PluginApiKeyCredential::OAuthClientSecret { secret })
                        if !private_key_method && !secret.trim().is_empty() =>
                    {
                        let value = serde_json::to_string(credential)
                            .map_err(|_| "Could not encode plugin credentials")?;
                        DefaultKeyringStore
                            .save(SERVICE, &binding, &value)
                            .map_err(|_| {
                                "Could not save plugin credentials in the system credential store"
                                    .into()
                            })
                    }
                    Some(
                        credential @ PluginApiKeyCredential::OAuthPrivateKeyJwt {
                            private_key,
                            algorithm,
                            ..
                        },
                    ) if private_key_method => {
                        if private_key.len() > 65_536 {
                            return Err("Registered OAuth key must be no larger than 64 KB".into());
                        }
                        validate_signing_key(algorithm, private_key.as_str())?;
                        let value = serde_json::to_string(credential)
                            .map_err(|_| "Could not encode plugin credentials")?;
                        DefaultKeyringStore
                            .save(SERVICE, &binding, &value)
                            .map_err(|_| {
                                "Could not save plugin credentials in the system credential store"
                                    .into()
                            })
                    }
                    None => DefaultKeyringStore
                        .delete(SERVICE, &binding)
                        .map(|_| ())
                        .map_err(|_| {
                            "Could not remove plugin credentials from the system credential store"
                                .into()
                        }),
                    _ => Err("Provide the registered OAuth client secret for this server".into()),
                };
            }
        }
    }
    let declaration = declaration_from_manifest(&contents, server)?;
    save_in(&DefaultKeyringStore, root, server, &declaration, credential)
}

fn declaration_from_manifest(
    contents: &str,
    server: &str,
) -> Result<PluginApiKeyDeclaration, String> {
    let manifest: Value =
        serde_json::from_str(contents).map_err(|_| "Invalid portable MCP manifest")?;
    if manifest.get("$schema").and_then(Value::as_str)
        != Some("https://agent-plugins.org/schemas/1.0.0/mcp.schema.json")
    {
        return Err("Unsupported portable MCP schema".into());
    }
    let server = manifest
        .get("mcpServers")
        .and_then(|servers| servers.get(server))
        .ok_or("No such plugin MCP server")?;
    if server.get("type").and_then(Value::as_str) != Some("streamable-http") {
        return Err("API-key authentication requires an HTTP MCP server".into());
    }
    let auth = server
        .pointer("/extensions/com.openai/auth")
        .ok_or("Server has no API-key declaration")?;
    if auth.get("type").and_then(Value::as_str) != Some("api_key") {
        return Err("Server does not declare API-key authentication".into());
    }
    if !auth.as_object().is_some_and(|auth| {
        auth.keys()
            .all(|key| matches!(key.as_str(), "type" | "headerScheme" | "headerName"))
    }) {
        return Err("Invalid API-key declaration fields".into());
    }
    let declaration = PluginApiKeyDeclaration {
        url: server
            .get("url")
            .and_then(Value::as_str)
            .ok_or("Missing server URL")?
            .into(),
        header_scheme: auth
            .get("headerScheme")
            .and_then(Value::as_str)
            .ok_or("Missing API-key header scheme")?
            .into(),
        header_name: match auth.get("headerName") {
            None => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => return Err("API-key headerName must be a string when provided".into()),
        },
    };
    declaration.validate()?;
    Ok(declaration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_keyring_store::tests::MockKeyringStore;

    fn declaration(scheme: &str) -> PluginApiKeyDeclaration {
        PluginApiKeyDeclaration {
            url: "https://example.test/mcp".into(),
            header_scheme: scheme.into(),
            header_name: None,
        }
    }

    #[test]
    fn constructs_only_the_declared_authentication_header() {
        let token = PluginApiKeyCredential::Token {
            token: "test-secret".into(),
        };
        assert_eq!(
            declaration("bearer").header(&token).unwrap(),
            ("Authorization".into(), "Bearer test-secret".into())
        );
        let mut custom = declaration("custom_header");
        custom.header_name = Some("X-Api-Key".into());
        assert_eq!(
            custom.header(&token).unwrap(),
            ("X-Api-Key".into(), "test-secret".into())
        );
        let basic = PluginApiKeyCredential::Basic {
            username: "alice".into(),
            password: "p:word".into(),
        };
        assert_eq!(
            declaration("basic").header(&basic).unwrap(),
            ("Authorization".into(), "Basic YWxpY2U6cDp3b3Jk".into())
        );
        let empty_password = PluginApiKeyCredential::Basic {
            username: "api-key-as-username".into(),
            password: "".into(),
        };
        assert!(declaration("basic").header(&empty_password).is_ok());
        assert!(declaration("basic").header(&token).is_err());
        assert!(!format!("{token:?}").contains("test-secret"));
    }

    #[test]
    fn rejects_header_injection_and_invalid_declarations() {
        let token = PluginApiKeyCredential::Token {
            token: "test\r\nX-Steal: value".into(),
        };
        assert!(declaration("bearer").header(&token).is_err());
        let token = PluginApiKeyCredential::Token {
            token: "key".into(),
        };
        for name in [
            "Host",
            "hoST",
            "bad name",
            "Content-Length",
            "MCP-Protocol-Version",
        ] {
            let mut custom = declaration("custom_header");
            custom.header_name = Some(name.into());
            assert!(custom.header(&token).is_err(), "{name}");
        }
        let mut bad = declaration("bearer");
        bad.header_name = Some("X-Api-Key".into());
        assert!(bad.header(&token).is_err());
        for invalid in [serde_json::json!(12), Value::Null] {
            let manifest = serde_json::json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"docs":{"type":"streamable-http","url":"https://docs.test/mcp","extensions":{"com.openai":{"auth":{"type":"api_key","headerScheme":"bearer","headerName":invalid}}}}}});
            assert!(declaration_from_manifest(&manifest.to_string(), "docs").is_err());
        }
    }

    #[test]
    fn credentials_are_bound_to_source_server_endpoint_and_declaration() {
        let declaration = declaration("bearer");
        let original = declaration
            .binding(Path::new("/tmp/plugin-a"), "docs")
            .unwrap();
        assert_ne!(
            original,
            declaration
                .binding(Path::new("/tmp/plugin-b"), "docs")
                .unwrap()
        );
        assert_ne!(
            original,
            declaration
                .binding(Path::new("/tmp/plugin-a"), "other")
                .unwrap()
        );
        let mut changed = declaration.clone();
        changed.url = "https://other.test/mcp".into();
        assert_ne!(
            original,
            changed.binding(Path::new("/tmp/plugin-a"), "docs").unwrap()
        );
        changed = declaration.clone();
        changed.header_scheme = "basic".into();
        assert_ne!(
            original,
            changed.binding(Path::new("/tmp/plugin-a"), "docs").unwrap()
        );
    }

    #[test]
    fn saves_reads_deletes_and_never_reuses_credentials_for_changed_endpoint() {
        let store = MockKeyringStore::default();
        let root = Path::new("/tmp/plugin-test");
        let declaration = declaration("bearer");
        assert!(load_header_in(&store, root, "docs", &declaration).is_err());
        let key = PluginApiKeyCredential::Token {
            token: "scoped-secret".into(),
        };
        save_in(&store, root, "docs", &declaration, Some(&key)).unwrap();
        assert_eq!(
            load_header_in(&store, root, "docs", &declaration)
                .unwrap()
                .1,
            "Bearer scoped-secret"
        );
        let mut changed = declaration.clone();
        changed.url = "https://changed.test/mcp".into();
        assert!(load_header_in(&store, root, "docs", &changed).is_err());
        save_in(&store, root, "docs", &declaration, None).unwrap();
        assert!(load_header_in(&store, root, "docs", &declaration).is_err());
    }

    #[test]
    fn oauth_secrets_are_bound_to_client_and_descriptor_and_do_not_leak() {
        let store = MockKeyringStore::default();
        let root = Path::new("/tmp/oauth-plugin");
        let auth = serde_json::json!({"type":"oauth","client":{"mode":"provided","clientId":"docs","tokenEndpointAuthMethod":"client_secret_post"}});
        let binding = oauth_binding(root, "docs", "https://docs.test/mcp", &auth).unwrap();
        let credential = PluginApiKeyCredential::OAuthClientSecret {
            secret: "oauth-private-secret".into(),
        };
        store
            .save(
                SERVICE,
                &binding,
                &serde_json::to_string(&credential).unwrap(),
            )
            .unwrap();
        let loaded =
            load_oauth_secret_in(&store, root, "docs", "https://docs.test/mcp", &auth).unwrap();
        assert_eq!(loaded.as_str(), "oauth-private-secret");
        assert!(!format!("{loaded:?}").contains("oauth-private-secret"));
        let mut changed = auth.clone();
        changed["client"]["clientId"] = serde_json::json!("different-client");
        assert!(
            load_oauth_secret_in(&store, root, "docs", "https://docs.test/mcp", &changed).is_err()
        );
    }

    #[test]
    fn registered_private_keys_remain_scoped_and_debug_redacted() {
        let store = MockKeyringStore::default();
        let root = Path::new("/tmp/jwt-plugin");
        let auth = serde_json::json!({"type":"oauth","client":{"mode":"provided","clientId":"docs","tokenEndpointAuthMethod":"private_key_jwt"}});
        let binding = oauth_binding(root, "docs", "https://docs.test/mcp", &auth).unwrap();
        let credential = PluginApiKeyCredential::OAuthPrivateKeyJwt {
            private_key: "test-only-key".into(),
            algorithm: "RS256".into(),
            key_id: Some("registered".into()),
        };
        assert!(!format!("{credential:?}").contains("test-only-key"));
        store
            .save(
                SERVICE,
                &binding,
                &serde_json::to_string(&credential).unwrap(),
            )
            .unwrap();
        let (key, algorithm, key_id) =
            load_oauth_private_key_in(&store, root, "docs", "https://docs.test/mcp", &auth)
                .unwrap();
        assert_eq!(key.as_str(), "test-only-key");
        assert_eq!(algorithm, "RS256");
        assert_eq!(key_id.as_deref(), Some("registered"));
        assert!(
            load_oauth_private_key_in(&store, root, "docs", "https://other.test/mcp", &auth)
                .is_err()
        );
    }
}
