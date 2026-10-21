use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

impl MessagesRequest {
    pub fn is_compaction_request(&self) -> bool {
        // A compaction request MUST ONLY be detected on the CURRENT turn (the last user message).
        // NEVER check system instructions, historical messages, or assistant responses,
        // otherwise subsequent normal turns will get stuck in an infinite compaction/summary loop!
        if let Some(last_msg) = self.messages.last() {
            if last_msg.role == "user" {
                match &last_msg.content {
                    AnthropicContent::Text(t) => {
                        if is_compaction_text(t) {
                            return true;
                        }
                    }
                    AnthropicContent::Blocks(blocks) => {
                        for b in blocks {
                            if let ContentBlock::Text { text } = b {
                                if is_compaction_text(text) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }

        false
    }
}

pub fn is_compaction_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();

    // Explicit markers emitted by known Claude Code / OpenCode versions.
    if lower.contains("your summary should include the following sections:")
        || (lower.contains("primary request and intent:") && lower.contains("key technical concepts:"))
        || (lower.contains("reminder: do not call any tools") && lower.contains("summary"))
        || lower.contains("free up context by summarizing the conversation")
        || lower.contains("you are compacting the conversation")
    {
        return true;
    }

    // Structural fallback for prompt revisions we have not seen yet: a long
    // instruction asking for a compacted summary of the context. The length
    // gate keeps ordinary "summarize this file" requests (which arrive with a
    // tool catalog) from being mistaken for compaction.
    let asks_to_summarize = lower.contains("summariz") || lower.contains("compact");
    let summary_scope = lower.contains("conversation")
        || lower.contains("context")
        || lower.contains("transcript");
    asks_to_summarize && summary_scope && text.len() >= 300
}

fn default_max_tokens() -> u32 {
    4096
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Single(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type", default = "default_text_type")]
    pub block_type: String,
    #[serde(default)]
    pub text: String,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

fn default_text_type() -> String {
    "text".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default = "default_tool_input")]
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    #[serde(rename = "image")]
    Image {
        #[serde(default)]
        source: Value,
    },
    #[serde(rename = "document")]
    Document {
        #[serde(default)]
        source: Value,
    },
    #[serde(other)]
    Other,
}

fn default_tool_input() -> Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnthropicTool {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_tool_schema")]
    pub input_schema: Value,
}

pub fn default_tool_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {}
    })
}

impl AnthropicTool {
    pub fn resolved_name(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else if let Some(t) = &self.tool_type {
            t.as_str()
        } else {
            "unknown_tool"
        }
    }

    pub fn schema(&self) -> &Value {
        &self.input_schema
    }
}
