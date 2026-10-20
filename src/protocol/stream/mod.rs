pub mod acp_adapter;
pub mod block_streamer;
pub mod engine;
pub mod events;
pub mod openai_adapter;
pub mod translator;

pub use acp_adapter::AcpStreamAdapter;
pub use block_streamer::AnthropicBlockStreamer;
pub use engine::{AnthropicStreamEngine, EngineState, FinishReason, StreamInput};
pub use events::SseEvent;
pub use openai_adapter::OpenAiStreamAdapter;
pub use translator::SseStreamTranslator;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_smooth_text_streaming_no_tools() {
        let mut streamer =
            AnthropicBlockStreamer::new("msg_1", "claude-sonnet-5-5", 100, HashMap::new());
        let _ = streamer.start();

        let ev1 = streamer.feed_text("Hello world! ");
        assert_eq!(ev1.len(), 2); // content_block_start + content_block_delta
        assert_eq!(ev1[0].event, "content_block_start");
        assert_eq!(ev1[1].event, "content_block_delta");
        assert_eq!(ev1[1].data["delta"]["text"], "Hello world! ");

        let ev2 = streamer.feed_text("This is smooth streaming.");
        assert_eq!(ev2.len(), 1); // content_block_delta only
        assert_eq!(ev2[0].data["delta"]["text"], "This is smooth streaming.");

        let final_evs = streamer.finish(100, 20, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[1].event, "message_delta");
        assert_eq!(final_evs[1].data["delta"]["stop_reason"], "end_turn");
        assert_eq!(final_evs[2].event, "message_stop");
    }

    #[test]
    fn test_interleaved_text_tool_and_trailing_questions() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer =
            AnthropicBlockStreamer::new("msg_2", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Feed pre-tool text
        let ev1 = streamer.feed_text("Trước tiên cần rà soát repo:\n\n");
        assert_eq!(ev1.len(), 2); // start(0, text) + delta(0, text)
        assert_eq!(ev1[0].data["index"], 0);

        // Feed tool call and trailing questions in one or multiple chunks
        let tool_chunk = "<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls -la\"}}\n</tool_call>\n\n## 6) Sẵn sàng spawn subagent\n1. Chuẩn hóa README\n2. Hoàn thiện toàn diện";
        let ev2 = streamer.feed_text(tool_chunk);

        assert_eq!(ev2.len(), 6);
        assert_eq!(ev2[0].event, "content_block_stop");
        assert_eq!(ev2[0].data["index"], 0);

        assert_eq!(ev2[1].event, "content_block_start");
        assert_eq!(ev2[1].data["index"], 1);
        assert_eq!(ev2[1].data["content_block"]["type"], "tool_use");
        assert_eq!(ev2[1].data["content_block"]["name"], "Bash");

        assert_eq!(ev2[2].event, "content_block_delta");
        assert_eq!(ev2[2].data["index"], 1);
        assert_eq!(ev2[2].data["delta"]["type"], "input_json_delta");

        assert_eq!(ev2[3].event, "content_block_stop");
        assert_eq!(ev2[3].data["index"], 1);

        assert_eq!(ev2[4].event, "content_block_start");
        assert_eq!(ev2[4].data["index"], 2);
        assert_eq!(ev2[4].data["content_block"]["type"], "text");

        assert_eq!(ev2[5].event, "content_block_delta");
        assert_eq!(ev2[5].data["index"], 2);
        let trailing_text = ev2[5].data["delta"]["text"].as_str().unwrap();
        assert!(trailing_text.contains("## 6) Sẵn sàng spawn subagent"));
        assert!(trailing_text.contains("1. Chuẩn hóa README"));

        let final_evs = streamer.finish(100, 50, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[0].data["index"], 2);
        assert_eq!(final_evs[1].event, "message_delta");
        assert_eq!(final_evs[1].data["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn test_partial_tag_split_across_chunks() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer =
            AnthropicBlockStreamer::new("msg_3", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Chunk 1 ends with partial prefix "<tool_"
        let ev1 = streamer.feed_text("Wait for command: <tool_");
        assert_eq!(ev1.len(), 2);
        assert_eq!(ev1[1].data["delta"]["text"], "Wait for command: ");

        // Chunk 2 completes opening tag and delivers tool body
        let ev2 = streamer.feed_text("call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"pwd\"}}\n</tool_call>\nDone!");
        assert_eq!(ev2[0].event, "content_block_stop"); // stops block 0
        assert_eq!(ev2[1].event, "content_block_start"); // tool_use block 1
        assert_eq!(ev2[2].event, "content_block_delta"); // input_json_delta
        assert_eq!(ev2[3].event, "content_block_stop"); // tool_use stop block 1
        assert_eq!(ev2[4].event, "content_block_start"); // text block 2
        assert_eq!(ev2[5].event, "content_block_delta"); // text_delta "Done!"

        let final_evs = streamer.finish(100, 30, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[0].data["index"], 2);
    }

    #[test]
    fn test_multiple_consecutive_tools_interleaved_with_text() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        casing_map.insert("read".to_string(), "Read".to_string());
        let mut streamer =
            AnthropicBlockStreamer::new("msg_4", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Feed pre text
        let _ = streamer.feed_text("Step 1: Check files\n");
        // Feed tool 1
        let _ = streamer.feed_text("<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>\n");
        // Feed tool 2
        let _ = streamer.feed_text("<tool_call>\n{\"name\": \"Read\", \"arguments\": {\"file_path\": \"README.md\"}}\n</tool_call>\n");
        // Feed trailing questions
        let evs =
            streamer.feed_text("All checks completed. Which task would you like to run next?");
        assert!(!evs.is_empty());

        let final_evs = streamer.finish(100, 60, 0);
        assert_eq!(final_evs.last().unwrap().event, "message_stop");
        assert_eq!(
            final_evs[final_evs.len() - 2].data["delta"]["stop_reason"],
            "tool_use"
        );
        assert_eq!(streamer.emitted_tool_count, 2);
    }

    #[test]
    fn test_unclosed_tool_call_repaired_at_stream_end() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer =
            AnthropicBlockStreamer::new("msg_5", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        // Model cuts off inside tool call without </tool_call>
        let _ = streamer.feed_text("Starting scan: <tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"whoami\"");
        let final_evs = streamer.finish(100, 20, 0);

        // Tool should be repaired and emitted as tool_use
        let tool_start = final_evs.iter().find(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"
        });
        assert!(tool_start.is_some());
        assert_eq!(final_evs.last().unwrap().event, "message_stop");
        assert_eq!(
            final_evs[final_evs.len() - 2].data["delta"]["stop_reason"],
            "tool_use"
        );
    }

    #[test]
    fn test_streamer_leaked_thinking_rerouted_from_text() {
        let mut streamer =
            AnthropicBlockStreamer::new("msg_6", "claude-sonnet-5-5", 100, HashMap::new())
                .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // Feed text containing leaked thinking closed by </think>
        let evs = streamer
            .feed_text("Inspecting codebase structure...</think>Here is the project overview.");

        assert_eq!(evs.len(), 6);
        assert_eq!(evs[0].event, "content_block_start");
        assert_eq!(evs[0].data["index"], 0);
        assert_eq!(evs[0].data["content_block"]["type"], "thinking");

        assert_eq!(evs[1].event, "content_block_delta");
        assert_eq!(evs[1].data["index"], 0);
        assert_eq!(
            evs[1].data["delta"]["thinking"],
            "Inspecting codebase structure..."
        );
        assert!(
            evs[1].data["delta"]["estimated_tokens"]
                .as_u64()
                .unwrap_or(0)
                > 0
        );

        assert_eq!(evs[2].event, "content_block_delta");
        assert_eq!(evs[2].data["index"], 0);
        assert_eq!(evs[2].data["delta"]["type"], "signature_delta");

        assert_eq!(evs[3].event, "content_block_stop");
        assert_eq!(evs[3].data["index"], 0);

        assert_eq!(evs[4].event, "content_block_start");
        assert_eq!(evs[4].data["index"], 1);
        assert_eq!(evs[4].data["content_block"]["type"], "text");

        assert_eq!(evs[5].event, "content_block_delta");
        assert_eq!(evs[5].data["index"], 1);
        assert_eq!(
            evs[5].data["delta"]["text"],
            "Here is the project overview."
        );

        let final_evs = streamer.finish(100, 40, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[0].data["index"], 1);
        assert_eq!(final_evs[1].data["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn test_streamer_thinking_16kb_segmentation() {
        let mut streamer =
            AnthropicBlockStreamer::new("msg_7", "claude-sonnet-5-5", 100, HashMap::new())
                .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // Feed 20KB thinking
        let chunk = "x".repeat(20000);
        let evs = streamer.feed_thinking(&chunk);

        // Must contain block stop for segment 0 and block start for segment 1
        let has_segment_stop = evs
            .iter()
            .any(|e| e.event == "content_block_stop" && e.data["index"] == 0);
        let has_segment_start = evs
            .iter()
            .any(|e| e.event == "content_block_start" && e.data["index"] == 1);
        assert!(has_segment_stop);
        assert!(has_segment_start);
    }

    #[test]
    fn test_streamer_action_tag() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer =
            AnthropicBlockStreamer::new("msg_action", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        let chunk = "Executing build:\n<action name=\"Bash\">{\"command\": \"cargo build\"}</action>\nDone.";
        let evs = streamer.feed_text(chunk);

        let has_tool_start = evs
            .iter()
            .any(|e| e.event == "content_block_start" && e.data["content_block"]["name"] == "Bash");
        assert!(has_tool_start);

        let final_evs = streamer.finish(100, 30, 0);
        assert_eq!(
            final_evs
                .iter()
                .find(|e| e.event == "message_delta")
                .unwrap()
                .data["delta"]["stop_reason"],
            "tool_use"
        );
    }

    #[test]
    fn test_streamer_dsml_tag() {
        let mut casing_map = HashMap::new();
        casing_map.insert("bash".to_string(), "Bash".to_string());
        let mut streamer =
            AnthropicBlockStreamer::new("msg_dsml", "claude-sonnet-5-5", 100, casing_map);
        let _ = streamer.start();

        let chunk = "Running command:\n<｜tool calls begin｜><｜tool call begin｜>function<｜tool sep｜>bash\n{\"command\": \"git status\"}<｜tool call end｜><｜tool calls end｜>";
        let evs = streamer.feed_text(chunk);

        let has_tool_start = evs
            .iter()
            .any(|e| e.event == "content_block_start" && e.data["content_block"]["name"] == "Bash");
        assert!(has_tool_start);

        let final_evs = streamer.finish(100, 30, 0);
        assert_eq!(
            final_evs
                .iter()
                .find(|e| e.event == "message_delta")
                .unwrap()
                .data["delta"]["stop_reason"],
            "tool_use"
        );
    }

    #[test]
    fn test_anthropic_block_streamer_thinking_as_text() {
        let mut streamer =
            AnthropicBlockStreamer::new("msg_t2", "claude-sonnet-5-5", 100, HashMap::new())
                .with_stream_thinking_as_text(true);
        let _ = streamer.start();

        let evs1 = streamer.feed_thinking("Exploring the problem space...");
        assert!(
            evs1.iter()
                .any(|e| e.event == "content_block_start"
                    && e.data["content_block"]["type"] == "text")
        );
        assert!(evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("💭 Thinking:\n")
        }));
        assert!(evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Exploring the problem space...")
        }));

        let evs2 = streamer.feed_text("Here is the solution.");
        assert!(evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("💭 (Done thinking)")
        }));
        assert!(evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Here is the solution.")
        }));

        let final_evs = streamer.finish(100, 50, 0);
        assert!(final_evs.iter().any(|e| e.event == "content_block_stop"));
        assert!(final_evs.iter().any(|e| e.event == "message_stop"));
    }

    #[test]
    fn test_sse_stream_translator_thinking_as_text() {
        let mut translator =
            SseStreamTranslator::new("msg_t1", "deepseek").with_stream_thinking_as_text(true);

        // Feed reasoning chunk
        let chunk1 = json!({
            "choices": [{
                "delta": {
                    "reasoning_content": "Analyzing steganography in avatar.png..."
                }
            }]
        })
        .to_string();
        let evs1 = translator.feed_chunk(&chunk1);

        // Must start text block and emit text deltas
        let has_text_start = evs1
            .iter()
            .any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "text");
        assert!(
            has_text_start,
            "Expected content_block_start with type text"
        );

        let has_thinking_prefix = evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("💭 Thinking:\n")
        });
        assert!(
            has_thinking_prefix,
            "Expected text_delta with thinking start marker"
        );

        let has_reasoning_text = evs1.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Analyzing steganography")
        });
        assert!(has_reasoning_text, "Expected reasoning chunk as text_delta");

        // Feed normal text chunk
        let chunk2 = json!({
            "choices": [{
                "delta": {
                    "content": "Found 3 hidden chunks."
                }
            }]
        })
        .to_string();
        let evs2 = translator.feed_chunk(&chunk2);

        // Must emit thinking end marker and then the normal text
        let has_end_marker = evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["type"] == "text_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("💭 (Done thinking)")
        });
        assert!(
            has_end_marker,
            "Expected text_delta with thinking end marker"
        );
    }

    #[test]
    fn test_thinking_after_text_never_splits_text_block() {
        let mut streamer =
            AnthropicBlockStreamer::new("msg_no_split", "claude-sonnet-5-5", 100, HashMap::new())
                .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // 1. First feed normal text
        let evs1 = streamer.feed_text(
            "Workflow tool không có trong session này — tôi fan-out trực tiếp bằng 7 sub",
        );
        let start_text_count = evs1
            .iter()
            .filter(|e| {
                e.event == "content_block_start" && e.data["content_block"]["type"] == "text"
            })
            .count();
        assert_eq!(start_text_count, 1);

        // 2. Leaked or delayed thinking chunk arrives mid-sentence
        let evs2 = streamer.feed_thinking("thinking from another session or delayed reasoning");
        // Must NOT close the text block or open a thinking block!
        let has_thinking_start = evs2.iter().any(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking"
        });
        let has_text_stop = evs2.iter().any(|e| e.event == "content_block_stop");
        assert!(
            !has_thinking_start,
            "Must NOT open thinking block after text started"
        );
        assert!(
            !has_text_stop,
            "Must NOT close text block on mid-sentence thinking"
        );
        let has_marker = evs2.iter().any(|e| {
            e.event == "content_block_delta"
                && e.data["delta"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("💭")
        });
        assert!(!has_marker, "Must NOT emit raw thinking markers into text");

        // 3. Second part of text arrives
        let evs3 = streamer.feed_text("agent song song, mỗi agent một giả thuyết độc lập.");
        // Must NOT start another text block (no extra bullet point)
        let has_new_text_start = evs3.iter().any(|e| e.event == "content_block_start");
        assert!(
            !has_new_text_start,
            "Must NOT start a new text block, sentence must remain unbroken"
        );

        let final_evs = streamer.finish(100, 50, 0);
        assert_eq!(final_evs[0].event, "content_block_stop");
        assert_eq!(final_evs[1].event, "message_delta");
    }

    #[test]
    fn test_streamer_thinking_between_tools_never_emits_text_markers() {
        let mut streamer =
            AnthropicBlockStreamer::new("msg_between", "claude-sonnet-5-5", 100, HashMap::new())
                .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        // 1. Initial text and tool
        let evs1 = streamer.feed_text("Let me inspect directory.\n<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>\n");
        assert!(evs1
            .iter()
            .any(|e| e.event == "content_block_start"
                && e.data["content_block"]["type"] == "tool_use"));

        // 2. OpenCode thought arrives after tool
        let evs2 = streamer.feed_thinking("The directory has 3 files. Let me continue.");
        // Must emit native thinking_delta, NEVER text_delta with 💭 Thinking:
        let has_thinking_block = evs2.iter().any(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking"
        });
        assert!(
            has_thinking_block,
            "Expected native thinking block between tools"
        );
        let has_thinking_delta = evs2.iter().any(|e| {
            e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
        });
        assert!(has_thinking_delta, "Expected native thinking_delta");
        let has_text_delta = evs2.iter().any(|e| e.data["delta"]["type"] == "text_delta");
        assert!(!has_text_delta, "Must NOT emit text_delta for thinking");
        let has_raw_marker = evs2.iter().any(|e| {
            e.data["delta"]["text"]
                .as_str()
                .unwrap_or("")
                .contains("💭")
        });
        assert!(!has_raw_marker, "Must NOT emit raw thinking markers");

        let finish_evs = streamer.finish(100, 50, 0);
        let has_sig = finish_evs.iter().any(|e| {
            e.event == "content_block_delta" && e.data["delta"]["type"] == "signature_delta"
        });
        assert!(
            has_sig,
            "Must emit signature_delta before stopping thinking block"
        );
    }

    #[test]
    fn test_streamer_thinking_closes_with_signature_delta_on_transition() {
        let mut streamer =
            AnthropicBlockStreamer::new("msg_sig", "claude-sonnet-5-5", 100, HashMap::new())
                .with_stream_thinking_as_text(false);
        let _ = streamer.start();

        let _ = streamer.feed_thinking("Contemplating solution...");
        let text_evs = streamer.feed_text("Here is the verified answer.");

        let sig_idx = text_evs.iter().position(|e| {
            e.event == "content_block_delta" && e.data["delta"]["type"] == "signature_delta"
        });
        let stop_idx = text_evs
            .iter()
            .position(|e| e.event == "content_block_stop" && e.data["index"] == 0);
        let text_start_idx = text_evs
            .iter()
            .position(|e| e.event == "content_block_start" && e.data["index"] == 1);

        assert!(sig_idx.is_some(), "Must emit signature_delta");
        assert!(
            stop_idx.is_some(),
            "Must emit content_block_stop for thinking"
        );
        assert!(
            text_start_idx.is_some(),
            "Must emit content_block_start for text"
        );
        assert!(
            sig_idx.unwrap() < stop_idx.unwrap(),
            "signature_delta must precede content_block_stop"
        );
        assert!(
            stop_idx.unwrap() < text_start_idx.unwrap(),
            "content_block_stop must precede text content_block_start"
        );
    }
}
