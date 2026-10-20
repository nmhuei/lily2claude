//! Layered prompt compiler.
//!
//! The compiler keeps the four layers separate so an ACP agent never sees two
//! conflicting system prompts, a duplicated tool catalog, or a tool result
//! detached from the conversation that produced it:
//!
//! 1. System instructions from `req.system`
//! 2. The declared tool catalog (omitted during compaction)
//! 3. Role-isolated conversation turns, including `<system-reminder>` blocks
//!    and tool use/result pairs
//! 4. The execution or compaction directive

use crate::protocol::anthropic::{AnthropicContent, ContentBlock, MessagesRequest, SystemPrompt};
use std::collections::HashSet;

pub fn build_acp_prompt(req: &MessagesRequest) -> String {
    let is_compact = req.is_compaction_request();
    let mut parts = Vec::new();

    let system_text = compile_system(req.system.as_ref());
    if !system_text.is_empty() {
        parts.push(format!("[System Instructions]\n{}", system_text));
    }

    if !is_compact {
        if let Some(tools) = compile_tools(req.tools.as_deref()) {
            parts.push(tools);
        }
    }

    let conversation = compile_conversation(req, is_compact);
    if !conversation.is_empty() {
        parts.push(conversation);
    }

    if is_compact {
        parts.push(
            "[COMPACTION DIRECTIVE]\nProduce ONLY the concise summary enclosed within <summary>...</summary> tags.\nDo NOT generate internal reasoning or thinking monologue.\nDo NOT attempt to call any tools.\nStart your response immediately with <summary>.".to_string(),
        );
    } else if req
        .tools
        .as_ref()
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
    {
        parts.push(
            "[Tool Execution Directive]\nAll tools are executed directly by the Claude Code harness via <tool_call> tags.\nNEVER use or call `execute` or `search`. DO NOT wrap tool calls inside JavaScript code.\nEvery tool listed under '[Tools Instruction]' MUST be called using standalone XML tags:\n<tool_call>\n{\"name\": \"<tool_name>\", \"arguments\": {<args>}}\n</tool_call>\nWhen the user asks you to perform an action with any declared tool, you MUST emit the <tool_call> block immediately. NEVER pretend a tool is unavailable or fabricate a response without invoking the tool. If you have already completed the user's request or received the necessary tool results, provide your final answer directly to the user without calling further tools.".to_string(),
        );
    }

    if parts.is_empty() {
        "Hello".to_string()
    } else {
        parts.join("\n\n---\n\n")
    }
}

fn compile_system(system: Option<&SystemPrompt>) -> String {
    let Some(system) = system else {
        return String::new();
    };

    let blocks: Vec<String> = match system {
        SystemPrompt::Single(text) => vec![text.trim().to_string()],
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.trim().to_string())
            .collect(),
    };

    let mut seen = HashSet::new();
    blocks
        .into_iter()
        .filter(|block| {
            let key = block.trim();
            !key.is_empty() && seen.insert(key.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn compile_tools(tools: Option<&[crate::protocol::anthropic::AnthropicTool]>) -> Option<String> {
    let tools = tools?;
    if tools.is_empty() {
        return None;
    }

    let mut section = String::from(
        "[Tools Instruction]\nTo invoke a tool, output:\n<tool_call>\n{\"name\": \"<tool_name>\", \"arguments\": {<args>}}\n</tool_call>\nExample (empty args for 0-param tools):\n<tool_call>\n{\"name\": \"CronList\", \"arguments\": {}}\n</tool_call>\n\nAvailable tools:\n",
    );
    let mut seen = HashSet::new();

    for tool in tools {
        let name = tool.resolved_name();
        if name.is_empty() || !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }

        let schema = tool.schema();
        let required: HashSet<&str> = schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let mut params = Vec::new();
        if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
            // Required parameters are listed first so the model sees the
            // non-negotiable shape before optional refinements.
            for property in properties.keys().filter(|p| required.contains(p.as_str())) {
                params.push(property.clone());
            }
            for property in properties.keys().filter(|p| !required.contains(p.as_str())) {
                params.push(format!("{}?", property));
            }
        }

        if params.is_empty() {
            section.push_str(&format!("- {}()\n", name));
        } else {
            section.push_str(&format!("- {}({})\n", name, params.join(", ")));
        }
    }

    section.push_str(
        "All tools are executed directly by the Claude Code harness. Never inspect local function catalogs.\n",
    );
    Some(section)
}

fn compile_conversation(req: &MessagesRequest, is_compact: bool) -> String {
    let mut turns = Vec::new();

    for message in &req.messages {
        let role_label = match message.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "system" => "System",
            other => other,
        };

        match &message.content {
            AnthropicContent::Text(text) => {
                let text = text.trim();
                if !text.is_empty() {
                    turns.push(format!("{}: {}", role_label, text));
                }
            }
            AnthropicContent::Blocks(blocks) => {
                let mut block_parts = Vec::new();
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text.trim().is_empty() {
                                block_parts.push(text.trim().to_string());
                            }
                        }
                        ContentBlock::Thinking { .. } => {
                            // Historical reasoning is intentionally not fed
                            // back to the upstream model.
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            block_parts.push(format!(
                                "<tool_call>\n{{\"name\": \"{}\", \"arguments\": {}}}\n</tool_call>",
                                name, input
                            ));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let raw_content = flatten_tool_result(content);
                            let text_content = if is_compact && raw_content.len() > 300 {
                                let cut =
                                    raw_content.floor_char_boundary(250.min(raw_content.len()));
                                format!(
                                    "{}...\n[Output truncated: {} chars]",
                                    &raw_content[..cut],
                                    raw_content.len()
                                )
                            } else {
                                raw_content
                            };

                            let err_str = if is_error.unwrap_or(false) {
                                " error=true"
                            } else {
                                ""
                            };
                            let id_line = if tool_use_id.is_empty() {
                                String::new()
                            } else {
                                format!("[tool_use_id: {}]\n", tool_use_id)
                            };
                            block_parts.push(format!(
                                "<tool_result{}>\n{}{}\n</tool_result>",
                                err_str,
                                id_line,
                                text_content.trim()
                            ));
                        }
                        ContentBlock::Image { .. } => {
                            block_parts.push("[Image attached]".to_string());
                        }
                        ContentBlock::Document { .. } => {
                            block_parts.push("[Document attached]".to_string());
                        }
                        ContentBlock::Other => {}
                    }
                }
                if !block_parts.is_empty() {
                    turns.push(format!("{}:\n{}", role_label, block_parts.join("\n\n")));
                }
            }
        }
    }

    turns.join("\n\n---\n\n")
}

