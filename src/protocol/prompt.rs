use crate::protocol::anthropic::{AnthropicContent, ContentBlock, MessagesRequest, SystemPrompt};

/// Compiles an Anthropic MessagesRequest (with multi-turn history, tool calls, tool results,
/// system instructions, and compaction directives) into a unified prompt string for ACP agents.
pub fn build_acp_prompt(req: &MessagesRequest) -> String {
    let mut parts = Vec::new();
    let is_compact = req.is_compaction_request();

    // 1. System Prompt Fusion
    let mut sys_text = String::new();
    if let Some(sys) = &req.system {
        match sys {
            SystemPrompt::Single(s) => sys_text.push_str(s),
            SystemPrompt::Blocks(blocks) => {
                let joined = blocks
                    .iter()
                    .map(|b| b.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                sys_text.push_str(&joined);
            }
        }
    }

    // 2. Compact Tools Instruction Injection (Omitted during compaction)
    if !is_compact {
        if let Some(tools) = &req.tools {
            if !tools.is_empty() {
                let mut tool_guide = String::from(
                    "\n\n[Tools Instruction]\nYou can call tools to perform actions. To call a tool, you MUST format it as:\n<tool_call>\n{\"name\": \"<tool_name>\", \"arguments\": {<args>}}\n</tool_call>\n\nAvailable tools:\n",
                );
                for t in tools {
                    let name = t.resolved_name();
                    match name {
                        "Bash" | "bash" => tool_guide.push_str("- bash: {\"command\": \"<command>\"} (Run shell commands in current project directory. Required parameter: 'command').\n"),
                        "Read" | "read" | "FileRead" | "file_read" => tool_guide.push_str("- read: {\"file_path\": \"<file_path>\"} (Read file contents from filesystem. Required parameter: 'file_path').\n"),
                        "Edit" | "edit" | "FileEdit" | "file_edit" => tool_guide.push_str("- edit: {\"file_path\": \"<file_path>\", \"old_string\": \"<exact text to replace>\", \"new_string\": \"<replacement text>\"} (Replace exact string in file).\n"),
                        "Write" | "write" | "FileWrite" | "file_write" => tool_guide.push_str("- write: {\"file_path\": \"<file_path>\", \"content\": \"<file content>\"} (Write file to filesystem).\n"),
                        "Glob" | "glob" | "GlobTool" => tool_guide.push_str("- glob: {\"pattern\": \"<glob_pattern>\"} (Find files matching pattern).\n"),
                        "Grep" | "grep" | "GrepTool" => tool_guide.push_str("- grep: {\"pattern\": \"<regex_pattern>\"} (Search file contents for pattern).\n"),
                        _ => {
                            let schema = t.schema();
                            let props: Vec<String> = schema
                                .get("properties")
                                .and_then(|p| p.as_object())
                                .map(|obj| obj.keys().cloned().collect())
                                .unwrap_or_default();
                            let desc = t.description.as_deref().unwrap_or("");
                            let short_desc = if desc.len() > 60 { &desc[..60] } else { desc };
                            tool_guide.push_str(&format!("- {}: keys: [{}] ({})\n", name, props.join(", "), short_desc));
                        }
                    }
                }
                tool_guide.push_str("Note: Core execution tools must be lowercase: bash, read, edit, write, glob, grep.\n");
                sys_text.push_str(&tool_guide);
            }
        }
    }

    // 3. System Messages in req.messages (e.g. SessionStart hooks) folded into sys_text
    for msg in &req.messages {
        if msg.role == "system" {
            let text = match &msg.content {
                AnthropicContent::Text(t) => t.trim().to_string(),
                AnthropicContent::Blocks(blocks) => {
                    blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.trim().to_string()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                }
            };
            if !text.is_empty() {
                if !sys_text.is_empty() {
                    sys_text.push_str("\n\n");
                }
                sys_text.push_str(&text);
            }
        }
    }

    if !sys_text.trim().is_empty() {
        parts.push(format!("[System Instructions]\n{}", sys_text.trim()));
    }

    // 4. Conversation Messages (User & Assistant)
    for msg in &req.messages {
        if msg.role == "system" {
            continue;
        }

        let role_label = match msg.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            other => other,
        };

        match &msg.content {
            AnthropicContent::Text(t) => {
                if !t.trim().is_empty() {
                    parts.push(format!("{}: {}", role_label, t.trim()));
                }
            }
            AnthropicContent::Blocks(blocks) => {
                let mut block_parts = Vec::new();
                for b in blocks {
                    match b {
                        ContentBlock::Text { text } => {
                            if !text.trim().is_empty() {
                                block_parts.push(text.trim().to_string());
                            }
                        }
                        ContentBlock::Thinking { .. } => {
                            // Strip historical internal thinking blocks
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            block_parts.push(format!(
                                "<tool_call>\n{{\"name\": \"{}\", \"arguments\": {}}}\n</tool_call>",
                                name, input
                            ));
                        }
                        ContentBlock::ToolResult {
                            content,
                            is_error,
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

                            let text_content = if is_compact && raw_content.len() > 300 {
                                format!(
                                    "{}...\n[Output truncated: {} chars]",
                                    &raw_content[..250],
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
                            block_parts.push(format!(
                                "<tool_result{}>\n{}\n</tool_result>",
                                err_str,
                                text_content.trim()
                            ));
                        }
                        ContentBlock::Image { .. } => {
                            block_parts.push("[Image attached]".to_string());
                        }
                        ContentBlock::Other => {}
                    }
                }
                if !block_parts.is_empty() {
                    parts.push(format!("{}:\n{}", role_label, block_parts.join("\n\n")));
                }
            }
        }
    }

    if is_compact {
        parts.push("[COMPACTION DIRECTIVE]\nProduce ONLY the concise summary enclosed within <summary>...</summary> tags.\nDo NOT generate internal reasoning or thinking monologue.\nDo NOT attempt to call any tools.\nStart your response immediately with <summary>.".to_string());
    }

    if parts.is_empty() {
        "Hello".to_string()
    } else {
        parts.join("\n\n---\n\n")
    }
}
