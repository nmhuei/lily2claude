use lily2claude::protocol::stream::SseStreamTranslator;

#[test]
fn test_sse_stream_state_machine_and_casing() {
    let mut translator = SseStreamTranslator::new("msg_live_01", "deepseek-v4.1-flash");
    translator.register_tool_casing("Bash");

    // Chunk 1: Reasoning
    let c1 = r#"{"choices":[{"delta":{"reasoning_content":"analyzing problem"}}]}"#;
    let ev1 = translator.feed_chunk(c1);
    assert!(ev1.iter().any(|e| e.event == "message_start"));
    assert!(ev1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking"));

    // Chunk 2: Tool call in lowercase 'bash'
    let c2 = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":"}}]}}]}"#;
    let ev2 = translator.feed_chunk(c2);
    assert!(ev2.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["name"] == "Bash")); // Restores PascalCase!

    // Chunk 3: Completion
    let c3 = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    let ev3 = translator.feed_chunk(c3);
    assert!(ev3.iter().any(|e| e.event == "message_delta" && e.data["delta"]["stop_reason"] == "tool_use"));
    assert!(ev3.iter().any(|e| e.event == "message_stop"));
}

#[test]
fn test_sse_stream_hallucinated_tool_call_repair() {
    let mut translator = SseStreamTranslator::new("msg_live_02", "deepseek-v4.1-flash");
    translator.register_tool_casing("Bash");

    // Chunk 1: Text with markdown bash command (hallucinated in text without native tool calls)
    let c1 = r#"{"choices":[{"delta":{"content":"I will run tests now:\n```bash\ncargo test\n```\n"}}]}"#;
    let ev1 = translator.feed_chunk(c1);
    assert!(ev1.iter().any(|e| e.event == "message_start"));
    assert!(ev1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "text"));

    // Chunk 2: Completion with finish_reason "stop" (model thinks it's done)
    let c2 = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
    let ev2 = translator.feed_chunk(c2);

    // Verify ToolCallRepairEngine synthesized the tool_use event!
    assert!(ev2.iter().any(|e| e.event == "content_block_start"
        && e.data["content_block"]["type"] == "tool_use"
        && e.data["content_block"]["name"] == "Bash"));

    assert!(ev2.iter().any(|e| e.event == "content_block_delta"
        && e.data["delta"]["type"] == "input_json_delta"
        && e.data["delta"]["partial_json"].as_str().unwrap().contains("cargo test")));

    // Stop reason must be upgraded to tool_use so Claude Code executes it!
    assert!(ev2.iter().any(|e| e.event == "message_delta" && e.data["delta"]["stop_reason"] == "tool_use"));
    assert!(ev2.iter().any(|e| e.event == "message_stop"));
}
