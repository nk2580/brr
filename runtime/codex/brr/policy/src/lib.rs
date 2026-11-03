pub const LOGIN_REQUIRED_ERROR: &str = "Sign in with ChatGPT to start a coding session.";
pub const CUSTOM_ENDPOINT_ERROR: &str = "Brr does not support custom inference endpoints.";

pub fn validate_endpoints(openai: Option<&str>, chatgpt: Option<&str>) -> Result<(), &'static str> {
    const OPENAI_DEFAULT: &str = "https://api.openai.com/v1";
    const CHATGPT_DEFAULT: &str = "https://chatgpt.com/backend-api";

    let openai_is_default = openai
        .map(|endpoint| is_default_endpoint(endpoint, OPENAI_DEFAULT))
        .unwrap_or(true);
    let chatgpt_is_default = chatgpt
        .map(|endpoint| is_default_endpoint(endpoint, CHATGPT_DEFAULT))
        .unwrap_or(true);

    if openai_is_default && chatgpt_is_default {
        Ok(())
    } else {
        Err(CUSTOM_ENDPOINT_ERROR)
    }
}

fn is_default_endpoint(endpoint: &str, default: &str) -> bool {
    endpoint == default || endpoint == format!("{default}/")
}

pub fn enforce_startup_overrides(overrides: &mut Vec<String>) {
    overrides.extend([
        "forced_login_method=\"chatgpt\"".to_string(),
        "model_provider=\"openai\"".to_string(),
        "cli_auth_credentials_store=\"file\"".to_string(),
        "approval_policy=\"on-request\"".to_string(),
        "sandbox_mode=\"workspace-write\"".to_string(),
        "features.plugins=true".to_string(),
        "features.mcp_2026_07_28=true".to_string(),
        "features.codex_apps_mcp_2026_07_28=true".to_string(),
        "features.mcp_tool_list_changed=true".to_string(),
        "features.mcp_connection_sharing=true".to_string(),
        "features.default_mode_request_user_input=true".to_string(),
    ]);
}

pub fn validate_login_mode(mode: &str) -> Result<(), &'static str> {
    match mode {
        "chatgpt" | "chatgptDeviceCode" => Ok(()),
        _ => Err(LOGIN_REQUIRED_ERROR),
    }
}

pub fn validate_inference(auth_mode: Option<&str>, provider: &str) -> Result<(), &'static str> {
    if auth_mode == Some("chatgpt") && provider == "openai" {
        Ok(())
    } else {
        Err(LOGIN_REQUIRED_ERROR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_immutable_startup_overrides_after_caller_values() {
        let mut overrides = vec!["model='caller'".to_string()];

        enforce_startup_overrides(&mut overrides);

        assert_eq!(
            overrides,
            [
                "model='caller'",
                "forced_login_method=\"chatgpt\"",
                "model_provider=\"openai\"",
                "cli_auth_credentials_store=\"file\"",
                "approval_policy=\"on-request\"",
                "sandbox_mode=\"workspace-write\"",
                "features.plugins=true",
                "features.mcp_2026_07_28=true",
                "features.codex_apps_mcp_2026_07_28=true",
                "features.mcp_tool_list_changed=true",
                "features.mcp_connection_sharing=true",
                "features.default_mode_request_user_input=true",
            ]
        );
    }

    #[test]
    fn permits_only_interactive_chatgpt_login_modes() {
        assert_eq!(validate_login_mode("chatgpt"), Ok(()));
        assert_eq!(validate_login_mode("chatgptDeviceCode"), Ok(()));
        for mode in ["apiKey", "chatgptAuthTokens", "bedrock", ""] {
            assert_eq!(validate_login_mode(mode), Err(LOGIN_REQUIRED_ERROR));
        }
    }

    #[test]
    fn enables_plugins_and_modern_mcp_without_allowing_caller_opt_out() {
        let mut overrides = vec![
            "features.plugins=false".to_string(),
            "features.mcp_2026_07_28=false".to_string(),
        ];
        enforce_startup_overrides(&mut overrides);
        for key in [
            "plugins",
            "mcp_2026_07_28",
            "codex_apps_mcp_2026_07_28",
            "mcp_tool_list_changed",
            "mcp_connection_sharing",
            "default_mode_request_user_input",
        ] {
            let prefix = format!("features.{key}=");
            assert_eq!(
                overrides
                    .iter()
                    .rev()
                    .find(|value| value.starts_with(&prefix)),
                Some(&format!("features.{key}=true"))
            );
        }
    }

    #[test]
    fn permits_inference_only_with_chatgpt_auth_and_openai_provider() {
        assert_eq!(validate_inference(Some("chatgpt"), "openai"), Ok(()));
        for (auth_mode, provider) in [
            (None, "openai"),
            (Some("apiKey"), "openai"),
            (Some("chatgptAuthTokens"), "openai"),
            (Some("chatgpt"), "bedrock"),
        ] {
            assert_eq!(
                validate_inference(auth_mode, provider),
                Err(LOGIN_REQUIRED_ERROR)
            );
        }
    }

    #[test]
    fn accepts_only_default_inference_endpoints() {
        for (openai, chatgpt) in [
            (None, None),
            (
                Some("https://api.openai.com/v1"),
                Some("https://chatgpt.com/backend-api/"),
            ),
            (
                Some("https://api.openai.com/v1/"),
                Some("https://chatgpt.com/backend-api"),
            ),
            (None, Some("https://chatgpt.com/backend-api/")),
            (Some("https://api.openai.com/v1"), None),
        ] {
            assert_eq!(validate_endpoints(openai, chatgpt), Ok(()));
        }
        for (openai, chatgpt) in [
            (
                Some("https://evil.example/v1"),
                Some("https://chatgpt.com/backend-api/"),
            ),
            (
                Some("https://attacker@api.openai.com/v1"),
                Some("https://chatgpt.com/backend-api/"),
            ),
            (
                Some("https://api.openai.com:443/v1"),
                Some("https://chatgpt.com/backend-api/"),
            ),
            (
                Some("https://api.openai.com/v1#fragment"),
                Some("https://chatgpt.com/backend-api/"),
            ),
            (
                Some("https://api.openai.com/v1"),
                Some("https://evil.example/backend-api/"),
            ),
            (
                Some("https://api.openai.com/v1"),
                Some("https://user@chatgpt.com/backend-api/"),
            ),
        ] {
            assert_eq!(
                validate_endpoints(openai, chatgpt),
                Err(CUSTOM_ENDPOINT_ERROR)
            );
        }
    }
}
