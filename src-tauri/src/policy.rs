use serde_json::Value;
use std::collections::BTreeSet;

/// Validate the narrow subset of app-server requests exposed by the UI.
pub fn validate_request(method: &str, params: &Value) -> Result<(), String> {
    let obj = params
        .as_object()
        .ok_or_else(|| "params must be a JSON object".to_string())?;

    match method {
        "brr/worktree/inspect" => {
            check_fields(obj, &["cwd"])?;
            required_string(obj, "cwd")
        }
        "brr/worktree/approve" => {
            check_fields(obj, &["cwd", "templates"])?;
            required_string(obj, "cwd")?;
            let templates = obj
                .get("templates")
                .and_then(Value::as_array)
                .ok_or("templates must be an array of commands")?;
            if templates.is_empty()
                || !templates
                    .iter()
                    .all(|value| value.as_str().is_some_and(|value| !value.trim().is_empty()))
            {
                return Err("Select the setup commands to approve".into());
            }
            Ok(())
        }
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
        "skills/list" => {
            check_fields(obj, &["cwds", "forceReload"])?;
            optional_bool(obj, "forceReload")?;
            optional_string_array(obj, "cwds")
        }
        "skills/config/write" => {
            check_fields(obj, &["path", "name", "enabled"])?;
            if obj.contains_key("path") == obj.contains_key("name") {
                return Err("provide exactly one of path or name".into());
            }
            optional_string(obj, "path")?;
            optional_string(obj, "name")?;
            required_bool(obj, "enabled")
        }
        "hooks/list" => {
            check_fields(obj, &["cwds", "threadId"])?;
            optional_string_array(obj, "cwds")?;
            optional_string(obj, "threadId")
        }
        "marketplace/add" => {
            check_fields(obj, &["source", "refName", "sparsePaths"])?;
            required_string(obj, "source")?;
            optional_string(obj, "refName")?;
            optional_string_array(obj, "sparsePaths")
        }
        "marketplace/remove" => {
            check_fields(obj, &["marketplaceName"])?;
            required_string(obj, "marketplaceName")
        }
        "marketplace/upgrade" => {
            check_fields(obj, &["marketplaceName"])?;
            optional_string(obj, "marketplaceName")
        }
        "plugin/list" => {
            check_fields(obj, &["cwds", "marketplaceKinds", "forceRefetch"])?;
            optional_string_array(obj, "cwds")?;
            if let Some(kinds) = obj.get("marketplaceKinds") {
                validate_string_enum_array(
                    kinds,
                    &[
                        "local",
                        "vertical",
                        "workspace-directory",
                        "shared-with-me",
                        "created-by-me-remote",
                    ],
                    "marketplaceKinds",
                )?;
            }
            optional_bool(obj, "forceRefetch")
        }
        "plugin/search" => {
            check_fields(obj, &["searchTerm", "scope", "cwds", "cursor", "limit"])?;
            required_string(obj, "searchTerm")?;
            if let Some(scope) = obj.get("scope") {
                validate_enum(scope, &["global", "workspace", "personal"], "scope")?;
            }
            optional_string_array(obj, "cwds")?;
            optional_string(obj, "cursor")?;
            optional_positive_integer(obj, "limit")
        }
        "plugin/installed" => {
            check_fields(obj, &["cwds", "installSuggestionPluginNames"])?;
            optional_string_array(obj, "cwds")?;
            optional_string_array(obj, "installSuggestionPluginNames")
        }
        "plugin/reconcile" => {
            check_fields(obj, &["reason"])?;
            optional_string(obj, "reason")
        }
        "plugin/mcpCredential/write" => {
            check_fields(obj, &["pluginId", "serverName", "credential"])?;
            required_string(obj, "pluginId")?;
            required_string(obj, "serverName")?;
            match obj.get("credential") {
                Some(Value::Null) => Ok(()),
                Some(Value::Object(credential)) => {
                    match credential.get("kind").and_then(Value::as_str) {
                        Some("token") => {
                            check_fields(credential, &["kind", "token"])?;
                            required_string(credential, "token")
                        }
                        Some("basic") => {
                            check_fields(credential, &["kind", "username", "password"])?;
                            required_string(credential, "username")?;
                            if credential.get("password").and_then(Value::as_str).is_some() {
                                Ok(())
                            } else {
                                Err("password must be a string".into())
                            }
                        }
                        Some("oauth_client_secret") => {
                            check_fields(credential, &["kind", "secret"])?;
                            required_string(credential, "secret")
                        }
                        Some("oauth_private_key_jwt") => {
                            check_fields(
                                credential,
                                &["kind", "privateKey", "algorithm", "keyId"],
                            )?;
                            required_string(credential, "privateKey")?;
                            if credential["privateKey"]
                                .as_str()
                                .is_some_and(|key| key.len() > 65_536)
                            {
                                return Err(
                                    "Registered OAuth key must be no larger than 64 KB".into()
                                );
                            }
                            match credential.get("algorithm").and_then(Value::as_str) {
                                Some(
                                    "RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512"
                                    | "ES256" | "ES384" | "EdDSA",
                                ) => {}
                                _ => return Err("Use an asymmetric OAuth signing algorithm".into()),
                            }
                            optional_string(credential, "keyId")
                        }
                        _ => Err("Unsupported plugin credential kind".into()),
                    }
                }
                _ => Err("credential must be an object or null".into()),
            }
        }
        "plugin/read" => {
            check_fields(
                obj,
                &["marketplacePath", "remoteMarketplaceName", "pluginName"],
            )?;
            optional_string(obj, "marketplacePath")?;
            optional_string(obj, "remoteMarketplaceName")?;
            required_string(obj, "pluginName")
        }
        "plugin/skill/read" => {
            check_fields(
                obj,
                &["remoteMarketplaceName", "remotePluginId", "skillName"],
            )?;
            required_string(obj, "remoteMarketplaceName")?;
            required_string(obj, "remotePluginId")?;
            required_string(obj, "skillName")
        }
        "plugin/install" => {
            check_fields(
                obj,
                &[
                    "marketplacePath",
                    "remoteMarketplaceName",
                    "installAttemptId",
                    "pluginName",
                ],
            )?;
            optional_string(obj, "marketplacePath")?;
            optional_string(obj, "remoteMarketplaceName")?;
            optional_string(obj, "installAttemptId")?;
            required_string(obj, "pluginName")
        }
        "plugin/uninstall" => {
            check_fields(obj, &["pluginId"])?;
            required_string(obj, "pluginId")
        }
        "app/list" => {
            check_fields(obj, &["cursor", "limit", "threadId", "forceRefetch"])?;
            optional_string(obj, "cursor")?;
            optional_positive_integer(obj, "limit")?;
            optional_string(obj, "threadId")?;
            optional_bool(obj, "forceRefetch")
        }
        "app/installed" => {
            check_fields(obj, &["threadId", "forceRefresh"])?;
            optional_string(obj, "threadId")?;
            optional_bool(obj, "forceRefresh")
        }
        "app/read" => {
            check_fields(obj, &["appIds", "threadId", "includeTools"])?;
            let ids = obj
                .get("appIds")
                .and_then(Value::as_array)
                .ok_or("appIds must be an array")?;
            if ids
                .iter()
                .any(|id| id.as_str().is_none_or(|id| id.trim().is_empty()))
            {
                return Err("appIds must contain nonempty strings".into());
            }
            optional_string(obj, "threadId")?;
            optional_bool(obj, "includeTools")
        }
        "config/read" => {
            check_fields(obj, &["includeLayers", "cwd"])?;
            optional_bool(obj, "includeLayers")?;
            optional_string(obj, "cwd")
        }
        "config/value/write" => {
            check_fields(
                obj,
                &[
                    "keyPath",
                    "value",
                    "mergeStrategy",
                    "filePath",
                    "expectedVersion",
                ],
            )?;
            required_string(obj, "keyPath")?;
            optional_string(obj, "filePath")?;
            reject_config_file_path(obj)?;
            optional_string(obj, "expectedVersion")?;
            validate_merge_strategy(obj)?;
            validate_scoped_config_edit(
                obj.get("keyPath").and_then(Value::as_str).unwrap(),
                obj.get("value")
                    .ok_or("missing required parameter 'value'")?,
            )
        }
        "config/batchWrite" => {
            check_fields(
                obj,
                &["edits", "filePath", "expectedVersion", "reloadUserConfig"],
            )?;
            optional_string(obj, "filePath")?;
            reject_config_file_path(obj)?;
            optional_string(obj, "expectedVersion")?;
            optional_bool(obj, "reloadUserConfig")?;
            let edits = obj
                .get("edits")
                .and_then(Value::as_array)
                .ok_or("edits must be an array")?;
            for edit in edits {
                let edit = edit.as_object().ok_or("each edit must be an object")?;
                check_fields(edit, &["keyPath", "value", "mergeStrategy"])?;
                required_string(edit, "keyPath")?;
                validate_merge_strategy(edit)?;
                validate_scoped_config_edit(
                    edit.get("keyPath").and_then(Value::as_str).unwrap(),
                    edit.get("value")
                        .ok_or("missing required parameter 'value'")?,
                )?;
            }
            Ok(())
        }
        "mcpServerStatus/list" => {
            check_fields(
                obj,
                &["cursor", "limit", "detail", "threadId", "serverName"],
            )?;
            optional_string(obj, "cursor")?;
            optional_positive_integer(obj, "limit")?;
            if let Some(v) = obj.get("detail") {
                validate_enum(v, &["full", "toolsAndAuthOnly"], "detail")?;
            }
            optional_string(obj, "threadId")?;
            optional_string(obj, "serverName")
        }
        "config/mcpServer/reload" => check_fields(obj, &[]),
        "mcpServer/oauth/login" => {
            check_fields(
                obj,
                &[
                    "name",
                    "threadId",
                    "clientRegistration",
                    "scopes",
                    "timeoutSecs",
                ],
            )?;
            required_string(obj, "name")?;
            optional_string(obj, "threadId")?;
            optional_string_array(obj, "scopes")?;
            optional_positive_integer(obj, "timeoutSecs")?;
            if let Some(v) = obj.get("clientRegistration") {
                validate_enum(v, &["auto", "cimd", "dcr"], "clientRegistration")?;
            }
            Ok(())
        }
        "mcpServer/resource/read" => {
            check_fields(
                obj,
                &[
                    "threadId",
                    "originCallId",
                    "server",
                    "uri",
                    "connectorId",
                    "target",
                ],
            )?;
            optional_string(obj, "threadId")?;
            optional_string(obj, "originCallId")?;
            required_string(obj, "server")?;
            required_string(obj, "uri")?;
            optional_string(obj, "connectorId")?;
            if let Some(target) = obj.get("target") {
                let target = target.as_object().ok_or("target must be an object")?;
                check_fields(target, &["connectorId", "linkId"])?;
                required_string(target, "connectorId")?;
                optional_string(target, "linkId")?;
            }
            Ok(())
        }
        "mcpServer/event/stream/start" => {
            check_fields(
                obj,
                &[
                    "threadId",
                    "server",
                    "subscriptionId",
                    "name",
                    "arguments",
                    "_meta",
                ],
            )?;
            required_string(obj, "threadId")?;
            required_string(obj, "server")?;
            required_string(obj, "subscriptionId")?;
            required_string(obj, "name")?;
            required_value(obj, "arguments")?;
            optional_value(obj, "_meta")
        }
        "mcpServer/event/stream/stop" => {
            check_fields(obj, &["subscriptionId"])?;
            required_string(obj, "subscriptionId")
        }
        "mcpServer/tool/call" => {
            check_fields(obj, &["threadId", "server", "tool", "arguments", "_meta"])?;
            required_string(obj, "threadId")?;
            required_string(obj, "server")?;
            required_string(obj, "tool")?;
            optional_value(obj, "arguments")?;
            optional_value(obj, "_meta")
        }
        "mcpServer/prompts/list" => {
            check_fields(obj, &["server", "threadId", "cursor"])?;
            required_string(obj, "server")?;
            optional_string(obj, "threadId")?;
            optional_string(obj, "cursor")
        }
        "mcpServer/prompt/get" => {
            check_fields(obj, &["server", "threadId", "name", "arguments"])?;
            required_string(obj, "server")?;
            optional_string(obj, "threadId")?;
            required_string(obj, "name")?;
            if let Some(arguments) = obj.get("arguments") {
                let arguments = arguments.as_object().ok_or("arguments must be an object")?;
                if arguments.values().any(|value| !value.is_string()) {
                    return Err("prompt arguments must contain string values".into());
                }
            }
            Ok(())
        }
        "mcpServer/completion/complete" => {
            check_fields(obj, &["server", "threadId", "ref", "argument", "context"])?;
            required_string(obj, "server")?;
            optional_string(obj, "threadId")?;
            let reference = obj
                .get("ref")
                .and_then(Value::as_object)
                .ok_or("ref must be an object")?;
            check_fields(reference, &["type", "name", "uri"])?;
            required_string(reference, "type")?;
            if !matches!(
                reference.get("type").and_then(Value::as_str),
                Some("ref/prompt" | "ref/resource")
            ) {
                return Err("ref.type must be 'ref/prompt' or 'ref/resource'".into());
            }
            if reference.get("type").and_then(Value::as_str) == Some("ref/prompt") {
                required_string(reference, "name")?;
            }
            if reference.get("type").and_then(Value::as_str) == Some("ref/resource") {
                required_string(reference, "uri")?;
            }
            let argument = obj
                .get("argument")
                .and_then(Value::as_object)
                .ok_or("argument must be an object")?;
            check_fields(argument, &["name", "value"])?;
            required_string(argument, "name")?;
            if argument.get("value").and_then(Value::as_str).is_none() {
                return Err("argument.value must be a string".into());
            }
            optional_value(obj, "context")
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

fn required_bool(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    obj.get(key)
        .and_then(Value::as_bool)
        .map(|_| ())
        .ok_or_else(|| format!("{key} must be a boolean"))
}

fn optional_string_array(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    if let Some(value) = obj.get(key) {
        if !value.is_null()
            && !value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        {
            return Err(format!("{key} must be an array of strings"));
        }
    }
    Ok(())
}

fn validate_enum(value: &Value, allowed: &[&str], key: &str) -> Result<(), String> {
    if value.as_str().is_some_and(|value| allowed.contains(&value)) {
        Ok(())
    } else {
        Err(format!("{key} has an unsupported value"))
    }
}

fn validate_string_enum_array(value: &Value, allowed: &[&str], key: &str) -> Result<(), String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array"))?;
    for value in values {
        validate_enum(value, allowed, key)?;
    }
    Ok(())
}

fn required_value(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    if obj.contains_key(key) {
        Ok(())
    } else {
        Err(format!("missing required parameter '{key}'"))
    }
}

fn optional_value(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    let _ = obj.get(key);
    Ok(())
}

fn validate_merge_strategy(obj: &serde_json::Map<String, Value>) -> Result<(), String> {
    validate_enum(
        obj.get("mergeStrategy")
            .ok_or("missing required parameter 'mergeStrategy'")?,
        &["replace", "upsert"],
        "mergeStrategy",
    )
}

fn reject_config_file_path(obj: &serde_json::Map<String, Value>) -> Result<(), String> {
    if obj.get("filePath").is_some_and(|value| !value.is_null()) {
        return Err("config writes must use the runtime's configured user config file".into());
    }
    Ok(())
}

fn validate_scoped_config_edit(key_path: &str, value: &Value) -> Result<(), String> {
    let segments = split_config_key_path(key_path)?;
    if segments.len() < 2 || segments.iter().any(|part| part.trim().is_empty()) {
        return Err(
            "config keyPath must target a named MCP, plugin, marketplace, or hook trust setting"
                .into(),
        );
    }
    match segments[0].as_str() {
        "mcp_servers" if segments.len() == 2 && value.is_null() => Ok(()),
        "mcp_servers" if segments.len() == 2 => validate_mcp_server_config(value),
        "mcp_servers" => {
            let leaf = segments[2..].join(".");
            if [
                "command",
                "args",
                "env",
                "env_vars",
                "cwd",
                "http_headers",
                "env_http_headers",
                "url",
                "bearer_token_env_var",
                "http_headers_helper",
                "environment_id",
                "auth",
                "startup_timeout_sec",
                "startup_timeout_ms",
                "tool_timeout_sec",
                "enabled",
                "required",
                "startup_readiness",
                "supports_parallel_tool_calls",
                "tool_input_schema_max_bytes",
                "omit_tools_from",
                "default_tools_approval_mode",
                "enabled_tools",
                "disabled_tools",
                "scopes",
                "oauth",
                "oauth_resource",
                "tools",
            ]
            .contains(&leaf.as_str())
            {
                if leaf == "command"
                    || leaf == "url"
                    || leaf == "cwd"
                    || leaf == "bearer_token_env_var"
                    || leaf == "http_headers_helper"
                {
                    nonempty_string_value("value", value)
                } else if leaf == "enabled" || leaf == "required" {
                    if value.is_boolean() {
                        Ok(())
                    } else {
                        Err(format!("{leaf} must be a boolean"))
                    }
                } else {
                    Ok(())
                }
            } else {
                Err(format!("unsupported MCP config field '{leaf}'"))
            }
        }
        "plugins" if segments.len() == 3 && segments[2] == "enabled" => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err("plugin enabled must be a boolean".into())
            }
        }
        "marketplaces" if segments.len() == 2 => {
            if value.is_object() || value.is_string() {
                Ok(())
            } else {
                Err("marketplace config must be an object or string".into())
            }
        }
        "hooks"
            if segments.len() == 4
                && segments[1] == "state"
                && !segments[2].trim().is_empty()
                && segments[3] == "trusted_hash" =>
        {
            if value.is_string() {
                Ok(())
            } else {
                Err("hook trusted_hash must be a string".into())
            }
        }
        _ => Err(format!("config keyPath '{key_path}' is not allowed")),
    }
}

