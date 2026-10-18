use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

impl SseEvent {
    pub fn new(event: impl Into<String>, data: Value) -> Self {
        Self {
            event: event.into(),
            data,
        }
    }

    pub fn to_wire_string(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.event, self.data)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BlockState {
    None,
    Thinking,
    Text,
    ToolUse,
}

#[derive(Debug, Default, Clone)]
struct PendingNativeToolCall {
    id: String,
    raw_name: String,
    arguments_buf: String,
}

pub struct SseStreamTranslator {
    message_id: String,
    model: String,
    started: bool,
    state: BlockState,
    block_index: usize,
    has_emitted_blocks: bool,
    finished: bool,
    casing_map: HashMap<String, String>,
    has_native_tool_calls: bool,
    pending_native_tool_calls: HashMap<usize, PendingNativeToolCall>,
    accumulated_text: String,
    streamed_text_len: usize,
    has_emitted_text: bool,
    input_tokens: u64,
    upstream_output_tokens: Option<u64>,
    pub is_compact: bool,
    pub stream_thinking_as_text: bool,
    in_thinking_mode: bool,
}

impl SseStreamTranslator {
    pub fn new(message_id: &str, model: &str) -> Self {
        let stream_thinking = std::env::var("L2C_STREAM_THINKING")
            .map(|v| v == "1" || v == "true" || v == "text")
            .unwrap_or(false);
        Self {
            message_id: message_id.to_string(),
            model: model.to_string(),
            started: false,
            state: BlockState::None,
            block_index: 0,
            has_emitted_blocks: false,
            finished: false,
            casing_map: HashMap::new(),
            has_native_tool_calls: false,
            pending_native_tool_calls: HashMap::new(),
            accumulated_text: String::new(),
            streamed_text_len: 0,
            has_emitted_text: false,
            input_tokens: 50,
            upstream_output_tokens: None,
            is_compact: false,
            stream_thinking_as_text: stream_thinking,
            in_thinking_mode: false,
        }
    }

    pub fn with_input_tokens(mut self, tokens: u64) -> Self {
        self.input_tokens = tokens;
        self
    }

    pub fn with_compaction(mut self, is_compact: bool) -> Self {
        self.is_compact = is_compact;
        self
    }

    pub fn with_stream_thinking_as_text(mut self, enabled: bool) -> Self {
        self.stream_thinking_as_text = enabled;
        self
    }

    pub fn register_tool_casing(&mut self, declared_name: &str) {
        self.casing_map
            .insert(declared_name.to_ascii_lowercase(), declared_name.to_string());
    }

    pub fn feed_chunk(&mut self, chunk_str: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let parsed: Value = match serde_json::from_str(chunk_str) {
            Ok(v) => v,
            Err(_) => return events,
        };

        if let Some(usage) = parsed.get("usage") {
            if let Some(in_tok) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                self.input_tokens = in_tok;
            }
            if let Some(out_tok) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                self.upstream_output_tokens = Some(out_tok);
            }
        }

