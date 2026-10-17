use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Self-repair common malformed JSON issues in tool call arguments
pub fn repair_tool_arguments(raw: &str) -> String {
    if serde_json::from_str::<Value>(raw).is_ok() {
        return raw.to_string();
    }

    let mut candidate = raw.trim().to_string();

    // Fix trailing comma in objects
    if candidate.contains(",}") {
        candidate = candidate.replace(",}", "}");
    }
    // Fix trailing comma in arrays
    if candidate.contains(",]") {
        candidate = candidate.replace(",]", "]");
    }
    // Balance open brace
    if candidate.starts_with('{') && !candidate.ends_with('}') {
        candidate.push('}');
    }

    if serde_json::from_str::<Value>(&candidate).is_ok() {
        return candidate;
    }

    raw.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairedToolCall {
    pub name: String,
    pub arguments: Value,
}

static RE_TOOL_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>").unwrap());

static RE_INVOKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<invoke\s+name=["']([^"']+)["']\s*>(.*?)</invoke>"#).unwrap());

static RE_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<parameter\s+name=["']([^"']+)["']\s*>(.*?)</parameter>"#).unwrap());

static RE_BASH_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)```(?:bash|sh|shell|zsh)\s*\n([\s\S]*?)\n```").unwrap());

pub struct ToolCallRepairEngine;

impl ToolCallRepairEngine {
    /// Detects hallucinated tool calls embedded in text (XML tags or markdown code fences)
    /// and synthesizes valid Anthropic tool use structures.
    pub fn detect_and_extract(
        text: &str,
        casing_map: &HashMap<String, String>,
    ) -> Vec<RepairedToolCall> {
        let mut results = Vec::new();

        // 1. Detect <tool_call> ... </tool_call>
        for cap in RE_TOOL_CALL.captures_iter(text) {
            let json_body = &cap[1];
            let repaired_body = repair_tool_arguments(json_body);
            if let Ok(val) = serde_json::from_str::<Value>(&repaired_body) {
                if let Some(raw_name) = val.get("name").and_then(|v| v.as_str()) {
                    let mapped_name = casing_map
                        .get(&raw_name.to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_else(|| raw_name.to_string());

                    let mut args_val = val
                        .get("arguments")
                        .or_else(|| val.get("parameters"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));

                    if let Some(s) = args_val.as_str() {
                        let rep_s = repair_tool_arguments(s);
                        if let Ok(inner) = serde_json::from_str::<Value>(&rep_s) {
                            args_val = inner;
                        }
                    }

                    results.push(RepairedToolCall {
                        name: mapped_name,
                        arguments: args_val,
                    });
                }
            }
        }

        if !results.is_empty() {
            return results;
        }

        // 2. Detect <invoke name="..."> ... </invoke>
        for cap in RE_INVOKE.captures_iter(text) {
            let raw_name = &cap[1];
            let body = &cap[2];
            let mapped_name = casing_map
                .get(&raw_name.to_ascii_lowercase())
                .cloned()
                .unwrap_or_else(|| raw_name.to_string());

            let mut args_map = serde_json::Map::new();
            for pcap in RE_PARAM.captures_iter(body) {
                let pname = pcap[1].to_string();
                let pval = pcap[2].trim().to_string();
                args_map.insert(pname, Value::String(pval));
            }

            results.push(RepairedToolCall {
                name: mapped_name,
                arguments: Value::Object(args_map),
            });
        }

        if !results.is_empty() {
            return results;
        }

        // 3. Detect markdown bash / sh blocks IF AND ONLY IF "bash" is declared by Claude Code
        if let Some(bash_name) = casing_map.get("bash") {
            for cap in RE_BASH_FENCE.captures_iter(text) {
                let cmd = cap[1].trim();
                if !cmd.is_empty() {
                    results.push(RepairedToolCall {
                        name: bash_name.clone(),
                        arguments: serde_json::json!({
                            "command": cmd
                        }),
                    });
                }
            }
        }

        results
    }

    /// Strips XML tool call artifacts from text so the user's terminal remains clean
    pub fn strip_tool_tags(text: &str) -> String {
        let stripped = RE_TOOL_CALL.replace_all(text, "");
        let stripped = RE_INVOKE.replace_all(&stripped, "");
        stripped.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_xml_tool_call() {
        let mut map = HashMap::new();
        map.insert("bash".to_string(), "Bash".to_string());

        let text = r#"I will check the status.
<tool_call>
{"name": "bash", "arguments": {"command": "git status"}}
</tool_call>"#;

        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "git status");

        let clean = ToolCallRepairEngine::strip_tool_tags(text);
        assert_eq!(clean, "I will check the status.");
    }

    #[test]
    fn test_extract_invoke_format() {
        let mut map = HashMap::new();
        map.insert("bash".to_string(), "Bash".to_string());

        let text = r#"Running command:
<invoke name="Bash">
<parameter name="command">cargo test</parameter>
</invoke>"#;

        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "cargo test");
    }

    #[test]
    fn test_extract_markdown_bash_fence() {
        let mut map = HashMap::new();
        map.insert("bash".to_string(), "Bash".to_string());

        let text = "Here is what we should run:\n```bash\ncargo check --all\n```\nLet's see.";
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "cargo check --all");
    }

    #[test]
    fn test_do_not_convert_bash_when_tool_not_registered() {
        let map = HashMap::new(); // Bash tool not allowed/declared
        let text = "```bash\nrm -rf /\n```";
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert!(calls.is_empty());
    }
}
