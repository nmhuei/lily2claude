use crate::engine::router::EngineRouter;
use crate::protocol::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};
use crate::protocol::stream::SseStreamTranslator;
use crate::protocol::transform::transform_anthropic_to_openai;
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct AppState {
    pub router: Arc<EngineRouter>,
    pub session_mgr: Arc<crate::engine::session::SessionManager>,
}

pub fn create_router(router: Arc<EngineRouter>) -> Router {
    let session_mgr = Arc::new(crate::engine::session::SessionManager::new());
    let state = AppState {
        router,
        session_mgr: session_mgr.clone(),
    };

    // Background idle cleanup: evicts sessions inactive for > 15 minutes
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            session_mgr.sweep_idle(std::time::Duration::from_secs(900)).await;
        }
    });

    Router::new()
        .route("/health", get(health_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/messages/count_tokens", post(count_tokens_handler))
        .route("/v1/messages", post(messages_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "lily2claude", "version": "0.1.0"}))
}

async fn models_handler() -> impl IntoResponse {
    Json(json!({
        "data": [
            {"id": "deepseek", "display_name": "DeepSeek V4.1 Flash"},
            {"id": "glm", "display_name": "GLM 5.3 Flash"},
            {"id": "kilo", "display_name": "Kilo Auto Free"},
            {"id": "luna", "display_name": "GPT-5.6 Luna"},
            {"id": "mimo", "display_name": "MiMo 2.6 Flash"},
            {"id": "nemotron", "display_name": "Nemotron 3.5 Lightning"}
        ]
    }))
}

async fn count_tokens_handler(Json(_req): Json<serde_json::Value>) -> impl IntoResponse {
    // Fast token calculation response in <1ms for Claude Code UI and compaction
    Json(json!({"input_tokens": 150}))
}

