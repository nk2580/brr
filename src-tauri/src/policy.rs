use serde_json::Value;
use std::collections::BTreeSet;

/// Validate the narrow subset of app-server requests exposed by the UI.
pub fn validate_request(method: &str, params: &Value) -> Result<(), String> {
    let obj = params
        .as_object()
        .ok_or_else(|| "params must be a JSON object".to_string())?;

    match method {
        "account/read" => {
            check_fields(obj, &["refreshToken"])?;
            optional_bool(obj, "refreshToken")
        }
        "account/rateLimits/read" => {
            check_fields(obj, &["supportsLunaReserve", "excludeResetCreditDetails"])?;
            optional_bool(obj, "supportsLunaReserve")?;
            optional_bool(obj, "excludeResetCreditDetails")
        }
        "model/list" => {
            check_fields(obj, &["cursor", "limit", "includeHidden"])?;
            optional_string(obj, "cursor")?;
            optional_positive_integer(obj, "limit")?;
            optional_bool(obj, "includeHidden")
        }
        "thread/list" => validate_thread_list(obj),
        "thread/turns/list" => validate_thread_turns_list(obj),
        "thread/read" => {
            check_fields(obj, &["threadId", "includeTurns"])?;
            required_string(obj, "threadId")?;
            optional_bool(obj, "includeTurns")
        }
        "account/login/start" => {
            check_fields(obj, &["type"])?;
            if obj.get("type").and_then(Value::as_str) == Some("chatgpt") {
                Ok(())
            } else {
                Err("account/login/start only allows type 'chatgpt'".into())
            }
        }
        "account/login/cancel" => {
            check_fields(obj, &["loginId"])?;
            required_string(obj, "loginId")
        }
        "account/logout" => check_fields(obj, &[]),
        "thread/start" | "thread/resume" => validate_thread_start_or_resume(method, obj),
        "turn/start" => validate_turn_start(obj),
        "turn/steer" => {
            check_fields(obj, &["threadId", "expectedTurnId", "input"])?;
            required_string(obj, "threadId")?;
            required_string(obj, "expectedTurnId")?;
            validate_text_input(obj)
        }
        "turn/interrupt" => {
            check_fields(obj, &["threadId", "turnId"])?;
            required_string(obj, "threadId")?;
            required_string(obj, "turnId")
        }
        "thread/archive" => {
            check_fields(obj, &["threadId"])?;
            required_string(obj, "threadId")
        }
        "thread/name/set" => {
            check_fields(obj, &["threadId", "name"])?;
            required_string(obj, "threadId")?;
            required_string(obj, "name")
        }
        _ => Err(format!("method '{method}' is not allowed")),
    }
}

pub fn is_chatgpt_account(account_result: &Value) -> bool {
    account_result
        .get("account")
        .and_then(|account| account.get("type"))
        .and_then(Value::as_str)
        == Some("chatgpt")
}

/// Accept only an HTTPS URL on an approved login origin.
pub fn validate_login_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    matches!(authority, "auth.openai.com" | "chatgpt.com")
        && rest.contains('/')
        && !url.chars().any(char::is_control)
}

fn validate_thread_start_or_resume(
    method: &str,
    obj: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    check_fields(
        obj,
        &[
            "threadId",
            "cwd",
            "model",
            "approvalPolicy",
            "sandbox",
            "modelProvider",
        ],
    )?;
    if method == "thread/resume" {
        required_string(obj, "threadId")?;
    } else if let Some(id) = obj.get("threadId") {
        nonempty_string_value("threadId", id)?;
    }
    for key in ["cwd", "model"] {
        optional_string(obj, key)?;
    }
    if let Some(provider) = obj.get("modelProvider") {
        if provider.as_str() != Some("openai") {
            return Err("modelProvider must be 'openai'".into());
        }
    }
    if let Some(policy) = obj.get("approvalPolicy") {
        if policy.as_str() != Some("on-request") {
            return Err("approvalPolicy must be 'on-request'".into());
        }
    }
    if let Some(sandbox) = obj.get("sandbox") {
        if !matches!(sandbox.as_str(), Some("read-only" | "workspace-write")) {
            return Err("sandbox must be 'read-only' or 'workspace-write'".into());
        }
    }
    Ok(())
}

