use super::PluginMcpConfigParseOutcome;
use super::PluginMcpServerAuthInfo;
use super::PluginMcpServerParseError;
use crate::plugin_api_key::{
    PluginApiKeyDeclaration, plugin_api_key_header, plugin_mcp_credential_configured,
    plugin_oauth_client_secret, plugin_oauth_private_key_jwt,
};
use codex_config::McpOAuthClientRegistrationMode;
use codex_config::McpOAuthTokenEndpointAuthMethod;
use codex_config::McpServerConfig;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map as JsonMap;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use url::Host;

// Published Agent Plugins v1 MCP schema:
// https://github.com/agentplugins/agent-plugins-spec/blob/main/schemas/1.0.0/mcp.schema.json
const AGENT_PLUGIN_MCP_SCHEMA_URI: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";
const SUPPORTED_AGENT_PLUGIN_MCP_SCHEMA_URIS: &[&str] = &[AGENT_PLUGIN_MCP_SCHEMA_URI];
const PLUGIN_ROOT_VARIABLE: &str = "PLUGIN_ROOT";
const PLUGIN_DATA_VARIABLE: &str = "PLUGIN_DATA";
const CLIENT_OWNED_HTTP_HEADERS: &[&str] = &[
    "accept",
    "authorization",
    "connection",
    "content-encoding",
    "content-length",
    "content-type",
    "host",
    "last-event-id",
    "mcp-protocol-version",
    "mcp-session-id",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentPluginMcpFile {
    #[serde(rename = "$schema")]
    schema: String,
    mcp_servers: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum AgentPluginMcpServer {
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        cwd: Option<String>,
        #[serde(default)]
        extensions: Option<AgentPluginMcpExtensions>,
    },
    #[serde(rename = "streamable-http")]
    StreamableHttp {
        url: String,
        headers: Option<BTreeMap<String, String>>,
        #[serde(default)]
        extensions: Option<AgentPluginMcpExtensions>,
    },
    #[serde(rename = "sse")]
    Sse {
        #[serde(rename = "url")]
        _url: String,
        #[serde(rename = "headers")]
        _headers: Option<BTreeMap<String, String>>,
        #[serde(default)]
        _extensions: Option<AgentPluginMcpExtensions>,
    },
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AgentPluginMcpExtensions {
    #[serde(rename = "com.openai")]
    com_openai: Option<AgentPluginOpenAiMcpExtensions>,
    #[serde(flatten)]
    _other_extensions: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentPluginOpenAiMcpExtensions {
    auth: Option<AgentPluginMcpAuth>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentPluginMcpAuth {
    #[serde(rename = "type")]
    auth_type: String,
    client: Option<AgentPluginMcpOAuthClient>,
    authorization_url: Option<String>,
    token_url: Option<String>,
    registration_url: Option<String>,
    authorization_server_base: Option<String>,
    resource: Option<String>,
    base_scopes: Option<Vec<String>>,
    default_scopes: Option<Vec<String>>,
    oidc_enabled: Option<bool>,
    oidc_configuration_url: Option<String>,
    oidc_userinfo_endpoint: Option<String>,
    oidc_scopes_supported: Option<Vec<String>>,
    header_scheme: Option<String>,
    header_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentPluginMcpOAuthClient {
    mode: String,
    client_id: Option<String>,
    token_endpoint_auth_method: Option<String>,
}

/// Translates an Agent Plugins `mcp.json` into Codex MCP configuration.
pub fn parse_agent_plugin_mcp_config(
    plugin_root: &Path,
    plugin_data_root: &Path,
    contents: &str,
) -> Result<PluginMcpConfigParseOutcome, serde_json::Error> {
    parse_agent_plugin_mcp_config_from(contents, plugin_root, plugin_data_root)
}

fn parse_agent_plugin_mcp_config_from(
    contents: &str,
    plugin_root: &Path,
    plugin_data_root: &Path,
) -> Result<PluginMcpConfigParseOutcome, serde_json::Error> {
    let AgentPluginMcpFile {
        schema,
        mcp_servers,
    } = serde_json::from_str(contents)?;
    if !SUPPORTED_AGENT_PLUGIN_MCP_SCHEMA_URIS.contains(&schema.as_str()) {
        return Err(plugin_mcp_json_error(format!(
            "unsupported Agent Plugins MCP schema `{schema}`; supported schemas: {}",
            SUPPORTED_AGENT_PLUGIN_MCP_SCHEMA_URIS.join(", ")
        )));
    }

    let mut outcome = PluginMcpConfigParseOutcome::default();
    for (name, value) in mcp_servers {
        let raw_auth_descriptor = value.pointer("/extensions/com.openai/auth").cloned();
        let auth_descriptor = sanitized_auth_descriptor(&value);
        if let Some(auth_descriptor) = auth_descriptor.clone() {
            let credential_configured = raw_auth_descriptor.as_ref().is_some_and(|auth| {
                requires_stored_credential(auth)
                    && value
                        .get("url")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|url| {
                            plugin_mcp_credential_configured(plugin_root, &name, url, auth)
                                .unwrap_or(false)
                        })
            });
            outcome.auth.push(PluginMcpServerAuthInfo {
                name: name.clone(),
                auth_descriptor: Some(auth_descriptor),
                credential_configured,
            });
        }
        match normalize_agent_plugin_mcp_server(
            &name,
            value,
            plugin_root,
            plugin_data_root,
            raw_auth_descriptor.as_ref(),
        ) {
            Ok(config) => {
                outcome.servers.insert(name, config);
            }
            Err(message) => outcome.errors.push(PluginMcpServerParseError {
                name,
                message,
                auth_descriptor,
            }),
        }
    }
    Ok(outcome)
}

fn requires_stored_credential(auth: &JsonValue) -> bool {
    match auth.get("type").and_then(JsonValue::as_str) {
        Some("api_key") => true,
        Some("oauth" | "mixed") => {
            auth.pointer("/client/mode").and_then(JsonValue::as_str) == Some("provided")
                && matches!(
                    auth.pointer("/client/tokenEndpointAuthMethod")
                        .and_then(JsonValue::as_str),
                    Some("client_secret_basic" | "client_secret_post" | "private_key_jwt")
                )
        }
        _ => false,
    }
}

fn sanitized_auth_descriptor(value: &JsonValue) -> Option<JsonValue> {
    let descriptor = value.pointer("/extensions/com.openai/auth")?.clone();
    let mut auth = serde_json::from_value::<AgentPluginMcpAuth>(descriptor).ok()?;
    for endpoint in [
        &mut auth.authorization_url,
        &mut auth.token_url,
        &mut auth.registration_url,
        &mut auth.authorization_server_base,
        &mut auth.resource,
        &mut auth.oidc_configuration_url,
        &mut auth.oidc_userinfo_endpoint,
    ] {
        if let Some(endpoint) = endpoint {
            let mut parsed = url::Url::parse(endpoint).ok()?;
            if !parsed.username().is_empty() || parsed.password().is_some() {
                parsed.set_username("").ok()?;
                parsed.set_password(None).ok()?;
            }
            // Auth metadata is display-only for unsupported flows. Drop all query
            // and fragment data so tokens and provider-specific secrets cannot escape.
            parsed.set_query(None);
            parsed.set_fragment(None);
            *endpoint = parsed.to_string();
        }
    }
    let mut descriptor = serde_json::to_value(auth).ok()?;
    if let Some(server_url) = value.get("url").and_then(JsonValue::as_str)
        && let Ok(mut parsed) = url::Url::parse(server_url)
    {
        if !parsed.username().is_empty() || parsed.password().is_some() {
            parsed.set_username("").ok()?;
            parsed.set_password(None).ok()?;
        }
        parsed.set_query(None);
        parsed.set_fragment(None);
        descriptor.as_object_mut()?.insert(
            "serverUrl".to_string(),
            JsonValue::String(parsed.to_string()),
        );
    }
    Some(descriptor)
}

fn normalize_agent_plugin_mcp_server(
    server_name: &str,
    value: JsonValue,
    plugin_root: &Path,
    plugin_data_root: &Path,
    auth_descriptor: Option<&JsonValue>,
) -> Result<McpServerConfig, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Agent Plugins MCP server must be an object".to_string())?;
    match object.get("type").and_then(JsonValue::as_str) {
        Some("stdio") => reject_explicit_null(object, "cwd")?,
        Some("streamable-http" | "sse") => reject_explicit_null(object, "headers")?,
        _ => {}
    }
    let server =
        serde_json::from_value::<AgentPluginMcpServer>(value).map_err(|err| err.to_string())?;
    let (mut object, extensions) = match server {
        AgentPluginMcpServer::Stdio {
            command,
            args,
            env,
            cwd,
            extensions,
        } => (
            normalize_agent_plugin_stdio_server(
                command,
                args,
                env,
                cwd,
                plugin_root,
                plugin_data_root,
            )?,
            extensions,
        ),
        AgentPluginMcpServer::StreamableHttp {
            url,
            headers,
            extensions,
        } => (
            normalize_agent_plugin_http_server(url, headers)?,
            extensions,
        ),
        AgentPluginMcpServer::Sse { .. } => {
            return Err("Agent Plugins legacy SSE transport is not supported by Codex".to_string());
        }
    };
    apply_agent_plugin_auth(
        server_name,
        plugin_root,
        &mut object,
        extensions,
        auth_descriptor,
    )?;
    serde_json::from_value(JsonValue::Object(object)).map_err(|err| err.to_string())
}

fn apply_agent_plugin_auth(
    server_name: &str,
    plugin_root: &Path,
    server: &mut JsonMap<String, JsonValue>,
    extensions: Option<AgentPluginMcpExtensions>,
    auth_descriptor: Option<&JsonValue>,
) -> Result<(), String> {
    let Some(auth) = extensions
        .and_then(|extensions| extensions.com_openai)
        .and_then(|extensions| extensions.auth)
    else {
        return Ok(());
    };

    match auth.auth_type.as_str() {
        "api_key" => {
            if auth.client.is_some()
                || auth.authorization_url.is_some()
                || auth.token_url.is_some()
                || auth.registration_url.is_some()
                || auth.authorization_server_base.is_some()
                || auth.resource.is_some()
                || auth.base_scopes.is_some()
                || auth.default_scopes.is_some()
                || auth.oidc_enabled.is_some()
                || auth.oidc_configuration_url.is_some()
                || auth.oidc_userinfo_endpoint.is_some()
                || auth.oidc_scopes_supported.is_some()
            {
                return Err("API-key auth cannot include OAuth client, endpoint, resource, or scope settings".to_string());
            }
            let url = server
                .get("url")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    "API-key authentication is only supported for HTTP MCP servers".to_string()
                })?;
            let declaration = PluginApiKeyDeclaration {
                url: url.to_string(),
                header_scheme: auth
                    .header_scheme
                    .clone()
                    .ok_or_else(|| "API-key auth requires headerScheme".to_string())?,
                header_name: auth.header_name.clone(),
            };
            let (header_name, header_value) =
                plugin_api_key_header(plugin_root, server_name, &declaration)?;
            let headers = server
                .entry("http_headers".to_string())
                .or_insert_with(|| JsonValue::Object(JsonMap::new()))
                .as_object_mut()
                .ok_or_else(|| "Invalid normalized API-key headers".to_string())?;
            headers.retain(|name, _| !name.eq_ignore_ascii_case(&header_name));
            headers.insert(header_name, JsonValue::String(header_value));
            server.insert("auth".to_string(), JsonValue::String("none".to_string()));
        }
        "none" => {
            if auth.client.is_some()
                || auth.authorization_url.is_some()
                || auth.token_url.is_some()
                || auth.registration_url.is_some()
                || auth.authorization_server_base.is_some()
                || auth.resource.is_some()
                || auth.base_scopes.is_some()
                || auth.default_scopes.is_some()
                || auth.oidc_enabled.is_some()
                || auth.oidc_configuration_url.is_some()
                || auth.oidc_userinfo_endpoint.is_some()
                || auth.oidc_scopes_supported.is_some()
                || auth.header_scheme.is_some()
                || auth.header_name.is_some()
            {
                return Err("auth.type `none` cannot include OAuth or API-key settings".to_string());
            }
            // Stdio has no HTTP authentication policy in the runtime config.
            // Omitting `auth = none` preserves its meaning (credentials are not
            // used) without asking the shared MCP config parser to accept HTTP
            // auth fields on a stdio transport.
            if server.contains_key("url") {
                server.insert("auth".to_string(), JsonValue::String("none".to_string()));
            }
        }
        "oauth" | "mixed" => {
            if !server.contains_key("url") {
                return Err(
                    "OAuth authentication is only supported for HTTP MCP servers".to_string(),
                );
            }
            if auth.header_scheme.is_some() || auth.header_name.is_some() {
                return Err("OAuth auth cannot include API-key header settings".to_string());
            }
            let mut oauth = JsonMap::new();
            let mut use_oauth_authentication = true;
            let mut client_registration = None;
            if let Some(client) = auth.client {
                match client.mode.as_str() {
                    "provided" => {
                        let client_id = client
                            .client_id
                            .filter(|client_id| !client_id.trim().is_empty())
                            .ok_or_else(|| {
                                "OAuth provided client requires a non-empty clientId".to_string()
                            })?;
                        if let Some(method) = client.token_endpoint_auth_method.as_deref() {
                            let method = match method {
                                "none" => McpOAuthTokenEndpointAuthMethod::None,
                                "client_secret_basic" => {
                                    McpOAuthTokenEndpointAuthMethod::ClientSecretBasic
                                }
                                "client_secret_post" => {
                                    McpOAuthTokenEndpointAuthMethod::ClientSecretPost
                                }
                                "private_key_jwt" => McpOAuthTokenEndpointAuthMethod::PrivateKeyJwt,
                                unsupported => {
                                    return Err(format!(
                                        "OAuth token endpoint auth method `{unsupported}` is not supported"
                                    ));
                                }
                            };
                            oauth.insert(
                                "client_token_endpoint_auth_method".to_string(),
                                serde_json::to_value(method)
                                    .expect("token endpoint method serializes"),
                            );
                            match method {
                                McpOAuthTokenEndpointAuthMethod::ClientSecretBasic
                                | McpOAuthTokenEndpointAuthMethod::ClientSecretPost => {
                                    let descriptor = auth_descriptor.ok_or_else(|| {
                                        "OAuth client secret setup requires a valid auth declaration"
                                            .to_string()
                                    })?;
                                    let url = server
                                        .get("url")
                                        .and_then(JsonValue::as_str)
                                        .ok_or_else(|| {
                                            "OAuth client secret is only supported for HTTP MCP servers"
                                                .to_string()
                                        })?;
                                    match plugin_oauth_client_secret(
                                        plugin_root,
                                        server_name,
                                        url,
                                        descriptor,
                                    ) {
                                        Ok(secret) => {
                                            oauth.insert(
                                                "client_secret".to_string(),
                                                JsonValue::String(secret.as_str().to_string()),
                                            );
                                        }
                                        Err(_) if auth.auth_type == "mixed" => {
                                            // Mixed packages promise an unauthenticated public
                                            // surface as well as OAuth. Keep that surface usable
                                            // until the user configures the confidential client.
                                            use_oauth_authentication = false;
                                        }
                                        Err(error) => return Err(error),
                                    }
                                }
                                McpOAuthTokenEndpointAuthMethod::PrivateKeyJwt => {
                                    let descriptor = auth_descriptor.ok_or_else(|| {
                                        "OAuth private-key JWT setup requires a valid auth declaration"
                                            .to_string()
                                    })?;
                                    let url = server
                                        .get("url")
                                        .and_then(JsonValue::as_str)
                                        .ok_or_else(|| {
                                            "OAuth private-key JWT is only supported for HTTP MCP servers"
                                                .to_string()
                                        })?;
                                    match plugin_oauth_private_key_jwt(
                                        plugin_root,
                                        server_name,
                                        url,
                                        descriptor,
                                    ) {
                                        Ok((private_key, algorithm, key_id)) => {
                                            oauth.insert(
                                                "private_key_jwt".to_string(),
                                                serde_json::json!({
                                                    "private_key": private_key.as_str(),
                                                    "algorithm": algorithm,
                                                    "key_id": key_id,
                                                }),
                                            );
                                        }
                                        Err(_) if auth.auth_type == "mixed" => {
                                            use_oauth_authentication = false;
                                        }
                                        Err(error) => return Err(error),
                                    }
                                }
                                _ => {}
                            }
                        }
                        oauth.insert("client_id".to_string(), JsonValue::String(client_id));
                    }
                    "dcr" => {
                        if client.client_id.is_some() || client.token_endpoint_auth_method.is_some()
                        {
                            return Err(
                                "OAuth dcr client cannot include clientId or tokenEndpointAuthMethod"
                                    .to_string(),
                            );
                        }
                        client_registration = Some(McpOAuthClientRegistrationMode::Dcr);
                    }
                    "cimd" => {
                        if client.client_id.is_some() || client.token_endpoint_auth_method.is_some()
                        {
                            return Err(
                                "OAuth cimd client cannot include clientId or tokenEndpointAuthMethod"
                                    .to_string(),
                            );
                        }
                        // CIMD is a preference, with DCR fallback when Brr has no
                        // host-owned native client metadata document configured.
                        client_registration = Some(McpOAuthClientRegistrationMode::Auto);
                    }
                    mode => {
                        return Err(format!(
                            "OAuth client mode `{mode}` must be provided, dcr, or cimd"
                        ));
                    }
                }
            }
            for (field, scopes) in [
                ("baseScopes", auth.base_scopes.as_ref()),
                ("defaultScopes", auth.default_scopes.as_ref()),
            ] {
                if scopes.is_some_and(|scopes| scopes.iter().any(|scope| scope.trim().is_empty())) {
                    return Err(format!("OAuth {field} must not contain empty values"));
                }
            }
            if let Some(scopes) = auth.base_scopes {
                oauth.insert(
                    "base_scopes".to_string(),
                    JsonValue::Array(scopes.into_iter().map(JsonValue::String).collect()),
                );
            }
            if let Some(scopes) = auth.default_scopes {
                oauth.insert(
                    "default_scopes".to_string(),
                    JsonValue::Array(scopes.into_iter().map(JsonValue::String).collect()),
                );
            }
            for (key, value) in [
                ("authorization_server_base", auth.authorization_server_base),
                ("authorization_url", auth.authorization_url),
                ("token_url", auth.token_url),
                ("registration_url", auth.registration_url),
                ("oidc_configuration_url", auth.oidc_configuration_url),
                ("oidc_userinfo_endpoint", auth.oidc_userinfo_endpoint),
            ] {
                if let Some(value) = value {
                    oauth.insert(key.to_string(), JsonValue::String(value));
                }
            }
            if let Some(enabled) = auth.oidc_enabled {
                oauth.insert("oidc_enabled".to_string(), JsonValue::Bool(enabled));
            }
            if let Some(scopes) = auth.oidc_scopes_supported {
                oauth.insert(
                    "oidc_scopes_supported".to_string(),
                    JsonValue::Array(scopes.into_iter().map(JsonValue::String).collect()),
                );
            }
            if let Some(registration) = client_registration {
                oauth.insert(
                    "client_registration".to_string(),
                    serde_json::to_value(registration).expect("registration mode serializes"),
                );
            }
            if let Some(resource) = auth.resource {
                let parsed = url::Url::parse(&resource)
                    .map_err(|_| "OAuth resource must be an absolute URL".to_string())?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(
                        "OAuth resource must be an HTTP(S) URL without credentials or fragment"
                            .to_string(),
                    );
                }
                server.insert("oauth_resource".to_string(), JsonValue::String(resource));
            }
            if use_oauth_authentication {
                server.insert("auth".to_string(), JsonValue::String("oauth".to_string()));
                if !oauth.is_empty() {
                    server.insert("oauth".to_string(), JsonValue::Object(oauth));
                }
            } else {
                server.insert("auth".to_string(), JsonValue::String("none".to_string()));
            }
        }
        unsupported => {
            return Err(format!(
                "auth.type `{unsupported}` is not supported by this runtime; the server is disabled instead of dropping its authentication requirement"
            ));
        }
    }
    Ok(())
}

