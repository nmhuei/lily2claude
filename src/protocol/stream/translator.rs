//! OpenAI Chat Completions SSE adapter.
//!
//! `SseStreamTranslator` is now a thin compatibility wrapper around the single
//! [`super::engine::AnthropicStreamEngine`].  All block lifecycle, tool
//! buffering, finish-reason mapping and error handling live in the engine.

use super::engine::{AnthropicStreamEngine, FinishReason, StreamInput};
use super::events::SseEvent;
use crate::protocol::anthropic::AnthropicTool;
use serde_json::Value;
use std::collections::HashMap;

pub struct SseStreamTranslator {
    pub engine: AnthropicStreamEngine,
    pub is_compact: bool,
    pub stream_thinking_as_text: bool,
}

impl SseStreamTranslator {
    pub fn new(message_id: &str, model: &str) -> Self {
        Self {
            engine: AnthropicStreamEngine::new(message_id, model, 50, HashMap::new()),
            is_compact: false,
            stream_thinking_as_text: false,
        }
    }

    pub fn with_input_tokens(mut self, tokens: u64) -> Self {
        self.engine.input_tokens = tokens;
        self
    }

    pub fn with_compaction(mut self, is_compact: bool) -> Self {
        self.is_compact = is_compact;
        self.engine.set_compaction(is_compact);
        self
    }

    pub fn with_stream_thinking_as_text(mut self, enabled: bool) -> Self {
        self.stream_thinking_as_text = enabled;
        self.engine.stream_thinking_as_text = enabled;
        self
    }

    /// Attach the real Claude Code tool catalog.  When this is called the
    /// strict Parse -> Normalize -> Validate path is used and legacy text
    /// heuristics are disabled.
    pub fn with_tools(mut self, tools: &[AnthropicTool]) -> Self {
        self.engine.set_tools(tools);
        self
    }

    pub fn register_tool_schema(&mut self, tool: &AnthropicTool) {
        if self.engine.strict_tools.is_none() {
            self.engine.set_tools(&[]);
        }
        self.engine.register_tool(tool);
    }

    /// Compatibility registration by name only.  No schema is available, so
    /// the engine uses the legacy normalization profile for this tool.
    pub fn register_tool_casing(&mut self, name: &str) {
        let declared = name.to_string();
        self.engine
            .casing_map
            .insert(declared.to_ascii_lowercase(), declared);
    }

    pub fn feed_chunk(&mut self, raw: &str) -> Vec<SseEvent> {
        if self.engine.finished {
            return Vec::new();
        }

        let value: Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };

        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("upstream stream error");
            return self.engine.feed_error(message);
        }

        let mut events = Vec::new();

        if let Some(usage) = value.get("usage") {
            events.extend(
                self.engine.feed_input(StreamInput::Usage {
                    input_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()),
                    output_tokens: usage.get("completion_tokens").and_then(|v| v.as_u64()),
                    cached_read_tokens: usage
                        .get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64()),
                }),
            );
        }

        let choice = value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first());

        if let Some(choice) = choice {
            if let Some(delta) = choice.get("delta") {
                if let Some(reasoning) = delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .and_then(|v| v.as_str())
                {
                    if !reasoning.is_empty() {
                        events.extend(self.engine.feed_thinking(reasoning));
                    }
                }

                if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        events.extend(self.engine.feed_text(text));
                    }
                }

                if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for (fallback_index, call) in tool_calls.iter().enumerate() {
                        let index = call
                            .get("index")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(fallback_index);
                        let id = call
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let function = call.get("function");
                        let name = function
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let arguments = function
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        events.extend(self.engine.feed_input(StreamInput::ToolCallDelta {
                            index,
                            id,
                            name,
                            arguments,
                        }));
                    }
                }
            }

            if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                let finish = FinishReason::from_openai(reason);
                events.extend(self.engine.finish_with_reason(
                    finish,
                    self.engine.input_tokens,
                    self.engine.output_tokens(),
                    self.engine.cached_read_tokens,
                ));
            }
        }

        events
    }

    pub fn finish_stream_if_needed(&mut self) -> Vec<SseEvent> {
        self.engine.finish_stream_if_needed()
    }

    pub fn accumulated_text(&self) -> &str {
        self.engine.accumulated_text()
    }

    pub fn output_tokens(&self) -> u64 {
        self.engine.output_tokens()
    }

    pub fn stop_reason(&self) -> Option<&'static str> {
        if self.engine.emitted_tool_count > 0 {
            Some("tool_use")
        } else {
            self.engine.finish_reason.map(|r| match r {
                FinishReason::MaxTokens => "max_tokens",
                FinishReason::StopSequence => "stop_sequence",
                FinishReason::Cancelled => "cancelled",
                _ => "end_turn",
            })
        }
    }
}
