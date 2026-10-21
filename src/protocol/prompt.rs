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

use crate::protocol::anthropic::{
    AnthropicContent, AnthropicMessage, ContentBlock, MessagesRequest, SystemPrompt,
};
use crate::protocol::tokens::TokenCalculator;
use std::collections::HashSet;

/// Layer separator, reused for both the output and its token accounting.
const TURN_SEPARATOR: &str = "\n\n---\n\n";

/// Real upstream ceiling for the hosted free models. OpenCode's DeepSeek rejects
/// anything above 262,139 tokens, so the compiled prompt must stay strictly
/// below it even though the catalog advertises 200,000 to Claude Code (a
/// deliberately conservative value that makes Claude Code autocompact with
/// headroom for the proxy's text expansion).
pub const DEFAULT_CONTEXT_LIMIT: usize = 262_139;

/// Generation room reserved out of the context window.
const MIN_OUTPUT_RESERVE: usize = 8_192;
const MAX_OUTPUT_RESERVE: usize = 65_536;

/// Tool results are ALWAYS truncated (head + tail kept) so a single
/// `cat huge.log` can never blow the window on its own.
const TOOL_RESULT_MAX: usize = 8_000;
const TOOL_RESULT_HEAD: usize = 6_000;
const TOOL_RESULT_TAIL: usize = 1_500;

/// A compaction summary only needs enough of a tool result to know what
/// happened, not the whole body.
const COMPACT_TOOL_RESULT_MAX: usize = 300;
const COMPACT_TOOL_RESULT_HEAD: usize = 250;
const COMPACT_TOOL_RESULT_TAIL: usize = 100;

/// Per-model context ceiling used when compiling a prompt.
///
/// Keys off the resolved upstream model, never the Claude-facing alias. This
/// table is intentionally conservative where the real ceiling is uncertain: an
/// underestimated limit costs a little context, an overestimated one costs the
/// whole turn to a `-32603` overflow.
pub fn model_context_limit(model: &str) -> usize {
    let m = model.to_ascii_lowercase();
    if m.contains("nemotron") {
        1_000_000
    } else if m.contains("luna") {
        400_000
    } else if m.contains("kilo") {
        256_000
    } else if m.contains("glm") {
        200_000
    } else if m.contains("mimo") {
        64_000
    } else {
        // DeepSeek, plus every Claude-family alias that is remapped onto it.
        DEFAULT_CONTEXT_LIMIT
    }
}

pub fn build_acp_prompt(req: &MessagesRequest) -> String {
    build_acp_prompt_with_budget(req, model_context_limit(&req.model))
}

/// Compile the layered prompt while guaranteeing the result fits inside
/// `context_limit` tokens minus a generation reserve. Oversized conversations
/// are compacted by dropping the oldest turns first (opencode's
/// `flattenAndDropOldest` pattern) and every tool result is truncated head+tail.
pub fn build_acp_prompt_with_budget(req: &MessagesRequest, context_limit: usize) -> String {
    let is_compact = req.is_compaction_request();
    let limit = context_limit.max(4_096);

    let output_reserve = (req.max_tokens as usize)
        .clamp(MIN_OUTPUT_RESERVE, MAX_OUTPUT_RESERVE)
        .min(limit / 2);
    let budget = limit.saturating_sub(output_reserve);

    let mut parts = Vec::new();
    let mut fixed_tokens = 0usize;

    let system_text = compile_system(req.system.as_ref());
    if !system_text.is_empty() {
        let section = format!("[System Instructions]\n{}", system_text);
        fixed_tokens = fixed_tokens
            .saturating_add(TokenCalculator::count_text(&section))
            .saturating_add(TokenCalculator::count_text(TURN_SEPARATOR));
        parts.push(section);
    }

    if !is_compact {
        if let Some(tools) = compile_tools(req.tools.as_deref()) {
            fixed_tokens = fixed_tokens
                .saturating_add(TokenCalculator::count_text(&tools))
                .saturating_add(TokenCalculator::count_text(TURN_SEPARATOR));
            parts.push(tools);
        }
    }

    // The directive is accounted for in the budget now but appended AFTER the
    // conversation: it must be the last thing the model reads.
    let directive = compile_directive(req, is_compact);
    if let Some(ref d) = directive {
        fixed_tokens = fixed_tokens
            .saturating_add(TokenCalculator::count_text(d))
            .saturating_add(TokenCalculator::count_text(TURN_SEPARATOR));
    }

    let conversation_budget = budget.saturating_sub(fixed_tokens);
    let conversation = compile_conversation(req, is_compact, conversation_budget);
    if !conversation.is_empty() {
        parts.push(conversation);
    }

    if let Some(d) = directive {
        parts.push(d);
    }

    if parts.is_empty() {
        "Hello".to_string()
    } else {
        parts.join(TURN_SEPARATOR)
    }
}

