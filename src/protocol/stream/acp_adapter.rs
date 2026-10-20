//! ACP -> unified stream engine adapter.

use super::engine::{AnthropicStreamEngine, FinishReason, StreamInput};
use super::events::SseEvent;
use crate::protocol::tokens::{AcpOutputChunk, TokenUsage};

pub struct AcpStreamAdapter {
    pub engine: AnthropicStreamEngine,
}

impl AcpStreamAdapter {
    pub fn new(engine: AnthropicStreamEngine) -> Self {
        Self { engine }
    }

    pub fn feed(&mut self, chunk: AcpOutputChunk) -> Vec<SseEvent> {
        match chunk {
            AcpOutputChunk::Text(text) => self.engine.feed_input(StreamInput::TextDelta(text)),
            AcpOutputChunk::Thinking(text) => {
                self.engine.feed_input(StreamInput::ThinkingDelta(text))
            }
            AcpOutputChunk::Usage(TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                ..
            }) => self.engine.feed_input(StreamInput::Usage {
                input_tokens: (input_tokens > 0).then_some(input_tokens),
                output_tokens: (output_tokens > 0).then_some(output_tokens),
                cached_read_tokens: (cache_read_input_tokens > 0)
                    .then_some(cache_read_input_tokens),
            }),
        }
    }

    pub fn finish(&mut self, output_tokens: u64) -> Vec<SseEvent> {
        self.engine.finish_with_reason(
            FinishReason::EndTurn,
            self.engine.input_tokens,
            output_tokens,
            self.engine.cached_read_tokens,
        )
    }

    pub fn finish_with_reason(
        &mut self,
        reason: FinishReason,
        output_tokens: u64,
    ) -> Vec<SseEvent> {
        self.engine.finish_with_reason(
            reason,
            self.engine.input_tokens,
            output_tokens,
            self.engine.cached_read_tokens,
        )
    }

    pub fn error(&mut self, message: &str) -> Vec<SseEvent> {
        self.engine.feed_error(message)
    }
}
