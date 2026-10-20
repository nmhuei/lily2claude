use lily2claude::protocol::anthropic::AnthropicTool;
use lily2claude::protocol::tool_engine::json_parser::*;
use lily2claude::protocol::tool_engine::schema_normalizer::{
    normalize_with_schema, NormalizationError,
};
use lily2claude::protocol::tool_engine::tag_lexer::*;
use lily2claude::protocol::tool_engine::validation::*;
use lily2claude::protocol::tool_engine::ToolCallEngine;
use serde_json::json;

#[test]
fn test_tag_lexer_xml_and_invoke() {
    let t1 = "Here is the call: <tool_call>{\"name\": \"Bash\"}</tool_call>";
    let m1 = scan_tool_tag_start(t1).unwrap();
    assert_eq!(m1.tag_name, "tool_call");
    assert_eq!(&t1[m1.start..m1.content_start], "<tool_call>");

    let t2 = "Command: <invoke name=\"Bash\"><parameter name=\"command\">pwd</parameter></invoke>";
    let m2 = scan_tool_tag_start(t2).unwrap();
    assert_eq!(m2.tag_name, "invoke");
    assert_eq!(m2.name.as_deref(), Some("Bash"));
}

#[test]
fn test_tag_lexer_holds_back_partial_prefix() {
    assert_eq!(partial_tag_prefix_len("hello <tool_ca"), "<tool_ca".len());
    assert_eq!(partial_tag_prefix_len("hello <tool_call>done"), 0);
}

#[test]
fn test_resilient_json_parser_unclosed_braces() {
    let unclosed = r#"{"command": "git status""#;
    let res = parse_resilient_json(unclosed);
    assert_eq!(res.unwrap_value()["command"], "git status");
}

#[test]
fn test_resilient_json_parser_unescaped_newlines() {
    let multiline = "{\"content\": \"line1\nline2\"}";
    let res = parse_resilient_json(multiline);
    assert_eq!(res.unwrap_value()["content"], "line1\nline2");
}

#[test]
fn test_resilient_json_parser_single_quotes_in_shell_preserved() {
    let script = "{'command': 'echo \"hello world\"'}";
    let res = parse_resilient_json(script);
    assert_eq!(res.unwrap_value()["command"], "echo \"hello world\"");
}

#[test]
fn test_resilient_json_parser_depth_cap() {
    let deep = format!("{}1{}", "[".repeat(20), "]".repeat(20));
    assert!(parse_resilient_json(&deep).value().is_none());
}

#[test]
fn test_schema_normalizer_alias_mapping() {
    let schema = json!({
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"]
    });
    let input = json!({"cmd": "pwd"});
    let normalized = normalize_with_schema("Bash", input, &schema).unwrap();
    assert_eq!(normalized["command"], "pwd");
    assert!(normalized.get("cmd").is_none());
}

#[test]
fn test_schema_normalizer_conflict_fails() {
    let schema = json!({
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"]
    });
    let input = json!({"cmd": "pwd", "command": "whoami"});
    let err = normalize_with_schema("Bash", input, &schema).unwrap_err();
    assert!(matches!(err, NormalizationError::AmbiguityConflict(_)));
}

#[test]
fn test_validation_required_and_type() {
    let schema = json!({
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"]
    });
    assert!(validate_arguments("Bash", &json!({"command": "pwd"}), &schema).is_ok());
    assert!(validate_arguments("Bash", &json!({}), &schema).is_err());
    assert!(validate_arguments("Bash", &json!({"command": 7}), &schema).is_err());
}

#[test]
fn test_validation_rejects_additional_properties_when_disallowed() {
    let schema = json!({
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
        "additionalProperties": false
    });
    let err = validate_arguments(
        "Bash",
        &json!({"command": "pwd", "injected": true}),
        &schema,
    )
    .unwrap_err();
    assert!(matches!(err, ValidationError::AdditionalProperty { .. }));
}

#[test]
fn test_strict_engine_only_emits_validated_calls() {
    let tools = vec![AnthropicTool {
        name: "Bash".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        }),
        ..Default::default()
    }];
    let engine = ToolCallEngine::new(&tools);

    let valid = engine
        .extract("<tool_call>{\"name\":\"bash\",\"arguments\":{\"cmd\":\"pwd\"}}</tool_call>");
    assert_eq!(valid.len(), 1);
    assert_eq!(valid[0].name, "Bash");
    assert_eq!(valid[0].arguments["command"], "pwd");

    let invalid = engine
        .extract("<tool_call>{\"name\":\"Bash\",\"arguments\":{\"command\":\"\"}}</tool_call>");
    assert!(invalid.is_empty());
}
