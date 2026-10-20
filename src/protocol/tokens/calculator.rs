use crate::protocol::anthropic::{
    AnthropicContent, AnthropicMessage, AnthropicTool, ContentBlock, MessagesRequest, SystemPrompt,
};
use std::sync::OnceLock;
use tiktoken_rs::{cl100k_base, CoreBPE};

static BPE_TOKENIZER: OnceLock<Option<CoreBPE>> = OnceLock::new();

fn get_bpe() -> Option<&'static CoreBPE> {
    BPE_TOKENIZER.get_or_init(|| cl100k_base().ok()).as_ref()
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
            ContentBlock::Document { .. } => 800,
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
