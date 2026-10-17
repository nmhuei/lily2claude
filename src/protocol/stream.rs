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

pub struct SseStreamTranslator {
    message_id: String,
    model: String,
    started: bool,
    state: BlockState,
    block_index: usize,
    finished: bool,
    casing_map: HashMap<String, String>,
    has_native_tool_calls: bool,
    accumulated_text: String,
}

impl SseStreamTranslator {
    pub fn new(message_id: &str, model: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
            model: model.to_string(),
            started: false,
            state: BlockState::None,
            block_index: 0,
            finished: false,
            casing_map: HashMap::new(),
            has_native_tool_calls: false,
            accumulated_text: String::new(),
        }
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
                        "usage": {"input_tokens": 150, "output_tokens": 0}
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
                        }
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

            // 2. Normal text
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    self.accumulated_text.push_str(text);
                    if self.state != BlockState::Text {
                        if self.state != BlockState::None {
                            events.push(self.create_event(
                                "content_block_stop",
                                json!({"type": "content_block_stop", "index": self.block_index}),
                            ));
                            self.block_index += 1;
                        }
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
                            "delta": {"type": "text_delta", "text": text}
                        }),
                    ));
                }
            }

            // 3. Native Tool Calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    self.has_native_tool_calls = true;
                    if let Some(func) = tc.get("function") {
                        let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("call_gen");
                        let raw_name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let mapped_name = self
                            .casing_map
                            .get(&raw_name.to_ascii_lowercase())
                            .cloned()
                            .unwrap_or_else(|| raw_name.to_string());
                        let args = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("");

                        if self.state != BlockState::ToolUse {
                            if self.state != BlockState::None {
                                events.push(self.create_event(
                                    "content_block_stop",
                                    json!({"type": "content_block_stop", "index": self.block_index}),
                                ));
                                self.block_index += 1;
                            }
                            self.state = BlockState::ToolUse;
                            events.push(self.create_event(
                                "content_block_start",
                                json!({
                                    "type": "content_block_start",
                                    "index": self.block_index,
                                    "content_block": {"type": "tool_use", "id": id, "name": mapped_name, "input": {}}
                                }),
                            ));
                        }
                        if !args.is_empty() {
                            events.push(self.create_event(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": self.block_index,
                                    "delta": {"type": "input_json_delta", "partial_json": args}
                                }),
                            ));
                        }
                    }
                }
            }
        }

        if let Some(reason) = finish_reason.and_then(|v| v.as_str()) {
            if !self.finished {
                self.finished = true;
                if self.state != BlockState::None {
                    events.push(self.create_event(
                        "content_block_stop",
                        json!({"type": "content_block_stop", "index": self.block_index}),
                    ));
                }

                let mut stop_reason = match reason {
                    "tool_calls" => "tool_use",
                    _ => "end_turn",
                };

                // Repair hallucinated tool calls if no native tool calls were present
                if !self.has_native_tool_calls {
                    let repaired = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(
                        &self.accumulated_text,
                        &self.casing_map,
                    );
                    if !repaired.is_empty() {
                        for (idx, rep) in repaired.into_iter().enumerate() {
                            self.block_index += 1;
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
                    }
                }

                events.push(self.create_event(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                        "usage": {"output_tokens": 80}
                    }),
                ));
                events.push(self.create_event("message_stop", json!({"type": "message_stop"})));
            }
        }

        events
    }

    /// Guarantees that active blocks and stream completion events are emitted
    /// even if upstream closes abruptly without sending finish_reason
    pub fn finish_stream_if_needed(&mut self) -> Vec<SseEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = Vec::new();
        if self.state != BlockState::None {
            events.push(self.create_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": self.block_index}),
            ));
        }

        let mut stop_reason = "end_turn";
        if !self.has_native_tool_calls {
            let repaired = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(
                &self.accumulated_text,
                &self.casing_map,
            );
            if !repaired.is_empty() {
                for (idx, rep) in repaired.into_iter().enumerate() {
                    self.block_index += 1;
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
            }
        }

        events.push(self.create_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"output_tokens": 80}
            }),
        ));
        events.push(self.create_event("message_stop", json!({"type": "message_stop"})));
        events
    }

    fn create_event(&self, event_name: &str, data: Value) -> SseEvent {
        SseEvent::new(event_name, data)
    }
}