fn normalize_agent_plugin_stdio_server(
    mut command: String,
    mut args: Vec<String>,
    mut env: BTreeMap<String, String>,
    cwd: Option<String>,
    plugin_root: &Path,
    plugin_data_root: &Path,
) -> Result<JsonMap<String, JsonValue>, String> {
    #[cfg(windows)]
    let has_windows_path_prefix = matches!(
        Path::new(&command).components().next(),
        Some(std::path::Component::Prefix(_))
    );
    #[cfg(not(windows))]
    let has_windows_path_prefix = false;
    let is_bare_command = !command.is_empty()
        && !command.contains('/')
        && !command.contains('\\')
        && !has_windows_path_prefix;
    let is_plugin_relative_command =
        command.starts_with("./") && is_portable_relative_path(&command);
    if !is_bare_command && !is_plugin_relative_command {
        return Err(
            "Agent Plugins stdio command must be a bare executable name or a contained `./` path"
                .to_string(),
        );
    }
    for reserved in [PLUGIN_ROOT_VARIABLE, PLUGIN_DATA_VARIABLE] {
        if env
            .keys()
            .any(|name| environment_variable_names_match(name, reserved))
        {
            return Err(format!(
                "Agent Plugins stdio `env` cannot override reserved variable `{reserved}`"
            ));
        }
    }
    #[cfg(windows)]
    {
        let mut normalized_env = BTreeMap::new();
        for (name, value) in env {
            let normalized_name = name.to_ascii_uppercase();
            if normalized_env.insert(normalized_name, value).is_some() {
                return Err(format!(
                    "duplicate case-insensitive Agent Plugins environment variable `{name}`"
                ));
            }
        }
        env = normalized_env;
    }

    let root_path = absolute_plugin_path(plugin_root)?;
    let data_root_path = absolute_plugin_path(plugin_data_root)?;
    let root = host_path_string(&root_path);
    let data_root = host_path_string(&data_root_path);
    if command.starts_with("./") {
        command = host_path_string(&resolve_contained_host_path(
            &command, &root_path, &root_path,
        )?);
    }
    for arg in &mut args {
        *arg = expand_agent_plugin_placeholders(arg, &root, &data_root);
    }
    for value in env.values_mut() {
        *value = expand_agent_plugin_placeholders(value, &root, &data_root);
    }

    let cwd = cwd.as_deref().unwrap_or("${PLUGIN_ROOT}");
    let Some(cwd_root) = parse_agent_plugin_cwd(cwd) else {
        return Err(
            "Agent Plugins stdio `cwd` must be a contained `./`, `${PLUGIN_ROOT}`, or `${PLUGIN_DATA}` path"
                .to_string(),
        );
    };
    let cwd = expand_agent_plugin_placeholders(cwd, &root, &data_root);
    let cwd_root = match cwd_root {
        AgentPluginCwdRoot::Package => &root_path,
        AgentPluginCwdRoot::Data => &data_root_path,
    };
    env.insert(PLUGIN_ROOT_VARIABLE.to_string(), root);
    env.insert(PLUGIN_DATA_VARIABLE.to_string(), data_root);

    Ok(JsonMap::from_iter([
        ("command".to_string(), JsonValue::String(command)),
        (
            "args".to_string(),
            JsonValue::Array(args.into_iter().map(JsonValue::String).collect()),
        ),
        ("env".to_string(), string_map_value(env)),
        (
            "cwd".to_string(),
            JsonValue::String(host_path_string(&resolve_contained_host_path(
                &cwd, cwd_root, cwd_root,
            )?)),
        ),
    ]))
}