fn compile_directive(req: &MessagesRequest, is_compact: bool) -> Option<String> {
    if is_compact {
        return Some(
            "[COMPACTION DIRECTIVE]\nProduce ONLY the concise summary enclosed within <summary>...</summary> tags.\nDo NOT generate internal reasoning or thinking monologue.\nDo NOT attempt to call any tools.\nStart your response immediately with <summary>.".to_string(),
        );
    }
    if req
        .tools
        .as_ref()
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
    {
        return Some(
            "[Tool Execution Directive]\nAll tools are executed directly by the Claude Code harness via <tool_call> tags.\nNEVER use or call `execute` or `search`. DO NOT wrap tool calls inside JavaScript code.\nEvery tool listed under '[Tools Instruction]' MUST be called using standalone XML tags:\n<tool_call>\n{\"name\": \"<tool_name>\", \"arguments\": {<args>}}\n</tool_call>\nWhen the user asks you to perform an action with any declared tool, you MUST emit the <tool_call> block immediately. NEVER pretend a tool is unavailable or fabricate a response without invoking the tool. If you have already completed the user's request or received the necessary tool results, provide your final answer directly to the user without calling further tools.".to_string(),
        );
    }
    None
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

fn compile_conversation(req: &MessagesRequest, is_compact: bool, budget: usize) -> String {
    let rendered: Vec<String> = req
        .messages
        .iter()
        .filter_map(|message| render_message(message, is_compact))
        .collect();

    if rendered.is_empty() {
        return String::new();
    }

    let sep_tokens = TokenCalculator::count_text(TURN_SEPARATOR);
    let costs: Vec<usize> = rendered
        .iter()
        .map(|turn| TokenCalculator::count_text(turn).saturating_add(sep_tokens))
        .collect();
    let mut total: usize = costs.iter().sum::<usize>().saturating_sub(sep_tokens);

    // Drop the OLDEST turns first, always keeping the two most recent turns so
    // the live request (and the tool result it depends on) stays intact.
    let mut start = 0usize;
    while total > budget && rendered.len().saturating_sub(start) > 2 {
        total = total.saturating_sub(costs[start]);
        start += 1;
    }
    let omitted = start;

    let mut body = rendered[start..].join(TURN_SEPARATOR);

    // Safety valve: if even the retained turns exceed the budget (e.g. one
    // enormous tool result), trim from the front — the recent text is what the
    // model must act on. The cut is sized from the observed bytes-per-token
    // density, so this converges in one or two passes instead of re-encoding
    // the whole body once per 25% slice.
    let body_tokens = TokenCalculator::count_text(&body);
    if body_tokens > budget {
        let notice = format!(
            "[Earlier conversation truncated to fit the {} token context window]\n\n",
            budget
        );
        let inner_budget = budget.saturating_sub(TokenCalculator::count_text(&notice));
        let mut tokens = body_tokens;
        let mut passes = 0;
        while tokens > inner_budget && body.len() > 256 && passes < 4 {
            let bytes_per_token = (body.len() as f64 / tokens.max(1) as f64).max(1.0);
            let excess = tokens.saturating_sub(inner_budget);
            // 5% overshoot guards against density drift across the cut point.
            let drop_bytes = ((excess as f64) * bytes_per_token * 1.05).ceil() as usize;
            let cut = floor_boundary(&body, drop_bytes.clamp(1, body.len()));
            body = body[cut..].to_string();
            tokens = TokenCalculator::count_text(&body);
            passes += 1;
        }
        // Last resort: the density estimate can be off by a wide margin when the
        // dropped region was unusually token-dense. Halve until the cap holds.
        while tokens > inner_budget && body.len() > 256 {
            let cut = floor_boundary(&body, (body.len() / 2).max(1));
            body = body[cut..].to_string();
            tokens = TokenCalculator::count_text(&body);
        }
        body = format!("{}{}", notice, body);
    } else if omitted > 0 {
        body = format!(
            "[Earlier conversation truncated: {} message(s) omitted to fit the context window]\n\n{}",
            omitted, body
        );
    }

    body
}

fn render_message(message: &AnthropicMessage, is_compact: bool) -> Option<String> {
    let role_label = match message.role.as_str() {
        "user" => "User",
        "assistant" => "Assistant",
        "system" => "System",
        other => other,
    };

    match &message.content {
        AnthropicContent::Text(text) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(format!("{}: {}", role_label, text))
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
                        let text_content = truncate_tool_result(&raw_content, is_compact);

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
            if block_parts.is_empty() {
                None
            } else {
                Some(format!("{}:\n{}", role_label, block_parts.join("\n\n")))
            }
        }
    }
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

/// Truncate a tool result to a head+tail budget, splicing an explicit marker in
/// the middle so the model knows the output was clipped. This applies on EVERY
/// turn, not only during compaction.
fn truncate_tool_result(raw: &str, is_compact: bool) -> String {
    let (max, head_len, tail_len) = if is_compact {
        (
            COMPACT_TOOL_RESULT_MAX,
            COMPACT_TOOL_RESULT_HEAD,
            COMPACT_TOOL_RESULT_TAIL,
        )
    } else {
        (TOOL_RESULT_MAX, TOOL_RESULT_HEAD, TOOL_RESULT_TAIL)
    };

    if raw.len() <= max {
        return raw.to_string();
    }

    let head_end = floor_boundary(raw, head_len);
    let tail_start = ceil_boundary(raw, raw.len().saturating_sub(tail_len)).max(head_end);

    let mut out = String::with_capacity(head_end + raw.len().saturating_sub(tail_start) + 64);
    out.push_str(&raw[..head_end]);
    out.push_str(&format!(
        "\n...\n[Output truncated: {} chars]\n...\n",
        raw.len()
    ));
    out.push_str(&raw[tail_start..]);
    out
}

/// Largest char boundary at or below `idx`.
fn floor_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary at or above `idx`.
fn ceil_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
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
    fn tool_execution_directive_stays_last() {
        let prompt = build_acp_prompt(&request_with_system_reminder());
        let convo_idx = prompt.find("User: <system-reminder>").unwrap();
        let directive_idx = prompt.find("[Tool Execution Directive]").unwrap();
        assert!(
            convo_idx < directive_idx,
            "the directive must follow the conversation, not precede it"
        );
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

    #[test]
    fn oversized_conversation_is_compacted_below_budget() {
        let mut req = request_with_system_reminder();
        // 200 exchanges of heavy text — far beyond any context window.
        for i in 0..200 {
            req.messages.push(AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Text(format!("turn {}: {}", i, "x".repeat(4000))),
            });
            req.messages.push(AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text(format!("reply {}", i)),
            });
        }

        let budget = 20_000usize;
        let prompt = build_acp_prompt_with_budget(&req, budget);

        assert!(
            TokenCalculator::count_text(&prompt) <= budget,
            "compiled prompt must fit the budget"
        );
        assert!(prompt.contains("truncated"), "oldest turns must be dropped");
        // The newest turns are the ones the model must act on, so they survive
        // every truncation pass.
        assert!(prompt.contains("reply 199"), "latest user turn must survive");
        // Truncation must never displace the directive from the end.
        let convo_idx = prompt.rfind("User: reply 199").unwrap();
        let directive_idx = prompt.find("[Tool Execution Directive]").unwrap();
        assert!(
            convo_idx < directive_idx,
            "the directive must still follow the conversation after truncation"
        );
        assert!(
            prompt.trim_end().ends_with("without calling further tools."),
            "the directive must remain the final bytes of the prompt"
        );
    }

    #[test]
    fn small_window_models_get_a_smaller_budget() {
        assert_eq!(model_context_limit("mimo-v2.6-flash-free"), 64_000);
        assert_eq!(model_context_limit("nemotron-3.5-lightning-free"), 1_000_000);
        assert_eq!(model_context_limit("deepseek-v4.1-flash"), DEFAULT_CONTEXT_LIMIT);
        // A Claude alias is remapped onto DeepSeek upstream, so it inherits that ceiling.
        assert_eq!(model_context_limit("claude-sonnet-5-5"), DEFAULT_CONTEXT_LIMIT);

        let mut req = request_with_system_reminder();
        req.model = "mimo-v2.6-flash-free".to_string();
        for i in 0..100 {
            req.messages.push(AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Text(format!("turn {}: {}", i, "y".repeat(4000))),
            });
            req.messages.push(AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text(format!("ping {}", i)),
            });
        }
        let prompt = build_acp_prompt_with_budget(&req, model_context_limit(&req.model));
        assert!(
            TokenCalculator::count_text(&prompt) <= 64_000,
            "a 64k model must not receive a deepseek-sized prompt"
        );
        assert!(prompt.contains("ping 99"));
    }

    #[test]
    fn tool_result_is_truncated_head_and_tail_on_normal_turns() {
        let mut req = request_with_system_reminder();
        let big = "A".repeat(8_000) + &"B".repeat(8_000);
        req.messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                id: "call_big".to_string(),
                name: "Bash".to_string(),
                input: json!({"command": "cat big.log"}),
            }]),
        });
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "call_big".to_string(),
                content: json!(big),
                is_error: Some(false),
            }]),
        });

        assert!(!req.is_compaction_request());
        let prompt = build_acp_prompt(&req);
        assert!(prompt.contains("[Output truncated: 16000 chars]"));
        assert!(!prompt.contains(&big));
        // Head and tail are both preserved around the marker.
        assert!(prompt.contains(&"A".repeat(6000)));
        assert!(prompt.contains(&"B".repeat(1500)));
    }
}
