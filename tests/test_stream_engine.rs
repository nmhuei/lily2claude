use lily2claude::protocol::anthropic::AnthropicTool;
use lily2claude::protocol::stream::engine::*;
use lily2claude::protocol::stream::events::SseEvent;
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

fn logical_text(events: &[SseEvent]) -> String {
    events
        .iter()
        .filter(|e| e.event == "content_block_delta")
        .filter_map(|e| e.data.pointer("/delta/text").and_then(|t| t.as_str()))
        .collect()
}

fn logical_tools(events: &[SseEvent]) -> Vec<(String, Value)> {
    let mut tools = Vec::new();
    let mut current: Option<(usize, String)> = None;
    let mut args = String::new();
    for event in events {
        match event.event.as_str() {
            "content_block_start" => {
                if let Some(block) = event.data.get("content_block") {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let index = event.data["index"].as_u64().unwrap_or(0) as usize;
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        current = Some((index, name));
                        args.clear();
                    }
                }
            }
            "content_block_delta" => {
                if let Some((index, _)) = &current {
                    if event.data["index"].as_u64() == Some(*index as u64)
                        && event.data.pointer("/delta/type").and_then(|t| t.as_str())
                            == Some("input_json_delta")
                    {
                        args.push_str(
                            event
                                .data
                                .pointer("/delta/partial_json")
                                .and_then(|t| t.as_str())
                                .unwrap_or(""),
                        );
                    }
                }
            }
            "content_block_stop" => {
                if let Some((index, name)) = current.take() {
                    if event.data["index"].as_u64() == Some(index as u64) {
                        tools.push((name, serde_json::from_str(&args).unwrap_or(Value::Null)));
                    }
                }
            }
            _ => {}
        }
    }
    tools
}

