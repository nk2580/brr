//! Exercises pending MRTR input cleanup over a real duplex transport.

use super::ElicitationClientService;
use crate::request_cancellation::RequestCancellation;
use crate::request_input::call_tool;
use crate::request_input::get_prompt;
use crate::rmcp_client::ElicitationPauseState;
use codex_protocol::mcp::OPENAI_ELICITATION_EXTENSION_ID;
use rmcp::RoleServer;
use rmcp::model::CallToolRequestParams;
use rmcp::model::ClientConfig;
use rmcp::model::ClientJsonRpcMessage;
use rmcp::model::CustomResult;
use rmcp::model::ElicitationAction;
use rmcp::model::GetPromptRequestParams;
use rmcp::model::GetPromptResult;
use rmcp::model::PromptMessage;
use rmcp::model::Role;
use rmcp::model::ServerJsonRpcMessage;
use rmcp::model::ServerResult;
use rmcp::service::ServiceError;
use rmcp::service::serve_directly;
use rmcp::transport::IntoTransport;
use rmcp::transport::Transport;
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[tokio::test]
async fn connection_closure_releases_pending_inputs_and_timeout_pause() -> anyhow::Result<()> {
    for mode in ["form", "openai/userVerification"] {
        for close_transport in [false, true] {
            let pause_state = ElicitationPauseState::new();
            let mut paused = pause_state.subscribe();
            let (route_tx, mut route_rx) = mpsc::unbounded_channel();
            let mut info = ClientConfig::default();
            info.capabilities.extensions = Some(
                [(
                    OPENAI_ELICITATION_EXTENSION_ID.into(),
                    serde_json::Map::from_iter([
                        ("userVerification".into(), json!({})),
                        ("form".into(), json!({})),
                    ]),
                )]
                .into_iter()
                .collect(),
            );
            let service = ElicitationClientService::new(
                info,
                Box::new(move |_, _| {
                    let (tx, rx) = oneshot::channel();
                    route_tx.send(tx).unwrap();
                    Box::pin(async move { Ok(rx.await?) })
                }),
                pause_state,
            );
            let (client_transport, server_transport) =
                tokio::io::duplex(/*max_buf_size*/ 4096);
            let client = serve_directly(service, client_transport, /*peer_info*/ None);
            let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_transport);
            let call = call_tool(
                &client,
                client.service(),
                CallToolRequestParams::new("test"),
            );
            tokio::pin!(call);
            let request = tokio::select! {
                result = &mut call => panic!("tool completed before input: {result:?}"),
                request = timeout(Duration::from_secs(/*secs*/ 5), server.receive()) => request?,
            };
            let Some(ClientJsonRpcMessage::Request(request)) = request else {
                anyhow::bail!("expected tool call");
            };
            let params = match mode {
                "form" => json!({
                    "mode": mode, "message": "Confirm",
                    "requestedSchema": {"type": "object", "properties": {}},
                }),
                _ => json!({
                    "mode": mode, "title": "Verify", "description": "", "challenge": "AQID",
                }),
            };
            server.send(ServerJsonRpcMessage::response(
                ServerResult::CustomResult(CustomResult(json!({
                    "resultType": "input_required",
                    "inputRequests": {"input": {"method": "openai/elicitation/create", "params": params}},
                }))),
                request.id,
            )).await?;
            let reply = tokio::select! {
                result = &mut call => panic!("tool completed before prompting: {result:?}"),
                reply = timeout(Duration::from_secs(/*secs*/ 5), route_rx.recv()) => reply?.unwrap(),
            };
            assert!(*paused.borrow());
            if close_transport {
                drop(server);
            } else {
                client.cancellation_token().cancel();
            }
            let result = timeout(Duration::from_secs(/*secs*/ 5), call).await?;
            assert!(
                matches!(result, Err(ServiceError::TransportClosed)),
                "{result:?}"
            );
            assert!(reply.is_closed());
            assert!(!*paused.borrow_and_update());
        }
    }
    Ok(())
}