        if !self.started {
            self.started = true;
            events.push(self.create_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": self.message_id,
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": self.input_tokens, "output_tokens": 0}
                    }
                }),
            ));
        }

        let choice = match parsed.pointer("/choices/0") {
            Some(c) => c,
            None => return events,
        };

        let delta = choice.get("delta");
        let finish_reason = choice.get("finish_reason");

        if let Some(delta) = delta {
            // 1. Thinking / Reasoning content
            if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                if !reasoning.is_empty() {
                    if self.stream_thinking_as_text {
                        if self.state != BlockState::Text {
                            if self.state != BlockState::None {
                                events.push(self.create_event(
                                    "content_block_stop",
                                    json!({"type": "content_block_stop", "index": self.block_index}),
                                ));
                                self.block_index += 1;
                            } else if self.has_emitted_blocks {
                                self.block_index += 1;
                            }
                            self.has_emitted_blocks = true;
                            self.has_emitted_text = true;
                            self.state = BlockState::Text;
                            events.push(self.create_event(
                                "content_block_start",
                                json!({
                                    "type": "content_block_start",
                                    "index": self.block_index,
                                    "content_block": {"type": "text", "text": ""}
                                }),
                            ));
                            events.push(self.create_event(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": self.block_index,
                                    "delta": {"type": "text_delta", "text": crate::protocol::transform::THINKING_START_MARKER}
                                }),
                            ));
                            self.in_thinking_mode = true;
                        } else if !self.in_thinking_mode {
                            events.push(self.create_event(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": self.block_index,
                                    "delta": {"type": "text_delta", "text": crate::protocol::transform::THINKING_START_MARKER}
                                }),
                            ));
                            self.in_thinking_mode = true;
                        }

                        events.push(self.create_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {"type": "text_delta", "text": reasoning}
                            }),
                        ));
                    } else {
                        if self.state == BlockState::Text {
                            // Text block is actively streaming; do not split mid-sentence
                        } else {
                            if self.state != BlockState::Thinking {
                                if self.state != BlockState::None {
                                    events.push(self.create_event(
                                        "content_block_stop",
                                        json!({"type": "content_block_stop", "index": self.block_index}),
                                    ));
                                    self.block_index += 1;
                                } else if self.has_emitted_blocks {
                                    self.block_index += 1;
                                }
                                self.has_emitted_blocks = true;
                                self.state = BlockState::Thinking;
                                events.push(self.create_event(
                                    "content_block_start",
                                    json!({
                                        "type": "content_block_start",
                                        "index": self.block_index,
                                        "content_block": {"type": "thinking", "thinking": ""}
                                    }),
                                ));
                            }
                            let est = (reasoning.len() / 4).max(1);
                            events.push(self.create_event(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": self.block_index,
                                    "delta": {
                                        "type": "thinking_delta",
                                        "thinking": reasoning,
                                        "estimated_tokens": est
                                    }
                                }),
                            ));
                        }
                    }
                }
            }

            // 2. Normal text with tool tag leak prevention
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    if self.in_thinking_mode {
                        self.in_thinking_mode = false;
                        events.push(self.create_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {"type": "text_delta", "text": format!("\n{}\n\n", crate::protocol::transform::THINKING_END_MARKER)}
                            }),
                        ));
                    }
                    self.accumulated_text.push_str(text);
                    let streamable_end = crate::protocol::repair::ToolCallRepairEngine::get_streamable_text_end(&self.accumulated_text);
                    if streamable_end > self.streamed_text_len {
                        let to_stream = &self.accumulated_text[self.streamed_text_len..streamable_end];
                        if self.state != BlockState::Text {
                            if self.state != BlockState::None {
                                if self.state == BlockState::Thinking {
                                    events.push(self.create_event(
                                        "content_block_delta",
                                        json!({
                                            "type": "content_block_delta",
                                            "index": self.block_index,
                                            "delta": {
                                                "type": "signature_delta",
                                                "signature": "EqQBCgIYAhIM1gbcuvLz1029487561"
                                            }
                                        }),
                                    ));
                                }
                                events.push(self.create_event(
                                    "content_block_stop",
                                    json!({"type": "content_block_stop", "index": self.block_index}),
                                ));
                                self.block_index += 1;
                            } else if self.has_emitted_blocks {
                                self.block_index += 1;
                            }
                            self.has_emitted_blocks = true;
                            self.has_emitted_text = true;
                            self.state = BlockState::Text;
                            events.push(self.create_event(
                                "content_block_start",
                                json!({
                                    "type": "content_block_start",
                                    "index": self.block_index,
                                    "content_block": {"type": "text", "text": ""}
                                }),
                            ));
                        }
                        events.push(self.create_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {"type": "text_delta", "text": to_stream}
                            }),
                        ));
                        self.streamed_text_len = streamable_end;
                    }
                }
            }

            // 3. Native Tool Calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                if self.in_thinking_mode {
                    self.in_thinking_mode = false;
                    events.push(self.create_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {"type": "text_delta", "text": format!("\n{}\n\n", crate::protocol::transform::THINKING_END_MARKER)}
                        }),
                    ));
                }
                for tc in tool_calls {
                    self.has_native_tool_calls = true;
                    let idx = tc.get("index").and_then(|v| v.as_u64()).map(|u| u as usize).unwrap_or(0);
                    let entry = self.pending_native_tool_calls.entry(idx).or_default();
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        entry.id = id.to_string();
                    }
                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                            entry.raw_name.push_str(name);
                        }
                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                            entry.arguments_buf.push_str(args);
                        }
                    }
                }
            }
        }

        if let Some(reason) = finish_reason.and_then(|v| v.as_str()) {
            events.extend(self.emit_completion_events(Some(reason)));
        }

        events
    }

    /// Emits completion events, flushing blocks and tool calls idempotently
    fn emit_completion_events(&mut self, _upstream_finish_reason: Option<&str>) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.finished {
            return events;
        }
        self.finished = true;

        if self.in_thinking_mode {
            self.in_thinking_mode = false;
            events.push(self.create_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": self.block_index,
                    "delta": {"type": "text_delta", "text": format!("\n{}\n\n", crate::protocol::transform::THINKING_END_MARKER)}
                }),
            ));
        }

        if self.state != BlockState::None && self.state != BlockState::ToolUse {
            if self.state == BlockState::Thinking {
                events.push(self.create_event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": self.block_index,
                        "delta": {
                            "type": "signature_delta",
                            "signature": "EqQBCgIYAhIM1gbcuvLz1029487561"
                        }
                    }),
                ));
            }
            events.push(self.create_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": self.block_index}),
            ));
            self.state = BlockState::None;
        }


        let mut has_valid_tool_calls = false;
        if self.has_native_tool_calls && !self.pending_native_tool_calls.is_empty() {
            let mut sorted_keys: Vec<_> = self.pending_native_tool_calls.keys().copied().collect();
            sorted_keys.sort_unstable();

            for k in sorted_keys {
                if let Some(call) = self.pending_native_tool_calls.remove(&k) {
                    let id = if call.id.is_empty() { format!("call_native_{}", k) } else { call.id };
                    let mapped_name = crate::protocol::repair::ToolCallRepairEngine::map_tool_name(&call.raw_name, &self.casing_map);
                    let rep_args = crate::protocol::repair::repair_tool_arguments(&call.arguments_buf);
                    let parsed: Value = serde_json::from_str(&rep_args).unwrap_or_else(|_| json!({}));
                    let norm_args = crate::protocol::repair::ToolCallRepairEngine::normalize_arguments_with_raw(&mapped_name, &call.raw_name, parsed);

                    if let Some(repaired) = crate::protocol::repair::ToolCallRepairEngine::sanitize_and_validate(mapped_name, norm_args, &self.casing_map) {
                        has_valid_tool_calls = true;
                        if self.has_emitted_blocks {
                            self.block_index += 1;
                        }
                        self.has_emitted_blocks = true;
                        events.push(self.create_event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": self.block_index,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": id,
                                    "name": repaired.name,
                                    "input": {}
                                }
                            }),
                        ));
                        events.push(self.create_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": self.block_index,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": repaired.arguments.to_string()
                                }
                            }),
                        ));
                        events.push(self.create_event(
                            "content_block_stop",
                            json!({"type": "content_block_stop", "index": self.block_index}),
                        ));
                    }
                }
            }
            self.state = BlockState::None;
        }

        let stop_reason;
        if has_valid_tool_calls {
            stop_reason = "tool_use";
        } else {
            // Repair hallucinated tool calls if no valid native tool calls were present
            let repaired = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(
                &self.accumulated_text,
                &self.casing_map,
            );
            if !repaired.is_empty() {
                for (idx, rep) in repaired.into_iter().enumerate() {
                    if self.has_emitted_blocks {
                        self.block_index += 1;
                    }
                    self.has_emitted_blocks = true;

                    events.push(self.create_event(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": self.block_index,
                            "content_block": {
                                "type": "tool_use",
                                "id": format!("call_rep_{}", idx),
                                "name": rep.name,
                                "input": {}
                            }
                        }),
                    ));
                    events.push(self.create_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": rep.arguments.to_string()
                            }
                        }),
                    ));
                    events.push(self.create_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": self.block_index}),
                    ));
                }
                stop_reason = "tool_use";
            } else {
                stop_reason = "end_turn";
                let raw_remaining = if self.accumulated_text.len() > self.streamed_text_len {
                    &self.accumulated_text[self.streamed_text_len..]
                } else {
                    ""
                };
                let cleaned_remaining = crate::protocol::repair::ToolCallRepairEngine::clean_user_visible_text(raw_remaining);

                if !cleaned_remaining.is_empty() {
                    // Flush held back text
                    if self.has_emitted_blocks {
                        self.block_index += 1;
                    }
                    self.has_emitted_blocks = true;
                    self.has_emitted_text = true;

                    events.push(self.create_event(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": self.block_index,
                            "content_block": {"type": "text", "text": ""}
                        }),
                    ));
                    events.push(self.create_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {"type": "text_delta", "text": cleaned_remaining}
                        }),
                    ));
                    events.push(self.create_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": self.block_index}),
                    ));
                    self.streamed_text_len = self.accumulated_text.len();
                } else if !self.has_emitted_text && stop_reason != "tool_use" {
                    // Critical: if only thinking was emitted, Claude Code requires at least one visible content block!
                    if self.has_emitted_blocks {
                        self.block_index += 1;
                    }
                    self.has_emitted_blocks = true;
                    self.has_emitted_text = true;
                    let cleaned_acc = crate::protocol::repair::ToolCallRepairEngine::clean_user_visible_text(&self.accumulated_text);
                    let fallback_text = if self.is_compact {
                        "<summary>\nConversation compacted.\n- Key decisions and project context preserved.\n</summary>".to_string()
                    } else if !cleaned_acc.is_empty() {
                        cleaned_acc
                    } else {
                        "Analyzing requirements and preparing next steps.".to_string()
                    };
                    events.push(self.create_event(
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": self.block_index,
                            "content_block": {"type": "text", "text": ""}
                        }),
                    ));
                    events.push(self.create_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {"type": "text_delta", "text": fallback_text}
                        }),
                    ));
                    events.push(self.create_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": self.block_index}),
                    ));
                }
            }
        }

        let out_tokens = self.upstream_output_tokens.unwrap_or_else(|| {
            crate::protocol::tokens::TokenCalculator::count_text(&self.accumulated_text).max(1) as u64
        });

        events.push(self.create_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {
                    "input_tokens": self.input_tokens,
                    "output_tokens": out_tokens
                }
            }),
        ));
        events.push(self.create_event("message_stop", json!({"type": "message_stop"})));

        events
    }

    /// Guarantees that active blocks and stream completion events are emitted
    /// even if upstream closes abruptly without sending finish_reason
    pub fn finish_stream_if_needed(&mut self) -> Vec<SseEvent> {
        self.emit_completion_events(None)
    }

    fn create_event(&self, event_name: &str, data: Value) -> SseEvent {
        SseEvent::new(event_name, data)
    }
}

