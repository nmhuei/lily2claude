use crate::protocol::anthropic::*;
use crate::protocol::openai::*;
use serde_json::json;

pub const THINKING_START_MARKER: &str = "💭 Thinking:\n";
pub const THINKING_END_MARKER: &str = "💭 (Done thinking)";

/// Strips streamed thinking text blocks and think tags from history so upstream models receive clean context.
pub fn strip_thinking_text(text: &str) -> String {
    let mut s = text.to_string();
    while let Some(start) = s.find(THINKING_START_MARKER) {
        if let Some(end) = s[start..].find(THINKING_END_MARKER) {
            s.replace_range(start..start + end + THINKING_END_MARKER.len(), "");
        } else {
            // Unclosed thinking marker (e.g. truncated stream)
            s.replace_range(start.., "");
            break;
        }
    }
    // Also remove residual think tags like <think>...</think>
    while let Some(start) = s.find("<think>") {
        if let Some(end) = s[start..].find("</think>") {
            s.replace_range(start..start + end + "</think>".len(), "");
        } else {
            s.replace_range(start.., "");
            break;
        }
    }
    s.trim().to_string()
}

pub fn transform_anthropic_to_openai(
    req: &MessagesRequest,
    is_opencode: bool,
) -> anyhow::Result<ChatCompletionsRequest> {
    let mut messages: Vec<OpenAiMessage> = Vec::new();

    // 1. System Prompt Fusion
    if let Some(sys) = &req.system {
        let sys_text = match sys {
            SystemPrompt::Single(s) => s.clone(),
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        };
        messages.push(OpenAiMessage {
            role: "system".to_string(),
            content: Some(sys_text),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    let is_compact = req.is_compaction_request();

    // 2. Iterate Messages & Normalize
    for msg in &req.messages {
        match &msg.content {
            AnthropicContent::Text(text) => {
                let text_val = if msg.role == "assistant" {
                    strip_thinking_text(text)
                } else {
                    text.clone()
                };
                if !text_val.is_empty() || msg.role == "user" {
                    messages.push(OpenAiMessage {
                        role: msg.role.clone(),
                        content: Some(text_val),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }
            AnthropicContent::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            let text_val = if msg.role == "assistant" {
                                strip_thinking_text(text)
                            } else {
                                text.clone()
                            };
                            if !text_val.is_empty() {
                                text_parts.push(text_val);
                            }
                        }
                        ContentBlock::Thinking { .. } => {
                            // Strip historic thinking blocks from assistant history!
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(OpenAiToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: OpenAiFunctionCall {
                                    name: name.to_ascii_lowercase(),
                                    arguments: input.to_string(),
                                },
                            });
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            let raw_content = if let Some(s) = content.as_str() {
                                s.to_string()
                            } else if let Some(arr) = content.as_array() {
                                let joined = arr.iter()
                                    .filter_map(|item| {
                                        if let Some(s) = item.as_str() {
                                            Some(s)
                                        } else {
                                            item.get("text").and_then(|t| t.as_str())
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if joined.is_empty() {
                                    content.to_string()
                                } else {
                                    joined
                                }
                            } else {
                                content.to_string()
                            };

                            let raw_content = strip_thinking_text(&raw_content);

                            let text_content = if is_compact && raw_content.len() > 300 {
                                format!(
                                    "{}...\n[Output truncated: {} chars]",
                                    &raw_content[..250],
                                    raw_content.len()
                                )
                            } else {
                                raw_content
                            };

                            messages.push(OpenAiMessage {
                                role: "tool".to_string(),
                                content: Some(text_content),
                                tool_calls: None,
                                tool_call_id: Some(tool_use_id.clone()),
                            });
                        }
                        ContentBlock::Image { .. } => {
                            text_parts.push("[Image attached]".to_string());
                        }
                        ContentBlock::Other => {}
                    }
                }

                if !text_parts.is_empty() || !tool_calls.is_empty() {
                    let text = if text_parts.is_empty() {
                        None
                    } else {
                        Some(text_parts.join("\n"))
                    };
                    let tools = if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    };
                    messages.push(OpenAiMessage {
                        role: msg.role.clone(),
                        content: text,
                        tool_calls: tools,
                        tool_call_id: None,
                    });
                }
            }
        }
    }

    // 3. Tool schemas conversion (Never pass tools during compaction)
    let mut tools = Vec::new();
    if !is_compact {
        if let Some(t_list) = &req.tools {
            for t in t_list {
                tools.push(OpenAiTool {
                    tool_type: "function".to_string(),
                    function: OpenAiFunctionDefinition {
                        name: t.resolved_name().to_ascii_lowercase(),
                        description: t.description.clone(),
                        parameters: t.schema().clone(),
                    },
                });
            }
        }

        // 4. Inject 6 core tools for OpenCode FreeTier bypass if missing (not during compaction)
        if is_opencode {
            let core = ["bash", "edit", "glob", "grep", "read", "write"];
            for c in &core {
                if !tools.iter().any(|t| t.function.name == *c) {
                    tools.push(OpenAiTool {
                        tool_type: "function".to_string(),
                        function: OpenAiFunctionDefinition {
                            name: c.to_string(),
                            description: Some(format!("Core {} tool", c)),
                            parameters: json!({"type": "object", "properties": {}}),
                        },
                    });
                }
            }
        }
    }

    Ok(ChatCompletionsRequest {
        model: req.model.clone(),
        messages,
        tools: if tools.is_empty() { None } else { Some(tools) },
        stream: req.stream,
        max_tokens: Some(req.max_tokens),
        temperature: req.temperature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_thinking_text_complete_block() {
        let input = "💭 Thinking:\nChecking the directory structure\nLooking for files\n💭 (Done thinking)\n\nI will now run ls.";
        let cleaned = strip_thinking_text(input);
        assert_eq!(cleaned, "I will now run ls.");
    }

    #[test]
    fn test_strip_thinking_text_unclosed_block() {
        let input = "💭 Thinking:\nPartial reasoning that got cut off...";
        let cleaned = strip_thinking_text(input);
        assert_eq!(cleaned, "");
    }

    #[test]
    fn test_strip_thinking_preserves_normal_text() {
        let input = "Here is an answer with no thinking at all.";
        let cleaned = strip_thinking_text(input);
        assert_eq!(cleaned, "Here is an answer with no thinking at all.");
    }

    #[test]
    fn test_transform_strips_thinking_from_history() {
        let req = MessagesRequest {
            model: "claude-sonnet-5".to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("Hi".to_string()),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(vec![
                        ContentBlock::Text {
                            text: "💭 Thinking:\nReasoning step 1\n💭 (Done thinking)\n\nHello user!".to_string(),
                        },
                    ]),
                },
            ],
            tools: None,
            stream: false,
            max_tokens: 1000,
            temperature: None,
            system: None,
        };

        let result = transform_anthropic_to_openai(&req, false).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[1].role, "assistant");
        assert_eq!(result.messages[1].content.as_deref(), Some("Hello user!"));
    }
}