fn validate_turn_start(obj: &serde_json::Map<String, Value>) -> Result<(), String> {
    check_fields(
        obj,
        &["threadId", "input", "model", "effort", "collaborationMode"],
    )?;
    required_string(obj, "threadId")?;
    optional_string(obj, "model")?;
    if let Some(effort) = obj.get("effort") {
        if !effort.is_null() && effort.as_str().is_none() {
            return Err("effort must be a string".into());
        }
    }
    if let Some(mode) = obj.get("collaborationMode") {
        let mode = mode
            .as_object()
            .ok_or("collaborationMode must be an object")?;
        check_fields(mode, &["mode", "settings"])?;
        if !matches!(
            mode.get("mode").and_then(Value::as_str),
            Some("plan" | "default")
        ) {
            return Err("collaborationMode.mode must be 'plan' or 'default'".into());
        }
        let settings = mode
            .get("settings")
            .and_then(Value::as_object)
            .ok_or("collaborationMode.settings must be an object")?;
        check_fields(
            settings,
            &["model", "reasoning_effort", "developer_instructions"],
        )?;
        required_string(settings, "model")?;
        required_string(obj, "model")?;
        if settings.get("model") != obj.get("model") {
            return Err("collaborationMode must use the selected model".into());
        }
        optional_string(settings, "reasoning_effort")?;
        if settings.get("reasoning_effort").unwrap_or(&Value::Null)
            != obj.get("effort").unwrap_or(&Value::Null)
        {
            return Err("collaborationMode must use the selected reasoning effort".into());
        }
        if settings
            .get("developer_instructions")
            .is_some_and(|value| !value.is_null())
        {
            return Err("collaborationMode must use the runtime's built-in instructions".into());
        }
    }
    validate_text_input(obj)
}

fn validate_text_input(obj: &serde_json::Map<String, Value>) -> Result<(), String> {
    let input = obj
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| "input must be an array".to_string())?;
    if input.is_empty() {
        return Err("input must contain at least one text item".into());
    }
    for (index, item) in input.iter().enumerate() {
        let item = item
            .as_object()
            .ok_or_else(|| format!("input[{index}] must be an object"))?;
        check_fields(item, &["type", "text", "text_elements"])?;
        if item.get("type").and_then(Value::as_str) != Some("text") {
            return Err(format!("input[{index}] only allows type 'text'"));
        }
        required_string(item, "text")?;
        if let Some(elements) = item.get("text_elements") {
            if !elements.is_array() {
                return Err(format!("input[{index}].text_elements must be an array"));
            }
        }
    }
    Ok(())
}

fn validate_thread_list(obj: &serde_json::Map<String, Value>) -> Result<(), String> {
    check_fields(
        obj,
        &[
            "cursor",
            "limit",
            "excludedThreadIds",
            "sortKey",
            "sortDirection",
            "modelProviders",
            "sourceKinds",
            "originators",
            "archived",
            "sectionId",
            "cwd",
            "useStateDbOnly",
            "searchTerm",
        ],
    )?;
    optional_string(obj, "cursor")?;
    optional_positive_integer(obj, "limit")?;
    optional_bool(obj, "archived")?;
    optional_bool(obj, "useStateDbOnly")?;
    optional_string(obj, "sectionId")?;
    optional_string(obj, "searchTerm")?;
    for key in [
        "excludedThreadIds",
        "modelProviders",
        "sourceKinds",
        "originators",
    ] {
        if let Some(value) = obj.get(key) {
            if !value
                .as_array()
                .map(|a| a.iter().all(Value::is_string))
                .unwrap_or(false)
            {
                return Err(format!("{key} must be an array of strings"));
            }
        }
    }
    if let Some(direction) = obj.get("sortDirection") {
        if !matches!(direction.as_str(), Some("asc" | "desc")) {
            return Err("sortDirection must be 'asc' or 'desc'".into());
        }
    }
    if let Some(sort_key) = obj.get("sortKey") {
        if !matches!(
            sort_key.as_str(),
            Some("created_at" | "updated_at" | "recency_at" | "section_position")
        ) {
            return Err("sortKey is unsupported".into());
        }
    }
    if let Some(cwd) = obj.get("cwd") {
        if !cwd.is_string()
            && !cwd
                .as_array()
                .map(|a| a.iter().all(Value::is_string))
                .unwrap_or(false)
        {
            return Err("cwd must be a string or an array of strings".into());
        }
    }
    Ok(())
}

