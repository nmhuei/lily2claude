//! Bounded, stateful JSON recovery.
//!
//! Tier 1 of the schema-driven tool engine.  Strict `serde_json` parsing is
//! attempted first.  Only when that fails do we run a deterministic scanner
//! that repairs the small set of mistakes local models commonly make.  The
//! scanner never evaluates code and never recurses without a hard depth cap.

use serde_json::Value;
use std::fmt;

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_NESTING_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    EmptyInput,
    InputTooLarge,
    NestingTooDeep,
    Syntax(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::EmptyInput => write!(f, "empty JSON input"),
            ParseError::InputTooLarge => write!(f, "JSON input exceeds 1 MiB limit"),
            ParseError::NestingTooDeep => write!(f, "JSON nesting exceeds depth limit"),
            ParseError::Syntax(msg) => write!(f, "invalid JSON: {}", msg),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    Valid(Value),
    Recovered(Value, String),
    Invalid(ParseError),
}

impl ParseResult {
    pub fn value(&self) -> Option<&Value> {
        match self {
            ParseResult::Valid(v) | ParseResult::Recovered(v, _) => Some(v),
            ParseResult::Invalid(_) => None,
        }
    }

    pub fn unwrap_value(self) -> Value {
        match self {
            ParseResult::Valid(v) | ParseResult::Recovered(v, _) => v,
            ParseResult::Invalid(err) => panic!("parse_resilient_json failed: {}", err),
        }
    }

    pub fn strategy(&self) -> Option<&str> {
        match self {
            ParseResult::Recovered(_, strategy) => Some(strategy.as_str()),
            _ => None,
        }
    }

    pub fn is_valid(&self) -> bool {
        matches!(self, ParseResult::Valid(_))
    }

    pub fn into_result(self) -> Result<Value, ParseError> {
        match self {
            ParseResult::Valid(v) | ParseResult::Recovered(v, _) => Ok(v),
            ParseResult::Invalid(err) => Err(err),
        }
    }
}

/// Parse tool arguments, applying bounded recovery on failure.
pub fn parse_resilient_json(input: &str) -> ParseResult {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return ParseResult::Invalid(ParseError::EmptyInput);
    }
    if trimmed.len() > MAX_INPUT_BYTES {
        return ParseResult::Invalid(ParseError::InputTooLarge);
    }
    if exceeds_nesting_limit(trimmed, MAX_NESTING_DEPTH) {
        return ParseResult::Invalid(ParseError::NestingTooDeep);
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return ParseResult::Valid(value);
    }

    let candidate = strip_code_fence(trimmed);
    if let Ok(value) = serde_json::from_str::<Value>(candidate) {
        return ParseResult::Recovered(value, "stripped_code_fence".to_string());
    }

    match recover_jsonish(candidate) {
        Ok((repaired, strategies)) => match serde_json::from_str::<Value>(&repaired) {
            Ok(value) => ParseResult::Recovered(value, strategies.join("+")),
            Err(err) => ParseResult::Invalid(ParseError::Syntax(err.to_string())),
        },
        Err(err) => ParseResult::Invalid(err),
    }
}

