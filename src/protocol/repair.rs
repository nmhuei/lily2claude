use serde_json::Value;

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