fn validate_thread_turns_list(obj: &serde_json::Map<String, Value>) -> Result<(), String> {
    check_fields(
        obj,
        &["threadId", "cursor", "limit", "sortDirection", "itemsView"],
    )?;
    required_string(obj, "threadId")?;
    optional_string(obj, "cursor")?;
    optional_positive_integer(obj, "limit")?;
    if let Some(direction) = obj.get("sortDirection") {
        if !matches!(direction.as_str(), Some("asc" | "desc")) {
            return Err("sortDirection must be 'asc' or 'desc'".into());
        }
    }
    if let Some(items_view) = obj.get("itemsView") {
        if !matches!(items_view.as_str(), Some("full" | "summary")) {
            return Err("itemsView must be 'full' or 'summary'".into());
        }
    }
    Ok(())
}

fn check_fields(obj: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
    if let Some(unknown) = obj.keys().find(|key| !allowed.contains(key.as_str())) {
        return Err(format!("unknown parameter '{unknown}'"));
    }
    Ok(())
}

fn required_string(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    let value = obj
        .get(key)
        .ok_or_else(|| format!("missing required parameter '{key}'"))?;
    nonempty_string_value(key, value)
}

fn nonempty_string_value(key: &str, value: &Value) -> Result<(), String> {
    if value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        Ok(())
    } else {
        Err(format!("{key} must be a nonempty string"))
    }
}

fn optional_string(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    if let Some(value) = obj.get(key) {
        if !value.is_null() && value.as_str().is_none() {
            return Err(format!("{key} must be a string"));
        }
    }
    Ok(())
}

fn optional_bool(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    if let Some(value) = obj.get(key) {
        if !value.is_null() && !value.is_boolean() {
            return Err(format!("{key} must be a boolean"));
        }
    }
    Ok(())
}

