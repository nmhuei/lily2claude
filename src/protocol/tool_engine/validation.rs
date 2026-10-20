//! Tier 3: validation against Claude Code's declared tool catalog.

use super::schema_normalizer::NormalizationError;
use crate::protocol::anthropic::AnthropicTool;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone)]
pub struct CatalogTool {
    pub declared_name: String,
    pub schema: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: Vec<CatalogTool>,
}

impl ToolCatalog {
    pub fn from_tools(tools: &[AnthropicTool]) -> Self {
        let mut catalog = Self::default();
        let mut seen = std::collections::HashSet::new();
        for tool in tools {
            let name = tool.resolved_name().to_string();
            if name.is_empty() || !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            catalog.tools.push(CatalogTool {
                declared_name: name,
                schema: tool.schema().clone(),
            });
        }
        catalog
    }

    pub fn from_casing_map(map: &HashMap<String, String>) -> Self {
        let mut catalog = Self::default();
        let mut seen = std::collections::HashSet::new();
        for name in map.values() {
            if seen.insert(name.to_ascii_lowercase()) {
                catalog.tools.push(CatalogTool {
                    declared_name: name.clone(),
                    schema: serde_json::json!({"type": "object"}),
                });
            }
        }
        catalog
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(|t| t.declared_name.as_str())
    }

    pub fn tools(&self) -> &[CatalogTool] {
        &self.tools
    }

    /// Case-insensitive resolution.  A unique version suffix is accepted so a
    /// model that writes `web_search` still resolves to a declared
    /// `web_search_20250305`.
    pub fn resolve(&self, raw_name: &str) -> Option<&CatalogTool> {
        let needle = raw_name.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return None;
        }

        if let Some(exact) = self
            .tools
            .iter()
            .find(|t| t.declared_name.to_ascii_lowercase() == needle)
        {
            return Some(exact);
        }

        let mut suffixed: Vec<&CatalogTool> = self
            .tools
            .iter()
            .filter(|t| {
                let declared = t.declared_name.to_ascii_lowercase();
                declared.starts_with(&format!("{}_", needle))
                    || needle.starts_with(&format!("{}_", declared))
            })
            .collect();
        if suffixed.len() == 1 {
            return suffixed.pop();
        }

