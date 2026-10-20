use lily2claude::protocol::anthropic::{
    AnthropicContent, AnthropicMessage, AnthropicTool, MessagesRequest,
};

use lily2claude::protocol::tokens::{
    extract_acp_usage, TokenCalculator, TokenUsage, TokenUsageTracker,
};
use serde_json::json;

#[test]
fn test_token_calculator_simple_text() {
    let count = TokenCalculator::count_text("Hello, world!");
    assert_eq!(count, 4); // "Hello", ",", " world", "!" in cl100k_base
}


#[test]
fn test_token_calculator_request() {
    let req = MessagesRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("What is the capital of France?".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Text("The capital of France is Paris.".to_string()),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("Thank you!".to_string()),
            },
        ],
        system: Some(lily2claude::protocol::anthropic::SystemPrompt::Single(
            "You are a helpful cybersecurity assistant.".to_string(),
        )),
        tools: Some(vec![AnthropicTool {
            name: "execute_bash".to_string(),
            description: Some("Execute bash shell command".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command to run"}
                },
                "required": ["command"]
            }),
            ..Default::default()
        }]),
        stream: true,
        max_tokens: 4096,
        temperature: None,
    };

    let total = TokenCalculator::count_request_tokens(&req);
    // Verified reasonable token count (~70-90 tokens including schemas and framing)
    assert!(total > 50 && total < 150, "Expected 50 < total < 150, got {}", total);
}

#[test]
fn test_extract_acp_usage_opencode_format() {
    let rpc_val = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "result": {
            "stopReason": "end_turn",
            "usage": {
                "inputTokens": 4085,
                "outputTokens": 32,
                "totalTokens": 4117,
                "thoughtTokens": 10,
                "cachedReadTokens": 2048,
                "cachedWriteTokens": 0
            }
        }
    });

    let usage = extract_acp_usage(&rpc_val).expect("Should extract valid usage");
    assert_eq!(usage.input_tokens, 4085);
    assert_eq!(usage.output_tokens, 32);
    assert_eq!(usage.cache_read_input_tokens, 2048);
    assert_eq!(usage.total(), 4117);
}

#[tokio::test]
async fn test_token_usage_tracker_record_and_snapshot() {
    let tracker = TokenUsageTracker::new();
    tracker
        .record_turn(
            "session_alpha",
            "deepseek",
            TokenUsage {
                input_tokens: 1200,
                output_tokens: 80,
                cache_read_input_tokens: 500,
                cache_creation_input_tokens: 0,
            },
        )
        .await;

    tracker
        .record_turn(
            "session_alpha",
            "deepseek",
            TokenUsage {
                input_tokens: 2400,
                output_tokens: 120,
                cache_read_input_tokens: 1000,
                cache_creation_input_tokens: 0,
            },
        )
        .await;

    tracker
        .record_turn(
            "session_beta",
            "glm",
            TokenUsage {
                input_tokens: 500,
                output_tokens: 50,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        )
        .await;

    let snap = tracker.snapshot().await;
    assert_eq!(snap.total_requests, 3);
    assert_eq!(snap.cumulative.input_tokens, 4100);
    assert_eq!(snap.cumulative.output_tokens, 250);
    assert_eq!(snap.cumulative.total_tokens, 4350);
    assert_eq!(snap.cumulative.cached_read_tokens, 1500);
    assert_eq!(snap.active_sessions_count, 2);

    let alpha_session = snap
        .sessions
        .iter()
        .find(|s| s.session_id == "session_alpha")
        .expect("session_alpha should exist");
    assert_eq!(alpha_session.request_count, 2);
    assert_eq!(alpha_session.input_tokens, 3600);
    assert_eq!(alpha_session.output_tokens, 200);
    assert_eq!(alpha_session.cached_read_tokens, 1500);

    let deepseek_model = snap.models.get("deepseek").expect("deepseek should exist");
    assert_eq!(deepseek_model.request_count, 2);
    assert_eq!(deepseek_model.input_tokens, 3600);
    assert_eq!(deepseek_model.output_tokens, 200);
}

#[tokio::test]
async fn test_token_usage_tracker_reset() {
    let tracker = TokenUsageTracker::new();
    tracker
        .record_turn(
            "session_alpha",
            "deepseek",
            TokenUsage {
                input_tokens: 1200,
                output_tokens: 80,
                cache_read_input_tokens: 500,
                cache_creation_input_tokens: 0,
            },
        )
        .await;

    let snap_before = tracker.snapshot().await;
    assert_eq!(snap_before.total_requests, 1);
    assert_eq!(snap_before.cumulative.input_tokens, 1200);

    tracker.reset().await;

    let snap_after = tracker.snapshot().await;
    assert_eq!(snap_after.total_requests, 0);
    assert_eq!(snap_after.cumulative.input_tokens, 0);
    assert_eq!(snap_after.cumulative.output_tokens, 0);
    assert_eq!(snap_after.cumulative.cached_read_tokens, 0);
    assert_eq!(snap_after.cumulative.total_tokens, 0);
    assert_eq!(snap_after.active_sessions_count, 0);
    assert!(snap_after.models.is_empty());
}