/// Split a TOML dotted key path while retaining dots inside quoted components.
/// Config paths use TOML's basic/literal quoted component syntax for plugin and
/// server names that contain punctuation, so plain `split('.')` is unsafe here.
fn split_config_key_path(key_path: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut had_quote = false;
    for character in key_path.chars() {
        if let Some(current_quote) = quote {
            if escaped {
                part.push(character);
                escaped = false;
            } else if current_quote == '"' && character == '\\' {
                escaped = true;
            } else if character == current_quote {
                quote = None;
                had_quote = true;
            } else {
                part.push(character);
            }
        } else {
            match character {
                '"' | '\'' if part.trim().is_empty() => {
                    quote = Some(character);
                    had_quote = true;
                }
                '.' => {
                    if part.trim().is_empty() {
                        return Err("config keyPath contains an empty component".into());
                    }
                    parts.push(std::mem::take(&mut part));
                    had_quote = false;
                }
                _ => part.push(character),
            }
        }
    }
    if quote.is_some() || escaped {
        return Err("config keyPath contains an unterminated quoted component".into());
    }
    if part.trim().is_empty() {
        return Err("config keyPath contains an empty component".into());
    }
    let _ = had_quote;
    parts.push(part);
    Ok(parts)
}

fn validate_mcp_server_config(value: &Value) -> Result<(), String> {
    let server = value
        .as_object()
        .ok_or("MCP server config must be an object")?;
    check_fields(
        server,
        &[
            "command",
            "args",
            "env",
            "env_vars",
            "cwd",
            "http_headers",
            "env_http_headers",
            "url",
            "bearer_token_env_var",
            "http_headers_helper",
            "environment_id",
            "auth",
            "startup_timeout_sec",
            "startup_timeout_ms",
            "tool_timeout_sec",
            "enabled",
            "required",
            "startup_readiness",
            "supports_parallel_tool_calls",
            "tool_input_schema_max_bytes",
            "omit_tools_from",
            "default_tools_approval_mode",
            "enabled_tools",
            "disabled_tools",
            "scopes",
            "oauth",
            "oauth_resource",
            "tools",
        ],
    )?;
    if let Some(command) = server.get("command") {
        nonempty_string_value("command", command)?;
    }
    if let Some(url) = server.get("url") {
        nonempty_string_value("url", url)?;
    }
    if let Some(args) = server.get("args") {
        if !args
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
        {
            return Err("args must be an array of strings".into());
        }
    }
    for key in ["env", "http_headers", "env_http_headers"] {
        if let Some(map) = server.get(key) {
            if !map.is_object() {
                return Err(format!("{key} must be an object"));
            }
        }
    }
    for key in ["enabled", "required", "supports_parallel_tool_calls"] {
        if let Some(v) = server.get(key) {
            if !v.is_boolean() {
                return Err(format!("{key} must be a boolean"));
            }
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
    fn allows_bounded_plugin_mcp_and_configuration_requests() {
        for (method, params) in [
            (
                "marketplace/add",
                json!({"source":"https://example.test/marketplace.git", "refName":"main", "sparsePaths":["plugins"]}),
            ),
            (
                "plugin/list",
                json!({"forceRefetch":true, "marketplaceKinds":["local"]}),
            ),
            (
                "plugin/install",
                json!({"pluginName":"reviewer", "remoteMarketplaceName":"curated"}),
            ),
            ("app/list", json!({"limit":20,"forceRefetch":true})),
            ("app/installed", json!({"forceRefresh":true})),
            (
                "app/read",
                json!({"appIds":["connector-1"],"includeTools":true}),
            ),
            ("skills/list", json!({"cwds":["/work"], "forceReload":true})),
            ("hooks/list", json!({"cwds":["/work"]})),
            ("config/read", json!({"includeLayers":true})),
            (
                "config/value/write",
                json!({"keyPath":"mcp_servers.review", "value":{"command":"node", "args":["server.js"], "env":{"TOKEN":"x"}}, "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"mcp_servers.\"review.server\"", "value":{"url":"https://mcp.example/api","http_headers":{"Authorization":"Bearer $TOKEN"},"env_http_headers":{"X-Token":"TOKEN_ENV"}}, "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"mcp_servers.\"review.server\"", "value":null, "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"hooks.state.\"plugin.with.dot.hook\\\"quoted\".trusted_hash", "value":"sha256:123", "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"plugins.\"name.with.dot@marketplace\".enabled", "value":true, "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"hooks.state.abc123.trusted_hash", "value":"abc123", "mergeStrategy":"replace"}),
            ),
            (
                "mcpServerStatus/list",
                json!({"detail":"full", "serverName":"review"}),
            ),
            (
                "mcpServer/tool/call",
                json!({"threadId":"t1", "server":"review", "tool":"search", "arguments":{"q":"docs"}}),
            ),
            (
                "mcpServer/resource/read",
                json!({"server":"review", "uri":"file:///resource"}),
            ),
            (
                "mcpServer/prompts/list",
                json!({"server":"review", "cursor":"page-2", "threadId":"t1"}),
            ),
            (
                "mcpServer/prompt/get",
                json!({"server":"review", "threadId":"t1", "name":"review", "arguments":{"path":"src/lib.rs"}}),
            ),
            (
                "mcpServer/completion/complete",
                json!({"server":"review", "ref":{"type":"ref/prompt","name":"review"}, "argument":{"name":"path","value":"src/"}}),
            ),
            (
                "mcpServer/completion/complete",
                json!({"server":"review", "ref":{"type":"ref/prompt","name":"review"}, "argument":{"name":"path","value":""}}),
            ),
            ("config/mcpServer/reload", json!({})),
        ] {
            assert!(
                validate_request(method, &params).is_ok(),
                "{method}: {params}"
            );
        }
    }

    #[test]
    fn rejects_configuration_bypasses_and_unknown_plugin_or_mcp_fields() {
        for (method, params) in [
            (
                "config/value/write",
                json!({"keyPath":"model_provider", "value":"other", "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"sandbox_mode", "value":"danger-full-access", "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"bypass_hook_trust", "value":true, "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"mcp_servers.review.enabled", "value":true, "filePath":"/tmp/another-config.toml", "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"hooks.state.abc123.bypass_hook_trust", "value":true, "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"hooks.state.\"id.with.dots\".bypass_hook_trust", "value":true, "mergeStrategy":"replace"}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"hooks.state.\"id.with.dots\".trusted_hash.extra", "value":"bad", "mergeStrategy":"replace"}),
            ),
            (
                "config/batchWrite",
                json!({"edits":[{"keyPath":"features.experimental_api", "value":true, "mergeStrategy":"replace"}]}),
            ),
            (
                "config/value/write",
                json!({"keyPath":"mcp_servers.review", "value":{"command":"node", "args":[], "mystery":true}, "mergeStrategy":"replace"}),
            ),
            (
                "plugin/list",
                json!({"forceRefetch":true, "arbitrary":true}),
            ),
            (
                "mcpServer/tool/call",
                json!({"threadId":"t1", "server":"review", "tool":"search", "unexpected":true}),
            ),
        ] {
            assert!(
                validate_request(method, &params).is_err(),
                "{method}: {params}"
            );
        }
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

    #[test]
    fn plugin_credentials_are_scoped_and_cannot_override_the_transport() {
        let valid = json!({"pluginId":"docs@test","serverName":"docs","credential":{"kind":"token","token":"secret"}});
        assert!(validate_request("plugin/mcpCredential/write", &valid).is_ok());
        let basic = json!({"pluginId":"docs@test","serverName":"docs","credential":{"kind":"basic","username":"key","password":""}});
        assert!(validate_request("plugin/mcpCredential/write", &basic).is_ok());
        assert!(validate_request(
            "plugin/mcpCredential/write",
            &json!({"pluginId":"docs@test","serverName":"docs","credential":null})
        )
        .is_ok());
        for credential in [
            json!({"kind":"token","token":""}),
            json!({"kind":"token","token":"key","url":"https://evil.test"}),
            json!({"kind":"basic","username":"key"}),
        ] {
            let mut invalid = valid.clone();
            invalid["credential"] = credential;
            assert!(validate_request("plugin/mcpCredential/write", &invalid).is_err());
        }
        let mut invalid = valid;
        invalid["url"] = json!("https://evil.test");
        assert!(validate_request("plugin/mcpCredential/write", &invalid).is_err());
    }

    #[test]
    fn accepts_only_scoped_asymmetric_registered_signing_key_setup() {
        let mut valid = json!({"pluginId":"docs@test","serverName":"docs","credential":{"kind":"oauth_private_key_jwt","privateKey":"test-only-pem","algorithm":"RS256","keyId":"registered"}});
        assert!(validate_request("plugin/mcpCredential/write", &valid).is_ok());
        valid["credential"]["algorithm"] = json!("HS256");
        assert!(validate_request("plugin/mcpCredential/write", &valid).is_err());
        valid["credential"]["algorithm"] = json!("RS256");
        valid["credential"]["tokenUrl"] = json!("https://evil.test");
        assert!(validate_request("plugin/mcpCredential/write", &valid).is_err());
    }
}
