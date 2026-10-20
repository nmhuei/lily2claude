//! Tier 2: schema-driven, non-destructive argument normalization.
//!
//! The normalizer only moves a value from an alias to a target property when
//! that target is actually declared by Claude Code's tool schema.  It never
//! rewrites a value that is already valid for the declared schema, and it
//! refuses ambiguous inputs instead of silently picking a winner.

use serde_json::{json, Map, Value};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizationError {
    InvalidSchema(String),
    AmbiguityConflict(String),
}

impl fmt::Display for NormalizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NormalizationError::InvalidSchema(msg) => write!(f, "invalid tool schema: {}", msg),
            NormalizationError::AmbiguityConflict(field) => {
                write!(
                    f,
                    "ambiguous tool arguments: both '{}' and an alias were supplied",
                    field
                )
            }
        }
    }
}

impl std::error::Error for NormalizationError {}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedArguments {
    pub value: Value,
    pub recovery: Vec<String>,
}

/// Alias registry.  A target is only applied when the schema declares that
/// target and the alias itself is not also a declared property.
const ALIASES: &[(&str, &[&str])] = &[
    (
        "command",
        &[
            "cmd",
            "code",
            "script",
            "run",
            "exec",
            "shell_command",
            "command_line",
        ],
    ),
    (
        "file_path",
        &[
            "path",
            "file",
            "filepath",
            "filename",
            "filePath",
            "uri",
            "target",
            "path_to_file",
            "source_file",
        ],
    ),
    (
        "content",
        &[
            "text",
            "contents",
            "data",
            "body",
            "file_content",
            "new_content",
        ],
    ),
    (
        "old_string",
        &[
            "old_text",
            "original_text",
            "search",
            "before",
            "find",
            "target_string",
        ],
    ),
    (
        "new_string",
        &[
            "new_text",
            "replacement",
            "after",
            "to",
            "replacement_string",
            "substitute",
            "replace_with",
        ],
    ),
    (
        "pattern",
        &[
            "glob",
            "regex",
            "file_pattern",
            "query",
            "search",
            "term",
            "file_glob",
        ],
    ),
    (
        "path",
        &[
            "directory",
            "dir",
            "folder",
            "root",
            "base_path",
            "working_directory",
        ],
    ),
    (
        "query",
        &[
            "q",
            "search_query",
            "keyword",
            "query_string",
            "term",
            "text",
        ],
    ),
    ("url", &["uri", "link", "address", "target_url", "endpoint"]),
    (
        "prompt",
        &[
            "task",
            "instruction",
            "query",
            "message",
            "input",
            "request",
        ],
    ),
    ("description", &["desc", "summary", "title"]),
    (
        "subagent_type",
        &["agent_type", "agent", "type", "subagent", "agent_name"],
    ),
    ("skill", &["skill_name", "name"]),
    ("taskId", &["task_id", "id"]),
    ("subject", &["title", "name", "task"]),
    ("script", &["workflow", "code", "body"]),
    ("cron", &["schedule", "cron_expression", "expression"]),
    ("to", &["recipient", "agent", "target"]),
    ("message", &["content", "text", "body"]),
    ("notebook_path", &["path", "file", "file_path", "notebook"]),
    ("new_source", &["source", "code", "content", "text"]),
    (
        "limit",
        &["num_lines", "count", "lines", "length", "max_lines"],
    ),
    ("offset", &["start_line", "line", "from", "start"]),
];

/// Normalize arguments against a declared JSON schema.
pub fn normalize_with_schema(
    tool_name: &str,
    input: Value,
    schema: &Value,
) -> Result<Value, NormalizationError> {
    normalize_with_schema_detailed(tool_name, input, schema).map(|n| n.value)
}

