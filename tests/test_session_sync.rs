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

#[tokio::test]
async fn test_session_manager_sweep_idle() {
    let mgr = SessionManager::new();
    mgr.sweep_idle(std::time::Duration::from_secs(0)).await;
    assert_eq!(mgr.active_count().await, 0);
}

#[test]
fn test_subagent_headers_isolate_session_keys() {
    use axum::http::HeaderMap;
    use lily2claude::engine::session::extract_session_key_from_headers_or_req;

    let req = MessagesRequest {
        model: "deepseek".to_string(),
        messages: vec![],
        system: None,
        tools: None,
        stream: false,
        max_tokens: 100,
        temperature: None,
    };

    let mut main_headers = HeaderMap::new();
    main_headers.insert("x-claude-code-session-id", "root-session-123".parse().unwrap());

    let mut subagent1_headers = HeaderMap::new();
    subagent1_headers.insert("x-claude-code-session-id", "root-session-123".parse().unwrap());
    subagent1_headers.insert("x-claude-code-agent-id", "subagent-audit-protocol".parse().unwrap());

    let mut subagent2_headers = HeaderMap::new();
    subagent2_headers.insert("x-claude-code-session-id", "root-session-123".parse().unwrap());
    subagent2_headers.insert("x-claude-code-agent-id", "subagent-audit-implementation".parse().unwrap());

    let main_key = extract_session_key_from_headers_or_req(&main_headers, &req);
    let sub1_key = extract_session_key_from_headers_or_req(&subagent1_headers, &req);
    let sub2_key = extract_session_key_from_headers_or_req(&subagent2_headers, &req);

    assert_eq!(main_key, "root-session-123");
    assert_eq!(sub1_key, "root-session-123:agent:subagent-audit-protocol");
    assert_eq!(sub2_key, "root-session-123:agent:subagent-audit-implementation");
    assert_ne!(main_key, sub1_key);
    assert_ne!(sub1_key, sub2_key);
}
