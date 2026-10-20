//! OpenAI Chat Completions SSE -> unified stream engine adapter.
//!
//! Handles partial UTF-8/line splits, `data:` with or without a space, the
//! final line when no newline is present, `[DONE]`, and structured upstream
//! errors.

use super::events::SseEvent;
use super::translator::SseStreamTranslator;
use crate::protocol::anthropic::AnthropicTool;

pub struct OpenAiStreamAdapter {
    translator: SseStreamTranslator,
    byte_buffer: Vec<u8>,
    done: bool,
}

impl OpenAiStreamAdapter {
    pub fn new(
        message_id: &str,
        model: &str,
        input_tokens: u64,
        is_compact: bool,
        tools: &[AnthropicTool],
    ) -> Self {
        let translator = SseStreamTranslator::new(message_id, model)
            .with_input_tokens(input_tokens)
            .with_compaction(is_compact)
            .with_tools(tools);
        Self {
            translator,
            byte_buffer: Vec::new(),
            done: false,
        }
    }

    pub fn translator(&self) -> &SseStreamTranslator {
        &self.translator
    }

    pub fn feed_bytes(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        if self.done {
            return Vec::new();
        }
        self.byte_buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some(newline) = self.byte_buffer.iter().position(|b| *b == b'\n') {
            let line = self.byte_buffer.drain(..=newline).collect::<Vec<u8>>();
            let line = String::from_utf8_lossy(&line);
            events.extend(self.process_line(line.trim_end_matches('\r')));
        }

        events
    }

    pub fn feed_text(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.feed_bytes(chunk.as_bytes())
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.byte_buffer.is_empty() {
            let line = String::from_utf8_lossy(&self.byte_buffer).to_string();
            self.byte_buffer.clear();
            events.extend(self.process_line(line.trim_end_matches('\r')));
        }
        events.extend(self.translator.finish_stream_if_needed());
        self.done = true;
        events
    }

    pub fn error(&mut self, message: &str) -> Vec<SseEvent> {
        self.translator.engine.feed_error(message)
    }

    pub fn output_tokens(&self) -> u64 {
        self.translator.output_tokens()
    }

    pub fn accumulated_text(&self) -> &str {
        self.translator.accumulated_text()
    }

    fn process_line(&mut self, line: &str) -> Vec<SseEvent> {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
            return Vec::new();
        }

        let payload = if let Some(rest) = line.strip_prefix("data:") {
            rest.trim_start()
        } else if line.starts_with('{') {
            // Some compatible gateways omit the SSE field entirely.
            line
        } else {
            return Vec::new();
        };

        if payload == "[DONE]" {
            let events = self.translator.finish_stream_if_needed();
            self.done = true;
            return events;
        }

        self.translator.feed_chunk(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn adapter() -> OpenAiStreamAdapter {
        OpenAiStreamAdapter::new("msg_test", "deepseek", 10, false, &[])
    }

    #[test]
    fn handles_partial_lines_and_data_without_space() {
        let mut adapter = adapter();
        let chunk1 = "data:{\"choices\":[{\"delta\":{\"content\":\"hel";
        let chunk2 =
            "lo\"}}]}\n\ndata:{\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        assert!(adapter.feed_text(chunk1).is_empty());
        let events = adapter.feed_text(chunk2);
        let text = events
            .iter()
            .filter(|e| e.event == "content_block_delta")
            .filter_map(|e| e.data["delta"]["text"].as_str())
            .collect::<String>();
        assert_eq!(text, "hello");
        assert!(events.iter().any(|e| e.event == "message_delta"));
    }

    #[test]
    fn length_maps_to_max_tokens() {
        let mut adapter = adapter();
        let events = adapter
            .feed_text("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n");
        let stop = events.iter().find(|e| e.event == "message_delta").unwrap();
        assert_eq!(stop.data["delta"]["stop_reason"], "max_tokens");
    }

    #[test]
    fn final_line_without_newline_is_processed() {
        let mut adapter = adapter();
        let first = adapter.feed_text("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}");
        assert!(first.is_empty());
        let events = adapter.finish();
        let text = events
            .iter()
            .filter(|e| e.event == "content_block_delta")
            .filter_map(|e| e.data["delta"]["text"].as_str())
            .collect::<String>();
        assert_eq!(text, "ok");
    }

    #[test]
    fn upstream_error_is_an_error_event_not_text() {
        let mut adapter = adapter();
        let events = adapter
            .feed_text("data: {\"error\":{\"type\":\"rate_limit\",\"message\":\"slow down\"}}\n\n");
        assert!(events.iter().any(|e| e.event == "error"));
        assert!(!events.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("slow down")
        }));
    }

    #[test]
    fn done_is_idempotent() {
        let mut adapter = adapter();
        let first = adapter.feed_text("data: [DONE]\n\n");
        assert!(first.iter().any(|e| e.event == "message_stop"));
        assert!(adapter.finish().is_empty());
    }

    #[test]
    fn utf8_split_across_byte_chunks_is_preserved() {
        let mut adapter = adapter();
        let payload = json!({
            "choices": [{"delta": {"content": "xin chào"}}]
        })
        .to_string();
        let wire = format!("data: {}\n\n", payload);
        let bytes = wire.as_bytes();
        let split = bytes.iter().position(|b| *b >= 0x80).unwrap() + 1;
        assert!(adapter.feed_bytes(&bytes[..split]).is_empty());
        let events = adapter.feed_bytes(&bytes[split..]);
        let text = events
            .iter()
            .filter(|e| e.event == "content_block_delta")
            .filter_map(|e| e.data["delta"]["text"].as_str())
            .collect::<String>();
        assert_eq!(text, "xin chào");
    }
}