fn flatten_tool_result(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(items) = content.as_array() {
        let joined = items
            .iter()
            .filter_map(|item| {
                if let Some(text) = item.as_str() {
                    Some(text)
                } else {
                    item.get("text").and_then(|text| text.as_str())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.is_empty() {
            return joined;
        }
    }
    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::anthropic::{AnthropicMessage, AnthropicTool};
    use serde_json::json;

    fn request_with_system_reminder() -> MessagesRequest {
        MessagesRequest {
            model: "deepseek".to_string(),
            system: Some(SystemPrompt::Single("You are Claude Code.".to_string())),
            tools: Some(vec![AnthropicTool {
                name: "Read".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string"},
                        "offset": {"type": "integer"},
                        "limit": {"type": "integer"}
                    },
                    "required": ["file_path"]
                }),
                ..Default::default()
            }]),
            stream: true,
            max_tokens: 100,
            temperature: None,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text(
                    "<system-reminder>Keep this exact reminder.</system-reminder>\nRead the file."
                        .to_string(),
                ),
            }],
        }
    }

    #[test]
    fn tools_are_a_separate_layer_and_required_params_come_first() {
        let prompt = build_acp_prompt(&request_with_system_reminder());
        assert!(prompt.contains("[System Instructions]\nYou are Claude Code."));
        assert!(prompt.contains("[Tools Instruction]"));
        assert!(prompt.contains("- Read(file_path, limit?, offset?)"));
        let system_idx = prompt.find("[System Instructions]").unwrap();
        let tools_idx = prompt.find("[Tools Instruction]").unwrap();
        assert!(system_idx < tools_idx);
    }

    #[test]
    fn system_reminder_is_preserved_verbatim_in_user_turn() {
        let prompt = build_acp_prompt(&request_with_system_reminder());
        assert!(prompt.contains("User: <system-reminder>Keep this exact reminder.</system-reminder>\nRead the file."));
    }

    #[test]
    fn tool_names_are_deduplicated_case_insensitively() {
        let mut req = request_with_system_reminder();
        req.tools = Some(vec![
            AnthropicTool {
                name: "Bash".to_string(),
                ..Default::default()
            },
            AnthropicTool {
                name: "bash".to_string(),
                ..Default::default()
            },
        ]);
        let prompt = build_acp_prompt(&req);
        assert_eq!(prompt.matches("Available tools:").count(), 1);
        assert_eq!(
            prompt.matches("- Bash").count() + prompt.matches("- bash").count(),
            1
        );
    }

    #[test]
    fn tool_result_linkage_and_document_placeholder() {
        let mut req = request_with_system_reminder();
        req.messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                id: "call_doc".to_string(),
                name: "Read".to_string(),
                input: json!({"file_path": "/tmp/report.pdf"}),
            }]),
        });
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_doc".to_string(),
                    content: json!("PDF text"),
                    is_error: Some(false),
                },
                ContentBlock::Document {
                    source: json!({"type": "base64", "media_type": "application/pdf", "data": "..."}),
                },
            ]),
        });
        let prompt = build_acp_prompt(&req);
        assert!(prompt.contains("[tool_use_id: call_doc]"));
        assert!(prompt.contains("PDF text"));
        assert!(prompt.contains("[Document attached]"));
    }

    #[test]
    fn compaction_omits_tool_catalog() {
        let mut req = request_with_system_reminder();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "REMINDER: Do NOT call any tools. Respond with plain text only — <summary>Summary</summary>"
                    .to_string(),
            ),
        });
        let prompt = build_acp_prompt(&req);
        assert!(!prompt.contains("[Tools Instruction]"));
        assert!(prompt.contains("[COMPACTION DIRECTIVE]"));
    }
}
