use lily2claude::protocol::anthropic::AnthropicTool;
use lily2claude::protocol::stream::{
    AcpStreamAdapter, AnthropicStreamEngine, OpenAiStreamAdapter, SseEvent, SseStreamTranslator,
};
use lily2claude::protocol::tokens::AcpOutputChunk;
use serde_json::{json, Value};
use std::collections::HashMap;

fn bash_tool() -> AnthropicTool {
    AnthropicTool {
        name: "Bash".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }),
        ..Default::default()
    }
}

fn visible_text(events: &[SseEvent]) -> String {
    events
        .iter()
        .filter(|e| e.event == "content_block_delta")
        .filter_map(|e| e.data.pointer("/delta/text").and_then(|t| t.as_str()))
        .collect()
}

fn tool_calls(events: &[SseEvent]) -> Vec<(String, Value)> {
    let mut result = Vec::new();
    let mut active: Option<(u64, String, String)> = None;
    for event in events {
        match event.event.as_str() {
            "content_block_start"
                if event.data.pointer("/content_block/type").and_then(|t| t.as_str())
                    == Some("tool_use") =>
            {
                active = Some((
                    event.data["index"].as_u64().unwrap_or(0),
                    event
                        .data
                        .pointer("/content_block/name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string(),
                    String::new(),
                ));
            }
            "content_block_delta" => {
                if let Some((index, _, args)) = active.as_mut() {
                    if event.data["index"].as_u64() == Some(*index)
                        && event.data.pointer("/delta/type").and_then(|t| t.as_str())
                            == Some("input_json_delta")
                    {
                        args.push_str(
                            event
                                .data
                                .pointer("/delta/partial_json")
                                .and_then(|v| v.as_str())
                                .unwrap_or(""),
                        );
                    }
                }
            }
            "content_block_stop" => {
                if let Some((index, name, args)) = active.take() {
                    if event.data["index"].as_u64() == Some(index) {
                        result.push((
                            name,
                            serde_json::from_str(&args).unwrap_or(Value::Null),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    result
}

#[test]
fn test_acp_and_openai_adapters_share_the_same_engine_semantics() {
    let text = "Running now.\n<tool_call>{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}</tool_call>\nDone.";

    let acp_engine = AnthropicStreamEngine::new("msg_shared", "deepseek", 10, HashMap::new())
        .with_tools(&[bash_tool()]);
    let mut acp = AcpStreamAdapter::new(acp_engine);
    let mut acp_events = Vec::new();
    for part in text.split_inclusive(' ') {
        acp_events.extend(acp.feed(AcpOutputChunk::Text(part.to_string())));
    }
    acp_events.extend(acp.finish(5));

    let mut openai = OpenAiStreamAdapter::new("msg_shared", "deepseek", 10, false, &[bash_tool()]);
    let mut openai_events = Vec::new();
    // Split the SSE payload at an awkward byte boundary to exercise the line
    // buffer, and deliberately omit the space after `data:`.
    let payload = json!({
        "choices": [{"delta": {"content": text}}]
    })
    .to_string();
    let wire = format!("data:{}\n\ndata: [DONE]\n\n", payload);
    let split = wire.len() / 2;
    openai_events.extend(openai.feed_bytes(&wire.as_bytes()[..split]));
    openai_events.extend(openai.feed_bytes(&wire.as_bytes()[split..]));
    openai_events.extend(openai.finish());

    assert_eq!(visible_text(&acp_events), visible_text(&openai_events));
    assert_eq!(tool_calls(&acp_events), tool_calls(&openai_events));
    assert_eq!(tool_calls(&acp_events)[0].1["command"], "pwd");
}

#[test]
fn test_sse_stream_state_machine_and_casing() {
    let mut translator = SseStreamTranslator::new("msg_live_01", "deepseek-v4.1-flash")
        .with_stream_thinking_as_text(false);
    translator.register_tool_casing("Bash");

    // Chunk 1: Reasoning
    let c1 = r#"{"choices":[{"delta":{"reasoning_content":"analyzing problem"}}]}"#;
    let ev1 = translator.feed_chunk(c1);
    assert!(ev1.iter().any(|e| e.event == "message_start"));
    assert!(ev1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking"));

    // Chunk 2: Tool call in lowercase 'bash'
    let c2 = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":\"ls -la\"}"}}]}}]}"#;
    let _ = translator.feed_chunk(c2);

    // Chunk 3: Completion
    let c3 = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    let ev3 = translator.feed_chunk(c3);
    assert!(ev3.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["name"] == "Bash")); // Restores PascalCase!
    assert!(ev3.iter().any(|e| e.event == "content_block_delta" && e.data["delta"].get("partial_json").and_then(|v| v.as_str()).map(|s| s.contains("ls -la")).unwrap_or(false)));
    assert!(ev3.iter().any(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "signature_delta"));
    assert!(ev3.iter().any(|e| e.event == "message_delta" && e.data["delta"]["stop_reason"] == "tool_use"));
    assert!(ev3.iter().any(|e| e.event == "message_stop"));
}

#[test]
fn test_sse_stream_thinking_as_text() {
    let mut translator = SseStreamTranslator::new("msg_live_think_text", "deepseek-v4.1-flash")
        .with_stream_thinking_as_text(true);

    // Chunk 1: Reasoning
    let c1 = r#"{"choices":[{"delta":{"reasoning_content":"analyzing problem"}}]}"#;
    let ev1 = translator.feed_chunk(c1);
    assert!(ev1.iter().any(|e| e.event == "message_start"));
    assert!(ev1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "text"));
    assert!(ev1.iter().any(|e| {
        e.event == "content_block_delta"
            && e.data["delta"]["type"] == "text_delta"
            && e.data["delta"]["text"].as_str().unwrap_or("").contains("💭 Thinking:\n")
    }));
    assert!(ev1.iter().any(|e| {
        e.event == "content_block_delta"
            && e.data["delta"]["type"] == "text_delta"
            && e.data["delta"]["text"].as_str().unwrap_or("").contains("analyzing problem")
    }));

    // Chunk 2: Normal content
    let c2 = r#"{"choices":[{"delta":{"content":"Found solution."}}]}"#;
    let ev2 = translator.feed_chunk(c2);
    assert!(ev2.iter().any(|e| {
        e.event == "content_block_delta"
            && e.data["delta"]["type"] == "text_delta"
            && e.data["delta"]["text"].as_str().unwrap_or("").contains("💭 (Done thinking)")
    }));
    assert!(ev2.iter().any(|e| {
        e.event == "content_block_delta"
            && e.data["delta"]["type"] == "text_delta"
            && e.data["delta"]["text"].as_str().unwrap_or("").contains("Found solution.")
    }));
}

