use super::events::SseEvent;
use crate::protocol::anthropic::AnthropicTool;
use crate::protocol::sanitize::{split_utf8_prefix, strip_system_tags, RENDER_DELTA_CHUNK_BYTES};
use crate::protocol::thinking::{ThinkingRouteItem, ThinkingRouter};
use crate::protocol::tool_engine::tag_lexer;
use crate::protocol::tool_engine::ToolCallEngine;
use crate::protocol::tracker::SseBlockTracker;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Inputs accepted by the unified stream state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamInput {
    ThinkingDelta(String),
    TextDelta(String),
    /// OpenAI-native tool call fragments.  They are buffered until finish so a
    /// tool call split across many chunks produces exactly one block.
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cached_read_tokens: Option<u64>,
    },
    Finish(FinishReason),
    Cancel,
    UpstreamError(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    Idle,
    Thinking,
    Text,
    ToolTagScan,
    ToolArguments,
    Finished,
}

impl FinishReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FinishReason::EndTurn => "end_turn",
            FinishReason::ToolUse => "tool_use",
            FinishReason::MaxTokens => "max_tokens",
            FinishReason::StopSequence => "stop_sequence",
            FinishReason::Cancelled => "cancelled",
        }
    }

    pub fn from_openai(reason: &str) -> Self {
        match reason {
            "tool_calls" | "function_call" => FinishReason::ToolUse,
            "length" | "max_tokens" => FinishReason::MaxTokens,
            "stop_sequence" => FinishReason::StopSequence,
            _ => FinishReason::EndTurn,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct PendingNativeToolCall {
    id: String,
    raw_name: String,
    arguments_buf: String,
}

/// Deterministic FSM for every Anthropic SSE stream source.
///
/// States: `Thinking -> TextStream -> ToolTagScan -> ToolArgsCollect`.
/// The same engine is used for ACP chunks and OpenAI SSE chunks; adapters only
/// translate transport payloads into [`StreamInput`] values.
pub struct AnthropicStreamEngine {
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
    pub cached_read_tokens: u64,
    pub strict_tools: Option<Vec<AnthropicTool>>,
    pub legacy_text_repair: bool,
    pub started: bool,
    pub finished: bool,
    pub state: EngineState,
    pub finish_reason: Option<FinishReason>,
    pub upstream_output_tokens: Option<u64>,
    accumulated_text: String,
    pending_native_tool_calls: HashMap<usize, PendingNativeToolCall>,
}

impl AnthropicStreamEngine {
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
            cached_read_tokens: 0,
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
            strict_tools: None,
            legacy_text_repair: true,
            started: false,
            finished: false,
            state: EngineState::Idle,
            finish_reason: None,
            upstream_output_tokens: None,
            accumulated_text: String::new(),
            pending_native_tool_calls: HashMap::new(),
        }
    }

    pub fn with_cached_tokens(mut self, cached_read_tokens: u64) -> Self {
        self.cached_read_tokens = cached_read_tokens;
        self
    }

    pub fn with_compaction(mut self, is_compact: bool) -> Self {
        self.set_compaction(is_compact);
        self
    }

    pub fn set_compaction(&mut self, is_compact: bool) {
        self.is_compact = is_compact;
    }

    pub fn with_stream_thinking_as_text(mut self, enabled: bool) -> Self {
        self.stream_thinking_as_text = enabled;
        self
    }

    /// Attach the real Claude Code tool catalog.  When present, every emitted
    /// call has passed Parse -> Normalize -> Validate; legacy text heuristics
    /// are disabled.
    pub fn with_tools(mut self, tools: &[AnthropicTool]) -> Self {
        self.set_tools(tools);
        self
    }

    /// Attach (or replace) the strict catalog without consuming the engine.
    pub fn set_tools(&mut self, tools: &[AnthropicTool]) {
        self.strict_tools = Some(tools.to_vec());
        self.legacy_text_repair = false;
        for tool in tools {
            let declared = tool.resolved_name();
            if !declared.is_empty() {
                self.casing_map
                    .entry(declared.to_ascii_lowercase())
                    .or_insert_with(|| declared.to_string());
            }
        }
    }

    pub fn register_tool(&mut self, tool: &AnthropicTool) {
        let declared = tool.resolved_name().to_string();
        if declared.is_empty() {
            return;
        }
        self.casing_map
            .insert(declared.to_ascii_lowercase(), declared.clone());
        if let Some(tools) = self.strict_tools.as_mut() {
            if !tools
                .iter()
                .any(|t| t.resolved_name().eq_ignore_ascii_case(&declared))
            {
                tools.push(tool.clone());
            }
        } else {
            self.strict_tools = Some(vec![tool.clone()]);
            self.legacy_text_repair = false;
        }
    }

    fn ensure_started(&mut self) -> Option<SseEvent> {
        if self.started {
            None
        } else {
            Some(self.start())
        }
    }

    pub fn start(&mut self) -> SseEvent {
        self.started = true;
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
                        "cache_read_input_tokens": self.cached_read_tokens
                    }
                }
            }),
        )
    }

    pub fn feed_thinking(&mut self, chunk: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if chunk.is_empty() || self.finished {
            return events;
        }
        if let Some(start) = self.ensure_started() {
            events.push(start);
        }
        if self.state != EngineState::Text {
            self.state = EngineState::Thinking;
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
        if chunk.is_empty() || self.finished {
            return events;
        }
        if let Some(start) = self.ensure_started() {
            events.push(start);
        }
        self.state = EngineState::Text;
        self.accumulated_text.push_str(chunk);

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

    /// Uniform entry point used by transport adapters.
    pub fn feed_input(&mut self, input: StreamInput) -> Vec<SseEvent> {
        match input {
            StreamInput::ThinkingDelta(text) => self.feed_thinking(&text),
            StreamInput::TextDelta(text) => self.feed_text(&text),
            StreamInput::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                let entry = self.pending_native_tool_calls.entry(index).or_default();
                if let Some(id) = id {
                    if !id.is_empty() {
                        entry.id = id;
                    }
                }
                if let Some(name) = name {
                    entry.raw_name.push_str(&name);
                }
                if let Some(arguments) = arguments {
                    entry.arguments_buf.push_str(&arguments);
                }
                Vec::new()
            }
            StreamInput::Usage {
                input_tokens,
                output_tokens,
                cached_read_tokens,
            } => {
                if let Some(v) = input_tokens {
                    self.input_tokens = v;
                }
                if let Some(v) = output_tokens {
                    self.upstream_output_tokens = Some(v);
                }
                if let Some(v) = cached_read_tokens {
                    self.cached_read_tokens = v;
                }
                Vec::new()
            }
            StreamInput::Finish(reason) => {
                self.finish_reason = Some(reason);
                self.finish_with_reason(
                    reason,
                    self.input_tokens,
                    self.estimated_output_tokens(),
                    self.cached_read_tokens,
                )
            }
            StreamInput::Cancel => self.cancel(),
            StreamInput::UpstreamError(message) => self.feed_error(&message),
        }
    }

    fn estimated_output_tokens(&self) -> u64 {
        self.upstream_output_tokens
            .unwrap_or_else(|| (self.accumulated_text.len() / 4).max(1) as u64)
    }

    /// Report a transport error without pretending it was assistant text.
    pub fn feed_error(&mut self, message: &str) -> Vec<SseEvent> {
        if self.finished {
            return Vec::new();
        }
        let mut events = Vec::new();
        if let Some(start) = self.ensure_started() {
            events.push(start);
        }
        for (kind, idx) in self.tracker.close_all() {
            if kind == "thinking" {
                events.push(self.signature_delta(idx));
            }
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": idx}),
            ));
        }
        self.finished = true;
        self.state = EngineState::Finished;
        events.push(SseEvent::new(
            "error",
            json!({
                "type": "error",
                "error": {"type": "upstream_error", "message": message}
            }),
        ));
        events
    }

    fn signature_delta(&self, index: usize) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "signature_delta",
                    "signature": "EqQBCgIYAhIM1gbcuvLz1029487561"
                }
            }),
        )
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

        // A real text block must close a preceding thinking block.  Anthropic
        // requires one active block at a time, and Claude Code relies on the
        // signature delta arriving before the first text delta.
        self.emit_close_thinking(events);

        self.text_buffer.push_str(chunk);

        loop {
            let open_tag_info = Self::find_tool_tag_start(&self.text_buffer);

            match open_tag_info {
                None => {
                    let holdback = Self::get_partial_tag_prefix_len(&self.text_buffer);
                    let mut safe_len = self.text_buffer.len().saturating_sub(holdback);
                    safe_len = self.text_buffer.floor_char_boundary(safe_len);

                    if safe_len > self.streamed_in_current_block {
                        let to_stream =
                            self.text_buffer[self.streamed_in_current_block..safe_len].to_string();
                        if !to_stream.trim().is_empty() || self.tracker.text_idx().is_some() {
                            self.emit_text_delta(&to_stream, events);
                            // Only consume the buffer when it was actually
                            // emitted.  Leading whitespace is held until real
                            // text arrives, which keeps chunked and single-shot
                            // streams byte-identical.
                            self.streamed_in_current_block = safe_len;
                        }
                    }
                    break;
                }
                Some((tag_start, open_tag_len, close_tag)) => {
                    let search_from = tag_start + open_tag_len;
                    if let Some(rel_close) = self.text_buffer[search_from..].find(close_tag) {
                        if tag_start > self.streamed_in_current_block {
                            let pre_text = self.text_buffer
                                [self.streamed_in_current_block..tag_start]
                                .to_string();
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

                        let close_end = search_from + rel_close + close_tag.len();
                        let tool_xml = &self.text_buffer[tag_start..close_end];
                        let calls = self.extract_calls(tool_xml, false);
                        for (name, arguments) in calls {
                            self.emit_tool_call(name, arguments, None, events);
                        }

                        self.text_buffer = self.text_buffer[close_end..].to_string();
                        self.streamed_in_current_block = 0;
                    } else {
                        if tag_start > self.streamed_in_current_block {
                            let pre_text = self.text_buffer
                                [self.streamed_in_current_block..tag_start]
                                .to_string();
                            if !pre_text.trim().is_empty() {
                                self.emit_text_delta(&pre_text, events);
                            }
                            self.streamed_in_current_block = tag_start;
                        }
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
        self.finish_with_reason(
            self.finish_reason.unwrap_or(FinishReason::EndTurn),
            final_input_tokens,
            final_output_tokens,
            cached_read_tokens,
        )
    }

    pub fn finish_with_reason(
        &mut self,
        reason: FinishReason,
        final_input_tokens: u64,
        final_output_tokens: u64,
        cached_read_tokens: u64,
    ) -> Vec<SseEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        self.state = EngineState::Finished;
        self.finish_reason = Some(reason);
        if final_input_tokens > 0 {
            self.input_tokens = final_input_tokens;
        }
        self.cached_read_tokens = cached_read_tokens;

        let mut events = Vec::new();
        if let Some(start) = self.ensure_started() {
            events.push(start);
        }

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

            let unclosed_xml = self.text_buffer[tag_start..].to_string();
            let calls = self.extract_calls(&unclosed_xml, true);

            if !calls.is_empty() {
                for (name, arguments) in calls {
                    self.emit_tool_call(name, arguments, None, &mut events);
                }
            } else {
                let cleaned =
                    crate::protocol::repair::ToolCallRepairEngine::clean_user_visible_text(
                        &unclosed_xml,
                    );
                let cleaned = strip_system_tags(&cleaned);
                if !cleaned.trim().is_empty()
                    || (self.tracker.text_idx().is_some() && !cleaned.is_empty())
                {
                    self.emit_text_delta(&cleaned, &mut events);
                }
            }
        } else if self.text_buffer.len() > self.streamed_in_current_block {
            let remaining = self.text_buffer[self.streamed_in_current_block..].to_string();
            let cleaned =
                crate::protocol::repair::ToolCallRepairEngine::clean_user_visible_text(&remaining);
            let cleaned = strip_system_tags(&cleaned);
            if !cleaned.trim().is_empty()
                || (self.tracker.text_idx().is_some() && !cleaned.is_empty())
            {
                self.emit_text_delta(&cleaned, &mut events);
            }
        }

        if self.is_compact
            && self.has_emitted_text_block
            && !self.text_buffer.contains("</summary>")
        {
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
                events.push(self.signature_delta(idx));
            }
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": idx}),
            ));
        }

        // Native OpenAI tool calls are buffered until finish, so a call split
        // across arbitrary SSE chunks emits exactly one complete block.
        if !self.pending_native_tool_calls.is_empty() {
            let mut keys: Vec<usize> = self.pending_native_tool_calls.keys().copied().collect();
            keys.sort_unstable();
            for key in keys {
                if let Some(call) = self.pending_native_tool_calls.remove(&key) {
                    let id = if call.id.is_empty() {
                        format!("call_native_{}", key)
                    } else {
                        call.id.clone()
                    };
                    if let Some((name, arguments)) = self.validate_native_call(&call) {
                        self.emit_tool_call(name, arguments, Some(id), &mut events);
                    }
                }
            }
        }

        if self.emitted_tool_count == 0 && self.legacy_text_repair {
            let repaired = crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(
                &self.accumulated_text,
                &self.casing_map,
            );
            for (index, rep) in repaired.into_iter().enumerate() {
                let id = format!("call_rep_{}", index);
                self.emit_tool_call(rep.name, rep.arguments, Some(id), &mut events);
            }
        }

        if !self.has_emitted_text_block && self.emitted_tool_count == 0 {
            let idx = self.tracker.next_index();
            events.push(SseEvent::new(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {"type": "text", "text": ""}
                }),
            ));
            if self.is_compact {
                events.push(SseEvent::new(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": {
                            "type": "text_delta",
                            "text": "<summary>\nConversation compacted.\n- Key decisions and project context preserved.\n</summary>"
                        }
                    }),
                ));
            }
            events.push(SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": idx}),
            ));
        }

        let stop_reason = if self.emitted_tool_count > 0 {
            "tool_use"
        } else {
            match reason {
                FinishReason::MaxTokens => "max_tokens",
                FinishReason::StopSequence => "stop_sequence",
                FinishReason::Cancelled => "cancelled",
                FinishReason::ToolUse | FinishReason::EndTurn => "end_turn",
            }
        };

        let output_tokens = if final_output_tokens > 0 {
            final_output_tokens
        } else {
            self.estimated_output_tokens()
        };

        events.push(SseEvent::new(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {
                    "output_tokens": output_tokens
                }
            }),
        ));

        events.push(SseEvent::new(
            "message_stop",
            json!({"type": "message_stop"}),
        ));

        events
    }

    fn validate_native_call(&self, call: &PendingNativeToolCall) -> Option<(String, Value)> {
        if let Some(tools) = &self.strict_tools {
            let engine = ToolCallEngine::new(tools);
            let parsed = crate::protocol::tool_engine::parse_resilient_json(&call.arguments_buf)
                .into_result()
                .unwrap_or_else(|_| json!({}));
            let candidate = crate::protocol::tool_engine::ToolCallCandidate {
                call_id: call.id.clone(),
                raw_name: call.raw_name.clone(),
                mapped_name: call.raw_name.clone(),
                raw_arguments: call.arguments_buf.clone(),
                parsed_arguments: parsed,
                recovery_strategy: None,
            };
            engine
                .validate_and_normalize(candidate)
                .ok()
                .map(|valid| (valid.name, valid.arguments))
        } else {
            let mapped = crate::protocol::repair::ToolCallRepairEngine::map_tool_name(
                &call.raw_name,
                &self.casing_map,
            );
            let repaired = crate::protocol::repair::repair_tool_arguments(&call.arguments_buf);
            let parsed: Value = serde_json::from_str(&repaired).unwrap_or_else(|_| json!({}));
            let norm = crate::protocol::repair::ToolCallRepairEngine::normalize_arguments_with_raw(
                &mapped,
                &call.raw_name,
                parsed,
            );
            crate::protocol::repair::ToolCallRepairEngine::sanitize_and_validate(
                mapped,
                norm,
                &self.casing_map,
            )
            .map(|valid| (valid.name, valid.arguments))
        }
    }

    fn extract_calls(&self, xml: &str, allow_unclosed: bool) -> Vec<(String, Value)> {
        if let Some(tools) = &self.strict_tools {
            let engine = ToolCallEngine::new(tools);
            let mut out = Vec::new();
            for candidate in engine.extract_candidates(xml, allow_unclosed) {
                if let Ok(valid) = engine.validate_and_normalize(candidate) {
                    if let Some(strategy) = &valid.recovery_strategy {
                        eprintln!(
                            "[tool_engine] recovered tool '{}' using {}",
                            valid.name, strategy
                        );
                    }
                    out.push((valid.name, valid.arguments));
                }
            }
            out
        } else {
            crate::protocol::repair::ToolCallRepairEngine::detect_and_extract(xml, &self.casing_map)
                .into_iter()
                .filter_map(|rep| {
                    crate::protocol::repair::ToolCallRepairEngine::sanitize_and_validate(
                        rep.name,
                        rep.arguments,
                        &self.casing_map,
                    )
                    .map(|valid| (valid.name, valid.arguments))
                })
                .collect()
        }
    }

    fn emit_tool_call(
        &mut self,
        name: String,
        arguments: Value,
        explicit_id: Option<String>,
        events: &mut Vec<SseEvent>,
    ) {
        let idx = self.tracker.next_index();
        let call_id = match explicit_id {
            Some(id) => id,
            None => format!("call_acp_{}_{}", self.message_id, self.emitted_tool_count),
        };
        self.emitted_tool_count += 1;

        events.push(SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": {
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": {}
                }
            }),
        ));
        events.push(SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": arguments.to_string()
                }
            }),
        ));
        events.push(SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": idx}),
        ));
    }

    fn find_tool_tag_start(s: &str) -> Option<(usize, usize, &'static str)> {
        tag_lexer::scan_tool_tag_start(s).map(|m| (m.start, m.open_tag_len(), m.close_tag))
    }

    fn get_partial_tag_prefix_len(s: &str) -> usize {
        tag_lexer::partial_tag_prefix_len(s)
    }

    /// Idempotent completion hook used by transport adapters.
    pub fn finish_stream_if_needed(&mut self) -> Vec<SseEvent> {
        if self.finished {
            Vec::new()
        } else {
            self.finish(
                self.input_tokens,
                self.estimated_output_tokens(),
                self.cached_read_tokens,
            )
        }
    }

    pub fn accumulated_text(&self) -> &str {
        &self.accumulated_text
    }

    pub fn output_tokens(&self) -> u64 {
        self.upstream_output_tokens
            .unwrap_or_else(|| self.estimated_output_tokens())
    }

    pub fn cancel(&mut self) -> Vec<SseEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finish_with_reason(
            FinishReason::Cancelled,
            self.input_tokens,
            self.estimated_output_tokens(),
            self.cached_read_tokens,
        )
    }
}
