//! Runs the owned extension boundaries without compiling the entire upstream test graph.
#![allow(dead_code)]
#[path = "../../../codex/codex-rs/codex-mcp/src/plugin_api_key.rs"]
mod plugin_api_key;
#[path = "../../../codex/codex-rs/rmcp-client/src/oauth_token_http_client.rs"]
mod oauth_token_http_client;
#[path = "../../../codex/codex-rs/rmcp-client/src/oauth_metadata_overrides.rs"]
mod oauth_metadata_overrides;
#[path = "../../../codex/codex-rs/rmcp-client/src/oauth_private_key_jwt.rs"]
mod oauth_private_key_jwt;
