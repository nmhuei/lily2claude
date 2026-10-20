use serde::{Deserialize, Serialize};

/// Standard token usage structure matching Anthropic API specification
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl TokenUsage {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    }

    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// Chunks emitted by ACP agents to streaming receivers
#[derive(Debug, Clone)]
pub enum AcpOutputChunk {
    Text(String),
    Thinking(String),
    Usage(TokenUsage),
}

impl From<String> for AcpOutputChunk {
    fn from(text: String) -> Self {
        AcpOutputChunk::Text(text)
    }
}

impl From<&str> for AcpOutputChunk {
    fn from(text: &str) -> Self {
        AcpOutputChunk::Text(text.to_string())
    }
}

/// Helper function to extract token usage from an OpenCode ACP JSON-RPC response
pub fn extract_acp_usage(val: &serde_json::Value) -> Option<TokenUsage> {
    let usage_obj = val
        .pointer("/result/usage")
        .or_else(|| val.pointer("/params/update/usage"))
        .or_else(|| val.pointer("/params/usage"))?;

    let input = usage_obj
        .get("inputTokens")
        .or_else(|| usage_obj.get("input_tokens"))
        .or_else(|| usage_obj.pointer("/turn/input"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let output = usage_obj
        .get("outputTokens")
        .or_else(|| usage_obj.get("output_tokens"))
        .or_else(|| usage_obj.pointer("/turn/output"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let cached_read = usage_obj
        .get("cachedReadTokens")
        .or_else(|| usage_obj.get("cache_read_input_tokens"))
        .or_else(|| usage_obj.pointer("/turn/cache/read"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let cached_write = usage_obj
        .get("cachedWriteTokens")
        .or_else(|| usage_obj.get("cache_creation_input_tokens"))
        .or_else(|| usage_obj.pointer("/turn/cache/write"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if input > 0 || output > 0 {
        Some(TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cached_read,
            cache_creation_input_tokens: cached_write,
        })
    } else {
        None
    }
}
