use crate::protocol::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Extracts a deterministic session key from conversation root message
pub fn extract_session_key(req: &MessagesRequest) -> String {
    let mut hasher = DefaultHasher::new();
    if let Some(first_msg) = req.messages.first() {
        first_msg.role.hash(&mut hasher);
        match &first_msg.content {
            AnthropicContent::Text(t) => t.hash(&mut hasher),
            AnthropicContent::Blocks(blocks) => {
                for b in blocks {
                    if let ContentBlock::Text { text } = b {
                        text.hash(&mut hasher);
                    }
                }
            }
        }
    } else {
        "empty_session".hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Extracts persistent session key prioritizing Claude Code's native session headers
pub fn extract_session_key_from_headers_or_req(
    headers: &axum::http::HeaderMap,
    req: &MessagesRequest,
) -> String {
    // 1. Claude Code native session header
    let base = if let Some(val) = headers.get("x-claude-code-session-id").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            Some(trimmed.to_string())
        } else {
            None
        }
    } else if let Some(val) = headers.get("x-lily-session").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            Some(trimmed.to_string())
        } else {
            None
        }
    } else if let Some(val) = headers.get("x-session-id").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            Some(trimmed.to_string())
        } else {
            None
        }
    } else {
        None
    }.unwrap_or_else(|| extract_session_key(req));

    // Isolate subagents: if Claude Code passes an agent ID, namespace the session key
    // so parallel subagents don't fight over the same worker slots or kill each other on eviction!
    if let Some(agent_id) = headers
        .get("x-claude-code-agent-id")
        .or_else(|| headers.get("x-agent-id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        format!("{}:agent:{}", base, agent_id)
    } else {
        base
    }
}
