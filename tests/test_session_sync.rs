use lily2claude::engine::session::{extract_session_key, SessionManager};
use lily2claude::protocol::anthropic::{AnthropicContent, AnthropicMessage, MessagesRequest};

#[test]
fn test_session_key_is_deterministic_across_turns() {
    let msg1 = AnthropicMessage {
        role: "user".to_string(),
        content: AnthropicContent::Text("Initial system and user prompt".to_string()),
    };
    let msg2 = AnthropicMessage {
        role: "assistant".to_string(),
        content: AnthropicContent::Text("Assistant response".to_string()),
    };
    let msg3 = AnthropicMessage {
        role: "user".to_string(),
        content: AnthropicContent::Text("Second turn user prompt".to_string()),
    };

    // Turn 1 request
    let req_turn1 = MessagesRequest {
        model: "deepseek".to_string(),
        messages: vec![msg1.clone()],
        system: None,
        tools: None,
        stream: false,
        max_tokens: 100,
        temperature: None,
    };

    // Turn 2 request (contains prior history + new turn)
    let req_turn2 = MessagesRequest {
        model: "deepseek".to_string(),
        messages: vec![msg1.clone(), msg2.clone(), msg3.clone()],
        system: None,
        tools: None,
        stream: false,
        max_tokens: 100,
        temperature: None,
    };

    let key1 = extract_session_key(&req_turn1);
    let key2 = extract_session_key(&req_turn2);

    assert_eq!(key1, key2, "Session key must remain identical across conversation turns");
    assert!(!key1.is_empty());
}

#[tokio::test]
async fn test_session_manager_initialization() {
    let mgr = SessionManager::new();
    assert_eq!(mgr.active_count().await, 0);
}
