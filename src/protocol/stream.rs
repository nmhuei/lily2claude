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
    input_tokens: u64,
    upstream_output_tokens: Option<u64>,
}

impl SseStreamTranslator {
    pub fn new(message_id: &str, model: &str) -> Self {
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
            input_tokens: 50,
            upstream_output_tokens: None,
        }
    }

    pub fn with_input_tokens(mut self, tokens: u64) -> Self {
        self.input_tokens = tokens;
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
                    events.push(self.create_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": self.block_index,
                            "delta": {"type": "thinking_delta", "thinking": reasoning}
                        }),
                    ));
                }
            }

            // 2. Normal text with tool tag leak prevention
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    self.accumulated_text.push_str(text);
                    let streamable_end = crate::protocol::repair::ToolCallRepairEngine::get_streamable_text_end(&self.accumulated_text);
                    if streamable_end > self.streamed_text_len {
                        let to_stream = &self.accumulated_text[self.streamed_text_len..streamable_end];
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
                for tc in tool_calls {
                    self.has_native_tool_calls = true;
                    let idx = tc.get("index").and_then(|v| v.as_u64()).map(|u| u as usize).unwrap_or(0);

                    let mut should_start = false;
                    let mut start_raw_name = String::new();
                    let mut start_id = String::new();

                    {
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

                        // Start tool_use content block immediately once tool name is known
                        if !entry.started && !entry.raw_name.is_empty() {
                            entry.started = true;
                            should_start = true;
                            start_raw_name = entry.raw_name.clone();
                            start_id = if entry.id.is_empty() { format!("call_native_{}", idx) } else { entry.id.clone() };
                        }
                    }

                    if should_start {
                        if self.state != BlockState::None {
                            events.push(SseEvent::new(
                                "content_block_stop",
                                json!({"type": "content_block_stop", "index": self.block_index}),
                            ));
                            self.block_index += 1;
                        } else if self.has_emitted_blocks {
                            self.block_index += 1;
                        }
                        self.has_emitted_blocks = true;
                        self.state = BlockState::ToolUse;

                        if let Some(entry) = self.pending_native_tool_calls.get_mut(&idx) {
                            entry.block_index = self.block_index;
                        }

                        let mapped_name = crate::protocol::repair::ToolCallRepairEngine::map_tool_name(&start_raw_name, &self.casing_map);

                        events.push(SseEvent::new(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": self.block_index,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": start_id,
                                    "name": mapped_name,
                                    "input": {}
                                }
                            }),
                        ));
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
    fn emit_completion_events(&mut self, upstream_finish_reason: Option<&str>) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.finished {
            return events;
        }
        self.finished = true;

        if self.state != BlockState::None && self.state != BlockState::ToolUse {
            events.push(self.create_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": self.block_index}),
            ));
            self.state = BlockState::None;
        }

        let mut stop_reason = match upstream_finish_reason {
            Some("tool_calls") => "tool_use",
            _ => "end_turn",
        };

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

                    let call_idx = if call.started {
                        call.block_index
                    } else {
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
                                    "name": mapped_name,
                                    "input": {}
                                }
                            }),
                        ));
                        self.block_index
                    };

                    events.push(self.create_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": call_idx,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": norm_args.to_string()
                            }
                        }),
                    ));
                    events.push(self.create_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": call_idx}),
                    ));
                }
            }
            self.state = BlockState::None;
            stop_reason = "tool_use";
        } else {

            // Repair hallucinated tool calls if no native tool calls were present
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
            } else if self.accumulated_text.len() > self.streamed_text_len {
                // If no tool calls were detected, flush any held back text
                let remaining = &self.accumulated_text[self.streamed_text_len..];
                if self.has_emitted_blocks {
                    self.block_index += 1;
                }
                self.has_emitted_blocks = true;

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
                        "delta": {"type": "text_delta", "text": remaining}
                    }),
                ));
                events.push(self.create_event(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": self.block_index}),
                ));
                self.streamed_text_len = self.accumulated_text.len();
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