fn exceeds_nesting_limit(input: &str, limit: usize) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    for c in input.chars() {
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            '}' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

fn strip_code_fence(input: &str) -> &str {
    if !input.starts_with("```") {
        return input;
    }
    let body = match input.find('\n') {
        Some(idx) => &input[idx + 1..],
        None => return input,
    };
    if let Some(end) = body.rfind("```") {
        body[..end].trim()
    } else {
        body.trim()
    }
}

fn recover_jsonish(input: &str) -> Result<(String, Vec<String>), ParseError> {
    let mut out = String::with_capacity(input.len() + 32);
    let mut stack: Vec<char> = Vec::new();
    let mut in_string: Option<char> = None;
    let mut escape = false;
    let mut strategies: Vec<String> = Vec::new();
    let mut pending_ident = String::new();
    let mut prev_significant: Option<char> = None;

    let chars: Vec<char> = input.chars().collect();
    let mut idx = 0usize;

    while idx < chars.len() {
        let c = chars[idx];

        if let Some(delim) = in_string {
            if escape {
                escape = false;
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '/' => out.push_str("\\/"),
                    'b' | 'f' | 'n' | 'r' | 't' => {
                        out.push('\\');
                        out.push(c);
                    }
                    'u' => {
                        let mut valid = idx + 4 < chars.len();
                        if valid {
                            for off in 1..=4 {
                                if !chars[idx + off].is_ascii_hexdigit() {
                                    valid = false;
                                    break;
                                }
                            }
                        }
                        if valid {
                            out.push_str("\\u");
                            for off in 1..=4 {
                                out.push(chars[idx + off]);
                            }
                            idx += 4;
                        } else {
                            // Keep an invalid \u sequence as a literal backslash
                            // followed by 'u', rather than corrupting the text.
                            out.push_str("\\\\u");
                            push_strategy(&mut strategies, "invalid_escape");
                        }
                    }
                    '\'' => {
                        // \' is not legal JSON. Inside our output string a
                        // literal apostrophe is safe.
                        out.push('\'');
                        push_strategy(&mut strategies, "single_quote_escape");
                    }
                    other => {
                        // \d, \., \$ and friends are common in shell/regex
                        // commands. Double the backslash so the parsed value
                        // contains the original one-character escape.
                        out.push_str("\\\\");
                        out.push(other);
                        push_strategy(&mut strategies, "invalid_escape");
                    }
                }
                idx += 1;
                continue;
            }

            if c == '\\' {
                escape = true;
                idx += 1;
                continue;
            }

            if c == delim {
                out.push('"');
                in_string = None;
                prev_significant = Some('"');
                idx += 1;
                continue;
            }

            // A literal double quote inside a single-quoted string must be
            // escaped in the converted JSON string.
            if delim == '\'' && c == '"' {
                out.push_str("\\\"");
                idx += 1;
                continue;
            }

            match c {
                '\n' => {
                    out.push_str("\\n");
                    push_strategy(&mut strategies, "escaped_control_char");
                }
                '\r' => {
                    out.push_str("\\r");
                    push_strategy(&mut strategies, "escaped_control_char");
                }
                '\t' => {
                    out.push_str("\\t");
                    push_strategy(&mut strategies, "escaped_control_char");
                }
                c if c.is_control() => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                    push_strategy(&mut strategies, "escaped_control_char");
                }
                _ => out.push(c),
            }
            idx += 1;
            continue;
        }

        // Outside a string.
        match c {
            '"' | '\'' => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                in_string = Some(c);
                out.push('"');
                if c == '\'' {
                    push_strategy(&mut strategies, "single_quotes");
                }
                idx += 1;
            }
            '{' => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                stack.push('}');
                if stack.len() > MAX_NESTING_DEPTH {
                    return Err(ParseError::NestingTooDeep);
                }
                out.push('{');
                prev_significant = Some('{');
                idx += 1;
            }
            '[' => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                stack.push(']');
                if stack.len() > MAX_NESTING_DEPTH {
                    return Err(ParseError::NestingTooDeep);
                }
                out.push('[');
                prev_significant = Some('[');
                idx += 1;
            }
            '}' | ']' => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                while out.ends_with(' ')
                    || out.ends_with('\t')
                    || out.ends_with('\n')
                    || out.ends_with('\r')
                {
                    out.pop();
                }
                if out.ends_with(',') {
                    out.pop();
                    push_strategy(&mut strategies, "trailing_comma");
                }

                let wanted = c;
                let mut matched = false;
                while let Some(top) = stack.pop() {
                    if top == wanted {
                        matched = true;
                        break;
                    }
                    // Auto-close an inner container that the model forgot.
                    out.push(top);
                    push_strategy(&mut strategies, "closed_container");
                }
                if matched {
                    out.push(wanted);
                    prev_significant = Some(wanted);
                }
                idx += 1;
            }
            ',' => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                out.push(',');
                prev_significant = Some(',');
                idx += 1;
            }
            ':' => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                out.push(':');
                prev_significant = Some(':');
                idx += 1;
            }
            c if c.is_whitespace() => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                out.push(c);
                idx += 1;
            }
            c if is_ident_start(c) => {
                // Accumulate the whole identifier.  Delimiter branches flush
                // it once, which is what lets `command:` be recognised as a
                // key rather than being emitted one character at a time.
                pending_ident.push(c);
                idx += 1;
            }
            _ => {
                flush_ident(
                    &mut out,
                    &mut pending_ident,
                    &mut prev_significant,
                    &mut strategies,
                );
                out.push(c);
                prev_significant = Some(c);
                idx += 1;
            }
        }
    }

    flush_ident(
        &mut out,
        &mut pending_ident,
        &mut prev_significant,
        &mut strategies,
    );

    if escape {
        out.push('\\');
        push_strategy(&mut strategies, "trailing_backslash");
    }
    if in_string.is_some() {
        out.push('"');
        push_strategy(&mut strategies, "closed_string");
    }

    while out.ends_with(' ') || out.ends_with('\t') || out.ends_with('\n') || out.ends_with('\r') {
        out.pop();
    }
    if out.ends_with(',') {
        out.pop();
        push_strategy(&mut strategies, "trailing_comma");
    }

    while let Some(closing) = stack.pop() {
        out.push(closing);
        push_strategy(&mut strategies, "closed_container");
    }

    if strategies.is_empty() {
        strategies.push("normalized_json".to_string());
    }

    Ok((out, strategies))
}

