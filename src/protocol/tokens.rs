use crate::protocol::anthropic::{AnthropicContent, AnthropicMessage, AnthropicTool, ContentBlock, MessagesRequest, SystemPrompt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tiktoken_rs::{cl100k_base, CoreBPE};
use tokio::sync::RwLock;

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

static BPE_TOKENIZER: OnceLock<Option<CoreBPE>> = OnceLock::new();

fn get_bpe() -> Option<&'static CoreBPE> {
    BPE_TOKENIZER
        .get_or_init(|| cl100k_base().ok())
        .as_ref()
}

/// High-performance token calculator for Anthropic Messages requests and text streams
pub struct TokenCalculator;

impl TokenCalculator {
    /// Calculate exact token count of arbitrary text using BPE (cl100k_base)
    /// with graceful character-ratio fallback if BPE is unavailable.
    pub fn count_text(text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        if let Some(bpe) = get_bpe() {
            bpe.encode_with_special_tokens(text).len()
        } else {
            // High-fidelity fallback heuristic (~3.8 chars per token for typical code/English)
            (text.len().saturating_add(3) / 4).max(1)
        }
    }

    /// Calculate tokens for a single Anthropic ContentBlock
    pub fn count_content_block(block: &ContentBlock) -> usize {
        match block {
            ContentBlock::Text { text } => Self::count_text(text),
            ContentBlock::Thinking { thinking, .. } => Self::count_text(thinking).saturating_add(4),
            ContentBlock::ToolUse { name, input, .. } => {
                let name_tokens = Self::count_text(name);
                let input_str = input.to_string();
                let input_tokens = Self::count_text(&input_str);
                // 8 tokens for tool_use XML/JSON wrapper overhead
                name_tokens.saturating_add(input_tokens).saturating_add(8)
            }
            ContentBlock::ToolResult { content, .. } => {
                let content_tokens = if let Some(s) = content.as_str() {
                    Self::count_text(s)
                } else {
                    Self::count_text(&content.to_string())
                };
                // 6 tokens for tool_result wrapping overhead
                content_tokens.saturating_add(6)
            }
            ContentBlock::Image { .. } => 1600,
            ContentBlock::Other => 10,
        }
    }

    /// Calculate tokens for a single AnthropicTool definition
    pub fn count_tool(tool: &AnthropicTool) -> usize {
        let name_tokens = Self::count_text(tool.resolved_name());
        let desc_tokens = tool
            .description
            .as_deref()
            .map(Self::count_text)
            .unwrap_or(0);
        let schema_str = tool.schema().to_string();
        let schema_tokens = Self::count_text(&schema_str);
        // 12 tokens for function calling schema wrapper structure
        name_tokens
            .saturating_add(desc_tokens)
            .saturating_add(schema_tokens)
            .saturating_add(12)
    }

    /// Calculate tokens for a single AnthropicMessage
    pub fn count_message(msg: &AnthropicMessage) -> usize {
        // 4 tokens per message for role framing: <|im_start|>role\n...<|im_end|>\n
        let framing_tokens: usize = 4;
        let content_tokens = match &msg.content {
            AnthropicContent::Text(t) => Self::count_text(t),
            AnthropicContent::Blocks(blocks) => {
                blocks.iter().map(Self::count_content_block).sum::<usize>()
            }
        };
        framing_tokens.saturating_add(content_tokens)
    }

    /// Calculate total input tokens for a complete MessagesRequest
    pub fn count_request_tokens(req: &MessagesRequest) -> usize {
        let mut total: usize = 0;

        // 1. System Prompt
        if let Some(sys) = &req.system {
            let sys_tokens = match sys {
                SystemPrompt::Single(s) => Self::count_text(s),
                SystemPrompt::Blocks(blocks) => {
                    blocks.iter().map(|b| Self::count_text(&b.text)).sum::<usize>()
                }
            };
            // 4 tokens for system role framing
            total = total.saturating_add(sys_tokens).saturating_add(4);
        }

        // 2. Tools declarations
        if let Some(tools) = &req.tools {
            let tools_tokens: usize = tools.iter().map(Self::count_tool).sum();
            total = total.saturating_add(tools_tokens);
        }

        // 3. Conversation Messages
        for msg in &req.messages {
            total = total.saturating_add(Self::count_message(msg));
        }

        // 4. Request envelope overhead (e.g. <|im_start|>assistant\n)
        total.saturating_add(3)
    }

}

/// Per-session token usage record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTokenRecord {
    pub session_id: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_read_tokens: u64,
    pub last_activity_epoch_secs: u64,
}

/// Per-model token usage record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokenRecord {
    pub model_name: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Full token usage snapshot returned by GET /v1/stats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsSummary {
    pub service: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub total_requests: u64,
    pub cumulative: CumulativeUsage,
    pub active_sessions_count: usize,
    pub sessions: Vec<SessionTokenRecord>,
    pub models: HashMap<String, ModelTokenRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CumulativeUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_read_tokens: u64,
    pub cached_creation_tokens: u64,
}

