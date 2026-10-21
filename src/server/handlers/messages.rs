use crate::protocol::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};
use crate::protocol::prompt::build_acp_prompt_with_budget;
use crate::protocol::stream::{AcpStreamAdapter, AnthropicStreamEngine, OpenAiStreamAdapter};
use crate::protocol::transform::transform_anthropic_to_openai;
use crate::server::state::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::StreamExt;
use serde_json::json;
use std::convert::Infallible;
use tokio_stream::wrappers::ReceiverStream;

fn should_degrade(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
}

/// Generates a unique Anthropic message id per streamed turn.
///
/// Claude Code indexes its conversation history by assistant `message.id`.
/// Reusing a constant id (e.g. `msg_live_acp`) makes every later assistant
/// message collapse into the first one, so tool results never reach the model
/// and it repeats the same tool call until `--max-turns` aborts the session.
fn next_message_id(prefix: &str) -> String {
    format!("{}_{}", prefix, uuid::Uuid::new_v4().simple())
}

pub async fn messages_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<MessagesRequest>,
) -> Response {
    let session_key = crate::engine::session::extract_session_key_from_headers_or_req(&headers, &req);
    let calculated_input_tokens = crate::protocol::tokens::TokenCalculator::count_request_tokens(&req) as u64;
    let is_compact = req.is_compaction_request();

    // `input_tokens` must describe the prompt the model actually receives: system
    // prompt + tool catalog + every message.  It used to be the size of the LAST
    // message only, with the rest relabelled as "cache_read" on the assumption
    // that earlier turns were cached.  Nothing ever confirmed that cache hit, and
    // when a turn ended on a large tool_result the reported prompt collapsed to
    // (or exploded to) the size of that single block — that is the 11.1k -> 90k
    // jump in Claude Code's context meter.  Report the measured prompt and leave
    // the cache fields for usage that the provider itself reports.
    let (initial_input_tokens, initial_cached_tokens) = (calculated_input_tokens.max(1), 0u64);

    let tools_summary: Vec<String> = req
        .tools
        .as_ref()
        .map(|ts| ts.iter().map(|t| t.resolved_name().to_string()).collect())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let last_preview = req
        .messages
        .last()
        .map(|m| {
            let text = match &m.content {
                AnthropicContent::Text(t) => t.clone(),
                AnthropicContent::Blocks(b) => b
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => text.clone(),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            format!("tool_result({}): err={:?} content={}", tool_use_id, is_error, content)
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            format!("tool_use({} - {}): {}", name, id, input)
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            format!("thinking: {}", thinking)
                        }
                        ContentBlock::Image { .. } => "[Image]".to_string(),
                        ContentBlock::Document { .. } => "[Document]".to_string(),
                        ContentBlock::Other => "[Other]".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" | "),
            };
            let preview = text.chars().take(200).collect::<String>().replace('\n', " ");
            format!("[{}] {}", m.role, preview)
        })
        .unwrap_or_default();

    let is_debug = std::env::var_os("L2C_DEBUG_LOG").is_some();
    if is_debug {
        let log_line = format!(
            "[ts={}] [REQ] is_compact={} model={} stream={} tools={:?} msgs={} last={}\n",
            now, is_compact, req.model, req.stream, tools_summary, req.messages.len(), last_preview
        );
        eprint!("{}", log_line);
        if let Ok(mut f) = crate::config::open_private_append(&crate::config::debug_log_path()) {
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
                match http_adapter
                    .send_request_with_session(&openai_req, Some(&session_key))
                    .await
                {
                    Ok(upstream_resp) => {
                        let status = upstream_resp.status();
                        if !status.is_success() {
                            if should_degrade(status) {
                                state.router.mark_provider_degraded(&target.provider, 180);
                            }
                            continue; // Try next candidate if upstream errored
                        }
                        let bytes = upstream_resp.bytes().await.unwrap_or_default();
                        let val: serde_json::Value = match serde_json::from_slice(&bytes) {
                            Ok(value) => value,
                            Err(err) => {
                                return (
                                    StatusCode::BAD_GATEWAY,
                                    format!("Invalid JSON from upstream: {}", err),
                                )
                                    .into_response();
                            }
                        };
                        if let Some(error) = val.get("error") {
                            if should_degrade(status) {
                                state.router.mark_provider_degraded(&target.provider, 180);
                            }
                            return (
                                StatusCode::BAD_GATEWAY,
                                format!("Upstream error: {}", error),
                            )
                                .into_response();
                        }
                        let content_text = val
                            .pointer("/choices/0/message/content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

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
            match http_adapter
                .send_request_with_session(&openai_req, Some(&session_key))
                .await
            {
                Ok(upstream_resp) => {
                    if !upstream_resp.status().is_success() {
                        let status = upstream_resp.status();
                        let body = upstream_resp.text().await.unwrap_or_default();
                        if is_debug {
                            let fail_line = format!(
                                "[ts={}] [UPSTREAM_FAIL] provider={} model={} status={} body={}\n",
                                now, target.provider, target.model, status, body.replace('\n', " ")
                            );
                            eprint!("{}", fail_line);
                            if let Ok(mut f) = crate::config::open_private_append(&crate::config::debug_log_path())
                            {
                                use std::io::Write;
                                let _ = f.write_all(fail_line.as_bytes());
                            }
                        }
                        if should_degrade(status) {
                            state.router.mark_provider_degraded(&target.provider, 180);
                        }
                        continue; // Rate limited or errored, try next candidate
                    }

                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(128);
                    let message_id = next_message_id("msg_live");
                    let model_clone = req.model.clone();
                    let request_model = req.model.clone();
                    let tools_decl = req.tools.clone().unwrap_or_default();
                    let tracker = state.token_tracker.clone();
                    let s_key = session_key.clone();

                    tokio::spawn(async move {
                        let mut adapter = OpenAiStreamAdapter::new(
                            &message_id,
                            &model_clone,
                            calculated_input_tokens,
                            is_compact,
                            &tools_decl,
                        );

                        let is_debug = std::env::var_os("L2C_DEBUG_LOG").is_some();
                        let mut stream = upstream_resp.bytes_stream();
                        'stream_loop: while let Some(item) = stream.next().await {
                            let events = match item {
                                Ok(bytes) => adapter.feed_bytes(&bytes),
                                Err(err) => adapter.error(&format!("upstream stream error: {}", err)),
                            };
                            for ev in events {
                                if is_debug {
                                    if ev.event == "content_block_start"
                                        && ev
                                            .data
                                            .get("content_block")
                                            .and_then(|b| b.get("type"))
                                            .and_then(|t| t.as_str())
                                            == Some("tool_use")
                                    {
                                        let tool_name = ev.data["content_block"]["name"].as_str().unwrap_or("unknown");
                                        let tool_id = ev.data["content_block"]["id"].as_str().unwrap_or("unknown");
                                        let t_log = format!("[ts={}] [SSE_TOOL] name={} id={}\n", now, tool_name, tool_id);
                                        if let Ok(mut f) = crate::config::open_private_append(&crate::config::debug_log_path())
                                        {
                                            use std::io::Write;
                                            let _ = f.write_all(t_log.as_bytes());
                                        }
                                    } else if ev.event == "content_block_delta"
                                        && ev
                                            .data
                                            .get("delta")
                                            .and_then(|d| d.get("type"))
                                            .and_then(|t| t.as_str())
                                            == Some("input_json_delta")
                                    {
                                        let args = ev.data["delta"]["partial_json"].as_str().unwrap_or("");
                                        let a_log = format!("[ts={}] [SSE_TOOL_INPUT] {}\n", now, args.replace('\n', " "));
                                        if let Ok(mut f) = crate::config::open_private_append(&crate::config::debug_log_path())
                                        {
                                            use std::io::Write;
                                            let _ = f.write_all(a_log.as_bytes());
                                        }
                                    }
                                }
                                let is_error = ev.event == "error";
                                let _ = tx
                                    .send(Ok(Event::default()
                                        .event(ev.event)
                                        .data(ev.data.to_string())))
                                    .await;
                                if is_error {
                                    break 'stream_loop;
                                }
                            }
                        }

                        // Guarantees clean stream closure even if upstream socket drops
                        let tail_events = adapter.finish();
                        for ev in tail_events {
                            let _ = tx
                                .send(Ok(Event::default()
                                    .event(ev.event)
                                    .data(ev.data.to_string())))
                                .await;
                        }

                        let output_tokens = adapter.output_tokens().max(1);
                        let usage = crate::protocol::tokens::TokenUsage::new(
                            calculated_input_tokens,
                            output_tokens,
                        );
                        tracker.record_turn(&s_key, &request_model, usage).await;
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
            // Budget against the model that will actually serve the turn, not the
            // Claude-facing alias: an alias may resolve to a much smaller context
            // window (e.g. mimo at 64k) than the name suggests.
            let context_limit = crate::protocol::prompt::model_context_limit(&target.model);
            let prompt_text = build_acp_prompt_with_budget(&req, context_limit);

            if !req.stream {
                let (prompt_tx, mut prompt_rx) =
                    tokio::sync::mpsc::channel::<crate::protocol::tokens::AcpOutputChunk>(32);
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
                        // Prefer the provider's own input count, but reject a
                        // cumulative session total replayed as a turn value.
                        let in_tokens = if usage.input_tokens > 0
                            && usage.input_tokens <= calculated_input_tokens.saturating_mul(4)
                        {
                            usage.input_tokens
                        } else {
                            calculated_input_tokens
                        };
                        let turn_usage = crate::protocol::tokens::TokenUsage::new(in_tokens, out_tokens);
                        state.token_tracker.record_turn(&session_key, &req.model, turn_usage).await;

                        let tools_decl = req.tools.clone().unwrap_or_default();
                        let tool_engine = crate::protocol::tool_engine::ToolCallEngine::new(&tools_decl);
                        let (valid_calls, errors) = tool_engine.extract_with_errors(&raw_text);
                        if !errors.is_empty() {
                            eprintln!(
                                "[tool_engine] rejected {} non-stream candidate(s) for model {}",
                                errors.len(),
                                req.model
                            );
                        }

                        let clean_text = crate::protocol::tool_engine::strip_tool_tags(&raw_text);
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

                        let has_tools = !valid_calls.is_empty();
                        for (idx, valid) in valid_calls.into_iter().enumerate() {
                            let call_id = format!("call_acp_nonstream_{}", idx);
                            content_blocks.push(json!({
                                "type": "tool_use",
                                "id": call_id,
                                "name": valid.name,
                                "input": valid.arguments
                            }));
                        }

                        if content_blocks.is_empty() {
                            content_blocks.push(json!({
                                "type": "text",
                                "text": ""
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
                            "usage": {"input_tokens": in_tokens, "output_tokens": out_tokens}
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
            let (prompt_tx, mut prompt_rx) =
                tokio::sync::mpsc::channel::<crate::protocol::tokens::AcpOutputChunk>(1024);
            let agent_clone = agent.clone();
            let model_clone = target.model.clone();
            let session_mgr = state.session_mgr.clone();
            let s_key = session_key.clone();
            let p_text = prompt_text.clone();

            let mut exec_task = tokio::spawn(async move {
                let res = agent_clone
                    .execute_turn(
                        &model_clone,
                        &p_text,
                        Some(&s_key),
                        Some(&session_mgr),
                        prompt_tx,
                    )
                    .await;
                if is_debug {
                    if let Err(ref e) = res {
                        eprintln!(">>> EXECUTE_TURN FAILED: {:?}", e);
                    } else {
                        eprintln!(">>> EXECUTE_TURN FINISHED OK");
                    }
                }
                res
            });

            // Quick check (50ms): a candidate that fails immediately should
            // fail over.  A candidate that *succeeds* immediately is valid:
            // some agents answer short prompts before the first SSE poll.
            let mut immediate_chunk: Option<crate::protocol::tokens::AcpOutputChunk> = None;
            let mut immediate_success: Option<crate::protocol::tokens::TokenUsage> = None;
            let immediate_result = tokio::select! {
                task_res = &mut exec_task => Some(task_res),
                chunk = prompt_rx.recv() => {
                    immediate_chunk = chunk;
                    None
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(50)) => None,
            };

            if let Some(task_res) = immediate_result {
                match task_res {
                    Ok(Ok(usage)) => {
                        immediate_success = Some(usage);
                    }
                    other => {
                        if is_debug {
                            eprintln!(">>> ACP candidate {} failed immediately (<50ms): {:?}", target.provider, other);
                        }
                        state.session_mgr.evict_session(&session_key, &target.provider).await;
                        state.router.mark_provider_degraded(&target.provider, 300);
                        continue;
                    }
                }
            }

            let (sse_tx, sse_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(1024);
            let tools_decl = req.tools.clone().unwrap_or_default();
            let message_id = next_message_id("msg_live_acp");
            let s_key_clone = session_key.clone();
            let req_model_clone = req.model.clone();
            let tracker_clone = state.token_tracker.clone();
            let session_mgr_stream = state.session_mgr.clone();
            let provider_name = target.provider.clone();

            let mut casing_map = std::collections::HashMap::new();
            for t in &tools_decl {
                let declared = t.resolved_name();
                casing_map.insert(declared.to_ascii_lowercase(), declared.to_string());
            }

            tokio::spawn(async move {
                let engine = AnthropicStreamEngine::new(
                    &message_id,
                    &req_model_clone,
                    initial_input_tokens,
                    casing_map,
                )
                .with_cached_tokens(initial_cached_tokens)
                .with_compaction(is_compact)
                .with_tools(&tools_decl);
                let mut streamer = AcpStreamAdapter::new(engine);

                // Send message_start immediately so Claude Code receives HTTP 200 OK headers and enters streaming mode
                let start_ev = streamer.engine.start();
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

                // Process initial chunk if captured during the 50ms check
                if let Some(first_item) = immediate_chunk {
                    if let crate::protocol::tokens::AcpOutputChunk::Usage(usage) = &first_item {
                        captured_usage = Some(*usage);
                    }
                    let initial_events = streamer.feed(first_item);
                    for ev in initial_events {
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

                let timeout_secs = std::env::var("L2C_STREAM_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(300);
                let inter_chunk_timeout = std::time::Duration::from_secs(timeout_secs);
                let ping_interval = std::time::Duration::from_secs(5);
                let mut last_activity = tokio::time::Instant::now();

                loop {
                    let recv_next = tokio::time::timeout(ping_interval, prompt_rx.recv()).await;
                    match recv_next {
                        Ok(Some(item)) => {
                            last_activity = tokio::time::Instant::now();
                            if let crate::protocol::tokens::AcpOutputChunk::Usage(usage) = &item {
                                captured_usage = Some(*usage);
                            }
                            let events = streamer.feed(item);

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
                        Ok(None) => break, // Stream completed normally
                        Err(_) => {
                            // 5s elapsed without a chunk from upstream
                            if last_activity.elapsed() >= inter_chunk_timeout {
                                if is_debug {
                                    eprintln!(
                                        "[Server SSE] Stream idle silence exceeded {:?}, completing stream gracefully.",
                                        inter_chunk_timeout
                                    );
                                }
                                break;
                            }
                            // Emit Anthropic SSE ping event to prevent client watchdog drop
                            if sse_tx
                                .send(Ok(Event::default().event("ping").data(serde_json::json!({"type": "ping"}).to_string())))
                                .await
                                .is_err()
                            {
                                exec_task.abort();
                                session_mgr_stream.evict_session(&s_key_clone, &provider_name).await;
                                return;
                            }
                        }
                    }
                }

                let mut exec_task = exec_task;
                let exec_res = if let Some(usage) = immediate_success {
                    Ok(Ok(Ok(usage)))
                } else {
                    tokio::select! {
                        res = &mut exec_task => Ok(res),
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                            exec_task.abort();
                            Err(())
                        }
                    }
                };

                let is_turn_ok = matches!(&exec_res, Ok(Ok(Ok(_))));

                if !is_turn_ok {
                    session_mgr_stream.evict_session(&s_key_clone, &provider_name).await;
                    if streamer.engine.emitted_tool_count > 0 || !streamer.engine.text_buffer.trim().is_empty() {
                        if is_debug {
                            eprintln!(">>> EXECUTE_TURN FAILED/TIMED OUT after emitting content: {:?}. Gracefully finishing turn so client can proceed.", exec_res);
                        }
                    } else {
                        let err_msg = match &exec_res {
                            Ok(Ok(Err(e))) => format!("Upstream agent error: {}", e),
                            Ok(Err(e)) => format!("Task execution error: {}", e),
                            Err(_) => "Task execution timed out".to_string(),
                            _ => "Unknown error executing turn".to_string(),
                        };
                        if is_debug {
                            eprintln!(">>> EXECUTE_TURN FAILED: {}. Emitting error message and cleanly finishing turn.", err_msg);
                        }
                        let err_events = streamer.engine.feed_text(&format!("\n\n⚠️ [{}]\n", err_msg));
                        for ev in err_events {
                            let _ = sse_tx
                                .send(Ok(Event::default().event(ev.event).data(ev.data.to_string())))
                                .await;
                        }
                    }
                }

                let total_prompt_tokens = calculated_input_tokens;

                // Token accounting policy:
                //   1. If the provider (ACP result / OpenAI SSE) reported usage for
                //      THIS turn, forward it verbatim — that is the only
                //      ground-truth number available.
                //   2. Otherwise report what we measured ourselves: the request
                //      size for input and the rendered text for output.  Never
                //      invent a cache split, and never clamp a plausible provider
                //      number down to a guess.
                // Always measure what the model actually rendered, even when the
                // provider reported usage: some report `output_tokens: 0`, and
                // recording a zero to the ledger (and to the SSE message_delta)
                // is worse than a tokenizer estimate.
                let measured_out = {
                    let text_tok = crate::protocol::tokens::TokenCalculator::count_text(&streamer.engine.text_buffer);
                    (text_tok + streamer.engine.emitted_tool_count * 15).max(1) as u64
                };

                let (final_input, final_cached_read) = match captured_usage {
                    // A sane per-turn report can be larger than our own estimate
                    // (different tokenizer, tool schemas we do not model), but a
                    // figure an order of magnitude past the whole request is a
                    // cumulative session total replayed as a turn value.  In that
                    // case fall back to the measured prompt instead of lying.
                    Some(u)
                        if u.input_tokens.saturating_add(u.cache_read_input_tokens) > 0
                            && u.input_tokens
                                .saturating_add(u.cache_read_input_tokens)
                                <= total_prompt_tokens.saturating_mul(4) =>
                    {
                        (u.input_tokens.max(1), u.cache_read_input_tokens)
                    }
                    _ => (initial_input_tokens, 0),
                };

                let upstream_output = captured_usage.map(|u| u.output_tokens).unwrap_or(0);
                let final_output = if upstream_output > 0 {
                    upstream_output
                } else {
                    measured_out
                };

                let final_usage = crate::protocol::tokens::TokenUsage {
                    input_tokens: final_input,
                    output_tokens: final_output,
                    cache_read_input_tokens: final_cached_read,
                    cache_creation_input_tokens: 0,
                };

                // Record into centralized token tracker
                tracker_clone
                    .record_turn(&s_key_clone, &req_model_clone, final_usage)
                    .await;

                if is_debug {
                    eprintln!(
                        "📊 [Token Tracker] Turn: {} in / {} out | Cumulative: {} in / {} out (Session: {})",
                        final_usage.input_tokens,
                        final_usage.output_tokens,
                        tracker_clone.total_input_tokens(),
                        tracker_clone.total_output_tokens(),
                        s_key_clone
                    );
                }

                streamer.engine.input_tokens = final_usage.input_tokens;
                streamer.engine.cached_read_tokens = final_usage.cache_read_input_tokens;
                let final_events = streamer.finish(final_usage.output_tokens);

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

#[cfg(test)]
mod tests {
    use super::next_message_id;

    #[test]
    fn streamed_turn_message_ids_are_unique() {
        let first = next_message_id("msg_live_acp");
        let second = next_message_id("msg_live_acp");

        assert!(first.starts_with("msg_live_acp_"));
        assert_ne!(
            first, second,
            "each streamed turn must use a distinct Anthropic message id"
        );
    }
}
