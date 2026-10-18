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

    // Background idle cleanup: evicts sessions inactive for > 5 minutes
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            session_mgr.sweep_idle(std::time::Duration::from_secs(300)).await;
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
    let is_compact = req.is_compaction_request();

    let tools_summary: Vec<String> = req.tools.as_ref().map(|ts| ts.iter().map(|t| t.resolved_name().to_string()).collect()).unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();

    let last_preview = req.messages.last().map(|m| {
        let text = match &m.content {
            AnthropicContent::Text(t) => t.clone(),
            AnthropicContent::Blocks(b) => {
                b.iter().map(|block| match block {
                    ContentBlock::Text { text } => text.clone(),
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        format!("tool_result({}): err={:?} content={}", tool_use_id, is_error, content)
                    },
                    ContentBlock::ToolUse { id, name, input } => {
                        format!("tool_use({} - {}): {}", name, id, input)
                    },
                    ContentBlock::Thinking { thinking, .. } => {
                        format!("thinking: {}", thinking)
                    },
                    ContentBlock::Image { .. } => "[Image]".to_string(),
                    ContentBlock::Other => "[Other]".to_string(),
                }).collect::<Vec<_>>().join(" | ")
            }
        };
        let preview = text.chars().take(200).collect::<String>().replace('\n', " ");
        format!("[{}] {}", m.role, preview)
    }).unwrap_or_default();

    let log_line = format!(
        "[ts={}] [REQ] is_compact={} model={} stream={} tools={:?} msgs={} last={}\n",
        now, is_compact, req.model, req.stream, tools_summary, req.messages.len(), last_preview
    );
    eprint!("{}", log_line);
    if std::env::var("L2C_DEBUG_LOG").is_ok() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/lily2claude_traffic.log") {
            use std::io::Write;
            let _ = f.write_all(log_line.as_bytes());
        }
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
                        let status = upstream_resp.status();
                        let body = upstream_resp.text().await.unwrap_or_default();
                        let fail_line = format!(
                            "[ts={}] [UPSTREAM_FAIL] provider={} model={} status={} body={}\n",
                            now, target.provider, target.model, status, body.replace('\n', " ")
                        );
                        eprint!("{}", fail_line);
                        if std::env::var("L2C_DEBUG_LOG").is_ok() {
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/lily2claude_traffic.log") {
                                use std::io::Write;
                                let _ = f.write_all(fail_line.as_bytes());
                            }
                        }
                        state.router.mark_provider_degraded(&target.provider, 180);
                        continue; // Rate limited or errored, try next candidate
                    }

                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
                    let model_clone = req.model.clone();
                    let tools_decl = req.tools.clone().unwrap_or_default();

                    tokio::spawn(async move {
                        let mut translator = SseStreamTranslator::new("msg_live", &model_clone)
                            .with_input_tokens(calculated_input_tokens)
                            .with_compaction(is_compact);
                        for t in tools_decl {
                            translator.register_tool_casing(&t.name);
                        }

                        let mut stream = upstream_resp.bytes_stream();
                        let mut line_buffer = String::new();
                        'stream_loop: while let Some(item) = stream.next().await {
                            if let Ok(bytes) = item {
                                line_buffer.push_str(&String::from_utf8_lossy(&bytes));
                                while let Some(idx) = line_buffer.find('\n') {
                                    let line_str = line_buffer[..idx].trim().to_string();
                                    line_buffer.drain(..=idx);
                                    if let Some(data) = line_str.strip_prefix("data: ") {
                                        if data == "[DONE]" {
                                            break 'stream_loop;
                                        }
                                        let events = translator.feed_chunk(data);
                                        for ev in events {
                                            if std::env::var("L2C_DEBUG_LOG").is_ok() {
                                                if ev.event == "content_block_start" && ev.data.get("content_block").and_then(|b| b.get("type")).and_then(|t| t.as_str()) == Some("tool_use") {
                                                    let tool_name = ev.data["content_block"]["name"].as_str().unwrap_or("unknown");
                                                    let tool_id = ev.data["content_block"]["id"].as_str().unwrap_or("unknown");
                                                    let t_log = format!("[ts={}] [SSE_TOOL] name={} id={}\n", now, tool_name, tool_id);
                                                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/lily2claude_traffic.log") {
                                                        use std::io::Write;
                                                        let _ = f.write_all(t_log.as_bytes());
                                                    }
                                                } else if ev.event == "content_block_delta" && ev.data.get("delta").and_then(|d| d.get("type")).and_then(|t| t.as_str()) == Some("input_json_delta") {
                                                    let args = ev.data["delta"]["partial_json"].as_str().unwrap_or("");
                                                    let a_log = format!("[ts={}] [SSE_TOOL_INPUT] {}\n", now, args.replace('\n', " "));
                                                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/lily2claude_traffic.log") {
                                                        use std::io::Write;
                                                        let _ = f.write_all(a_log.as_bytes());
                                                    }
                                                }
                                            }
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
                    Ok((raw_text, Ok(Ok(usage)))) => {
                        let out_tokens = if usage.output_tokens > 0 {
                            usage.output_tokens
                        } else {
                            crate::protocol::tokens::TokenCalculator::count_text(&raw_text).max(1) as u64
                        };
                        let turn_usage = crate::protocol::tokens::TokenUsage::new(calculated_input_tokens, out_tokens);
                        state.token_tracker.record_turn(&session_key, &req.model, turn_usage).await;

                        let casing_map: std::collections::HashMap<String, String> = req.tools.as_ref().map(|ts| {
                            ts.iter().map(|t| (t.resolved_name().to_ascii_lowercase(), t.resolved_name().to_string())).collect()
                        }).unwrap_or_default();

                        let repaired_tools = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(&raw_text, &casing_map);
                        let clean_text = crate::protocol::repair::ToolCallRepairEngine::clean_user_visible_text(&raw_text);
                        let clean_text = crate::protocol::sanitize::strip_system_tags(&clean_text);

                        let mut content_blocks = Vec::new();
                        if !clean_text.trim().is_empty() {
                            let text_val = if is_compact && !clean_text.contains("<summary>") {
                                format!("<summary>\n{}\n</summary>", clean_text.trim())
                            } else {
                                clean_text.trim().to_string()
                            };
                            content_blocks.push(json!({
                                "type": "text",
                                "text": text_val
                            }));
                        }

                        let has_tools = !repaired_tools.is_empty();
                        for (idx, tool) in repaired_tools.into_iter().enumerate() {
                            if let Some(valid) = crate::protocol::repair::ToolCallRepairEngine::sanitize_and_validate(tool.name, tool.arguments, &casing_map) {
                                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
                                let call_id = format!("call_acp_{}_{}", now_ms, idx);
                                content_blocks.push(json!({
                                    "type": "tool_use",
                                    "id": call_id,
                                    "name": valid.name,
                                    "input": valid.arguments
                                }));
                            }
                        }

                        if content_blocks.is_empty() {
                            content_blocks.push(json!({
                                "type": "text",
                                "text": if is_compact { "<summary>\nCompaction complete.\n</summary>" } else { "Done." }
                            }));
                        }

                        let stop_reason = if has_tools { "tool_use" } else { "end_turn" };

                        return Json(json!({
                            "id": "msg_non_stream",
                            "type": "message",
                            "role": "assistant",
                            "model": req.model,
                            "content": content_blocks,
                            "stop_reason": stop_reason,
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
                let res = agent_clone
                    .execute_turn(
                        &model_clone,
                        &p_text,
                        Some(&s_key),
                        Some(&session_mgr),
                        prompt_tx,
                    )
                    .await;
                if let Err(ref e) = res {
                    eprintln!(">>> EXECUTE_TURN FAILED: {:?}", e);
                } else {
                    eprintln!(">>> EXECUTE_TURN FINISHED OK");
                }
                res
            });

            let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
            let tools_decl = req.tools.clone().unwrap_or_default();
            let s_key_clone = session_key.clone();
            let req_model_clone = req.model.clone();
            let tracker_clone = state.token_tracker.clone();
            let session_mgr_stream = state.session_mgr.clone();
            let provider_name = target.provider.clone();

            let mut casing_map = std::collections::HashMap::new();
            for t in &tools_decl {
                casing_map.insert(t.name.to_ascii_lowercase(), t.name.clone());
            }

            tokio::spawn(async move {
                let mut streamer = crate::protocol::stream::AnthropicBlockStreamer::new(
                    "msg_live_acp",
                    &req_model_clone,
                    calculated_input_tokens,
                    casing_map,
                ).with_compaction(is_compact);

                let start_ev = streamer.start();
                if sse_tx
                    .send(Ok(Event::default()
                        .event(start_ev.event)
                        .data(start_ev.data.to_string())))
                    .await
                    .is_err()
                {
                    exec_task.abort();
                    session_mgr_stream.evict_session(&s_key_clone, &provider_name).await;
                    return;
                }

                let mut captured_usage: Option<crate::protocol::tokens::TokenUsage> = None;
                let inter_chunk_timeout = std::time::Duration::from_secs(300);

                loop {
                    let recv_next = tokio::time::timeout(inter_chunk_timeout, prompt_rx.recv()).await;
                    match recv_next {
                        Ok(Some(item)) => {
                            let events = match item {
                                crate::protocol::tokens::AcpOutputChunk::Thinking(chunk) => {
                                    streamer.feed_thinking(&chunk)
                                }
                                crate::protocol::tokens::AcpOutputChunk::Text(chunk) => {
                                    streamer.feed_text(&chunk)
                                }
                                crate::protocol::tokens::AcpOutputChunk::Usage(usage) => {
                                    captured_usage = Some(usage);
                                    Vec::new()
                                }
                            };

                            for ev in events {
                                if sse_tx
                                    .send(Ok(Event::default().event(ev.event).data(ev.data.to_string())))
                                    .await
                                    .is_err()
                                {
                                    exec_task.abort();
                                    session_mgr_stream.evict_session(&s_key_clone, &provider_name).await;
                                    return;
                                }
                            }
                        }
                        _ => break, // Stream completed or idle timeout reached
                    }
                }

                let mut exec_task = exec_task;
                let exec_res = tokio::select! {
                    res = &mut exec_task => Ok(res),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                        exec_task.abort();
                        Err(())
                    }
                };

                let is_turn_ok = match &exec_res {
                    Ok(Ok(Ok(_))) => true,
                    _ => false,
                };

                if !is_turn_ok {
                    session_mgr_stream.evict_session(&s_key_clone, &provider_name).await;
                    if streamer.emitted_tool_count > 0 {
                        eprintln!(">>> EXECUTE_TURN FAILED after emitting tool calls: {:?}. Gracefully finishing turn so tool execution can proceed.", exec_res);
                    } else {
                        eprintln!(">>> EXECUTE_TURN FAILED: {:?}. Emitting API error event.", exec_res);
                        let _ = sse_tx.send(Ok(Event::default().event("error").data(serde_json::json!({
                            "type": "error",
                            "error": {
                                "type": "api_error",
                                "message": "Upstream inference error. Retrying..."
                            }
                        }).to_string()))).await;
                        return;
                    }
                }

                let final_input = captured_usage
                    .map(|u| u.input_tokens)
                    .unwrap_or(calculated_input_tokens);

                let final_output = captured_usage
                    .map(|u| u.output_tokens)
                    .unwrap_or_else(|| {
                        let text_tok = crate::protocol::tokens::TokenCalculator::count_text(&streamer.text_buffer);
                        (text_tok + streamer.emitted_tool_count * 15).max(1) as u64
                    });

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

                let final_events = streamer.finish(
                    final_usage.input_tokens,
                    final_usage.output_tokens,
                    final_usage.cache_read_input_tokens,
                );

                for ev in final_events {
                    let _ = sse_tx
                        .send(Ok(Event::default().event(ev.event).data(ev.data.to_string())))
                        .await;
                }
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

pub use crate::protocol::prompt::build_acp_prompt;