fn reject_explicit_null(object: &JsonMap<String, JsonValue>, field: &str) -> Result<(), String> {
    if object.get(field).is_some_and(JsonValue::is_null) {
        return Err(format!(
            "Agent Plugins MCP `{field}` must use its declared type when present"
        ));
    }
    Ok(())
}

fn environment_variable_names_match(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn normalize_agent_plugin_http_server(
    url: String,
    mut headers: Option<BTreeMap<String, String>>,
) -> Result<JsonMap<String, JsonValue>, String> {
    validate_agent_plugin_url(&url)?;
    if let Some(configured_headers) = headers.as_mut() {
        validate_agent_plugin_headers(configured_headers)?;
        configured_headers.retain(|name, _| {
            !CLIENT_OWNED_HTTP_HEADERS
                .iter()
                .any(|owned| name.eq_ignore_ascii_case(owned))
        });
    }
    let mut object = JsonMap::from_iter([("url".to_string(), JsonValue::String(url))]);
    if let Some(headers) = headers.filter(|headers| !headers.is_empty()) {
        object.insert("http_headers".to_string(), string_map_value(headers));
    }
    Ok(object)
}

fn validate_agent_plugin_url(raw_url: &str) -> Result<(), String> {
    if raw_url.is_empty() {
        return Err("Agent Plugins HTTP server requires a non-empty `url`".to_string());
    }
    let parsed = url::Url::parse(raw_url)
        .map_err(|err| format!("invalid Agent Plugins MCP URL `{raw_url}`: {err}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Agent Plugins MCP URL must be absolute HTTP or HTTPS".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        return Err(
            "Agent Plugins MCP URL must not contain user information or a fragment".to_string(),
        );
    }
    let is_loopback = match parsed.host() {
        Some(Host::Domain(host)) => host == "localhost",
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if parsed.scheme() == "http" && !is_loopback {
        return Err("non-loopback Agent Plugins MCP endpoints must use HTTPS".to_string());
    }
    Ok(())
}

fn validate_agent_plugin_headers(headers: &BTreeMap<String, String>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for (name, value) in headers {
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(format!(
                "duplicate case-insensitive Agent Plugins HTTP header `{name}`"
            ));
        }
        if !is_valid_http_header_name(name) {
            return Err(format!("invalid Agent Plugins HTTP header name `{name}`"));
        }
        if value
            .bytes()
            .any(|byte| (byte < 32 && byte != b'\t') || byte == 127)
        {
            return Err(format!(
                "invalid Agent Plugins HTTP header value for `{name}`"
            ));
        }
    }
    Ok(())
}

fn string_map_value(values: BTreeMap<String, String>) -> JsonValue {
    JsonValue::Object(
        values
            .into_iter()
            .map(|(name, value)| (name, JsonValue::String(value)))
            .collect(),
    )
}

#[derive(Clone, Copy, Debug)]
enum AgentPluginCwdRoot {
    Package,
    Data,
}

fn parse_agent_plugin_cwd(value: &str) -> Option<AgentPluginCwdRoot> {
    if value == "./" {
        return Some(AgentPluginCwdRoot::Package);
    }
    if let Some(relative) = value.strip_prefix("./")
        && is_portable_path_suffix(relative)
    {
        return Some(AgentPluginCwdRoot::Package);
    }
    for (placeholder, root) in [
        ("${PLUGIN_ROOT}", AgentPluginCwdRoot::Package),
        ("${PLUGIN_DATA}", AgentPluginCwdRoot::Data),
    ] {
        if value == placeholder {
            return Some(root);
        }
        if let Some(relative) = value.strip_prefix(&format!("{placeholder}/"))
            && (relative.is_empty() || is_portable_path_suffix(relative))
        {
            return Some(root);
        }
    }
    None
}

fn expand_agent_plugin_placeholders(value: &str, plugin_root: &str, plugin_data: &str) -> String {
    const ROOT: &str = "${PLUGIN_ROOT}";
    const DATA: &str = "${PLUGIN_DATA}";
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    loop {
        let next = match (remaining.find(ROOT), remaining.find(DATA)) {
            (Some(root), Some(data)) if root <= data => Some((root, ROOT, plugin_root)),
            (Some(_), Some(data)) => Some((data, DATA, plugin_data)),
            (Some(root), None) => Some((root, ROOT, plugin_root)),
            (None, Some(data)) => Some((data, DATA, plugin_data)),
            (None, None) => None,
        };
        let Some((index, placeholder, replacement)) = next else {
            output.push_str(remaining);
            break;
        };
        output.push_str(&remaining[..index]);
        output.push_str(replacement);
        remaining = &remaining[index + placeholder.len()..];
    }
    output
}

fn absolute_plugin_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|err| format!("failed to resolve plugin path: {err}"))
    }?;
    resolve_existing_path_prefix(&absolute)
}