use crate::protocol::sanitize::{split_utf8_prefix, strip_system_tags, RENDER_DELTA_CHUNK_BYTES};
use crate::protocol::thinking::{ThinkingRouteItem, ThinkingRouter};
use crate::protocol::tracker::SseBlockTracker;

/// Robust incremental SSE stream emitter for Anthropic Messages API.
///
/// Features:
/// 1. Smooth real-time token streaming without multi-second stalls (256-byte chunk pacing).
/// 2. Zero truncation of post-tool content (preserves questions, tables, and explanations).
/// 3. Strict Anthropic block lifecycle compliance via SseBlockTracker.
/// 4. Interleaved text -> tool_use -> text block transitions.
/// 5. Thinking segmentation (16KB) and automatic leaked tag routing via ThinkingRouter.
pub struct AnthropicBlockStreamer {
    pub message_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub casing_map: HashMap<String, String>,
    pub tracker: SseBlockTracker,
    pub thinking_router: ThinkingRouter,
    pub text_buffer: String,
    pub streamed_in_current_block: usize,
    pub emitted_tool_count: usize,
    pub has_emitted_text_block: bool,
    pub is_compact: bool,
    pub stream_thinking_as_text: bool,
    pub in_thinking_mode: bool,
}

impl AnthropicBlockStreamer {
    pub fn new(
        message_id: &str,
        model: &str,
        input_tokens: u64,
        casing_map: HashMap<String, String>,
    ) -> Self {
        let stream_thinking = std::env::var("L2C_STREAM_THINKING")
            .map(|v| v == "1" || v == "true" || v == "text")
            .unwrap_or(false);
        Self {
            message_id: message_id.to_string(),
            model: model.to_string(),
            input_tokens,
            casing_map,
            tracker: SseBlockTracker::new(),
            thinking_router: ThinkingRouter::new(),
            text_buffer: String::new(),
            streamed_in_current_block: 0,
            emitted_tool_count: 0,
            has_emitted_text_block: false,
            is_compact: false,
            stream_thinking_as_text: stream_thinking,
            in_thinking_mode: false,
        }
    }

    pub fn with_compaction(mut self, is_compact: bool) -> Self {
        self.is_compact = is_compact;
        self
    }

    pub fn with_stream_thinking_as_text(mut self, enabled: bool) -> Self {
        self.stream_thinking_as_text = enabled;
        self
    }

