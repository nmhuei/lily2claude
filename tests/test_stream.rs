use lily2claude::protocol::stream::SseStreamTranslator;

#[test]
fn test_sse_stream_state_machine_and_casing() {
    let mut translator = SseStreamTranslator::new("msg_live_01", "deepseek-v4.1-flash");
    translator.register_tool_casing("Bash");

    // Chunk 1: Reasoning
    let c1 = r#"{"choices":[{"delta":{"reasoning_content":"analyzing problem"}}]}"#;
    let ev1 = translator.feed_chunk(c1);
    assert!(ev1.iter().any(|e| e.contains("message_start")));
    assert!(ev1.iter().any(|e| e.contains("thinking")));

    // Chunk 2: Tool call in lowercase 'bash'
    let c2 = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":"}}]}}]}"#;
    let ev2 = translator.feed_chunk(c2);
    assert!(ev2.iter().any(|e| e.contains("tool_use")));
    assert!(ev2.iter().any(|e| e.contains("Bash"))); // Restores PascalCase!

    // Chunk 3: Completion
    let c3 = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    let ev3 = translator.feed_chunk(c3);
    assert!(ev3.iter().any(|e| e.contains("message_delta")));
    assert!(ev3.iter().any(|e| e.contains("tool_use")));
    assert!(ev3.iter().any(|e| e.contains("message_stop")));
}