pub fn normalize_with_schema_detailed(
    _tool_name: &str,
    input: Value,
    schema: &Value,
) -> Result<NormalizedArguments, NormalizationError> {
    let mut recovery = Vec::new();

    // A missing/object-less schema means there are no declared properties.
    // Preserve the arguments exactly as supplied.
    let Some(schema_obj) = schema.as_object() else {
        return Ok(NormalizedArguments {
            value: input,
            recovery,
        });
    };

    let Some(properties) = schema_obj.get("properties").and_then(|p| p.as_object()) else {
        return Ok(NormalizedArguments {
            value: input,
            recovery,
        });
    };

    let mut args = match input {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => {
            // A bare string can be the JSON serialization of an object.
            if let Value::String(raw) = other {
                if let Ok(Value::Object(map)) =
                    crate::protocol::tool_engine::json_parser::parse_resilient_json(&raw)
                        .into_result()
                {
                    recovery.push("parsed_string_arguments".to_string());
                    map
                } else {
                    return Err(NormalizationError::InvalidSchema(
                        "tool arguments must be a JSON object".to_string(),
                    ));
                }
            } else {
                return Err(NormalizationError::InvalidSchema(
                    "tool arguments must be a JSON object".to_string(),
                ));
            }
        }
    };

    // Alias mapping: only for declared targets, only when the alias is not a
    // declared property in its own right.
    for (target, aliases) in ALIASES {
        if !properties.contains_key(*target) {
            continue;
        }

        let mut conflicts = Vec::new();
        for alias in *aliases {
            if properties.contains_key(*alias) {
                // Both names are real fields in this schema; never treat one
                // as an alias for the other.
                continue;
            }
            if args.contains_key(*alias) {
                conflicts.push(*alias);
            }
        }

        if args.contains_key(*target) && !conflicts.is_empty() {
            return Err(NormalizationError::AmbiguityConflict((*target).to_string()));
        }

        if !args.contains_key(*target) {
            if conflicts.len() > 1 {
                return Err(NormalizationError::AmbiguityConflict((*target).to_string()));
            }
            if let Some(alias) = conflicts.first() {
                if let Some(value) = args.remove(*alias) {
                    args.insert((*target).to_string(), value);
                    recovery.push(format!("alias:{}->{}", alias, target));
                }
            }
        }
    }

    // Value-level normalization that is still schema-driven.
    if properties.contains_key("subagent_type") {
        if let Some(value) = args.get_mut("subagent_type") {
            if let Some(s) = value.as_str() {
                let normalized = match s {
                    "" | "general" | "default" => Some("general-purpose"),
                    "explore" => Some("Explore"),
                    "plan" => Some("Plan"),
                    _ => None,
                };
                if let Some(normalized) = normalized {
                    *value = json!(normalized);
                    recovery.push("subagent_type_alias".to_string());
                }
            }
        }
    }

    // Coerce primitive values only when the schema explicitly asks for that
    // primitive.  A failed coercion is left intact so Tier 3 can report a
    // structured type error.
    for (name, prop_schema) in properties {
        let Some(expected) = schema_type(prop_schema) else {
            continue;
        };
        let Some(value) = args.get_mut(name) else {
            continue;
        };

        match expected {
            "integer" => {
                if let Some(s) = value.as_str() {
                    if let Ok(n) = s.parse::<i64>() {
                        *value = json!(n);
                        recovery.push(format!("coerce:{}:integer", name));
                    }
                } else if let Some(n) = value.as_f64() {
                    if n.fract() == 0.0 {
                        *value = json!(n as i64);
                    }
                }
            }
            "number" => {
                if let Some(s) = value.as_str() {
                    if let Ok(n) = s.parse::<f64>() {
                        *value = json!(n);
                        recovery.push(format!("coerce:{}:number", name));
                    }
                }
            }
            "boolean" => match value {
                Value::String(s) if s.eq_ignore_ascii_case("true") => {
                    *value = json!(true);
                    recovery.push(format!("coerce:{}:boolean", name));
                }
                Value::String(s) if s.eq_ignore_ascii_case("false") => {
                    *value = json!(false);
                    recovery.push(format!("coerce:{}:boolean", name));
                }
                Value::Number(n) if n.as_i64() == Some(1) => {
                    *value = json!(true);
                    recovery.push(format!("coerce:{}:boolean", name));
                }
                Value::Number(n) if n.as_i64() == Some(0) => {
                    *value = json!(false);
                    recovery.push(format!("coerce:{}:boolean", name));
                }
                _ => {}
            },
            "object" => {
                if let Value::String(raw) = value {
                    if let Ok(Value::Object(parsed)) =
                        crate::protocol::tool_engine::json_parser::parse_resilient_json(raw)
                            .into_result()
                    {
                        *value = Value::Object(parsed);
                        recovery.push(format!("coerce:{}:object", name));
                    }
                }
            }
            _ => {}
        }
    }

    // Enforce `additionalProperties: false` when the schema declares it.  The
    // normalizer removes unknown keys instead of passing them to Claude Code,
    // which would reject the call.
    let additional_allowed = schema_obj
        .get("additionalProperties")
        .map(|v| v.as_bool().unwrap_or(true))
        .unwrap_or(true);
    if !additional_allowed {
        let declared: std::collections::HashSet<&str> =
            properties.keys().map(|s| s.as_str()).collect();
        let before = args.len();
        args.retain(|k, _| declared.contains(k.as_str()));
        if args.len() != before {
            recovery.push("dropped_additional_properties".to_string());
        }
    }

    Ok(NormalizedArguments {
        value: Value::Object(args),
        recovery,
    })
}

fn schema_type(schema: &Value) -> Option<&str> {
    match schema.get("type") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Array(types)) => types.iter().find_map(|t| t.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
            "additionalProperties": false
        })
    }

    #[test]
    fn alias_is_mapped() {
        let out = normalize_with_schema("Bash", json!({"cmd": "pwd"}), &bash_schema()).unwrap();
        assert_eq!(out["command"], "pwd");
        assert!(out.get("cmd").is_none());
    }

    #[test]
    fn conflict_is_rejected() {
        let err = normalize_with_schema(
            "Bash",
            json!({"cmd": "pwd", "command": "whoami"}),
            &bash_schema(),
        )
        .unwrap_err();
        assert!(matches!(err, NormalizationError::AmbiguityConflict(_)));
    }

    #[test]
    fn declared_alias_is_not_rewritten() {
        let schema = json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "glob": {"type": "string"}
            }
        });
        let out = normalize_with_schema("Grep", json!({"pattern": "x", "glob": "*.rs"}), &schema)
            .unwrap();
        assert_eq!(out["glob"], "*.rs");
    }

    #[test]
    fn additional_properties_are_removed_when_declared() {
        let out = normalize_with_schema(
            "Bash",
            json!({"command": "pwd", "cwd": "/tmp"}),
            &bash_schema(),
        )
        .unwrap();
        assert_eq!(out, json!({"command": "pwd"}));
    }
}
