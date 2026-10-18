use crate::engine::router::EngineRouter;
use crate::protocol::anthropic::{AnthropicContent, ContentBlock, MessagesRequest, SystemPrompt};
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
    pub token_tracker: Arc<crate::protocol::tokens::TokenUsageTracker>,
}

pub fn create_router(router: Arc<EngineRouter>) -> Router {
    let session_mgr = Arc::new(crate::engine::session::SessionManager::new());
    let token_tracker = Arc::new(crate::protocol::tokens::TokenUsageTracker::new());
    let state = AppState {
        router,
        session_mgr: session_mgr.clone(),
        token_tracker,
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
        .route("/v1/stats", get(stats_handler))
        .route("/v1/usage", get(stats_handler))
        .route("/v1/messages/count_tokens", post(count_tokens_handler))
        .route("/v1/messages", post(messages_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "lily2claude", "version": "0.1.0"}))
}

async fn stats_handler(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.token_tracker.snapshot().await;
    Json(snapshot)
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

async fn count_tokens_handler(Json(req_val): Json<serde_json::Value>) -> impl IntoResponse {
    // Attempt exact calculation by deserializing into MessagesRequest
    let tokens = if let Ok(req) = serde_json::from_value::<MessagesRequest>(req_val.clone()) {
        crate::protocol::tokens::TokenCalculator::count_request_tokens(&req)
    } else {
        // High-precision BPE fallback on raw payload string
        let raw = req_val.to_string();
        crate::protocol::tokens::TokenCalculator::count_text(&raw).max(1)
    };
    Json(json!({"input_tokens": tokens}))
}


async fn messages_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<MessagesRequest>,
) -> Response {
    let session_key = crate::engine::session::extract_session_key_from_headers_or_req(&headers, &req);
    let calculated_input_tokens = crate::protocol::tokens::TokenCalculator::count_request_tokens(&req) as u64;

    let tools_summary: Vec<String> = req.tools.as_ref().map(|ts| ts.iter().map(|t| t.name.clone()).collect()).unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let log_line = format!(
        "[ts={}] [REQ] model={} stream={} tools={:?} msgs={}\n",
        now, req.model, req.stream, tools_summary, req.messages.len()
    );
    eprint!("{}", log_line);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/lily2claude_traffic.log") {
        use std::io::Write;
        let _ = f.write_all(log_line.as_bytes());
    }

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
                            state.router.mark_provider_degraded(&target.provider, 180);
                            continue; // Try next candidate if upstream errored
                        }
                        let bytes = upstream_resp.bytes().await.unwrap_or_default();
                        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                        let content_text = val
                            .pointer("/choices/0/message/content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Session Title");

                        let out_tokens = crate::protocol::tokens::TokenCalculator::count_text(content_text).max(1) as u64;
                        let turn_usage = crate::protocol::tokens::TokenUsage::new(calculated_input_tokens, out_tokens);
                        state.token_tracker.record_turn(&session_key, &req.model, turn_usage).await;

                        return Json(json!({
                            "id": "msg_title",
                            "type": "message",
                            "role": "assistant",
                            "model": req.model,
                            "content": [{"type": "text", "text": content_text}],
                            "stop_reason": "end_turn",
                            "usage": {"input_tokens": calculated_input_tokens, "output_tokens": out_tokens}
                        }))
                        .into_response();
                    }
                    Err(_) => {
                        state.router.mark_provider_degraded(&target.provider, 180);
                        continue; // Try next candidate
                    }
                }
            }

            // 2. Streaming branch: Claude Code SSE interaction
            match http_adapter.send_request_with_session(&openai_req, Some(&session_key)).await {
                Ok(upstream_resp) => {
                    if !upstream_resp.status().is_success() {
                        state.router.mark_provider_degraded(&target.provider, 180);
                        continue; // Rate limited or errored, try next candidate
                    }

                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
                    let model_clone = req.model.clone();
                    let tools_decl = req.tools.clone().unwrap_or_default();

                    tokio::spawn(async move {
                        let mut translator = SseStreamTranslator::new("msg_live", &model_clone)
                            .with_input_tokens(calculated_input_tokens);
                        for t in tools_decl {
                            translator.register_tool_casing(&t.name);
                        }

                        let mut stream = upstream_resp.bytes_stream();
                        'stream_loop: while let Some(item) = stream.next().await {
                            if let Ok(bytes) = item {
                                let text = String::from_utf8_lossy(&bytes);
                                for line in text.lines() {
                                    let line = line.trim();
                                    if let Some(data) = line.strip_prefix("data: ") {
                                        if data == "[DONE]" {
                                            break 'stream_loop;
                                        }
                                        let events = translator.feed_chunk(data);
                                        for ev in events {
                                            let _ = tx
                                                .send(Ok(Event::default()
                                                    .event(ev.event)
                                                    .data(ev.data.to_string())))
                                                .await;
                                        }
                                    }
                                }
                            }
                        }

                        // Guarantees clean stream closure even if upstream socket drops
                        let tail_events = translator.finish_stream_if_needed();
                        for ev in tail_events {
                            let _ = tx
                                .send(Ok(Event::default()
                                    .event(ev.event)
                                    .data(ev.data.to_string())))
                                .await;
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
            let prompt_text = build_acp_prompt(&req);

            if !req.stream {
                let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<crate::protocol::tokens::AcpOutputChunk>(32);
                let agent_clone = agent.clone();
                let model_clone = target.model.clone();
                let s_key = session_key.clone();
                let session_mgr = state.session_mgr.clone();
                let p_text = prompt_text.clone();

                let handle = tokio::spawn(async move {
                    agent_clone
                        .execute_turn(
                            &model_clone,
                            &p_text,
                            Some(&s_key),
                            Some(&session_mgr),
                            prompt_tx,
                        )
                        .await
                });

                let non_stream_timeout = std::time::Duration::from_secs(180);
                let wait_res = tokio::time::timeout(non_stream_timeout, async {
                    let mut title_text = String::new();
                    while let Some(chunk) = prompt_rx.recv().await {
                        if let crate::protocol::tokens::AcpOutputChunk::Text(t) = chunk {
                            title_text.push_str(&t);
                        }
                    }
                    (title_text, handle.await)
                })
                .await;

                match wait_res {
                    Ok((title_text, Ok(Ok(usage)))) => {
                        let out_tokens = if usage.output_tokens > 0 {
                            usage.output_tokens
                        } else {
                            crate::protocol::tokens::TokenCalculator::count_text(&title_text).max(1) as u64
                        };
                        let turn_usage = crate::protocol::tokens::TokenUsage::new(calculated_input_tokens, out_tokens);
                        state.token_tracker.record_turn(&session_key, &req.model, turn_usage).await;

                        let final_title = if title_text.trim().is_empty() {
                            "Session Title"
                        } else {
                            title_text.trim()
                        };

                        return Json(json!({
                            "id": "msg_title",
                            "type": "message",
                            "role": "assistant",
                            "model": req.model,
                            "content": [{"type": "text", "text": final_title}],
                            "stop_reason": "end_turn",
                            "usage": {"input_tokens": calculated_input_tokens, "output_tokens": out_tokens}
                        }))
                        .into_response();
                    }
                    _ => {
                        state.session_mgr.evict_session(&session_key, &target.provider).await;
                        state.router.mark_provider_degraded(&target.provider, 300);
                        continue;
                    }
                }
            }

            // Streaming mode for Local ACP Agent
            let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<crate::protocol::tokens::AcpOutputChunk>(32);
            let agent_clone = agent.clone();
            let model_clone = target.model.clone();
            let session_mgr = state.session_mgr.clone();
            let s_key = session_key.clone();
            let p_text = prompt_text.clone();

            let exec_task = tokio::spawn(async move {
                agent_clone
                    .execute_turn(
                        &model_clone,
                        &p_text,
                        Some(&s_key),
                        Some(&session_mgr),
                        prompt_tx,
                    )
                    .await
            });

            // Wait for first chunk (text or thinking) to guarantee candidate is responsive (max 60s timeout)
            let mut initial_usage: Option<crate::protocol::tokens::TokenUsage> = None;
            let first_chunk_timeout = std::time::Duration::from_secs(60);

            let recv_res = tokio::time::timeout(first_chunk_timeout, async {
                while let Some(chunk) = prompt_rx.recv().await {
                    match &chunk {
                        crate::protocol::tokens::AcpOutputChunk::Text(t) => {
                            if !t.is_empty() {
                                if t.contains("provider.quota")
                                    || t.contains("Rate limit exceeded")
                                    || t.contains("rate limit exceeded")
                                    || t.contains("at capacity")
                                    || t.contains("429")
                                {
                                    tracing::warn!(candidate = %target.provider, error = %t, "Candidate hit rate limit/quota, trying next candidate...");
                                    return None;
                                }
                                return Some(chunk);
                            }
                        }
                        crate::protocol::tokens::AcpOutputChunk::Thinking(th) => {
                            if !th.is_empty() {
                                return Some(chunk);
                            }
                        }
                        crate::protocol::tokens::AcpOutputChunk::Usage(u) => {
                            initial_usage = Some(*u);
                        }
                    }
                }
                None
            })
            .await;

            let first_chunk = match recv_res {
                Ok(Some(chunk)) => chunk,
                _ => {
                    tracing::warn!(candidate = %target.provider, model = %target.model, "Candidate timed out or closed without output, marking degraded and attempting next candidate...");
                    exec_task.abort();
                    state.session_mgr.evict_session(&session_key, &target.provider).await;
                    state.router.mark_provider_degraded(&target.provider, 300);
                    if target.provider == "opencode" {
                        if let Some(pool) = state.session_mgr.warp_pool() {
                            pool.mark_rate_limited("http://127.0.0.1:40001", 300);
                            pool.mark_rate_limited("http://127.0.0.1:40002", 300);
                            pool.mark_rate_limited("http://127.0.0.1:40010", 300);
                        }
                    }
                    continue;
                }
            };
            let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
            let tools_decl = req.tools.clone().unwrap_or_default();
            let s_key_clone = session_key.clone();
            let req_model_clone = req.model.clone();
            let tracker_clone = state.token_tracker.clone();

            tokio::spawn(async move {
                let start_ev = json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_live_acp",
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": req_model_clone,
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": calculated_input_tokens,
                            "output_tokens": 1,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0
                        }
                    }
                });
                let _ = sse_tx
                    .send(Ok(Event::default()
                        .event("message_start")
                        .data(start_ev.to_string())))
                    .await;

                #[derive(PartialEq)]
                enum ActiveBlock {
                    None,
                    Thinking(usize),
                    Text(usize),
                }

                let mut current_block = ActiveBlock::None;
                let mut next_block_index = 0usize;
                let mut accumulated = String::new();
                let mut streamed_text_len = 0usize;
                let mut captured_usage: Option<crate::protocol::tokens::TokenUsage> = initial_usage;

                // Process initial chunk
                match first_chunk {
                    crate::protocol::tokens::AcpOutputChunk::Thinking(th) => {
                        let idx = next_block_index;
                        next_block_index += 1;
                        let blk_start = json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {"type": "thinking", "thinking": ""}
                        });
                        let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                        let delta = json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {"type": "thinking_delta", "thinking": th}
                        });
                        let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                        current_block = ActiveBlock::Thinking(idx);
                    }
                    crate::protocol::tokens::AcpOutputChunk::Text(t) => {
                        accumulated.push_str(&t);
                        let streamable_end = get_streamable_text_end(&accumulated);
                        if streamable_end > streamed_text_len {
                            let to_stream = &accumulated[streamed_text_len..streamable_end];
                            let idx = next_block_index;
                            next_block_index += 1;
                            let blk_start = json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": {"type": "text", "text": ""}
                            });
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                            let delta = json!({
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": {"type": "text_delta", "text": to_stream}
                            });
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                            current_block = ActiveBlock::Text(idx);
                            streamed_text_len = streamable_end;
                        }
                    }
                    crate::protocol::tokens::AcpOutputChunk::Usage(u) => {
                        captured_usage = Some(u);
                    }
                }

                let inter_chunk_timeout = std::time::Duration::from_secs(60);
                loop {
                    let recv_next = tokio::time::timeout(inter_chunk_timeout, prompt_rx.recv()).await;
                    match recv_next {
                        Ok(Some(item)) => {
                            match item {
                                crate::protocol::tokens::AcpOutputChunk::Thinking(chunk) => {
                                    if !chunk.is_empty() {
                                        match current_block {
                                            ActiveBlock::Thinking(idx) => {
                                                let delta = json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "thinking_delta", "thinking": chunk}
                                                });
                                                let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                                            }
                                            ActiveBlock::Text(prev_idx) => {
                                                let _ = sse_tx.send(Ok(Event::default().event("content_block_stop").data(json!({"type": "content_block_stop", "index": prev_idx}).to_string()))).await;
                                                let idx = next_block_index;
                                                next_block_index += 1;
                                                let blk_start = json!({
                                                    "type": "content_block_start",
                                                    "index": idx,
                                                    "content_block": {"type": "thinking", "thinking": ""}
                                                });
                                                let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                                                let delta = json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "thinking_delta", "thinking": chunk}
                                                });
                                                let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                                                current_block = ActiveBlock::Thinking(idx);
                                            }
                                            ActiveBlock::None => {
                                                let idx = next_block_index;
                                                next_block_index += 1;
                                                let blk_start = json!({
                                                    "type": "content_block_start",
                                                    "index": idx,
                                                    "content_block": {"type": "thinking", "thinking": ""}
                                                });
                                                let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                                                let delta = json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "thinking_delta", "thinking": chunk}
                                                });
                                                let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                                                current_block = ActiveBlock::Thinking(idx);
                                            }
                                        }
                                    }
                                }
                                crate::protocol::tokens::AcpOutputChunk::Text(chunk) => {
                                    if !chunk.is_empty() {
                                        accumulated.push_str(&chunk);
                                        let streamable_end = get_streamable_text_end(&accumulated);
                                        if streamable_end > streamed_text_len {
                                            let to_stream = &accumulated[streamed_text_len..streamable_end];
                                            match current_block {
                                                ActiveBlock::Text(idx) => {
                                                    let delta = json!({
                                                        "type": "content_block_delta",
                                                        "index": idx,
                                                        "delta": {"type": "text_delta", "text": to_stream}
                                                    });
                                                    let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                                                }
                                                ActiveBlock::Thinking(prev_idx) => {
                                                    let _ = sse_tx.send(Ok(Event::default().event("content_block_stop").data(json!({"type": "content_block_stop", "index": prev_idx}).to_string()))).await;
                                                    let idx = next_block_index;
                                                    next_block_index += 1;
                                                    let blk_start = json!({
                                                        "type": "content_block_start",
                                                        "index": idx,
                                                        "content_block": {"type": "text", "text": ""}
                                                    });
                                                    let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                                                    let delta = json!({
                                                        "type": "content_block_delta",
                                                        "index": idx,
                                                        "delta": {"type": "text_delta", "text": to_stream}
                                                    });
                                                    let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                                                    current_block = ActiveBlock::Text(idx);
                                                }
                                                ActiveBlock::None => {
                                                    let idx = next_block_index;
                                                    next_block_index += 1;
                                                    let blk_start = json!({
                                                        "type": "content_block_start",
                                                        "index": idx,
                                                        "content_block": {"type": "text", "text": ""}
                                                    });
                                                    let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                                                    let delta = json!({
                                                        "type": "content_block_delta",
                                                        "index": idx,
                                                        "delta": {"type": "text_delta", "text": to_stream}
                                                    });
                                                    let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                                                    current_block = ActiveBlock::Text(idx);
                                                }
                                            }
                                            streamed_text_len = streamable_end;
                                        }
                                    }
                                }
                                crate::protocol::tokens::AcpOutputChunk::Usage(usage) => {
                                    captured_usage = Some(usage);
                                }
                            }
                        }
                        _ => break, // Stream completed or idle timeout reached
                    }
                }

                let mut casing_map = std::collections::HashMap::new();
                for t in &tools_decl {
                    casing_map.insert(t.name.to_ascii_lowercase(), t.name.clone());
                }
                let repaired = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(
                    &accumulated,
                    &casing_map,
                );

                // If no tool calls were repaired, flush any remaining held-back text
                if repaired.is_empty() && accumulated.len() > streamed_text_len {
                    let remaining = &accumulated[streamed_text_len..];
                    match current_block {
                        ActiveBlock::Text(idx) => {
                            let delta = json!({
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": {"type": "text_delta", "text": remaining}
                            });
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                        }
                        ActiveBlock::Thinking(prev_idx) => {
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_stop").data(json!({"type": "content_block_stop", "index": prev_idx}).to_string()))).await;
                            let idx = next_block_index;
                            next_block_index += 1;
                            let blk_start = json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": {"type": "text", "text": ""}
                            });
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                            let delta = json!({
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": {"type": "text_delta", "text": remaining}
                            });
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                            current_block = ActiveBlock::Text(idx);
                        }
                        ActiveBlock::None => {
                            let idx = next_block_index;
                            next_block_index += 1;
                            let blk_start = json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": {"type": "text", "text": ""}
                            });
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_start").data(blk_start.to_string()))).await;
                            let delta = json!({
                                "type": "content_block_delta",
                                "index": idx,
                                "delta": {"type": "text_delta", "text": remaining}
                            });
                            let _ = sse_tx.send(Ok(Event::default().event("content_block_delta").data(delta.to_string()))).await;
                            current_block = ActiveBlock::Text(idx);
                        }
                    }
                }

                // Close active block
                match current_block {
                    ActiveBlock::Thinking(idx) | ActiveBlock::Text(idx) => {
                        let _ = sse_tx.send(Ok(Event::default().event("content_block_stop").data(json!({"type": "content_block_stop", "index": idx}).to_string()))).await;
                    }
                    ActiveBlock::None => {}
                }

                let mut stop_reason = "end_turn";
                if !repaired.is_empty() {
                    for (i, rep) in repaired.iter().enumerate() {
                        let current_idx = next_block_index + i;
                        let tc_start = json!({
                            "type": "content_block_start",
                            "index": current_idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": format!("call_acp_{}_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(), i),
                                "name": rep.name,
                                "input": {}
                            }
                        });
                        let _ = sse_tx
                            .send(Ok(Event::default()
                                .event("content_block_start")
                                .data(tc_start.to_string())))
                            .await;
                        let tc_delta = json!({
                            "type": "content_block_delta",
                            "index": current_idx,
                            "delta": {"type": "input_json_delta", "partial_json": rep.arguments.to_string()}
                        });
                        let _ = sse_tx
                            .send(Ok(Event::default()
                                .event("content_block_delta")
                                .data(tc_delta.to_string())))
                            .await;
                        let tc_stop = json!({"type": "content_block_stop", "index": current_idx});
                        let _ = sse_tx
                            .send(Ok(Event::default()
                                .event("content_block_stop")
                                .data(tc_stop.to_string())))
                            .await;
                    }
                    stop_reason = "tool_use";
                }

                // Calculate or extract final turn tokens
                let final_output = captured_usage
                    .map(|u| u.output_tokens)
                    .unwrap_or_else(|| {
                        let text_tok = crate::protocol::tokens::TokenCalculator::count_text(&accumulated);
                        let tc_tok: usize = repaired.iter().map(|r| {
                            crate::protocol::tokens::TokenCalculator::count_text(&r.name)
                                + crate::protocol::tokens::TokenCalculator::count_text(&r.arguments.to_string())
                                + 8
                        }).sum();
                        (text_tok + tc_tok).max(1) as u64
                    });

                let final_input = captured_usage
                    .map(|u| u.input_tokens)
                    .unwrap_or(calculated_input_tokens);

                let final_cached_read = captured_usage
                    .map(|u| u.cache_read_input_tokens)
                    .unwrap_or(0);

                let final_usage = crate::protocol::tokens::TokenUsage {
                    input_tokens: final_input,
                    output_tokens: final_output,
                    cache_read_input_tokens: final_cached_read,
                    cache_creation_input_tokens: 0,
                };

                // Record into centralized token tracker
                tracker_clone.record_turn(&s_key_clone, &req_model_clone, final_usage).await;

                eprintln!(
                    "📊 [Token Tracker] Turn: {} in / {} out | Cumulative: {} in / {} out (Session: {})",
                    final_usage.input_tokens,
                    final_usage.output_tokens,
                    tracker_clone.total_input_tokens(),
                    tracker_clone.total_output_tokens(),
                    s_key_clone
                );

                let msg_delta = json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                    "usage": {
                        "input_tokens": final_usage.input_tokens,
                        "output_tokens": final_usage.output_tokens,
                        "cache_read_input_tokens": final_usage.cache_read_input_tokens,
                        "cache_creation_input_tokens": 0
                    }
                });
                let _ = sse_tx
                    .send(Ok(Event::default()
                        .event("message_delta")
                        .data(msg_delta.to_string())))
                    .await;

                let msg_stop = json!({"type": "message_stop"});
                let _ = sse_tx
                    .send(Ok(Event::default()
                        .event("message_stop")
                        .data(msg_stop.to_string())))
                    .await;
            });

            return Sse::new(ReceiverStream::new(sse_rx))
                .keep_alive(KeepAlive::default())
                .into_response();
        }
    }

    (StatusCode::BAD_GATEWAY, "All upstream candidate providers failed").into_response()
}


