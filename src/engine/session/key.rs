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
    if let Some(val) = headers.get("x-claude-code-session-id").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // 2. Custom lily session header
    if let Some(val) = headers.get("x-lily-session").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // 3. Generic session header
    if let Some(val) = headers.get("x-session-id").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // 4. Fallback to conversation root message hash
    extract_session_key(req)
}