#[test]
fn test_sse_stream_directory_read_converted_to_bash() {
    let mut translator = SseStreamTranslator::new("msg_live_dir", "deepseek-v4.1-flash");
    translator.register_tool_casing("Read");
    translator.register_tool_casing("Bash");

    let c1 = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c_dir","function":{"name":"Read","arguments":"{\"file_path\":\"/tmp\"}"}}]}}]}"#;
    let _ = translator.feed_chunk(c1);

    let c2 = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    let ev2 = translator.feed_chunk(c2);

    // /tmp is a directory, so Read must be automatically converted to Bash "ls -la /tmp"
    assert!(ev2.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["name"] == "Bash"));
    assert!(ev2.iter().any(|e| e.event == "content_block_delta" && e.data["delta"]["partial_json"].as_str().unwrap().contains("ls -la /tmp")));
    assert!(ev2.iter().any(|e| e.event == "message_delta" && e.data["delta"]["stop_reason"] == "tool_use"));
}

#[test]
fn test_sse_stream_empty_tool_call_dropped() {
    let mut translator = SseStreamTranslator::new("msg_live_empty", "deepseek-v4.1-flash");
    translator.register_tool_casing("Bash");

    // Empty tool call with no command
    let c1 = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c_empty","function":{"name":"Bash","arguments":"{}"}}]}}]}"#;
    let _ = translator.feed_chunk(c1);

    let c2 = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    let ev2 = translator.feed_chunk(c2);

    // Empty tool call should be dropped, stop_reason should be end_turn
    assert!(!ev2.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"));
    assert!(ev2.iter().any(|e| e.event == "message_delta" && e.data["delta"]["stop_reason"] == "end_turn"));
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

#[test]
fn test_sse_stream_partial_text_then_error_emits_error_not_end_turn() {
    let mut engine = AnthropicStreamEngine::new("msg_test", "deepseek", 100, HashMap::new());
    // Simulate model outputting partial text "Đang ho"
    let ev1 = engine.feed_text("Đang ho");
    assert!(ev1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "text"));
    assert!(ev1.iter().any(|e| e.event == "content_block_delta" && e.data["delta"]["text"] == "Đang ho"));

    // Now turn fails with a rate-limit / quota error
    let err_events = engine.feed_error("Internal error: Free usage limit reached. provider.free-limit");

    // Must emit content_block_stop to cleanly close the open text block
    assert!(err_events.iter().any(|e| e.event == "content_block_stop"));
    // Must emit error event with type "overloaded_error"
    let err_ev = err_events.iter().find(|e| e.event == "error").expect("error event present");
    assert_eq!(err_ev.data["error"]["type"], "overloaded_error");
    // Must NEVER emit stop_reason: "end_turn"!
    assert!(!err_events.iter().any(|e| e.event == "message_delta" && e.data["delta"]["stop_reason"] == "end_turn"));
}

