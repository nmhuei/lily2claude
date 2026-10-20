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
            ..Default::default()
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

#[test]
fn test_build_acp_prompt_compaction_truncates_and_injects_directive() {
    let huge_output = "a".repeat(1000);
    let req = MessagesRequest {
        model: "deepseek".to_string(),
        system: Some(SystemPrompt::Single("You are Claude.".to_string())),
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: Some("Execute command".to_string()),
            input_schema: json!({}),
            ..Default::default()
        }]),
        stream: true,
        max_tokens: 4096,
        temperature: None,
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("Read logs".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Blocks(vec![ContentBlock::ToolUse {
                    id: "call_log".to_string(),
                    name: "Bash".to_string(),
                    input: json!({"command": "cat /var/log/syslog"}),
                }]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_log".to_string(),
                    content: json!(huge_output),
                    is_error: Some(false),
                }]),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text(
                    "REMINDER: Do NOT call any tools. Respond with plain text only — <summary>Primary Request and Intent: ...</summary>".to_string(),
                ),
            },
        ],
    };

    assert!(req.is_compaction_request());
    let prompt = build_acp_prompt(&req);

    // 1. [Tools Instruction] MUST NOT be present
    assert!(!prompt.contains("[Tools Instruction]"));

    // 2. Huge tool result MUST be truncated
    assert!(prompt.contains("[Output truncated: 1000 chars]"));
    assert!(!prompt.contains(&huge_output));

    // 3. [COMPACTION DIRECTIVE] MUST be injected
    assert!(prompt.contains("[COMPACTION DIRECTIVE]"));
    assert!(prompt.contains("Produce ONLY the concise summary enclosed within <summary>...</summary> tags."));
}

#[tokio::test]
async fn test_turn_after_compaction_is_not_compaction() {
    let req = MessagesRequest {
        model: "claude-sonnet-5-5".to_string(),
        system: None,
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: Some("Run command".to_string()),
            input_schema: serde_json::json!({"type": "object"}),
            ..Default::default()
        }]),
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("Free up context by summarizing the conversation".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Text("<summary>\n1. Primary Request and Intent: Audit project\n</summary>".to_string()),
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("tiếp tục triển khai kế hoạch đi".to_string()),
            },
        ],
        stream: true,
        max_tokens: 4096,
        temperature: None,
    };

    assert!(!req.is_compaction_request(), "Turn after compaction must NOT be detected as compaction request");
    let prompt = build_acp_prompt(&req);
    assert!(prompt.contains("[Tools Instruction]"), "Tools instruction must be restored on subsequent turn");
    assert!(!prompt.contains("[COMPACTION DIRECTIVE]"), "Compaction directive must NOT be injected on normal turn");
}

