use crate::protocol::anthropic::*;
use crate::protocol::openai::*;
use serde_json::json;

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
                messages.push(OpenAiMessage {
                    role: msg.role.clone(),
                    content: Some(text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            AnthropicContent::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
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
                                arr.iter()
                                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            } else {
                                content.to_string()
                            };

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