    pub fn start(&self) -> SseEvent {
        SseEvent::new(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": 1,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0
                    }
                }
            }),
        )
    }

    pub fn feed_thinking(&mut self, chunk: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if chunk.is_empty() {
            return events;
        }

        let items = self.thinking_router.process_thinking(chunk);
        for item in items {
            match item {
                ThinkingRouteItem::SegmentBreak => {
                    if !self.stream_thinking_as_text {
                        self.emit_close_thinking(&mut events);
                    }
                }
                ThinkingRouteItem::Thinking(fragment) => {
                    self.emit_thinking_fragment(&fragment, &mut events);
                }
                ThinkingRouteItem::Text(fragment) => {
                    self.feed_text_internal(&fragment, &mut events);
                }
            }
        }

        events
    }

    pub fn feed_text(&mut self, chunk: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if chunk.is_empty() {
            return events;
        }

        let items = self.thinking_router.process_text(chunk);
        for item in items {
            match item {
                ThinkingRouteItem::Thinking(fragment) => {
                    self.emit_thinking_fragment(&fragment, &mut events);
                }
                ThinkingRouteItem::SegmentBreak => {
                    if !self.stream_thinking_as_text {
                        self.emit_close_thinking(&mut events);
                    }
                }
                ThinkingRouteItem::Text(fragment) => {
                    self.feed_text_internal(&fragment, &mut events);
                }
            }
        }

        events
    }

    fn emit_close_thinking(&mut self, events: &mut Vec<SseEvent>) {
        if let Some(idx) = self.tracker.close_thinking() {
            events.push(SseEvent::new(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {
                        "type": "signature_delta",
                        "signature": "EqQBCgIYAhIM1gbcuvLz1029487561"
                    }
                }),
            ));
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": idx}),
            ));
        }
    }

    fn emit_thinking_fragment(&mut self, fragment: &str, events: &mut Vec<SseEvent>) {
        if fragment.is_empty() {
            return;
        }

        // Only stream thinking as text if explicitly requested
        if self.stream_thinking_as_text {
            if !self.in_thinking_mode {
                self.in_thinking_mode = true;
                self.emit_text_delta(crate::protocol::transform::THINKING_START_MARKER, events);
            }
            self.emit_text_delta(fragment, events);
            return;
        }

        // If a text block is currently actively streaming, do NOT split it mid-sentence
        if self.tracker.text_idx().is_some() {
            return;
        }

        let (idx, is_new, closed_text) = self.tracker.ensure_thinking();
        if let Some(c_idx) = closed_text {
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": c_idx}),
            ));
        }
        if is_new {
            events.push(SseEvent::new(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {"type": "thinking", "thinking": ""}
                }),
            ));
        }

        let mut remaining = fragment;
        while !remaining.is_empty() {
            let (sub, rest) = split_utf8_prefix(remaining, RENDER_DELTA_CHUNK_BYTES);
            remaining = rest;
            let est = (sub.len() / 4).max(1);
            events.push(SseEvent::new(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": sub,
                        "estimated_tokens": est
                    }
                }),
            ));
        }
    }

    fn feed_text_internal(&mut self, chunk: &str, events: &mut Vec<SseEvent>) {
        if chunk.is_empty() {
            return;
        }

        if self.in_thinking_mode {
            self.in_thinking_mode = false;
            let close_marker = format!("\n{}\n\n", crate::protocol::transform::THINKING_END_MARKER);
            self.emit_text_delta(&close_marker, events);
        }

        self.text_buffer.push_str(chunk);

        loop {
            let open_tag_info = Self::find_tool_tag_start(&self.text_buffer);

            match open_tag_info {
                None => {
                    let holdback = Self::get_partial_tag_prefix_len(&self.text_buffer);
                    let safe_len = self.text_buffer.len().saturating_sub(holdback);

                    if safe_len > self.streamed_in_current_block {
                        let to_stream = self.text_buffer[self.streamed_in_current_block..safe_len].to_string();
                        if !to_stream.trim().is_empty() || self.tracker.text_idx().is_some() {
                            self.emit_text_delta(&to_stream, events);
                        }
                        self.streamed_in_current_block = safe_len;
                    }
                    break;
                }
                Some((tag_start, open_tag_len, close_tag)) => {
                    if tag_start > self.streamed_in_current_block {
                        let pre_text = self.text_buffer[self.streamed_in_current_block..tag_start].to_string();
                        if !pre_text.trim().is_empty() {
                            self.emit_text_delta(&pre_text, events);
                        }
                        self.streamed_in_current_block = tag_start;
                    }

                    if let Some(c_idx) = self.tracker.close_text() {
                        events.push(SseEvent::new(
                            "content_block_stop",
                            json!({"type": "content_block_stop", "index": c_idx}),
                        ));
                    }
                    self.emit_close_thinking(events);

                    let search_from = tag_start + open_tag_len;
                    if let Some(rel_close) = self.text_buffer[search_from..].find(close_tag) {
                        let close_end = search_from + rel_close + close_tag.len();
                        let tool_xml = &self.text_buffer[tag_start..close_end];

                        let repaired = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(
                            tool_xml,
                            &self.casing_map,
                        );

                        for rep in repaired {
                            if let Some(valid) = crate::protocol::repair::ToolCallRepairEngine::sanitize_and_validate(
                                rep.name,
                                rep.arguments,
                                &self.casing_map,
                            ) {
                                let tc_idx = self.tracker.next_index();
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis();
                                let call_id = format!("call_acp_{}_{}", now_ms, self.emitted_tool_count);
                                self.emitted_tool_count += 1;

                                events.push(SseEvent::new(
                                    "content_block_start",
                                    json!({
                                        "type": "content_block_start",
                                        "index": tc_idx,
                                        "content_block": {
                                            "type": "tool_use",
                                            "id": call_id,
                                            "name": valid.name,
                                            "input": {}
                                        }
                                    }),
                                ));

                                events.push(SseEvent::new(
                                    "content_block_delta",
                                    json!({
                                        "type": "content_block_delta",
                                        "index": tc_idx,
                                        "delta": {
                                            "type": "input_json_delta",
                                            "partial_json": valid.arguments.to_string()
                                        }
                                    }),
                                ));

                                events.push(SseEvent::new(
                                    "content_block_stop",
                                    json!({"type": "content_block_stop", "index": tc_idx}),
                                ));
                            }
                        }

                        self.text_buffer = self.text_buffer[close_end..].to_string();
                        self.streamed_in_current_block = 0;
                    } else {
                        break;
                    }
                }
            }
        }
    }

    fn emit_text_delta(&mut self, text: &str, events: &mut Vec<SseEvent>) {
        if text.is_empty() {
            return;
        }
        let (idx, is_new, closed_thinking) = self.tracker.ensure_text();
        if let Some(c_idx) = closed_thinking {
            events.push(SseEvent::new(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": c_idx,
                    "delta": {
                        "type": "signature_delta",
                        "signature": "EqQBCgIYAhIM1gbcuvLz1029487561"
                    }
                }),
            ));
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": c_idx}),
            ));
        }
        if is_new {
            events.push(SseEvent::new(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {"type": "text", "text": ""}
                }),
            ));
            self.has_emitted_text_block = true;
        }

        let mut actual_text = text.to_string();
        if self.is_compact && is_new && !actual_text.contains("<summary>") {
            actual_text = format!("<summary>\n{}", actual_text);
        }

        let mut remaining = actual_text.as_str();
        while !remaining.is_empty() {
            let (sub, rest) = split_utf8_prefix(remaining, RENDER_DELTA_CHUNK_BYTES);
            remaining = rest;
            events.push(SseEvent::new(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {"type": "text_delta", "text": sub}
                }),
            ));
        }
    }

    pub fn finish(
        &mut self,
        final_input_tokens: u64,
        final_output_tokens: u64,
        cached_read_tokens: u64,
    ) -> Vec<SseEvent> {
        let mut events = Vec::new();

        let leftover_items = self.thinking_router.finish();
        for item in leftover_items {
            match item {
                ThinkingRouteItem::Thinking(fragment) => {
                    self.emit_thinking_fragment(&fragment, &mut events);
                }
                ThinkingRouteItem::Text(fragment) => {
                    self.text_buffer.push_str(&fragment);
                }
                ThinkingRouteItem::SegmentBreak => {}
            }
        }

        if self.in_thinking_mode {
            self.in_thinking_mode = false;
            let close_marker = format!("\n{}\n\n", crate::protocol::transform::THINKING_END_MARKER);
            self.emit_text_delta(&close_marker, &mut events);
        }

        if let Some((tag_start, _, _)) = Self::find_tool_tag_start(&self.text_buffer) {
            if tag_start > self.streamed_in_current_block {
                let pre = self.text_buffer[self.streamed_in_current_block..tag_start].to_string();
                if !pre.trim().is_empty() {
                    self.emit_text_delta(&pre, &mut events);
                }
            }

            if let Some(c_idx) = self.tracker.close_text() {
                events.push(SseEvent::new(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": c_idx}),
                ));
            }

            let unclosed_xml = &self.text_buffer[tag_start..];
            let repaired = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(
                unclosed_xml,
                &self.casing_map,
            );

            if !repaired.is_empty() {
                for rep in repaired {
                    if let Some(valid) = crate::protocol::repair::ToolCallRepairEngine::sanitize_and_validate(
                        rep.name,
                        rep.arguments,
                        &self.casing_map,
                    ) {
                        let tc_idx = self.tracker.next_index();
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis();
                        let call_id = format!("call_acp_{}_{}", now_ms, self.emitted_tool_count);
                        self.emitted_tool_count += 1;

                        events.push(SseEvent::new(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": tc_idx,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": call_id,
                                    "name": valid.name,
                                    "input": {}
                                }
                            }),
                        ));
                        events.push(SseEvent::new(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": tc_idx,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": valid.arguments.to_string()
                                }
                            }),
                        ));
                        events.push(SseEvent::new(
                            "content_block_stop",
                            json!({"type": "content_block_stop", "index": tc_idx}),
                        ));
                    }
                }
            } else {
                let cleaned = crate::protocol::repair::ToolCallRepairEngine::clean_user_visible_text(unclosed_xml);
                let cleaned = strip_system_tags(&cleaned);
                if !cleaned.trim().is_empty() || (self.tracker.text_idx().is_some() && !cleaned.is_empty()) {
                    self.emit_text_delta(&cleaned, &mut events);
                }
            }
        } else if self.text_buffer.len() > self.streamed_in_current_block {
            let remaining = &self.text_buffer[self.streamed_in_current_block..];
            let cleaned = crate::protocol::repair::ToolCallRepairEngine::clean_user_visible_text(remaining);
            let cleaned = strip_system_tags(&cleaned);
            if !cleaned.trim().is_empty() || (self.tracker.text_idx().is_some() && !cleaned.is_empty()) {
                self.emit_text_delta(&cleaned, &mut events);
            }
        }

        if self.is_compact && self.has_emitted_text_block && !self.text_buffer.contains("</summary>") {
            if let Some(t_idx) = self.tracker.text_idx() {
                events.push(SseEvent::new(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": t_idx,
                        "delta": {"type": "text_delta", "text": "\n</summary>"}
                    }),
                ));
            }
        }

        for (kind, idx) in self.tracker.close_all() {
            if kind == "thinking" {
                events.push(SseEvent::new(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": {
                            "type": "signature_delta",
                            "signature": "EqQBCgIYAhIM1gbcuvLz1029487561"
                        }
                    }),
                ));
            }
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": idx}),
            ));
        }

        if !self.has_emitted_text_block && self.emitted_tool_count == 0 {
            let idx = self.tracker.next_index();
            let fallback_text = if self.is_compact {
                "<summary>\nConversation compacted.\n- Key decisions and project context preserved.\n</summary>"
            } else {
                "Analyzing requirements and preparing next steps."
            };
            events.push(SseEvent::new(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {"type": "text", "text": ""}
                }),
            ));
            events.push(SseEvent::new(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": idx,
                    "delta": {
                        "type": "text_delta",
                        "text": fallback_text
                    }
                }),
            ));
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": idx}),
            ));
        }

        let stop_reason = if self.emitted_tool_count > 0 {
            "tool_use"
        } else {
            "end_turn"
        };

        events.push(SseEvent::new(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {
                    "input_tokens": final_input_tokens,
                    "output_tokens": final_output_tokens,
                    "cache_read_input_tokens": cached_read_tokens,
                    "cache_creation_input_tokens": 0
                }
            }),
        ));

        events.push(SseEvent::new("message_stop", json!({"type": "message_stop"})));

        events
    }

    fn find_tool_tag_start(s: &str) -> Option<(usize, usize, &'static str)> {
        let tag_candidates = [
            ("<tool_call", "</tool_call>"),
            ("<function_call", "</function_call>"),
            ("<invoke", "</invoke>"),
            ("<action", "</action>"),
            ("<｜tool call begin｜>", "<｜tool call end｜>"),
            ("<|tool call begin|>", "<|tool call end|>"),
        ];

        let mut earliest: Option<(usize, usize, &'static str)> = None;
        for (open_prefix, close_tag) in tag_candidates {
            if let Some(pos) = s.find(open_prefix) {
                if let Some(rel_gt) = s[pos..].find('>') {
                    let open_tag_len = rel_gt + 1;
                    match earliest {
                        Some((earliest_pos, _, _)) if pos < earliest_pos => {
                            earliest = Some((pos, open_tag_len, close_tag));
                        }
                        None => {
                            earliest = Some((pos, open_tag_len, close_tag));
                        }
                        _ => {}
                    }
                }
            }
        }
        earliest
    }

    fn get_partial_tag_prefix_len(s: &str) -> usize {
        let prefixes = [
            "<tool_call>", "<tool_call ", "<tool_call\n", "<tool_call", "<tool_cal", "<tool_ca", "<tool_c", "<tool_", "<tool", "<too", "<to",
            "<function_call>", "<function_call ", "<function_call\n", "<function_call", "<function_ca", "<function_c", "<function_", "<function",
            "<invoke ", "<invoke\n", "<invoke", "<invok", "<invo", "<inv",
            "<action>", "<action ", "<action\n", "<action", "<actio", "<acti", "<act", "<ac",
            "<｜tool call begin｜>", "<｜tool call begin", "<｜tool call", "<｜tool", "<｜too", "<｜to", "<｜t", "<｜",
            "<|tool call begin|>", "<|tool call begin", "<|tool call", "<|tool", "<|too", "<|to",
            "<",
        ];
        for p in prefixes {
            if s.ends_with(p) {
                return p.len();
            }
        }
        0
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smooth_text_streaming_no_tools() {
        let mut streamer = AnthropicBlockStreamer::new("msg_1", "claude-sonnet-5-5", 100, HashMap::new());
        let _ = streamer.start();

        let ev1 = streamer.feed_text("Hello world! ");
        assert_eq!(ev1.len(), 2); // content_block_start + content_block_delta
        assert_eq!(ev1[0].event, "content_block_start");
        assert_eq!(ev1[1].event, "content_block_delta");
        assert_eq!(ev1[1].data["delta"]["text"], "Hello world! ");

        let ev2 = streamer.feed_text("This is smooth streaming.");
        assert_eq!(ev2.len(), 1); // content_block_delta only
        assert_eq!(ev2[0].data["delta"]["text"], "This is smooth streaming.");

        let final_evs = streamer.finish(100, 20, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[1].event, "message_delta");
        assert_eq!(final_evs[1].data["delta"]["stop_reason"], "end_turn");
        assert_eq!(final_evs[2].event, "message_stop");
    }

    #[test]
    fn test_interleaved_text_tool_and_trailing_questions() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer = AnthropicBlockStreamer::new("msg_2", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Feed pre-tool text
        let ev1 = streamer.feed_text("Trước tiên cần rà soát repo:\n\n");
        assert_eq!(ev1.len(), 2); // start(0, text) + delta(0, text)
        assert_eq!(ev1[0].data["index"], 0);

        // Feed tool call and trailing questions in one or multiple chunks
        let tool_chunk = "<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls -la\"}}\n</tool_call>\n\n## 6) Sẵn sàng spawn subagent\n1. Chuẩn hóa README\n2. Hoàn thiện toàn diện";
        let ev2 = streamer.feed_text(tool_chunk);

        // Expected sequence:
        // 1. content_block_stop(0) for pre-text
        // 2. content_block_start(1, tool_use)
        // 3. content_block_delta(1, input_json_delta)
        // 4. content_block_stop(1)
        // 5. content_block_start(2, text)
        // 6. content_block_delta(2, text_delta: "## 6) Sẵn sàng spawn subagent...")
        assert_eq!(ev2.len(), 6);
        assert_eq!(ev2[0].event, "content_block_stop");
        assert_eq!(ev2[0].data["index"], 0);

        assert_eq!(ev2[1].event, "content_block_start");
        assert_eq!(ev2[1].data["index"], 1);
        assert_eq!(ev2[1].data["content_block"]["type"], "tool_use");
        assert_eq!(ev2[1].data["content_block"]["name"], "Bash");

        assert_eq!(ev2[2].event, "content_block_delta");
        assert_eq!(ev2[2].data["index"], 1);
        assert_eq!(ev2[2].data["delta"]["type"], "input_json_delta");

        assert_eq!(ev2[3].event, "content_block_stop");
        assert_eq!(ev2[3].data["index"], 1);

        assert_eq!(ev2[4].event, "content_block_start");
        assert_eq!(ev2[4].data["index"], 2);
        assert_eq!(ev2[4].data["content_block"]["type"], "text");

        assert_eq!(ev2[5].event, "content_block_delta");
        assert_eq!(ev2[5].data["index"], 2);
        let trailing_text = ev2[5].data["delta"]["text"].as_str().unwrap();
        assert!(trailing_text.contains("## 6) Sẵn sàng spawn subagent"));
        assert!(trailing_text.contains("1. Chuẩn hóa README"));

        let final_evs = streamer.finish(100, 50, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[0].data["index"], 2);
        assert_eq!(final_evs[1].event, "message_delta");
        assert_eq!(final_evs[1].data["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn test_partial_tag_split_across_chunks() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer = AnthropicBlockStreamer::new("msg_3", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Chunk 1 ends with partial prefix "<tool_"
        let ev1 = streamer.feed_text("Wait for command: <tool_");
        assert_eq!(ev1.len(), 2);
        assert_eq!(ev1[1].data["delta"]["text"], "Wait for command: ");

        // Chunk 2 completes opening tag and delivers tool body
        let ev2 = streamer.feed_text("call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"pwd\"}}\n</tool_call>\nDone!");
        assert_eq!(ev2[0].event, "content_block_stop"); // stops block 0
        assert_eq!(ev2[1].event, "content_block_start"); // tool_use block 1
        assert_eq!(ev2[2].event, "content_block_delta"); // input_json_delta
        assert_eq!(ev2[3].event, "content_block_stop"); // tool_use stop block 1
        assert_eq!(ev2[4].event, "content_block_start"); // text block 2
        assert_eq!(ev2[5].event, "content_block_delta"); // text_delta "Done!"

        let final_evs = streamer.finish(100, 30, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[0].data["index"], 2);
    }

    #[test]
    fn test_multiple_consecutive_tools_interleaved_with_text() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        casing_map.insert("read".to_string(), "Read".to_string());
        let mut streamer = AnthropicBlockStreamer::new("msg_4", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Feed pre text
        let _ = streamer.feed_text("Step 1: Check files\n");
        // Feed tool 1
        let _ = streamer.feed_text("<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>\n");
        // Feed tool 2
        let _ = streamer.feed_text("<tool_call>\n{\"name\": \"Read\", \"arguments\": {\"file_path\": \"README.md\"}}\n</tool_call>\n");
        // Feed trailing questions
        let evs = streamer.feed_text("All checks completed. Which task would you like to run next?");
        assert!(!evs.is_empty());

        let final_evs = streamer.finish(100, 60, 0);
        assert_eq!(final_evs.last().unwrap().event, "message_stop");
        assert_eq!(final_evs[final_evs.len() - 2].data["delta"]["stop_reason"], "tool_use");
        assert_eq!(streamer.emitted_tool_count, 2);
    }

    #[test]
    fn test_unclosed_tool_call_repaired_at_stream_end() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer = AnthropicBlockStreamer::new("msg_5", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Model cuts off inside tool call without </tool_call>
        let _ = streamer.feed_text("Starting scan: <tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"whoami\"");
        let final_evs = streamer.finish(100, 20, 0);

        // Tool should be repaired and emitted as tool_use
        let tool_start = final_evs.iter().find(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use");
        assert!(tool_start.is_some());
        assert_eq!(final_evs.last().unwrap().event, "message_stop");
        assert_eq!(final_evs[final_evs.len() - 2].data["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn test_streamer_leaked_thinking_rerouted_from_text() {
        let mut streamer = AnthropicBlockStreamer::new("msg_6", "claude-sonnet-5-5", 100, HashMap::new())
            .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // Feed text containing leaked thinking closed by </think>
        let evs = streamer.feed_text("Inspecting codebase structure...</think>Here is the project overview.");

        // Expected:
        // 1. content_block_start(0, thinking)
        // 2. content_block_delta(0, thinking_delta: "Inspecting codebase structure...")
        // 3. content_block_delta(0, signature_delta)
        // 4. content_block_stop(0)
        // 5. content_block_start(1, text)
        // 6. content_block_delta(1, text_delta: "Here is the project overview.")
        assert_eq!(evs.len(), 6);
        assert_eq!(evs[0].event, "content_block_start");
        assert_eq!(evs[0].data["index"], 0);
        assert_eq!(evs[0].data["content_block"]["type"], "thinking");

        assert_eq!(evs[1].event, "content_block_delta");
        assert_eq!(evs[1].data["index"], 0);
        assert_eq!(evs[1].data["delta"]["thinking"], "Inspecting codebase structure...");
        assert!(evs[1].data["delta"]["estimated_tokens"].as_u64().unwrap_or(0) > 0);

        assert_eq!(evs[2].event, "content_block_delta");
        assert_eq!(evs[2].data["index"], 0);
        assert_eq!(evs[2].data["delta"]["type"], "signature_delta");

        assert_eq!(evs[3].event, "content_block_stop");
        assert_eq!(evs[3].data["index"], 0);

        assert_eq!(evs[4].event, "content_block_start");
        assert_eq!(evs[4].data["index"], 1);
        assert_eq!(evs[4].data["content_block"]["type"], "text");

        assert_eq!(evs[5].event, "content_block_delta");
        assert_eq!(evs[5].data["index"], 1);
        assert_eq!(evs[5].data["delta"]["text"], "Here is the project overview.");

        let final_evs = streamer.finish(100, 40, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[0].data["index"], 1);
        assert_eq!(final_evs[1].data["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn test_streamer_thinking_16kb_segmentation() {
        let mut streamer = AnthropicBlockStreamer::new("msg_7", "claude-sonnet-5-5", 100, HashMap::new())
            .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // Feed 20KB thinking
        let chunk = "x".repeat(20000);
        let evs = streamer.feed_thinking(&chunk);

        // Must contain block stop for segment 0 and block start for segment 1
        let has_segment_stop = evs.iter().any(|e| e.event == "content_block_stop" && e.data["index"] == 0);
        let has_segment_start = evs.iter().any(|e| e.event == "content_block_start" && e.data["index"] == 1);
        assert!(has_segment_stop);
        assert!(has_segment_start);
    }

    #[test]
    fn test_streamer_action_tag() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer = AnthropicBlockStreamer::new("msg_action", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        let chunk = "Executing build:\n<action name=\"Bash\">{\"command\": \"cargo build\"}</action>\nDone.";
        let evs = streamer.feed_text(chunk);

        let has_tool_start = evs.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["name"] == "Bash");
        assert!(has_tool_start);

        let final_evs = streamer.finish(100, 30, 0);
        assert_eq!(final_evs.iter().find(|e| e.event == "message_delta").unwrap().data["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn test_streamer_dsml_tag() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer = AnthropicBlockStreamer::new("msg_dsml", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        let chunk = "Running command:\n<｜tool calls begin｜><｜tool call begin｜>function<｜tool sep｜>bash\n{\"command\": \"git status\"}<｜tool call end｜><｜tool calls end｜>";
        let evs = streamer.feed_text(chunk);

        let has_tool_start = evs.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["name"] == "Bash");
        assert!(has_tool_start);

        let final_evs = streamer.finish(100, 30, 0);
        assert_eq!(final_evs.iter().find(|e| e.event == "message_delta").unwrap().data["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn test_anthropic_block_streamer_thinking_as_text() {
        let mut streamer = AnthropicBlockStreamer::new("msg_t2", "claude-sonnet-5-5", 100, HashMap::new())
            .with_stream_thinking_as_text(true);
        let _ = streamer.start();

        let evs1 = streamer.feed_thinking("Exploring the problem space...");
        assert!(evs1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "text"));
        assert!(evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("💭 Thinking:\n")
        }));
        assert!(evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("Exploring the problem space...")
        }));

        let evs2 = streamer.feed_text("Here is the solution.");
        assert!(evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("💭 (Done thinking)")
        }));
        assert!(evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("Here is the solution.")
        }));

        let final_evs = streamer.finish(100, 50, 0);
        assert!(final_evs.iter().any(|e| e.event == "content_block_stop"));
        assert!(final_evs.iter().any(|e| e.event == "message_stop"));
    }

    #[test]
    fn test_sse_stream_translator_thinking_as_text() {
        let mut translator = SseStreamTranslator::new("msg_t1", "deepseek")
            .with_stream_thinking_as_text(true);

        // Feed reasoning chunk
        let chunk1 = json!({
            "choices": [{
                "delta": {
                    "reasoning_content": "Analyzing steganography in avatar.png..."
                }
            }]
        }).to_string();
        let evs1 = translator.feed_chunk(&chunk1);

        // Must start text block and emit text deltas
        let has_text_start = evs1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "text");
        assert!(has_text_start, "Expected content_block_start with type text");

        let has_thinking_prefix = evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("💭 Thinking:\n")
        });
        assert!(has_thinking_prefix, "Expected text_delta with thinking start marker");

        let has_reasoning_text = evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("Analyzing steganography")
        });
        assert!(has_reasoning_text, "Expected reasoning chunk as text_delta");

        // Feed normal text chunk
        let chunk2 = json!({
            "choices": [{
                "delta": {
                    "content": "Found 3 hidden chunks."
                }
            }]
        }).to_string();
        let evs2 = translator.feed_chunk(&chunk2);

        // Must emit thinking end marker and then the normal text
        let has_end_marker = evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("💭 (Done thinking)")
        });
        assert!(has_end_marker, "Expected text_delta with thinking end marker");
    }

    #[test]
    fn test_thinking_after_text_never_splits_text_block() {
        let mut streamer = AnthropicBlockStreamer::new("msg_no_split", "claude-sonnet-5-5", 100, HashMap::new())
            .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // 1. First feed normal text
        let evs1 = streamer.feed_text("Workflow tool không có trong session này — tôi fan-out trực tiếp bằng 7 sub");
        let start_text_count = evs1.iter().filter(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "text").count();
        assert_eq!(start_text_count, 1);

        // 2. Leaked or delayed thinking chunk arrives mid-sentence
        let evs2 = streamer.feed_thinking("thinking from another session or delayed reasoning");
        // Must NOT close the text block or open a thinking block!
        let has_thinking_start = evs2.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking");
        let has_text_stop = evs2.iter().any(|e| e.event == "content_block_stop");
        assert!(!has_thinking_start, "Must NOT open thinking block after text started");
        assert!(!has_text_stop, "Must NOT close text block on mid-sentence thinking");
        let has_marker = evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["text"].as_str().unwrap_or("").contains("💭")
        });
        assert!(!has_marker, "Must NOT emit raw thinking markers into text");

        // 3. Second part of text arrives
        let evs3 = streamer.feed_text("agent song song, mỗi agent một giả thuyết độc lập.");
        // Must NOT start another text block (no extra bullet point)
        let has_new_text_start = evs3.iter().any(|e| e.event == "content_block_start");
        assert!(!has_new_text_start, "Must NOT start a new text block, sentence must remain unbroken");

        let final_evs = streamer.finish(100, 50, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[1].event, "message_delta");
    }

    #[test]
    fn test_streamer_thinking_between_tools_never_emits_text_markers() {
        let mut streamer = AnthropicBlockStreamer::new("msg_between", "claude-sonnet-5-5", 100, HashMap::new())
            .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // 1. Initial text and tool
        let evs1 = streamer.feed_text("Let me inspect directory.\n<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>\n");
        assert!(evs1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"));

        // 2. OpenCode thought arrives after tool
        let evs2 = streamer.feed_thinking("The directory has 3 files. Let me continue.");
        // Must emit native thinking_delta, NEVER text_delta with 💭 Thinking:
        let has_thinking_block = evs2.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking");
        assert!(has_thinking_block, "Expected native thinking block between tools");
        let has_thinking_delta = evs2.iter().any(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta");
        assert!(has_thinking_delta, "Expected native thinking_delta");
        let has_text_delta = evs2.iter().any(|e| e.data["delta"]["type"] == "text_delta");
        assert!(!has_text_delta, "Must NOT emit text_delta for thinking");
        let has_raw_marker = evs2.iter().any(|e| e.data["delta"]["text"].as_str().unwrap_or("").contains("💭"));
        assert!(!has_raw_marker, "Must NOT emit raw thinking markers");

        let finish_evs = streamer.finish(100, 50, 0);
        let has_sig = finish_evs.iter().any(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "signature_delta");
        assert!(has_sig, "Must emit signature_delta before stopping thinking block");
    }

    #[test]
    fn test_streamer_thinking_closes_with_signature_delta_on_transition() {
        let mut streamer = AnthropicBlockStreamer::new("msg_sig", "claude-sonnet-5-5", 100, HashMap::new())
            .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        let _ = streamer.feed_thinking("Contemplating solution...");
        let text_evs = streamer.feed_text("Here is the verified answer.");

        let sig_idx = text_evs.iter().position(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "signature_delta");
        let stop_idx = text_evs.iter().position(|e| e.event == "content_block_stop" && e.data["index"] == 0);
        let text_start_idx = text_evs.iter().position(|e| e.event == "content_block_start" && e.data["index"] == 1);

        assert!(sig_idx.is_some(), "Must emit signature_delta");
        assert!(stop_idx.is_some(), "Must emit content_block_stop for thinking");
        assert!(text_start_idx.is_some(), "Must emit content_block_start for text");
        assert!(sig_idx.unwrap() < stop_idx.unwrap(), "signature_delta must precede content_block_stop");
        assert!(stop_idx.unwrap() < text_start_idx.unwrap(), "content_block_stop must precede text content_block_start");
    }
}


