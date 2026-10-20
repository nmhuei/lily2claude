//! Schema-driven tool engine.
//!
//! Public contract:
//! * Tier 1 — [`json_parser::parse_resilient_json`]
//! * Tier 2 — [`schema_normalizer::normalize_with_schema`]
//! * Tier 3 — [`validation::validate_arguments`]
//!
//! The engine only emits a tool call when all three tiers succeed.

pub mod json_parser;
pub mod schema_normalizer;
pub mod tag_lexer;
pub mod validation;

pub use json_parser::{parse_resilient_json, ParseError, ParseResult};
pub use schema_normalizer::{
    normalize_with_schema, normalize_with_schema_detailed, NormalizationError, NormalizedArguments,
};
pub use tag_lexer::{
    scan_partial_tool_tag_start, scan_tool_tag_end, scan_tool_tag_start, streamable_text_end,
    strip_tool_tags, TagMatch,
};
pub use validation::{validate_arguments, CatalogTool, ToolCatalog, ValidationError};

use crate::protocol::anthropic::AnthropicTool;
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallCandidate {
    pub call_id: String,
    pub raw_name: String,
    pub mapped_name: String,
    pub raw_arguments: String,
    pub parsed_arguments: Value,
    pub recovery_strategy: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub recovery_strategy: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ToolCallEngine {
    catalog: ToolCatalog,
}

impl ToolCallEngine {
    pub fn new(tools: &[AnthropicTool]) -> Self {
        Self {
            catalog: ToolCatalog::from_tools(tools),
        }
    }

    pub fn with_catalog(catalog: ToolCatalog) -> Self {
        Self { catalog }
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    /// Parse, normalize and validate a single candidate.
    pub fn validate_and_normalize(
        &self,
        candidate: ToolCallCandidate,
    ) -> Result<ValidatedToolCall, ValidationError> {
        let resolved = self
            .catalog
            .resolve(&candidate.raw_name)
            .ok_or_else(|| ValidationError::UnknownTool(candidate.raw_name.clone()))?;

        let normalized = normalize_with_schema_detailed(
            &resolved.declared_name,
            candidate.parsed_arguments,
            &resolved.schema,
        )?;

        validate_arguments(&resolved.declared_name, &normalized.value, &resolved.schema)?;

        let mut recovery = Vec::new();
        if let Some(extra) = candidate.recovery_strategy {
            recovery.push(extra);
        }
        recovery.extend(normalized.recovery);

        Ok(ValidatedToolCall {
            call_id: candidate.call_id,
            name: resolved.declared_name.clone(),
            arguments: normalized.value,
            recovery_strategy: (!recovery.is_empty()).then(|| recovery.join("+")),
        })
    }

    /// Extract every valid tool call from an agent text fragment.
    pub fn extract(&self, text: &str) -> Vec<ValidatedToolCall> {
        self.extract_with_errors(text).0
    }

    /// Extract valid calls and return rejected candidates alongside them.
    pub fn extract_with_errors(
        &self,
        text: &str,
    ) -> (Vec<ValidatedToolCall>, Vec<ValidationError>) {
        let mut valid = Vec::new();
        let mut errors = Vec::new();

        for candidate in self.extract_candidates(text, true) {
            match self.validate_and_normalize(candidate) {
                Ok(call) => valid.push(call),
                Err(err) => errors.push(err),
            }
        }

        (valid, errors)
    }

    /// Scan an envelope fragment.  When `allow_unclosed` is true the parser
    /// will attempt bounded recovery on an envelope missing its closing tag.
    pub fn extract_candidates(&self, text: &str, allow_unclosed: bool) -> Vec<ToolCallCandidate> {
        let mut candidates = Vec::new();
        let mut cursor = 0usize;
        let mut sequence = 0usize;

        while cursor < text.len() {
            let rest = &text[cursor..];
            let Some(found) = tag_lexer::scan_tool_tag_start(rest) else {
                break;
            };

            let open_abs = cursor + found.start;
            let content_abs = cursor + found.content_start;

            let Some(rel_end) = rest[found.content_start..].find(found.close_tag) else {
                if allow_unclosed {
                    let body = &text[content_abs..];
                    for candidate in
                        parse_envelope_body(found.tag_name, found.name.as_deref(), body, sequence)
                    {
                        candidates.push(candidate);
                    }
                }
                break;
            };

            let close_abs = content_abs + rel_end;
            let block_end = close_abs + found.close_tag.len();
            let body = &text[content_abs..close_abs];

            for candidate in
                parse_envelope_body(found.tag_name, found.name.as_deref(), body, sequence)
            {
                candidates.push(candidate);
                sequence += 1;
            }

            cursor = block_end.max(open_abs + 1);
        }

        candidates
    }

    /// Parse a single complete envelope (useful for streaming).
    pub fn parse_complete_block(&self, block: &str) -> Vec<ToolCallCandidate> {
        self.extract_candidates(block, false)
    }
}

fn parse_envelope_body(
    tag_name: &str,
    attr_name: Option<&str>,
    body: &str,
    sequence: usize,
) -> Vec<ToolCallCandidate> {
    match tag_name {
        "invoke" => parse_invoke(attr_name, body, sequence)
            .into_iter()
            .collect(),
        "dsml" => parse_dsml(body, sequence).into_iter().collect(),
        _ => parse_json_envelope(attr_name, body, sequence)
            .into_iter()
            .collect(),
    }
}

fn parse_invoke(attr_name: Option<&str>, body: &str, sequence: usize) -> Option<ToolCallCandidate> {
    let name = attr_name?;
    let mut map = Map::new();
    let mut cursor = 0usize;
    while let Some(pos) = body[cursor..].find("<parameter") {
        let abs = cursor + pos;
        let open = &body[abs..];
        let Some(gt) = open.find('>') else {
            break;
        };
        let open_tag = &open[..gt + 1];
        let param_name = tag_lexer::parse_name_attr(open_tag)?;
        let value_start = abs + gt + 1;
        let Some(close_rel) = body[value_start..].find("</parameter>") else {
            let raw = body[value_start..].trim();
            insert_parameter(&mut map, &param_name, raw);
            break;
        };
        let raw = body[value_start..value_start + close_rel].trim();
        insert_parameter(&mut map, &param_name, raw);
        cursor = value_start + close_rel + "</parameter>".len();
    }

    let arguments = Value::Object(map);
    Some(ToolCallCandidate {
        call_id: format!("call_tool_{}", sequence),
        raw_name: name.to_string(),
        mapped_name: name.to_string(),
        raw_arguments: arguments.to_string(),
        parsed_arguments: arguments,
        recovery_strategy: None,
    })
}

fn insert_parameter(map: &mut Map<String, Value>, name: &str, raw: &str) {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        map.insert(name.to_string(), value);
    } else {
        map.insert(name.to_string(), Value::String(raw.to_string()));
    }
}

fn parse_dsml(body: &str, sequence: usize) -> Option<ToolCallCandidate> {
    let (sep_abs, sep_len) = if let Some(pos) = body.find("<｜tool sep｜>") {
        (pos, "<｜tool sep｜>".len())
    } else if let Some(pos) = body.find("<|tool sep|>") {
        (pos, "<|tool sep|>".len())
    } else {
        return None;
    };

    let after_sep = &body[sep_abs + sep_len..];
    let mut lines = after_sep.lines();
    let raw_name = lines.next()?.trim();
    if raw_name.is_empty() {
        return None;
    }
    let json_body = lines.collect::<Vec<_>>().join("\n");
    let parsed = json_parser::parse_resilient_json(&json_body);
    let (arguments, recovery) = match parsed {
        ParseResult::Valid(v) => (v, None),
        ParseResult::Recovered(v, strategy) => (v, Some(strategy)),
        ParseResult::Invalid(_) => return None,
    };

    Some(ToolCallCandidate {
        call_id: format!("call_tool_{}", sequence),
        raw_name: raw_name.to_string(),
        mapped_name: raw_name.to_string(),
        raw_arguments: arguments.to_string(),
        parsed_arguments: arguments,
        recovery_strategy: recovery,
    })
}

fn parse_json_envelope(
    attr_name: Option<&str>,
    body: &str,
    sequence: usize,
) -> Option<ToolCallCandidate> {
    let parsed = json_parser::parse_resilient_json(body);
    let (value, recovery) = match parsed {
        ParseResult::Valid(v) => (v, None),
        ParseResult::Recovered(v, strategy) => (v, Some(strategy)),
        ParseResult::Invalid(_) => return None,
    };

    let mut obj = match value {
        Value::Object(obj) => obj,
        other => {
            // A bare scalar/array cannot identify a tool.
            let name = attr_name?;
            let args = Value::Array(vec![other]);
            return Some(ToolCallCandidate {
                call_id: format!("call_tool_{}", sequence),
                raw_name: name.to_string(),
                mapped_name: name.to_string(),
                raw_arguments: args.to_string(),
                parsed_arguments: args,
                recovery_strategy: recovery,
            });
        }
    };

    // OpenAI-style nested function object.
    if let Some(function) = obj.remove("function").and_then(|f| f.as_object().cloned()) {
        let name = function
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
            .or_else(|| attr_name.map(|s| s.to_string()));
        let arguments = function
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        if let Some(name) = name {
            let arguments = parse_arguments_value(arguments);
            return Some(build_candidate(name, arguments, recovery, sequence));
        }
    }

    let raw_name = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .or_else(|| obj.get("tool_name"))
        .or_else(|| obj.get("action"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| attr_name.map(|s| s.to_string()));

    let name = raw_name?;

    let arguments = obj
        .get("arguments")
        .or_else(|| obj.get("parameters"))
        .or_else(|| obj.get("args"))
        .or_else(|| obj.get("input"))
        .or_else(|| obj.get("action_input"))
        .cloned()
        .map(parse_arguments_value)
        .unwrap_or_else(|| {
            let mut copy = obj.clone();
            for key in ["name", "tool", "tool_name", "action"] {
                copy.remove(key);
            }
            Value::Object(copy)
        });

    Some(build_candidate(name, arguments, recovery, sequence))
}

fn parse_arguments_value(value: Value) -> Value {
    if let Value::String(raw) = value {
        match json_parser::parse_resilient_json(&raw) {
            ParseResult::Valid(v) | ParseResult::Recovered(v, _) => v,
            ParseResult::Invalid(_) => Value::String(raw),
        }
    } else {
        value
    }
}

fn build_candidate(
    raw_name: String,
    arguments: Value,
    recovery: Option<String>,
    sequence: usize,
) -> ToolCallCandidate {
    ToolCallCandidate {
        call_id: format!("call_tool_{}", sequence),
        mapped_name: raw_name.clone(),
        raw_name,
        raw_arguments: arguments.to_string(),
        parsed_arguments: arguments,
        recovery_strategy: recovery,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_and_validates_tool_call() {
        let tools = vec![AnthropicTool {
            name: "Bash".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
                "additionalProperties": false
            }),
            ..Default::default()
        }];
        let engine = ToolCallEngine::new(&tools);
        let text = "x <tool_call>{\"name\":\"bash\",\"arguments\":{\"cmd\":\"pwd\"}}</tool_call> y";
        let calls = engine.extract(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments, json!({"command": "pwd"}));
    }

    #[test]
    fn unknown_tool_is_not_emitted() {
        let engine = ToolCallEngine::new(&[]);
        let calls = engine.extract(
            "<tool_call>{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}</tool_call>",
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn invoke_parameters_are_parsed() {
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
        let text = "<invoke name=\"Bash\"><parameter name=\"command\">pwd</parameter></invoke>";
        let calls = engine.extract(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["command"], "pwd");
    }
}
