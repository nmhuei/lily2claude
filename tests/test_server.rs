use lily2claude::config::Config;
use lily2claude::engine::router::EngineRouter;
use lily2claude::protocol::anthropic::{
    AnthropicContent, AnthropicMessage, AnthropicTool, ContentBlock, MessagesRequest, SystemPrompt,
};
use lily2claude::server::{build_acp_prompt, create_router};
use serde_json::json;

#[tokio::test]
async fn test_server_routes_setup() {
    let config = Config::load_default();
    let router = std::sync::Arc::new(EngineRouter::new(config));
    let _app = create_router(router);
}

#[test]
fn test_build_acp_prompt_preserves_multiturn_tools_and_system() {
    let req = MessagesRequest {
        model: "deepseek".to_string(),
        system: Some(SystemPrompt::Single("You are a helpful cybersecurity assistant.".to_string())),
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: Some("Execute a shell command".to_string()),
            input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }]),
        stream: true,
        max_tokens: 4096,
        temperature: None,
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("Check Linux kernel version".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Blocks(vec![
                    ContentBlock::Text {
                        text: "I will run uname -r to check kernel version:".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_123".to_string(),
                        name: "Bash".to_string(),
                        input: json!({"command": "uname -r"}),
                    },
                ]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_123".to_string(),
                    content: json!("6.18.31-kali1"),
                    is_error: Some(false),
                }]),
            },
        ],
    };

    let prompt = build_acp_prompt(&req);

    assert!(prompt.contains("[System Instructions]"));
    assert!(prompt.contains("You are a helpful cybersecurity assistant."));
    assert!(prompt.contains("[Tools Instruction]"));
    assert!(prompt.contains("Bash"));
    assert!(prompt.contains("User: Check Linux kernel version"));
    assert!(prompt.contains("I will run uname -r to check kernel version:"));
    assert!(prompt.contains("<tool_call>"));
    assert!(prompt.contains("\"name\": \"Bash\""));
    assert!(prompt.contains("uname -r"));
    assert!(prompt.contains("<tool_result>"));
    assert!(prompt.contains("6.18.31-kali1"));
}

#[test]
fn test_get_streamable_text_end_filters_tool_tags() {
    use lily2claude::server::get_streamable_text_end;

    // Normal text without tool tags
    let s1 = "Hello, world! How are you?";
    assert_eq!(get_streamable_text_end(s1), s1.len());

    // Text ending with partial tag prefix
    let s2 = "I will check the directory now <tool_";
    assert_eq!(get_streamable_text_end(s2), "I will check the directory now ".len());

    // Text containing complete <tool_call> tag
    let s3 = "Checking...\n<tool_call>\n{\"name\": \"Read\"}\n</tool_call>";
    assert_eq!(get_streamable_text_end(s3), "Checking...\n".len());

    // Text starting directly with <tool_call>
    let s4 = "<tool_call>\n{\"name\": \"Bash\"}\n</tool_call>";
    assert_eq!(get_streamable_text_end(s4), 0);
}