/// Thread-safe centralized token tracker stored in AppState
pub struct TokenUsageTracker {
    start_time: Instant,
    total_requests: AtomicU64,
    total_input_tokens: AtomicU64,
    total_output_tokens: AtomicU64,
    total_cached_read_tokens: AtomicU64,
    total_cached_creation_tokens: AtomicU64,
    sessions: Arc<RwLock<HashMap<String, SessionTokenRecord>>>,
    models: Arc<RwLock<HashMap<String, ModelTokenRecord>>>,
}

impl Default for TokenUsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenUsageTracker {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            total_requests: AtomicU64::new(0),
            total_input_tokens: AtomicU64::new(0),
            total_output_tokens: AtomicU64::new(0),
            total_cached_read_tokens: AtomicU64::new(0),
            total_cached_creation_tokens: AtomicU64::new(0),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            models: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn total_input_tokens(&self) -> u64 {
        self.total_input_tokens.load(Ordering::Relaxed)
    }

    pub fn total_output_tokens(&self) -> u64 {
        self.total_output_tokens.load(Ordering::Relaxed)
    }

    pub fn total_requests(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Record completed turn usage for a session and model
    pub async fn record_turn(
        &self,
        session_id: &str,
        model: &str,
        usage: TokenUsage,
    ) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.total_input_tokens.fetch_add(usage.input_tokens, Ordering::Relaxed);
        self.total_output_tokens.fetch_add(usage.output_tokens, Ordering::Relaxed);
        self.total_cached_read_tokens.fetch_add(usage.cache_read_input_tokens, Ordering::Relaxed);
        self.total_cached_creation_tokens.fetch_add(usage.cache_creation_input_tokens, Ordering::Relaxed);

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 1. Update session record
        let mut sessions_lock = self.sessions.write().await;
        let session_rec = sessions_lock
            .entry(session_id.to_string())
            .or_insert_with(|| SessionTokenRecord {
                session_id: session_id.to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cached_read_tokens: 0,
                last_activity_epoch_secs: now_secs,
            });
        session_rec.request_count = session_rec.request_count.saturating_add(1);
        session_rec.input_tokens = session_rec.input_tokens.saturating_add(usage.input_tokens);
        session_rec.output_tokens = session_rec.output_tokens.saturating_add(usage.output_tokens);
        session_rec.total_tokens = session_rec.input_tokens.saturating_add(session_rec.output_tokens);
        session_rec.cached_read_tokens = session_rec.cached_read_tokens.saturating_add(usage.cache_read_input_tokens);
        session_rec.last_activity_epoch_secs = now_secs;

        // 2. Update model record
        let mut models_lock = self.models.write().await;
        let model_rec = models_lock
            .entry(model.to_string())
            .or_insert_with(|| ModelTokenRecord {
                model_name: model.to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            });
        model_rec.request_count = model_rec.request_count.saturating_add(1);
        model_rec.input_tokens = model_rec.input_tokens.saturating_add(usage.input_tokens);
        model_rec.output_tokens = model_rec.output_tokens.saturating_add(usage.output_tokens);
        model_rec.total_tokens = model_rec.input_tokens.saturating_add(model_rec.output_tokens);
    }

    /// Generate full summary snapshot
    pub async fn snapshot(&self) -> TokenStatsSummary {
        let uptime_seconds = self.start_time.elapsed().as_secs();
        let total_requests = self.total_requests.load(Ordering::Relaxed);
        let in_tok = self.total_input_tokens.load(Ordering::Relaxed);
        let out_tok = self.total_output_tokens.load(Ordering::Relaxed);
        let cache_read = self.total_cached_read_tokens.load(Ordering::Relaxed);
        let cache_create = self.total_cached_creation_tokens.load(Ordering::Relaxed);

        let sessions_lock = self.sessions.read().await;
        let mut sessions_vec: Vec<SessionTokenRecord> = sessions_lock.values().cloned().collect();
        sessions_vec.sort_by(|a, b| b.last_activity_epoch_secs.cmp(&a.last_activity_epoch_secs));

        let models_lock = self.models.read().await;
        let models_map = models_lock.clone();

        TokenStatsSummary {
            service: "lily2claude".to_string(),
            version: "0.1.0".to_string(),
            uptime_seconds,
            total_requests,
            cumulative: CumulativeUsage {
                input_tokens: in_tok,
                output_tokens: out_tok,
                total_tokens: in_tok.saturating_add(out_tok),
                cached_read_tokens: cache_read,
                cached_creation_tokens: cache_create,
            },
            active_sessions_count: sessions_vec.len(),
            sessions: sessions_vec,
            models: models_map,
        }
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