        None
    }

    pub fn contains(&self, raw_name: &str) -> bool {
        self.resolve(raw_name).is_some()
    }

    pub fn schema_for(&self, raw_name: &str) -> Option<&Value> {
        self.resolve(raw_name).map(|t| &t.schema)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    UnknownTool(String),
    Parse(String),
    Normalization(String),
    MissingRequired {
        tool: String,
        field: String,
    },
    EmptyRequired {
        tool: String,
        field: String,
    },
    TypeMismatch {
        tool: String,
        field: String,
        expected: String,
        actual: String,
    },
    InvalidEnum {
        tool: String,
        field: String,
        value: String,
    },
    AdditionalProperty {
        tool: String,
        field: String,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::UnknownTool(t) => {
                write!(f, "tool '{}' is not declared by Claude Code", t)
            }
            ValidationError::Parse(msg) => write!(f, "tool argument parse error: {}", msg),
            ValidationError::Normalization(msg) => {
                write!(f, "tool argument normalization error: {}", msg)
            }
            ValidationError::MissingRequired { tool, field } => {
                write!(f, "tool '{}' is missing required field '{}'", tool, field)
            }
            ValidationError::EmptyRequired { tool, field } => {
                write!(f, "tool '{}' has an empty required field '{}'", tool, field)
            }
            ValidationError::TypeMismatch {
                tool,
                field,
                expected,
                actual,
            } => write!(
                f,
                "tool '{}' field '{}' expected {}, got {}",
                tool, field, expected, actual
            ),
            ValidationError::InvalidEnum { tool, field, value } => write!(
                f,
                "tool '{}' field '{}' has unsupported value '{}'",
                tool, field, value
            ),
            ValidationError::AdditionalProperty { tool, field } => {
                write!(f, "tool '{}' has undeclared field '{}'", tool, field)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

impl From<NormalizationError> for ValidationError {
    fn from(value: NormalizationError) -> Self {
        ValidationError::Normalization(value.to_string())
    }
}

/// Validate a normalized argument object against a tool schema.
pub fn validate_arguments(
    tool_name: &str,
    args: &Value,
    schema: &Value,
) -> Result<(), ValidationError> {
    let Some(args_obj) = args.as_object() else {
        return Err(ValidationError::TypeMismatch {
            tool: tool_name.to_string(),
            field: "<root>".to_string(),
            expected: "object".to_string(),
            actual: json_type(args).to_string(),
        });
    };

    let schema_obj = match schema.as_object() {
        Some(obj) => obj,
        None => return Ok(()),
    };

    let properties = schema_obj
        .get("properties")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();

    if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array()) {
        for field in required.iter().filter_map(|f| f.as_str()) {
            let Some(value) = args_obj.get(field) else {
                return Err(ValidationError::MissingRequired {
                    tool: tool_name.to_string(),
                    field: field.to_string(),
                });
            };
            if value.is_null() {
                return Err(ValidationError::MissingRequired {
                    tool: tool_name.to_string(),
                    field: field.to_string(),
                });
            }
            if let Some(s) = value.as_str() {
                if s.trim().is_empty() {
                    return Err(ValidationError::EmptyRequired {
                        tool: tool_name.to_string(),
                        field: field.to_string(),
                    });
                }
            }
        }
    }

    let additional_allowed = schema_obj
        .get("additionalProperties")
        .map(|v| v.as_bool().unwrap_or(true))
        .unwrap_or(true);
    if !additional_allowed {
        for field in args_obj.keys() {
            if !properties.contains_key(field) {
                return Err(ValidationError::AdditionalProperty {
                    tool: tool_name.to_string(),
                    field: field.clone(),
                });
            }
        }
    }

    for (field, property_schema) in &properties {
        let Some(value) = args_obj.get(field) else {
            continue;
        };
        if value.is_null() {
            continue;
        }

        if let Some(expected) = property_schema.get("type") {
            if !type_matches(expected, value) {
                return Err(ValidationError::TypeMismatch {
                    tool: tool_name.to_string(),
                    field: field.clone(),
                    expected: describe_type(expected),
                    actual: json_type(value).to_string(),
                });
            }
        }

        if let Some(values) = property_schema.get("enum").and_then(|v| v.as_array()) {
            if !values.iter().any(|v| v == value) {
                return Err(ValidationError::InvalidEnum {
                    tool: tool_name.to_string(),
                    field: field.clone(),
                    value: value.to_string(),
                });
            }
        }
    }

    Ok(())
}

fn type_matches(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(expected) => single_type_matches(expected, value),
        Value::Array(types) => types.iter().any(|t| {
            t.as_str()
                .map(|t| single_type_matches(t, value))
                .unwrap_or(false)
        }),
        _ => true,
    }
}

fn single_type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value
                    .as_f64()
                    .map(|n| n.is_finite() && n.fract() == 0.0)
                    .unwrap_or(false)
        }
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn describe_type(expected: &Value) -> String {
    match expected {
        Value::String(s) => s.clone(),
        Value::Array(types) => types
            .iter()
            .filter_map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join("|"),
        _ => "unknown".to_string(),
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.as_i64().is_some() || n.as_u64().is_some() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_and_types_are_enforced() {
        let schema = json!({
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"]
        });
        assert!(validate_arguments("Bash", &json!({"command": "pwd"}), &schema).is_ok());
        assert!(matches!(
            validate_arguments("Bash", &json!({}), &schema),
            Err(ValidationError::MissingRequired { .. })
        ));
        assert!(matches!(
            validate_arguments("Bash", &json!({"command": 42}), &schema),
            Err(ValidationError::TypeMismatch { .. })
        ));
        assert!(matches!(
            validate_arguments("Bash", &json!({"command": "  "}), &schema),
            Err(ValidationError::EmptyRequired { .. })
        ));
    }

    #[test]
    fn version_suffix_resolves_uniquely() {
        let tool = AnthropicTool {
            name: "web_search_20250305".to_string(),
            ..Default::default()
        };
        let catalog = ToolCatalog::from_tools(&[tool]);
        assert_eq!(
            catalog.resolve("web_search").unwrap().declared_name,
            "web_search_20250305"
        );
    }
}
