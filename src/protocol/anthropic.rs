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
        // 1. Check system instructions
        if let Some(sys) = &self.system {
            match sys {
                SystemPrompt::Single(s) => {
                    if is_compaction_text(s) {
                        return true;
                    }
                }
                SystemPrompt::Blocks(blocks) => {
                    for b in blocks {
                        if is_compaction_text(&b.text) {
                            return true;
                        }
                    }
                }
            }
        }

        // 2. Check messages in reverse (last user message contains Claude Code compaction prompt)
        for msg in self.messages.iter().rev() {
            match &msg.content {
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

        false
    }
}

pub fn is_compaction_text(text: &str) -> bool {
    (text.contains("<summary>") && text.contains("Primary Request and Intent"))
        || text.contains("REMINDER: Do NOT call any tools. Respond with plain text only")
        || text.contains("Free up context by summarizing the conversation")
        || text.contains("You are compacting the conversation")
        || (text.contains("<summary>") && text.contains("</summary>") && text.contains("summariz"))
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
        if self.input_schema.get("properties").is_some() || self.input_schema.get("type").is_some() {
            return &self.input_schema;
        }
        &self.input_schema
    }
}