pub fn get_streamable_text_end(s: &str) -> usize {
    crate::protocol::repair::ToolCallRepairEngine::get_streamable_text_end(s)
}

pub fn build_acp_prompt(req: &MessagesRequest) -> String {
    let mut parts = Vec::new();

    // 1. System Prompt Fusion
    let mut sys_text = String::new();
    if let Some(sys) = &req.system {
        match sys {
            SystemPrompt::Single(s) => sys_text.push_str(s),
            SystemPrompt::Blocks(blocks) => {
                let joined = blocks
                    .iter()
                    .map(|b| b.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                sys_text.push_str(&joined);
            }
        }
    }

    // 2. Compact Tools Instruction Injection
    if let Some(tools) = &req.tools {
        if !tools.is_empty() {
            let mut tool_guide = String::from(
                "\n\n[Tools Instruction]\nYou can call tools to perform actions. To call a tool, you MUST format it as:\n<tool_call>\n{\"name\": \"<tool_name>\", \"arguments\": {<args>}}\n</tool_call>\n\nAvailable tools:\n",
            );
            for t in tools {
                let name = &t.name;
                match name.as_str() {
                    "Bash" => tool_guide.push_str("- Bash: {\"command\": \"<command>\"} (Run shell commands. NOTE: Always execute commands from the working directory. Do NOT pass 'cwd' parameter; use 'cd <dir> && <cmd>' inside command string if changing directory).\n"),
                    "Read" => tool_guide.push_str("- Read: {\"file_path\": \"<file_path>\"} (Read file contents from filesystem. Do NOT use FileRead or View; use Read with 'file_path').\n"),
                    "Edit" => tool_guide.push_str("- Edit: {\"file_path\": \"<file_path>\", \"old_string\": \"<exact text to replace>\", \"new_string\": \"<replacement text>\"} (Replace exact string in file).\n"),
                    "Write" => tool_guide.push_str("- Write: {\"file_path\": \"<file_path>\", \"content\": \"<file content>\"} (Write file to filesystem).\n"),
                    _ => {
                        let props: Vec<String> = t
                            .input_schema
                            .get("properties")
                            .and_then(|p| p.as_object())
                            .map(|obj| obj.keys().cloned().collect())
                            .unwrap_or_default();
                        let desc = t.description.as_deref().unwrap_or("");
                        let short_desc = if desc.len() > 60 { &desc[..60] } else { desc };
                        tool_guide.push_str(&format!("- {}: keys: [{}] ({})\n", name, props.join(", "), short_desc));
                    }
                }
            }
            sys_text.push_str(&tool_guide);
        }
    }

    if !sys_text.trim().is_empty() {
        parts.push(format!("[System Instructions]\n{}", sys_text.trim()));
    }

    // 3. Conversation Messages
    for msg in &req.messages {
        let role_label = match msg.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            other => other,
        };

        match &msg.content {
            AnthropicContent::Text(t) => {
                if !t.trim().is_empty() {
                    parts.push(format!("{}: {}", role_label, t.trim()));
                }
            }
            AnthropicContent::Blocks(blocks) => {
                let mut block_parts = Vec::new();
                for b in blocks {
                    match b {
                        ContentBlock::Text { text } => {
                            if !text.trim().is_empty() {
                                block_parts.push(text.trim().to_string());
                            }
                        }
                        ContentBlock::Thinking { .. } => {
                            // Strip historical internal thinking blocks
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            block_parts.push(format!(
                                "<tool_call>\n{{\"name\": \"{}\", \"arguments\": {}}}\n</tool_call>",
                                name, input
                            ));
                        }
                        ContentBlock::ToolResult {
                            content,
                            is_error,
                            ..
                        } => {
                            let text_content = if let Some(s) = content.as_str() {
                                s.to_string()
                            } else if let Some(arr) = content.as_array() {
                                arr.iter()
                                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            } else {
                                content.to_string()
                            };

                            let err_str = if is_error.unwrap_or(false) {
                                " error=true"
                            } else {
                                ""
                            };
                            block_parts.push(format!(
                                "<tool_result{}>\n{}\n</tool_result>",
                                err_str,
                                text_content.trim()
                            ));
                        }
                    }
                }
                if !block_parts.is_empty() {
                    parts.push(format!("{}:\n{}", role_label, block_parts.join("\n\n")));
                }
            }
        }
    }

    if parts.is_empty() {
        "Hello".to_string()
    } else {
        parts.join("\n\n---\n\n")
    }
}