async fn messages_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<MessagesRequest>,
) -> Response {
    let session_key = crate::engine::session::extract_session_key(&req);
    eprintln!("=== INCOMING CLAUDE CODE HEADERS ===");
    for (name, val) in &headers {
        eprintln!("  HEADER: {} = {:?}", name, val);
    }
    eprintln!("=====================================");

    let candidates = state.router.resolve_alias(&req.model);
    if candidates.is_empty() {
        return (StatusCode::BAD_REQUEST, "No candidate providers for model").into_response();
    }

    for target in &candidates {
        let is_opencode = target.provider == "opencode";

        if let Some(http_adapter) = state.router.get_http_adapter(&target.provider) {
            let mut openai_req = match transform_anthropic_to_openai(&req, is_opencode) {
                Ok(r) => r,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };
            openai_req.model = target.model.clone();

            // 1. Non-streaming branch: Claude Code background session title generation
            if !req.stream {
                match http_adapter.send_request_with_session(&openai_req, Some(&session_key)).await {
                    Ok(upstream_resp) => {
                        if !upstream_resp.status().is_success() {
                            continue; // Try next candidate if upstream errored
                        }
                        let bytes = upstream_resp.bytes().await.unwrap_or_default();
                        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                        let content_text = val
                            .pointer("/choices/0/message/content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Session Title");

                        return Json(json!({
                            "id": "msg_title",
                            "type": "message",
                            "role": "assistant",
                            "model": req.model,
                            "content": [{"type": "text", "text": content_text}],
                            "stop_reason": "end_turn",
                            "usage": {"input_tokens": 50, "output_tokens": 15}
                        }))
                        .into_response();
                    }
                    Err(_) => continue, // Try next candidate
                }
            }

            // 2. Streaming branch: Claude Code SSE interaction
            match http_adapter.send_request_with_session(&openai_req, Some(&session_key)).await {
                Ok(upstream_resp) => {
                    if !upstream_resp.status().is_success() {
                        continue; // Rate limited or errored, try next candidate
                    }

                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
                    let model_clone = req.model.clone();
                    let tools_decl = req.tools.clone().unwrap_or_default();

                    tokio::spawn(async move {
                        let mut translator = SseStreamTranslator::new("msg_live", &model_clone);
                        for t in tools_decl {
                            translator.register_tool_casing(&t.name);
                        }

                        let mut stream = upstream_resp.bytes_stream();
                        while let Some(item) = stream.next().await {
                            if let Ok(bytes) = item {
                                let text = String::from_utf8_lossy(&bytes);
                                for line in text.lines() {
                                    let line = line.trim();
                                    if let Some(data) = line.strip_prefix("data: ") {
                                        if data == "[DONE]" {
                                            break;
                                        }
                                        let events = translator.feed_chunk(data);
                                        for ev in events {
                                            let _ = tx.send(Ok(Event::default().data(ev))).await;
                                        }
                                    }
                                }
                            }
                        }
                    });

                    return Sse::new(ReceiverStream::new(rx))
                        .keep_alive(KeepAlive::default())
                        .into_response();
                }
                Err(_) => continue, // Try next candidate
            }
        }

        // 3. Local ACP Agent branch
        let agent_reg = crate::agents::AgentRegistry::default();
        if let Some(agent) = agent_reg.get(&target.provider) {
            eprintln!(">>> TOTAL MESSAGES IN REQ: {}", req.messages.len());
            let mut prompt_parts = Vec::new();
            for (idx, m) in req.messages.iter().enumerate() {
                let text = match &m.content {
                    AnthropicContent::Text(t) => t.clone(),
                    AnthropicContent::Blocks(b) => b.iter().filter_map(|blk| match blk {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n\n"),
                };
                let snippet: String = text.chars().take(80).collect();
                eprintln!(">>> MSG[{}]: role={} snippet={}", idx, m.role, snippet);
                if !text.is_empty() {
                    prompt_parts.push(text);
                }
            }
            let prompt_text = if prompt_parts.is_empty() {
                "hello".to_string()
            } else {
                prompt_parts.join("\n\n---\n\n")
            };

            if !req.stream {
                let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<String>(32);
                let agent_clone = agent.clone();
                let model_clone = target.model.clone();
                let session_mgr = state.session_mgr.clone();
                let s_key = session_key.clone();
                tokio::spawn(async move {
                    let _ = agent_clone
                        .execute_turn(
                            &model_clone,
                            &prompt_text,
                            Some(&s_key),
                            Some(&session_mgr),
                            prompt_tx,
                        )
                        .await;
                });

                let mut full_text = String::new();
                while let Some(chunk) = prompt_rx.recv().await {
                    full_text.push_str(&chunk);
                }
                let clean_text = if full_text.trim().is_empty() {
                    "Session".to_string()
                } else {
                    full_text.trim().to_string()
                };

                return Json(json!({
                    "id": "msg_title",
                    "type": "message",
                    "role": "assistant",
                    "model": req.model,
                    "content": [{"type": "text", "text": clean_text}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 50, "output_tokens": 15}
                }))
                .into_response();
            }

            // Streaming mode for Local ACP Agent
            let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<String>(32);
            let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
            let agent_clone = agent.clone();
            let model_clone = target.model.clone();
            let req_model = req.model.clone();
            let session_mgr = state.session_mgr.clone();
            let s_key = session_key.clone();

            tokio::spawn(async move {
                let _ = agent_clone
                    .execute_turn(
                        &model_clone,
                        &prompt_text,
                        Some(&s_key),
                        Some(&session_mgr),
                        prompt_tx,
                    )
                    .await;
            });

            tokio::spawn(async move {
                let start_ev = json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_live_acp",
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": req_model,
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": 50, "output_tokens": 1}
                    }
                });
                let _ = sse_tx.send(Ok(Event::default().data(start_ev.to_string()))).await;

                let blk_start = json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""}
                });
                let _ = sse_tx.send(Ok(Event::default().data(blk_start.to_string()))).await;

                while let Some(chunk) = prompt_rx.recv().await {
                    let delta = json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": chunk}
                    });
                    let _ = sse_tx.send(Ok(Event::default().data(delta.to_string()))).await;
                }

                let blk_stop = json!({"type": "content_block_stop", "index": 0});
                let _ = sse_tx.send(Ok(Event::default().data(blk_stop.to_string()))).await;

                let msg_delta = json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {"output_tokens": 20}
                });
                let _ = sse_tx.send(Ok(Event::default().data(msg_delta.to_string()))).await;

                let msg_stop = json!({"type": "message_stop"});
                let _ = sse_tx.send(Ok(Event::default().data(msg_stop.to_string()))).await;
            });

            return Sse::new(ReceiverStream::new(sse_rx))
                .keep_alive(KeepAlive::default())
                .into_response();
        }
    }

    (StatusCode::BAD_GATEWAY, "All upstream candidate providers failed").into_response()
}