fn logical_blocks(events: &[SseEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.event == "content_block_start")
        .filter_map(|e| {
            e.data
                .pointer("/content_block/type")
                .and_then(|t| t.as_str())
                .map(|t| match t {
                    "tool_use" => format!(
                        "tool_use:{}",
                        e.data
                            .pointer("/content_block/name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                    ),
                    other => other.to_string(),
                })
        })
        .collect()
}

fn stop_reason(events: &[SseEvent]) -> Option<String> {
    events
        .iter()
        .find(|e| e.event == "message_delta")
        .and_then(|e| e.data.pointer("/delta/stop_reason"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

#[test]
fn test_chunk_invariance_property() {
    let full_text = "I will check the directory.\n<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"pwd\"}}\n</tool_call>\nDone!";

    // Test Case A: fed as a single chunk.
    let mut engine_a = AnthropicStreamEngine::new("msg_a", "deepseek", 100, HashMap::new())
        .with_tools(&[bash_tool()]);
    let mut events_a = engine_a.feed_text(full_text);
    events_a.extend(engine_a.finish(100, 10, 0));

    // Test Case B: fed one Unicode scalar at a time.
    let mut engine_b = AnthropicStreamEngine::new("msg_a", "deepseek", 100, HashMap::new())
        .with_tools(&[bash_tool()]);
    let mut events_b = Vec::new();
    for ch in full_text.chars() {
        events_b.extend(engine_b.feed_text(&ch.to_string()));
    }
    events_b.extend(engine_b.finish(100, 10, 0));

    assert_eq!(logical_blocks(&events_a), logical_blocks(&events_b));
    assert_eq!(logical_text(&events_a), logical_text(&events_b));
    assert_eq!(logical_tools(&events_a), logical_tools(&events_b));
    assert_eq!(stop_reason(&events_a), stop_reason(&events_b));
    assert_eq!(stop_reason(&events_a).as_deref(), Some("tool_use"));
}

#[test]
fn test_chunk_invariance_compatibility_mode() {
    let full_text = "I will check the directory.\n<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"pwd\"}}\n</tool_call>\nDone!";

    let mut engine_a = AnthropicStreamEngine::new("msg_compat", "deepseek", 100, HashMap::new());
    let mut events_a = engine_a.feed_text(full_text);
    events_a.extend(engine_a.finish(100, 10, 0));

    let mut engine_b = AnthropicStreamEngine::new("msg_compat", "deepseek", 100, HashMap::new());
    let mut events_b = Vec::new();
    for ch in full_text.chars() {
        events_b.extend(engine_b.feed_text(&ch.to_string()));
    }
    events_b.extend(engine_b.finish(100, 10, 0));

    assert_eq!(logical_blocks(&events_a), logical_blocks(&events_b));
    assert_eq!(logical_text(&events_a), logical_text(&events_b));
    assert_eq!(logical_tools(&events_a), logical_tools(&events_b));
    assert_eq!(logical_tools(&events_a)[0].1["command"], "pwd");
}

#[test]
fn test_blocks_open_and_close_exactly_once() {
    let mut engine = AnthropicStreamEngine::new("msg_lifecycle", "deepseek", 10, HashMap::new())
        .with_tools(&[bash_tool()]);
    let mut events = engine.feed_thinking("planning");
    events.extend(engine.feed_text(
        "Before.\n<tool_call>{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}</tool_call>\nAfter.",
    ));
    events.extend(engine.finish(10, 5, 0));

    let mut starts = HashMap::new();
    let mut stops = Vec::new();
    for event in &events {
        match event.event.as_str() {
            "content_block_start" => {
                let index = event.data["index"].as_u64().unwrap() as usize;
                let block_type = event
                    .data
                    .pointer("/content_block/type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                assert!(
                    starts.insert(index, block_type).is_none(),
                    "block {index} started twice"
                );
            }
            "content_block_stop" => {
                let index = event.data["index"].as_u64().unwrap() as usize;
                assert!(!stops.contains(&index), "block {index} stopped twice");
                stops.push(index);
            }
            _ => {}
        }
    }
    let mut ordered: Vec<_> = starts.iter().collect();
    ordered.sort_unstable_by_key(|(index, _)| **index);
    let start_indexes: Vec<_> = ordered.iter().map(|(index, _)| **index).collect();
    let block_types: Vec<_> = ordered
        .iter()
        .map(|(_, block_type)| block_type.as_str())
        .collect();
    stops.sort_unstable();
    assert_eq!(start_indexes, stops);
    assert_eq!(block_types, vec!["thinking", "text", "tool_use", "text"]);
    assert_eq!(
        events.iter().filter(|e| e.event == "message_start").count(),
        1
    );
    assert_eq!(
        events.iter().filter(|e| e.event == "message_stop").count(),
        1
    );

    let text = logical_text(&events);
    assert!(!text.contains("<tool_call>"));
    assert!(!text.contains("EqQ"));
}

#[test]
fn test_cancel_and_upstream_error_terminate() {
    let mut cancelled = AnthropicStreamEngine::new("msg_cancel", "deepseek", 10, HashMap::new());
    let _ = cancelled.feed_text("partial");
    let events = cancelled.cancel();
    assert!(events.iter().any(|e| e.event == "message_delta"
        && e.data
            .pointer("/delta/stop_reason")
            .and_then(|s| s.as_str())
            == Some("cancelled")));
    assert!(events.iter().any(|e| e.event == "message_stop"));
    assert!(cancelled.feed_text("ignored").is_empty());

    let mut errored = AnthropicStreamEngine::new("msg_error", "deepseek", 10, HashMap::new());
    let _ = errored.feed_text("partial");
    let events = errored.feed_error("boom");
    assert!(events.iter().any(|e| e.event == "error"));
    assert!(errored.feed_text("ignored").is_empty());
}

#[test]
fn test_interleaved_thinking_text_tool_text() {
    let mut engine = AnthropicStreamEngine::new("msg_1", "deepseek", 100, HashMap::new())
        .with_tools(&[bash_tool()]);
    let ev1 = engine.feed_thinking("Planning steps...");
    assert!(
        ev1.iter()
            .any(|e| e.event == "content_block_start"
                && e.data["content_block"]["type"] == "thinking")
    );

    let ev2 = engine.feed_text(
        "Let me run pwd: <tool_call>{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}</tool_call>\nWaiting for output.",
    );
    // A signature delta must close thinking before the text block.
    assert!(
        ev2.iter()
            .any(|e| e.data.pointer("/delta/type").and_then(|t| t.as_str())
                == Some("signature_delta"))
    );
    assert!(ev2.iter().any(|e| e
        .data
        .pointer("/content_block/type")
        .and_then(|t| t.as_str())
        == Some("tool_use")));
}

#[test]
fn test_partial_tag_is_not_leaked_as_text() {
    let mut engine = AnthropicStreamEngine::new("msg_partial", "deepseek", 10, HashMap::new())
        .with_tools(&[bash_tool()]);
    let first = engine.feed_text("Checking now <tool_ca");
    assert!(!logical_text(&first).contains("<tool_ca"));
    let second =
        engine.feed_text("ll>{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}</tool_call>");
    let tools = logical_tools(&second);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].1["command"], "pwd");
}

#[test]
fn test_unclosed_tool_call_is_repaired_at_finish() {
    let mut engine = AnthropicStreamEngine::new("msg_unclosed", "deepseek", 10, HashMap::new())
        .with_tools(&[bash_tool()]);
    let _ = engine
        .feed_text("<tool_call>{\"name\":\"Bash\",\"arguments\":{\"command\":\"git status\"}");
    let tail = engine.finish(10, 5, 0);
    assert_eq!(logical_tools(&tail)[0].1["command"], "git status");
}

#[test]
fn test_upstream_max_tokens_is_preserved() {
    let mut engine = AnthropicStreamEngine::new("msg_len", "deepseek", 10, HashMap::new());
    let events = engine.finish_with_reason(FinishReason::MaxTokens, 10, 5, 0);
    assert_eq!(stop_reason(&events).as_deref(), Some("max_tokens"));
}

#[test]
fn test_parallel_tool_calls_in_same_turn() {
    let mut engine = AnthropicStreamEngine::new("msg_parallel", "deepseek", 100, HashMap::new())
        .with_tools(&[bash_tool()]);

    let stream_chunk = r#"
<tool_call>
{"name": "Bash", "arguments": {"command": "echo 1"}}
</tool_call>
<tool_call>
{"name": "Bash", "arguments": {"command": "echo 2"}}
</tool_call>
"#;
    let ev1 = engine.feed_text(stream_chunk);
    let tools = logical_tools(&ev1);
    assert_eq!(tools.len(), 2, "Both parallel tool calls must be captured in the same turn");
    assert_eq!(tools[0].0, "Bash");
    assert_eq!(tools[0].1["command"], "echo 1");
    assert_eq!(tools[1].0, "Bash");
    assert_eq!(tools[1].1["command"], "echo 2");

    let tail = engine.finish(100, 20, 0);
    assert_eq!(stop_reason(&tail).as_deref(), Some("tool_use"));
}

#[test]
fn test_finish_is_idempotent() {
    let mut engine = AnthropicStreamEngine::new("msg_idem", "deepseek", 10, HashMap::new());
    let _ = engine.feed_text("hello");
    assert!(!engine.finish(10, 1, 0).is_empty());
    assert!(engine.finish(10, 1, 0).is_empty());
}