fn flush_ident(
    out: &mut String,
    ident: &mut String,
    prev_significant: &mut Option<char>,
    strategies: &mut Vec<String>,
) {
    if ident.is_empty() {
        return;
    }

    let token = std::mem::take(ident);
    let previous = *prev_significant;
    let next_nonspace = out.chars().rev().find(|c| !c.is_whitespace());

    // Unquoted object keys: `{ command: "ls" }` or `, timeout: 5`.
    if previous == Some('{')
        || previous == Some(',')
        || next_nonspace == Some('{')
        || next_nonspace == Some(',')
    {
        // We can only know it is a key by looking ahead; the caller has
        // already consumed the token. The colon is processed next, so quote
        // identifiers that appear where a key can start.
        out.push('"');
        out.push_str(&token);
        out.push('"');
        push_strategy(strategies, "unquoted_key");
        *prev_significant = Some('"');
        return;
    }

    match token.as_str() {
        "True" => {
            out.push_str("true");
            push_strategy(strategies, "python_literal");
        }
        "False" => {
            out.push_str("false");
            push_strategy(strategies, "python_literal");
        }
        "None" => {
            out.push_str("null");
            push_strategy(strategies, "python_literal");
        }
        _ => out.push_str(&token),
    }
    *prev_significant = Some('0');
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn push_strategy(strategies: &mut Vec<String>, name: &str) {
    if !strategies.iter().any(|s| s == name) {
        strategies.push(name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_json_is_valid() {
        assert_eq!(
            parse_resilient_json(r#"{"command":"pwd"}"#).unwrap_value(),
            json!({"command": "pwd"})
        );
    }

    #[test]
    fn unclosed_brace_and_string() {
        let res = parse_resilient_json(r#"{"command": "git status""#);
        assert_eq!(res.unwrap_value()["command"], "git status");
    }

    #[test]
    fn unescaped_newlines() {
        let multiline = "{\"content\": \"line1\nline2\"}";
        let res = parse_resilient_json(multiline);
        assert_eq!(res.unwrap_value()["content"], "line1\nline2");
    }

    #[test]
    fn python_dict_and_shell_quotes() {
        let script = "{'command': 'echo \"hello world\"'}";
        let res = parse_resilient_json(script);
        assert_eq!(res.unwrap_value()["command"], "echo \"hello world\"");
    }

    #[test]
    fn python_literals_and_unquoted_keys() {
        let res = parse_resilient_json("{command: \"ls -la\", replace_all: True}");
        let v = res.unwrap_value();
        assert_eq!(v["command"], "ls -la");
        assert_eq!(v["replace_all"], true);
    }

    #[test]
    fn invalid_regex_escapes_are_doubled() {
        let res = parse_resilient_json(r#"{"command": "grep -P '\d+' file.txt"}"#);
        assert_eq!(res.unwrap_value()["command"], r#"grep -P '\d+' file.txt"#);

        let res2 = parse_resilient_json(r#"{"command": "grep 'foo\.bar\$' file.txt"}"#);
        assert_eq!(
            res2.unwrap_value()["command"],
            r#"grep 'foo\.bar\$' file.txt"#
        );
    }

    #[test]
    fn trailing_comma_and_nested_containers() {
        let res = parse_resilient_json(r#"{"a": [1,2,], "b": {"c": 3,},}"#);
        assert_eq!(res.unwrap_value(), json!({"a": [1,2], "b": {"c": 3}}));
    }

    #[test]
    fn too_deep_is_rejected() {
        let deep = "[".repeat(20) + &"]".repeat(20);
        assert!(matches!(
            parse_resilient_json(&deep),
            ParseResult::Invalid(ParseError::NestingTooDeep)
        ));
    }
}