#[tokio::test]
async fn prompt_get_preserves_arguments_and_request_state_across_mrtr_rounds() -> anyhow::Result<()>
{
    let (route_tx, mut route_rx) = mpsc::unbounded_channel();
    let mut info = ClientConfig::default();
    info.capabilities.extensions = Some(
        [(
            OPENAI_ELICITATION_EXTENSION_ID.into(),
            serde_json::Map::from_iter([("form".into(), json!({}))]),
        )]
        .into_iter()
        .collect(),
    );
    let service = ElicitationClientService::new(
        info,
        Box::new(move |_, _| {
            route_tx.send(()).unwrap();
            Box::pin(async move {
                Ok(crate::ElicitationResponse {
                    action: ElicitationAction::Accept,
                    content: Some(json!({"confirmed": true})),
                    meta: None,
                })
            })
        }),
        ElicitationPauseState::new(),
    );
    let (client_transport, server_transport) = tokio::io::duplex(4096);
    let client = serve_directly(service, client_transport, None);
    let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_transport);
    let mut params = GetPromptRequestParams::new("review");
    params.arguments = Some(serde_json::Map::from_iter([(
        "path".to_string(),
        json!("src/lib.rs"),
    )]));
    let get = get_prompt(
        &client,
        client.service(),
        params,
        RequestCancellation::OnDrop,
    );
    tokio::pin!(get);

    let first = tokio::select! {
        result = &mut get => anyhow::bail!("prompt completed before its first request: {result:?}"),
        request = timeout(Duration::from_secs(5), server.receive()) => request??.unwrap(),
    };
    let ClientJsonRpcMessage::Request(first) = first else {
        anyhow::bail!("expected first prompts/get request")
    };
    assert_eq!(first.method.as_str(), "prompts/get");
    assert_eq!(first.params.get("name"), Some(&json!("review")));
    assert_eq!(
        first.params.get("arguments"),
        Some(&json!({"path":"src/lib.rs"}))
    );
    let first_id = first.id.clone();
    server.send(ServerJsonRpcMessage::response(
        ServerResult::CustomResult(CustomResult(json!({
            "resultType":"input_required",
            "requestState":"opaque.\"state\".value",
            "inputRequests":{"input": {"method":"openai/elicitation/create", "params":{"mode":"form","message":"Confirm","requestedSchema":{"type":"object","properties":{}}}}}
        }))),
        first.id,
    )).await?;
    let _elicitation = tokio::select! {
        result = &mut get => anyhow::bail!("prompt completed before elicitation: {result:?}"),
        route = timeout(Duration::from_secs(5), route_rx.recv()) => route?.unwrap(),
    };
    let second = tokio::select! {
        result = &mut get => anyhow::bail!("prompt completed before its continuation request: {result:?}"),
        request = timeout(Duration::from_secs(5), server.receive()) => request??.unwrap(),
    };
    let ClientJsonRpcMessage::Request(second) = second else {
        anyhow::bail!("expected retry prompts/get request")
    };
    assert_ne!(
        first_id, second.id,
        "each continuation round must use a fresh JSON-RPC id"
    );
    assert_eq!(second.params.get("name"), Some(&json!("review")));
    assert_eq!(
        second.params.get("arguments"),
        Some(&json!({"path":"src/lib.rs"}))
    );
    assert_eq!(
        second.params.get("requestState"),
        Some(&json!("opaque.\"state\".value"))
    );
    assert_eq!(
        second.params.pointer("/inputResponses/input/action"),
        Some(&json!("accept"))
    );
    server
        .send(ServerJsonRpcMessage::response(
            ServerResult::GetPromptResult(GetPromptResult::new(vec![PromptMessage::new_text(
                Role::User,
                "done",
            )])),
            second.id,
        ))
        .await?;
    let result = timeout(Duration::from_secs(5), get).await??;
    assert_eq!(result.messages.len(), 1);
    Ok(())
}
