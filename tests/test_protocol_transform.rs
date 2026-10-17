use lily2claude::protocol::anthropic::*;
use lily2claude::protocol::repair::repair_tool_arguments;
use lily2claude::protocol::transform::transform_anthropic_to_openai;
use serde_json::json;

#[test]
fn test_strip_historic_thinking_blocks_and_sanitize() {
    let req = MessagesRequest {
        model: "deepseek-v4.1-flash".to_string(),
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hello".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Blocks(vec![
                    ContentBlock::Thinking {
                        thinking: "prior thinking to strip".to_string(),
                    },
                    ContentBlock::Text {
                        text: "assistant response".to_string(),
                    },
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_abc".to_string(),
                    content: json!("tool output"),
                    is_error: None,
                }]),
            },
        ],
        system: Some(SystemPrompt::Single("system prompt".to_string())),
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: Some("run bash".to_string()),
            input_schema: json!({"type": "object"}),
        }]),
        stream: true,
        max_tokens: 4096,
        temperature: None,
    };

    let openai_req = transform_anthropic_to_openai(&req, true).expect("Transform must succeed");

    // Assistant content must NOT contain thinking block object
    assert_eq!(openai_req.messages[2].role, "assistant");
    assert_eq!(openai_req.messages[2].content.as_deref(), Some("assistant response"));

    // Tool result must be role: "tool"
    assert_eq!(openai_req.messages[3].role, "tool");
    assert_eq!(openai_req.messages[3].tool_call_id.as_deref(), Some("call_abc"));

    // Core tools must be present (lowercase)
    let tools = openai_req.tools.unwrap();
    let names: Vec<String> = tools.into_iter().map(|t| t.function.name).collect();
    assert!(names.contains(&"bash".to_string()));
    assert!(names.contains(&"edit".to_string()));
    assert!(names.contains(&"read".to_string()));
    assert!(names.contains(&"write".to_string()));
    assert!(names.contains(&"glob".to_string()));
    assert!(names.contains(&"grep".to_string()));
}

#[test]
fn test_repair_tool_arguments() {
    let broken_json = "{\"command\": \"echo 'test'\",}";
    let fixed = repair_tool_arguments(broken_json);
    assert_eq!(fixed, "{\"command\": \"echo 'test'\"}");
}
