use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::LazyLock;

/// Self-repair common malformed JSON issues in tool call arguments
pub fn repair_tool_arguments(raw: &str) -> String {
    let mut candidate = raw.trim().to_string();

    // Strip markdown code fences if model wrapped json in ```json ... ```
    if candidate.starts_with("```") {
        if let Some(first_nl) = candidate.find('\n') {
            candidate = candidate[first_nl + 1..].to_string();
        }
        if let Some(last_fence) = candidate.rfind("```") {
            candidate = candidate[..last_fence].trim().to_string();
        }
    }

    if serde_json::from_str::<Value>(&candidate).is_ok() {
        return candidate;
    }

    // Fix unclosed quote if truncated inside string
    let quote_count = candidate.chars().filter(|c| *c == '"').count();
    if quote_count % 2 == 1 {
        candidate.push('"');
    }

    // Fix trailing comma in objects
    if candidate.contains(",}") {
        candidate = candidate.replace(",}", "}");
    }
    // Fix trailing comma in arrays
    if candidate.contains(",]") {
        candidate = candidate.replace(",]", "]");
    }

    // Balance unclosed braces
    let open_braces = candidate.chars().filter(|c| *c == '{').count();
    let close_braces = candidate.chars().filter(|c| *c == '}').count();
    if open_braces > close_braces {
        for _ in 0..(open_braces - close_braces) {
            candidate.push('}');
        }
    }

    // Balance unclosed brackets
    let open_brackets = candidate.chars().filter(|c| *c == '[').count();
    let close_brackets = candidate.chars().filter(|c| *c == ']').count();
    if open_brackets > close_brackets {
        for _ in 0..(open_brackets - close_brackets) {
            candidate.push(']');
        }
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

static RE_FUNCTION_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<function_call(?:\s+name=["']([^"']+)["'])?>\s*(.*?)\s*</function_call>"#).unwrap());

static RE_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<parameter\s+name=["']([^"']+)["']\s*>(.*?)</parameter>"#).unwrap());

static RE_BASH_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)```(?:bash|sh|shell|zsh)\s*\n([\s\S]*?)\n```").unwrap());

pub struct ToolCallRepairEngine;

impl ToolCallRepairEngine {
    /// Strips any raw XML tool call tags or unclosed tool markers from text intended for user display.
    pub fn clean_user_visible_text(raw: &str) -> String {
        let mut text = raw.to_string();
        text = RE_TOOL_CALL.replace_all(&text, "").to_string();
        text = RE_INVOKE.replace_all(&text, "").to_string();
        text = RE_FUNCTION_CALL.replace_all(&text, "").to_string();

        // Only strip unclosed opening tags if there is no closing tag
        for tag in ["<tool_call", "<invoke", "<function_call"] {
            if let Some(pos) = text.rfind(tag) {
                let close_tag = match tag {
                    "<tool_call" => "</tool_call>",
                    "<invoke" => "</invoke>",
                    "<function_call" => "</function_call>",
                    _ => "",
                };
                if !close_tag.is_empty() && !text[pos..].contains(close_tag) {
                    text.truncate(pos);
                }
            }
        }

        text.replace("</tool_call>", "")
            .replace("</invoke>", "")
            .replace("</function_call>", "")
            .trim()
            .to_string()
    }

    pub fn get_streamable_text_end(s: &str) -> usize {
        let tool_markers = [
            "<tool_call",
            "<function_call",
            "<invoke",
            "<tool>",
            "<action",
            "```bash",
            "```sh",
            "```shell",
            "```zsh",
            "```json",
        ];
        let mut earliest = s.len();
        for marker in tool_markers {
            if let Some(pos) = s.find(marker) {
                if pos < earliest {
                    earliest = pos;
                }
            }
        }

        if earliest < s.len() {
            return earliest;
        }

        let partial_prefixes = [
            "<tool_call", "<tool_cal", "<tool_ca", "<tool_c", "<tool_", "<tool", "<too", "<to", "<t", "<",
            "<function_call", "<function_cal", "<function_ca", "<function_c", "<function_", "<function", "<functio", "<functi", "<funct", "<func", "<fun", "<fu", "<f",
            "<invoke", "<invok", "<invo", "<inv", "<in", "<i",
            "<action", "<actio", "<acti", "<act", "<ac", "<a",
            "```bash", "```bas", "```ba", "```b",
            "```shell", "```shel", "```she", "```sh", "```s",
            "```zsh", "```zs", "```z",
            "```json", "```jso", "```js", "```j",
            "```", "``", "`",
        ];

        for p in partial_prefixes {
            if s.ends_with(p) {
                return s.len() - p.len();
            }
        }

        s.len()
    }

    pub fn map_tool_name(raw_name: &str, casing_map: &HashMap<String, String>) -> String {
        let lower = raw_name.to_ascii_lowercase();
        match lower.as_str() {
            "read" | "fileread" | "readfile" | "file_read" | "view" | "cat" | "show" | "view_file" => {
                if let Some(r) = casing_map.get("read") {
                    r.clone()
                } else if let Some(v) = casing_map.get("view") {
                    v.clone()
                } else {
                    "Read".to_string()
                }
            }
            "edit" | "fileedit" | "editfile" | "file_edit" | "modify" | "patch" => {
                casing_map.get("edit").cloned().unwrap_or_else(|| "Edit".to_string())
            }
            "write" | "filewrite" | "file_write" | "createfile" | "create_file" | "create" | "save" => {
                casing_map.get("write").cloned().unwrap_or_else(|| "Write".to_string())
            }
            "bash" | "execute_bash" | "execute" | "sh" | "terminal" | "shell" | "run" | "exec" | "command" | "cmd" => {
                casing_map.get("bash").cloned().unwrap_or_else(|| "Bash".to_string())
            }
            "glob" | "globtool" | "find_files" | "find" | "filesearch" | "file_search" => {
                if let Some(g) = casing_map.get("glob") {
                    g.clone()
                } else if let Some(gt) = casing_map.get("globtool") {
                    gt.clone()
                } else {
                    casing_map.get("bash").cloned().unwrap_or_else(|| "Bash".to_string())
                }
            }
            "grep" | "greptool" | "search_files" | "search" | "content_search" => {
                if let Some(g) = casing_map.get("grep") {
                    g.clone()
                } else if let Some(gt) = casing_map.get("greptool") {
                    gt.clone()
                } else {
                    casing_map.get("bash").cloned().unwrap_or_else(|| "Bash".to_string())
                }
            }
            "ls" | "dir" | "list_directory" | "list_dir" | "list_files" | "listdirectory" => {
                if let Some(l) = casing_map.get("ls") {
                    l.clone()
                } else {
                    casing_map.get("bash").cloned().unwrap_or_else(|| "Bash".to_string())
                }
            }
            "web_search" | "websearch" => {
                casing_map.get("websearch").cloned().unwrap_or_else(|| "WebSearch".to_string())
            }
            "web_fetch" | "webfetch" | "fetch" | "curl" => {
                casing_map.get("webfetch").cloned().unwrap_or_else(|| "WebFetch".to_string())
            }
            "notebook_edit" | "notebookedit" => {
                casing_map.get("notebookedit").cloned().unwrap_or_else(|| "NotebookEdit".to_string())
            }
            "cron_create" | "croncreate" => {
                casing_map.get("croncreate").cloned().unwrap_or_else(|| "CronCreate".to_string())
            }
            "cron_delete" | "crondelete" => {
                casing_map.get("crondelete").cloned().unwrap_or_else(|| "CronDelete".to_string())
            }
            "cron_list" | "cronlist" => {
                casing_map.get("cronlist").cloned().unwrap_or_else(|| "CronList".to_string())
            }
            "send_message" | "sendmessage" => {
                casing_map.get("sendmessage").cloned().unwrap_or_else(|| "SendMessage".to_string())
            }
            "skill" | "call_skill" | "load_skill" | "run_skill" => {
                casing_map.get("skill").cloned().unwrap_or_else(|| "Skill".to_string())
            }
            "workflow" | "run_workflow" => {
                casing_map.get("workflow").cloned().unwrap_or_else(|| "Workflow".to_string())
            }
            "other" => {
                "other".to_string()
            }
            other => {
                if let Some(cased) = casing_map.get(other) {
                    cased.clone()
                } else if other.starts_with("git") || other.starts_with("cargo") || other.starts_with("npm") {
                    casing_map.get("bash").cloned().unwrap_or_else(|| "Bash".to_string())
                } else {
                    raw_name.to_string()
                }
            }
        }
    }

    pub fn normalize_arguments(tool_name: &str, args: Value) -> Value {
        Self::normalize_arguments_with_raw(tool_name, tool_name, args)
    }

    pub fn normalize_arguments_with_raw(target_name: &str, raw_name: &str, mut args: Value) -> Value {
        if let Some(obj) = args.as_object_mut() {
            let lower_target = target_name.to_ascii_lowercase();
            let lower_raw = raw_name.to_ascii_lowercase();

            if lower_target == "bash" {
                // If synthesized from ls/dir
                if lower_raw == "ls" || lower_raw == "dir" || lower_raw == "list_directory" || lower_raw == "list_dir" || lower_raw == "list_files" || lower_raw == "listdirectory" {
                    let path = obj.remove("path")
                        .or_else(|| obj.remove("directory"))
                        .or_else(|| obj.remove("dir"))
                        .or_else(|| obj.remove("target"))
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| ".".to_string());
                    obj.insert("command".to_string(), Value::String(format!("ls -la {}", path)));
                } else if lower_raw == "grep" || lower_raw == "greptool" || lower_raw == "search_files" || lower_raw == "content_search" {
                    let pat = obj.remove("pattern")
                        .or_else(|| obj.remove("query"))
                        .or_else(|| obj.remove("regex"))
                        .or_else(|| obj.remove("search"))
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    let path = obj.remove("path")
                        .or_else(|| obj.remove("directory"))
                        .or_else(|| obj.remove("dir"))
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| ".".to_string());
                    obj.insert("command".to_string(), Value::String(format!("grep -rn '{}' {}", pat, path)));
                } else if lower_raw == "glob" || lower_raw == "globtool" || lower_raw == "find_files" || lower_raw == "find" || lower_raw == "filesearch" || lower_raw == "file_search" {
                    let pat = obj.remove("pattern")
                        .or_else(|| obj.remove("glob"))
                        .or_else(|| obj.remove("file_pattern"))
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "*".to_string());
                    let path = obj.remove("path")
                        .or_else(|| obj.remove("directory"))
                        .or_else(|| obj.remove("dir"))
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| ".".to_string());
                    obj.insert("command".to_string(), Value::String(format!("find {} -name '{}'", path, pat)));
                } else if lower_raw.starts_with("git") || lower_raw.starts_with("cargo") || lower_raw.starts_with("npm") {
                    if !obj.contains_key("command") {
                        let subcmd = lower_raw.replace('_', " ");
                        obj.insert("command".to_string(), Value::String(subcmd));
                    }
                }

                // If command is an array, join into string
                if let Some(arr) = obj.get("command").and_then(|v| v.as_array()) {
                    let joined = arr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ");
                    obj.insert("command".to_string(), Value::String(joined));
                }

                // Handle cmd, code, script, run, exec aliases
                if !obj.contains_key("command") {
                    if let Some(cmd) = obj.remove("cmd").or_else(|| obj.remove("code")).or_else(|| obj.remove("script")).or_else(|| obj.remove("run")).or_else(|| obj.remove("exec")) {
                        obj.insert("command".to_string(), cmd);
                    }
                }

                // If command is present and args/arguments array is present, append them
                if let Some(cmd_val) = obj.get("command").and_then(|v| v.as_str().map(|s| s.to_string())) {
                    if let Some(args_arr) = obj.remove("args").or_else(|| obj.remove("arguments")).and_then(|v| v.as_array().cloned()) {
                        let args_str = args_arr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ");
                        if !args_str.is_empty() {
                            obj.insert("command".to_string(), Value::String(format!("{} {}", cmd_val, args_str)));
                        }
                    }
                }

                // Fallback if command is still missing but path is present (infer ls)
                if !obj.contains_key("command") {
                    if let Some(path) = obj.remove("path").or_else(|| obj.remove("directory")) {
                        let path_str = path.as_str().unwrap_or(".");
                        obj.insert("command".to_string(), Value::String(format!("ls -la {}", path_str)));
                    }
                }

                // Timeout string normalization
                if let Some(to_val) = obj.get("timeout") {
                    if let Some(s) = to_val.as_str() {
                        if let Ok(num) = s.parse::<u64>() {
                            obj.insert("timeout".to_string(), json!(num));
                        }
                    }
                }

                // Strictly retain only valid Claude Code Bash parameters: command, description, timeout
                // Note: Never prepend cd before git because Claude Code security sandbox explicitly denies it.
                // Claude Code CLI is already running in the workspace directory.
                obj.retain(|k, _| k == "command" || k == "description" || k == "timeout");
            } else if lower_target == "read" || lower_target == "view" {
                let target_param = if lower_target == "view" { "path" } else { "file_path" };
                let alt_param = if lower_target == "view" { "file_path" } else { "path" };

                if let Some(val) = obj.remove(alt_param)
                    .or_else(|| obj.remove("file"))
                    .or_else(|| obj.remove("filepath"))
                    .or_else(|| obj.remove("filename"))
                    .or_else(|| obj.remove("uri"))
                    .or_else(|| obj.remove("target"))
                    .or_else(|| obj.remove("path_to_file"))
                    .or_else(|| obj.remove("source_file")) {
                    if !obj.contains_key(target_param) {
                        obj.insert(target_param.to_string(), val);
                    }
                }

                // Handle view_range: [start, end]
                if let Some(vr) = obj.remove("view_range").and_then(|v| v.as_array().cloned()) {
                    if vr.len() >= 2 {
                        if let (Some(start), Some(end)) = (vr[0].as_u64(), vr[1].as_u64()) {
                            obj.insert("offset".to_string(), json!(start));
                            obj.insert("limit".to_string(), json!(end.saturating_sub(start) + 1));
                        }
                    }
                }

                // Handle line/start_line to offset
                if !obj.contains_key("offset") {
                    if let Some(l) = obj.remove("start_line").or_else(|| obj.remove("line")).or_else(|| obj.remove("from")) {
                        if let Some(s) = l.as_str() {
                            if let Ok(num) = s.parse::<u64>() {
                                obj.insert("offset".to_string(), json!(num));
                            }
                        } else if l.is_number() {
                            obj.insert("offset".to_string(), l);
                        }
                    }
                }

                // Handle end_line / to
                if let Some(el) = obj.remove("end_line").or_else(|| obj.remove("to")) {
                    if !obj.contains_key("limit") {
                        let end_num = if let Some(s) = el.as_str() {
                            s.parse::<u64>().ok()
                        } else {
                            el.as_u64()
                        };
                        if let Some(end) = end_num {
                            let offset = obj.get("offset").and_then(|v| v.as_u64()).unwrap_or(1);
                            obj.insert("limit".to_string(), json!(end.saturating_sub(offset) + 1));
                        }
                    }
                }

                // Handle limit / num_lines / count / lines
                if !obj.contains_key("limit") {
                    if let Some(c) = obj.remove("num_lines").or_else(|| obj.remove("count")).or_else(|| obj.remove("lines")).or_else(|| obj.remove("length")) {
                        if let Some(s) = c.as_str() {
                            if let Ok(num) = s.parse::<u64>() {
                                obj.insert("limit".to_string(), json!(num));
                            }
                        } else if c.is_number() {
                            obj.insert("limit".to_string(), c);
                        }
                    }
                }

                // If offset is a string number, parse to integer
                if let Some(off_val) = obj.get("offset") {
                    if let Some(s) = off_val.as_str() {
                        if let Ok(num) = s.parse::<u64>() {
                            obj.insert("offset".to_string(), json!(num));
                        }
                    }
                }

                // Strictly retain only valid Claude Code Read parameters: file_path, offset, limit
                obj.retain(|k, _| k == "file_path" || k == "offset" || k == "limit");
            } else if lower_target == "edit" {
                if let Some(val) = obj.remove("path")
                    .or_else(|| obj.remove("file"))
                    .or_else(|| obj.remove("filepath"))
                    .or_else(|| obj.remove("filename"))
                    .or_else(|| obj.remove("target"))
                    .or_else(|| obj.remove("path_to_file")) {
                    if !obj.contains_key("file_path") {
                        obj.insert("file_path".to_string(), val);
                    }
                }
                if let Some(ot) = obj.remove("old_text")
                    .or_else(|| obj.remove("original_text"))
                    .or_else(|| obj.remove("search"))
                    .or_else(|| obj.remove("before"))
                    .or_else(|| obj.remove("replace"))
                    .or_else(|| obj.remove("target_string"))
                    .or_else(|| obj.remove("find")) {
                    if !obj.contains_key("old_string") {
                        obj.insert("old_string".to_string(), ot);
                    }
                }
                if let Some(nt) = obj.remove("new_text")
                    .or_else(|| obj.remove("replacement"))
                    .or_else(|| obj.remove("after"))
                    .or_else(|| obj.remove("to"))
                    .or_else(|| obj.remove("replacement_string"))
                    .or_else(|| obj.remove("substitute"))
                    .or_else(|| obj.remove("replace_with")) {
                    if !obj.contains_key("new_string") {
                        obj.insert("new_string".to_string(), nt);
                    }
                }
                if let Some(ra) = obj.get("replace_all") {
                    if let Some(s) = ra.as_str() {
                        let b = s.eq_ignore_ascii_case("true");
                        obj.insert("replace_all".to_string(), json!(b));
                    }
                }

                // Strictly retain only valid Claude Code Edit parameters: file_path, old_string, new_string, replace_all
                obj.retain(|k, _| k == "file_path" || k == "old_string" || k == "new_string" || k == "replace_all");
            } else if lower_target == "write" {
                if let Some(val) = obj.remove("path")
                    .or_else(|| obj.remove("file"))
                    .or_else(|| obj.remove("filepath"))
                    .or_else(|| obj.remove("filename"))
                    .or_else(|| obj.remove("target"))
                    .or_else(|| obj.remove("path_to_file")) {
                    if !obj.contains_key("file_path") {
                        obj.insert("file_path".to_string(), val);
                    }
                }
                if let Some(t) = obj.remove("text")
                    .or_else(|| obj.remove("code"))
                    .or_else(|| obj.remove("contents"))
                    .or_else(|| obj.remove("data"))
                    .or_else(|| obj.remove("body"))
                    .or_else(|| obj.remove("file_content"))
                    .or_else(|| obj.remove("new_content")) {
                    if !obj.contains_key("content") {
                        obj.insert("content".to_string(), t);
                    }
                }

                // Strictly retain only valid Claude Code Write parameters: file_path, content
                obj.retain(|k, _| k == "file_path" || k == "content");
            } else if lower_target == "websearch" {
                if let Some(q) = obj.remove("q")
                    .or_else(|| obj.remove("search_query"))
                    .or_else(|| obj.remove("term"))
                    .or_else(|| obj.remove("keyword"))
                    .or_else(|| obj.remove("query_string")) {
                    if !obj.contains_key("query") {
                        obj.insert("query".to_string(), q);
                    }
                }
            } else if lower_target == "webfetch" {
                if let Some(u) = obj.remove("uri")
                    .or_else(|| obj.remove("link"))
                    .or_else(|| obj.remove("address"))
                    .or_else(|| obj.remove("target_url"))
                    .or_else(|| obj.remove("endpoint")) {
                    if !obj.contains_key("url") {
                        obj.insert("url".to_string(), u);
                    }
                }
                if !obj.contains_key("prompt") {
                    obj.insert("prompt".to_string(), json!("Fetch and return content"));
                }
            } else if lower_target == "notebookedit" {
                if let Some(p) = obj.remove("path")
                    .or_else(|| obj.remove("file"))
                    .or_else(|| obj.remove("file_path"))
                    .or_else(|| obj.remove("notebook")) {
                    if !obj.contains_key("notebook_path") {
                        obj.insert("notebook_path".to_string(), p);
                    }
                }
                if let Some(s) = obj.remove("source")
                    .or_else(|| obj.remove("code"))
                    .or_else(|| obj.remove("content"))
                    .or_else(|| obj.remove("text")) {
                    if !obj.contains_key("new_source") {
                        obj.insert("new_source".to_string(), s);
                    }
                }
            } else if lower_target == "croncreate" {
                if let Some(c) = obj.remove("schedule")
                    .or_else(|| obj.remove("cron_expression"))
                    .or_else(|| obj.remove("expression")) {
                    if !obj.contains_key("cron") {
                        obj.insert("cron".to_string(), c);
                    }
                }
                if let Some(p) = obj.remove("message")
                    .or_else(|| obj.remove("instruction"))
                    .or_else(|| obj.remove("task")) {
                    if !obj.contains_key("prompt") {
                        obj.insert("prompt".to_string(), p);
                    }
                }
            } else if lower_target == "sendmessage" {
                if let Some(t) = obj.remove("recipient")
                    .or_else(|| obj.remove("agent"))
                    .or_else(|| obj.remove("target")) {
                    if !obj.contains_key("to") {
                        obj.insert("to".to_string(), t);
                    }
                }
                if let Some(m) = obj.remove("content")
                    .or_else(|| obj.remove("text"))
                    .or_else(|| obj.remove("body")) {
                    if !obj.contains_key("message") {
                        obj.insert("message".to_string(), m);
                    }
                }
            } else if lower_target == "skill" {
                if let Some(s) = obj.remove("name").or_else(|| obj.remove("skill_name")).or_else(|| obj.remove("skill")) {
                    obj.insert("skill".to_string(), s);
                }
                obj.remove("arguments");
                obj.remove("args");
            }
        }
        args
    }

    pub fn sanitize_and_validate(mut mapped_name: String, mut norm_args: Value, casing_map: &HashMap<String, String>) -> Option<RepairedToolCall> {
        let lower = mapped_name.to_ascii_lowercase();
        if lower == "read" || lower == "view" {
            if let Some(path_str) = norm_args.get("file_path").or_else(|| norm_args.get("path")).and_then(|v| v.as_str()) {
                if path_str.trim().is_empty() {
                    return None;
                }
                // Check if target is a directory - if so, convert to Bash ls -la to avoid EISDIR error
                let p = std::path::Path::new(path_str);
                if p.is_dir() {
                    mapped_name = casing_map.get("bash").cloned().unwrap_or_else(|| "Bash".to_string());
                    norm_args = serde_json::json!({
                        "command": format!("ls -la {}", path_str)
                    });
                }
            } else {
                return None;
            }
        } else if lower == "bash" {
            let has_cmd = norm_args.get("command").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false);
            if !has_cmd {
                return None;
            }
        } else if lower == "write" {
            let has_path = norm_args.get("file_path").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false);
            if !has_path {
                return None;
            }
        } else if lower == "edit" {
            let has_path = norm_args.get("file_path").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false);
            let has_old = norm_args.get("old_string").and_then(|v| v.as_str()).is_some();
            let has_new = norm_args.get("new_string").and_then(|v| v.as_str()).is_some();
            if !has_path || !has_old || !has_new {
                return None;
            }
        } else if lower == "skill" {
            let has_skill = norm_args.get("skill").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false);
            if !has_skill {
                return None;
            }
        } else if lower == "other" {
            // Check if this was a Skill invocation (e.g. OpenCode ACP sends kind="other" for skills)
            if let Some(skill_name) = norm_args.get("skill").or_else(|| norm_args.get("name")).and_then(|v| v.as_str()) {
                if !skill_name.trim().is_empty() && casing_map.contains_key("skill") {
                    mapped_name = casing_map.get("skill").cloned().unwrap_or_else(|| "Skill".to_string());
                    norm_args = serde_json::json!({
                        "skill": skill_name
                    });
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }

        // Drop any tool not declared by Claude Code to prevent "No such tool available" fatal error
        if !casing_map.is_empty() && !casing_map.contains_key(&mapped_name.to_ascii_lowercase()) {
            return None;
        }

        Some(RepairedToolCall {
            name: mapped_name,
            arguments: norm_args,
        })
    }

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
                    let mapped_name = Self::map_tool_name(raw_name, casing_map);

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

                    let norm_args = Self::normalize_arguments_with_raw(&mapped_name, raw_name, args_val);

                    if let Some(call) = Self::sanitize_and_validate(mapped_name, norm_args, casing_map) {
                        results.push(call);
                    }
                }
            }
        }

        // If standard regex found nothing, check for unclosed <tool_call> at end of text
        if results.is_empty() {
            if let Some(pos) = text.rfind("<tool_call>") {
                let after = &text[pos + 11..];
                let repaired_body = repair_tool_arguments(after);
                if let Ok(val) = serde_json::from_str::<Value>(&repaired_body) {
                    if let Some(raw_name) = val.get("name").and_then(|v| v.as_str()) {
                        let mapped_name = Self::map_tool_name(raw_name, casing_map);
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
                        let norm_args = Self::normalize_arguments_with_raw(&mapped_name, raw_name, args_val);
                        if let Some(call) = Self::sanitize_and_validate(mapped_name, norm_args, casing_map) {
                            results.push(call);
                        }
                    }
                }
            }
        }

        if !results.is_empty() {
            return results;
        }

        // 2. Detect <function_call name="..."> ... </function_call>
        for cap in RE_FUNCTION_CALL.captures_iter(text) {
            let body = &cap[2];
            let repaired_body = repair_tool_arguments(body);
            if let Ok(val) = serde_json::from_str::<Value>(&repaired_body) {
                let raw_name = cap.get(1).map(|m| m.as_str()).or_else(|| val.get("name").and_then(|v| v.as_str())).unwrap_or("");
                if !raw_name.is_empty() {
                    let mapped_name = Self::map_tool_name(raw_name, casing_map);
                    let mut args_val = val.get("arguments").or_else(|| val.get("parameters")).cloned().unwrap_or(val.clone());
                    if let Some(s) = args_val.as_str() {
                        let rep_s = repair_tool_arguments(s);
                        if let Ok(inner) = serde_json::from_str::<Value>(&rep_s) {
                            args_val = inner;
                        }
                    }
                    let norm_args = Self::normalize_arguments_with_raw(&mapped_name, raw_name, args_val);
                    if let Some(call) = Self::sanitize_and_validate(mapped_name, norm_args, casing_map) {
                        results.push(call);
                    }
                }
            }
        }

        if !results.is_empty() {
            return results;
        }

        // 3. Detect <invoke name="..."> ... </invoke>
        for cap in RE_INVOKE.captures_iter(text) {
            let raw_name = &cap[1];
            let body = &cap[2];
            let mapped_name = Self::map_tool_name(raw_name, casing_map);

            let mut args_map = serde_json::Map::new();
            for pcap in RE_PARAM.captures_iter(body) {
                let pname = pcap[1].to_string();
                let pval = pcap[2].trim().to_string();
                args_map.insert(pname, Value::String(pval));
            }

            let norm_args = Self::normalize_arguments_with_raw(&mapped_name, raw_name, Value::Object(args_map));

            if let Some(call) = Self::sanitize_and_validate(mapped_name, norm_args, casing_map) {
                results.push(call);
            }
        }

        if !results.is_empty() {
            return results;
        }

        // 4. Detect markdown bash / sh blocks IF AND ONLY IF "bash" is declared by Claude Code
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
        let stripped = RE_FUNCTION_CALL.replace_all(&stripped, "");
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

    #[test]
    fn test_fileread_mapped_to_read_and_path_normalized() {
        let mut map = HashMap::new();
        map.insert("read".to_string(), "Read".to_string());

        let text = r#"<tool_call>
{"name": "FileRead", "arguments": {"path": "/home/user/project/file.txt"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[0].arguments["file_path"], "/home/user/project/file.txt");
    }

    #[test]
    fn test_bash_cwd_stripped_and_prepended() {
        let mut map = HashMap::new();
        map.insert("bash".to_string(), "Bash".to_string());

        let text = r#"<tool_call>
{"name": "Bash", "arguments": {"command": "cargo test", "cwd": "/home/user/project"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "cargo test");
        assert!(calls[0].arguments.get("cwd").is_none());
    }

    #[test]
    fn test_clean_user_visible_text_preserves_post_tool_text() {
        let text = "Rà soát cấu trúc repo:\n<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"ls\"}}\n</tool_call>\n\n## Sẵn sàng spawn subagent\n1. Chuẩn hóa README\n2. Hoàn thiện toàn diện";
        let cleaned = ToolCallRepairEngine::clean_user_visible_text(text);
        assert!(cleaned.contains("Rà soát cấu trúc repo:"));
        assert!(cleaned.contains("## Sẵn sàng spawn subagent"));
        assert!(cleaned.contains("1. Chuẩn hóa README"));
        assert!(!cleaned.contains("<tool_call>"));
        assert!(!cleaned.contains("</tool_call>"));
    }
}