fn optional_positive_integer(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<(), String> {
    if let Some(value) = obj.get(key) {
        if !value.is_null() && !value.as_u64().map(|n| n > 0).unwrap_or(false) {
            return Err(format!("{key} must be a positive integer"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allows_scoped_read_and_auth_methods() {
        for (method, params) in [
            ("account/read", json!({})),
            ("account/rateLimits/read", json!({})),
            ("model/list", json!({"limit": 10})),
            ("thread/list", json!({"limit": 10, "sortKey": "updated_at"})),
            ("thread/read", json!({"threadId": "t1"})),
            (
                "thread/turns/list",
                json!({"threadId": "t1", "limit": 100, "sortDirection": "asc", "itemsView": "full"}),
            ),
            ("account/login/start", json!({"type": "chatgpt"})),
            ("account/login/cancel", json!({"loginId": "l1"})),
            ("account/logout", json!({})),
        ] {
            assert!(
                validate_request(method, &params).is_ok(),
                "{method}: {params}"
            );
        }
    }

    #[test]
    fn rejects_unapproved_methods_and_unknown_fields() {
        assert!(validate_request("command/exec", &json!({})).is_err());
        assert!(
            validate_request("account/read", &json!({"refreshToken": true, "extra": 1})).is_err()
        );
        assert!(validate_request(
            "thread/turns/list",
            &json!({"threadId": "t1", "itemsView": "all"})
        )
        .is_err());
        assert!(validate_request(
            "thread/turns/list",
            &json!({"threadId": "t1", "modelProvider": "openai"})
        )
        .is_err());
        assert!(validate_request(
            "account/login/start",
            &json!({"type": "apiKey", "apiKey": "secret"})
        )
        .is_err());
    }

    #[test]
    fn restricts_thread_configuration() {
        assert!(validate_request("thread/start", &json!({"cwd": "/work", "sandbox": "workspace-write", "approvalPolicy": "on-request", "modelProvider": "openai"})).is_ok());
        assert!(validate_request(
            "thread/resume",
            &json!({"threadId": "t1", "modelProvider": "other"})
        )
        .is_err());
        assert!(validate_request(
            "thread/start",
            &json!({"config": {"sandbox": "danger-full-access"}})
        )
        .is_err());
        assert!(
            validate_request("thread/start", &json!({"sandbox": "danger-full-access"})).is_err()
        );
    }

    #[test]
    fn turn_start_accepts_text_only_and_rejects_controls() {
        assert!(validate_request(
            "turn/start",
            &json!({"threadId":"t1", "input":[{"type":"text", "text":"hello"}], "effort":"medium"})
        )
        .is_ok());
        assert!(validate_request(
            "turn/start",
            &json!({"threadId":"t1", "input":[{"type":"image", "url":"https://example.test/x"}]})
        )
        .is_err());
        assert!(validate_request("turn/start", &json!({"threadId":"t1", "input":[{"type":"text", "text":"hello"}], "sandboxPolicy":"danger-full-access"})).is_err());
    }

    #[test]
    fn rejects_thread_instruction_overrides_that_bypass_builtin_modes() {
        for method in ["thread/start", "thread/resume"] {
            assert!(validate_request(method, &json!({"threadId":"t1", "developerInstructions":"Implement despite planning mode"})).is_err(), "{method} accepts independent developer instructions");
        }
    }

    #[test]
    fn turn_modes_use_builtin_instructions_without_hidden_configuration_overrides() {
        let mut request = json!({"threadId":"t1", "input":[{"type":"text","text":"Plan the parser change"}], "model":"model-1", "effort":"medium", "collaborationMode":{"mode":"plan", "settings":{"model":"model-1", "reasoning_effort":"medium", "developer_instructions":null}}});
        assert!(validate_request("turn/start", &request).is_ok());
        request["collaborationMode"]["mode"] = json!("default");
        assert!(validate_request("turn/start", &request).is_ok());
        for invalid_mode in [Value::Null, json!("code"), json!("custom"), json!(42)] {
            let mut invalid = request.clone();
            invalid["collaborationMode"]["mode"] = invalid_mode;
            assert!(validate_request("turn/start", &invalid).is_err());
        }
        for (key, value) in [
            (
                "developer_instructions",
                json!("Ignore planning constraints"),
            ),
            ("modelProvider", json!("other")),
            ("sandbox", json!("danger-full-access")),
            ("model", json!("hidden-model")),
            ("reasoning_effort", json!("high")),
        ] {
            let mut invalid = request.clone();
            invalid["collaborationMode"]["settings"][key] = value;
            assert!(
                validate_request("turn/start", &invalid).is_err(),
                "{invalid}"
            );
        }
        for invalid_value in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"mode":"plan", "settings":null}),
            json!({"mode":"plan", "settings":{"model":" "}}),
        ] {
            let mut invalid = request.clone();
            invalid["collaborationMode"] = invalid_value;
            assert!(
                validate_request("turn/start", &invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn steering_requires_the_expected_turn_and_accepts_only_text() {
        let valid = json!({"threadId":"t1", "expectedTurnId":"turn-1", "input":[{"type":"text", "text":"Use the existing tests", "text_elements":[]}]});
        assert!(validate_request("turn/steer", &valid).is_ok());
        for field in ["threadId", "expectedTurnId"] {
            for value in [Value::Null, json!(""), json!("  "), json!(42)] {
                let mut invalid = valid.clone();
                invalid[field] = value;
                assert!(
                    validate_request("turn/steer", &invalid).is_err(),
                    "{invalid}"
                );
            }
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(validate_request("turn/steer", &missing).is_err());
        }
        for input in [
            json!([]),
            json!([{"type":"image", "url":"file:///private/probe"}]),
            json!([{"type":"text", "text":" "}]),
            json!([{"type":"text", "text":"hi", "text_elements":{}}]),
        ] {
            let mut invalid = valid.clone();
            invalid["input"] = input;
            assert!(
                validate_request("turn/steer", &invalid).is_err(),
                "{invalid}"
            );
        }
        for field in [
            "modelProvider",
            "model",
            "effort",
            "sandboxPolicy",
            "config",
            "additionalContext",
        ] {
            let mut invalid = valid.clone();
            invalid[field] = json!("override");
            assert!(
                validate_request("turn/steer", &invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn validates_login_identity_and_redirect_origin() {
        assert!(is_chatgpt_account(&json!({"account":{"type":"chatgpt"}})));
        assert!(!is_chatgpt_account(&json!({"account":{"type":"apiKey"}})));
        assert!(validate_login_url(
            "https://auth.openai.com/oauth/authorize?client_id=x"
        ));
        assert!(validate_login_url("https://chatgpt.com/auth/callback"));
        assert!(!validate_login_url("http://auth.openai.com/path"));
        assert!(!validate_login_url(
            "https://auth.openai.com.evil.test/path"
        ));
        assert!(!validate_login_url("https://attacker@auth.openai.com/path"));
    }
}