fn resolve_contained_host_path(
    value: &str,
    root: &Path,
    allowed_root: &Path,
) -> Result<PathBuf, String> {
    let value = Path::new(value);
    let path = if value.is_absolute() {
        value.to_path_buf()
    } else {
        root.join(value)
    };
    let path = resolve_existing_path_prefix(&path)?;
    if !path.starts_with(allowed_root) {
        return Err(format!(
            "expanded path `{}` must remain within `{}`",
            value.display(),
            allowed_root.display()
        ));
    }
    Ok(path)
}

fn resolve_existing_path_prefix(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path.to_path_buf();
    let mut missing_components = Vec::<OsString>::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut resolved) => {
                for component in missing_components.iter().rev() {
                    resolved.push(component);
                }
                return Ok(lexical_normalize(&resolved));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&existing)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    return Err(format!(
                        "failed to resolve symlinked path `{}`",
                        path.display()
                    ));
                }
                let Some(component) = existing.components().next_back() else {
                    return Err(format!(
                        "failed to resolve path `{}`: {err}",
                        path.display()
                    ));
                };
                if matches!(
                    component,
                    std::path::Component::Prefix(_) | std::path::Component::RootDir
                ) {
                    return Err(format!(
                        "failed to resolve path `{}`: {err}",
                        path.display()
                    ));
                }
                missing_components.push(component.as_os_str().to_os_string());
                if !existing.pop() {
                    return Err(format!(
                        "failed to resolve path `{}`: {err}",
                        path.display()
                    ));
                }
            }
            Err(err) => {
                return Err(format!(
                    "failed to resolve path `{}`: {err}",
                    path.display()
                ));
            }
        }
    }
}

fn host_path_string(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    #[cfg(windows)]
    if let Some(path) = rendered.strip_prefix(r"\\?\") {
        return path
            .strip_prefix(r"UNC\")
            .map(|path| format!(r"\\{path}"))
            .unwrap_or_else(|| path.to_string());
    }
    rendered.into_owned()
}

fn is_portable_relative_path(value: &str) -> bool {
    value
        .strip_prefix("./")
        .is_some_and(is_portable_path_suffix)
}

fn is_portable_path_suffix(value: &str) -> bool {
    !value.is_empty() && !value.contains('\\')
}

fn is_valid_http_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn plugin_mcp_json_error(message: impl Into<String>) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}
