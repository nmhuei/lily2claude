//! Backward-compatible facade over [`crate::protocol::tool_engine`].
//!
//! The six historical modules (`casing`, `cleaner`, `extractor`, `json_args`,
//! `normalization`, `validation`) have been removed.  This module keeps the
//! public API used by older callers and tests, while production code uses the
//! schema-driven [`crate::protocol::tool_engine::ToolCallEngine`].
//!
//! The compatibility helpers intentionally contain a few legacy heuristics
//! (markdown bash fences, subagent prose).  They are only reachable through
//! [`ToolCallRepairEngine`], never through the production stream engine when a
//! real tool catalog is supplied.

use crate::protocol::tool_engine::json_parser::{parse_resilient_json, ParseResult};
use crate::protocol::tool_engine::tag_lexer::{
    scan_tool_tag_start, streamable_text_end, strip_tool_tags as strip_envelopes,
};
use crate::protocol::tool_engine::{ToolCallCandidate, ToolCallEngine, ValidatedToolCall};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairedToolCall {
    pub name: String,
    pub arguments: Value,
}

/// Parse tool arguments with bounded recovery and return a JSON string.
pub fn repair_tool_arguments(raw: &str) -> String {
    let trimmed = raw.trim();

    // Keep the author's whitespace when the only defect is a trailing comma.
    // This is both friendlier and preserves the literal argument string that
    // older callers assert on.
    let without_trailing = remove_trailing_commas(trimmed);
    if without_trailing != trimmed && serde_json::from_str::<Value>(&without_trailing).is_ok() {
        return without_trailing;
    }

    match parse_resilient_json(raw) {
        ParseResult::Valid(value) | ParseResult::Recovered(value, _) => value.to_string(),
        ParseResult::Invalid(_) => raw.to_string(),
    }
}

fn remove_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape = false;
    let mut idx = 0usize;
    while idx < chars.len() {
        let c = chars[idx];
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            idx += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            idx += 1;
            continue;
        }
        if c == ',' {
            let mut next = idx + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            if next < chars.len() && (chars[next] == '}' || chars[next] == ']') {
                idx += 1;
                continue;
            }
        }
        out.push(c);
        idx += 1;
    }
    out
}

/// Map increasingly creative model spellings to the declared tool casing.
pub fn map_tool_name(raw_name: &str, casing_map: &HashMap<String, String>) -> String {
    let lower = raw_name.to_ascii_lowercase();
    let pick = |keys: &[&str], fallback: &str| -> String {
        for key in keys {
            if let Some(value) = casing_map.get(*key) {
                return value.clone();
            }
        }
        fallback.to_string()
    };

    match lower.as_str() {
        "read" | "fileread" | "readfile" | "file_read" | "view" | "cat" | "show" | "view_file" => {
            pick(&["read", "fileread", "file_read", "view"], "Read")
        }
        "edit" | "fileedit" | "editfile" | "file_edit" | "modify" | "patch" => {
            pick(&["edit", "fileedit", "file_edit"], "Edit")
        }
        "write" | "filewrite" | "file_write" | "createfile" | "create_file" | "create" | "save" => {
            pick(&["write", "filewrite", "file_write"], "Write")
        }
        "bash" | "execute_bash" | "execute" | "sh" | "terminal" | "shell" | "run" | "exec"
        | "command" | "cmd" => pick(&["bash"], "Bash"),
        "glob" | "globtool" | "find_files" | "find" | "filesearch" | "file_search" => {
            pick(&["glob", "globtool"], "Glob")
        }
        "grep" | "greptool" | "search_files" | "search" | "content_search" => {
            pick(&["grep", "greptool"], "Grep")
        }
        "ls" | "dir" | "list_directory" | "list_dir" | "list_files" | "listdirectory" => {
            pick(&["ls", "bash"], "Bash")
        }
        "web_search" | "websearch" => {
            if let Some(value) = casing_map
                .get("websearch")
                .or_else(|| casing_map.get("web_search"))
            {
                return value.clone();
            }
            if let Some(key) = casing_map.keys().find(|k| k.starts_with("web_search_")) {
                return casing_map[key].clone();
            }
            "WebSearch".to_string()
        }
        "web_fetch" | "webfetch" | "fetch" | "curl" => pick(&["webfetch"], "WebFetch"),
        "notebook_edit" | "notebookedit" => pick(&["notebookedit"], "NotebookEdit"),
        "cron_create" | "croncreate" => pick(&["croncreate"], "CronCreate"),
        "cron_delete" | "crondelete" => pick(&["crondelete"], "CronDelete"),
        "cron_list" | "cronlist" => pick(&["cronlist"], "CronList"),
        "send_message" | "sendmessage" => pick(&["sendmessage"], "SendMessage"),
        "skill" | "call_skill" | "load_skill" | "run_skill" => pick(&["skill"], "Skill"),
        "agent" | "subagent" | "sub_agent" | "delegate" | "fork" => pick(&["agent"], "Agent"),
        "taskcreate" | "task_create" | "create_task" => pick(&["taskcreate"], "TaskCreate"),
        "taskupdate" | "task_update" | "update_task" => pick(&["taskupdate"], "TaskUpdate"),
        "taskget" | "task_get" | "get_task" => pick(&["taskget"], "TaskGet"),
        "tasklist" | "task_list" | "list_tasks" => pick(&["tasklist"], "TaskList"),
        "taskstop" | "task_stop" | "stop_task" => pick(&["taskstop"], "TaskStop"),
        "askuserquestion" | "ask_user_question" | "ask" => {
            pick(&["askuserquestion"], "AskUserQuestion")
        }
        "workflow" | "run_workflow" => pick(&["workflow"], "Workflow"),
        "other" => "other".to_string(),
        other => {
            if let Some(cased) = casing_map.get(other) {
                cased.clone()
            } else if other.starts_with("git")
                || other.starts_with("cargo")
                || other.starts_with("npm")
            {
                pick(&["bash"], "Bash")
            } else {
                raw_name.to_string()
            }
        }
    }
}

/// Legacy argument normalization.  Production uses schema rules instead.
pub fn normalize_arguments(tool_name: &str, args: Value) -> Value {
    normalize_arguments_with_raw(tool_name, tool_name, args)
}

pub fn normalize_arguments_with_raw(target_name: &str, raw_name: &str, mut args: Value) -> Value {
    let Some(obj) = args.as_object_mut() else {
        return args;
    };
    let lower_target = target_name.to_ascii_lowercase();
    let lower_raw = raw_name.to_ascii_lowercase();

    let take = |obj: &mut serde_json::Map<String, Value>, keys: &[&str]| -> Option<Value> {
        for key in keys {
            if let Some(value) = obj.remove(*key) {
                return Some(value);
            }
        }
        None
    };

    if lower_target == "bash" {
        if matches!(
            lower_raw.as_str(),
            "ls" | "dir" | "list_directory" | "list_dir" | "list_files" | "listdirectory"
        ) {
            let path = take(obj, &["path", "directory", "dir", "target"])
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| ".".to_string());
            obj.insert(
                "command".to_string(),
                Value::String(format!("ls -la {}", path)),
            );
        } else if matches!(
            lower_raw.as_str(),
            "grep" | "greptool" | "search_files" | "content_search"
        ) {
            let pat = take(obj, &["pattern", "query", "regex", "search"])
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            let path = take(obj, &["path", "directory", "dir"])
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| ".".to_string());
            obj.insert(
                "command".to_string(),
                Value::String(format!("grep -rn '{}' {}", pat, path)),
            );
        } else if matches!(
            lower_raw.as_str(),
            "glob" | "globtool" | "find_files" | "find" | "filesearch" | "file_search"
        ) {
            let pat = take(obj, &["pattern", "glob", "file_pattern"])
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "*".to_string());
            let path = take(obj, &["path", "directory", "dir"])
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| ".".to_string());
            obj.insert(
                "command".to_string(),
                Value::String(format!("find {} -name '{}'", path, pat)),
            );
        } else if (lower_raw.starts_with("git")
            || lower_raw.starts_with("cargo")
            || lower_raw.starts_with("npm"))
            && !obj.contains_key("command")
        {
            obj.insert(
                "command".to_string(),
                Value::String(lower_raw.replace('_', " ")),
            );
        }

        if let Some(arr) = obj.get("command").and_then(|v| v.as_array()) {
            let joined = arr
                .iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            obj.insert("command".to_string(), Value::String(joined));
        }

        if !obj.contains_key("command") {
            if let Some(cmd) = take(obj, &["cmd", "code", "script", "run", "exec"]) {
                obj.insert("command".to_string(), cmd);
            }
        }

        if let Some(cmd_val) = obj
            .get("command")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
        {
            if let Some(args_arr) =
                take(obj, &["args", "arguments"]).and_then(|v| v.as_array().cloned())
            {
                let args_str = args_arr
                    .iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !args_str.is_empty() {
                    obj.insert(
                        "command".to_string(),
                        Value::String(format!("{} {}", cmd_val, args_str)),
                    );
                }
            }
        }

        if !obj.contains_key("command") {
            if let Some(path) = take(obj, &["path", "directory"]) {
                let path_str = path.as_str().unwrap_or(".");
                obj.insert(
                    "command".to_string(),
                    Value::String(format!("ls -la {}", path_str)),
                );
            }
        }

        if let Some(Value::String(s)) = obj.get("timeout") {
            if let Ok(num) = s.parse::<u64>() {
                obj.insert("timeout".to_string(), json!(num));
            }
        }

        obj.retain(|k, _| k == "command" || k == "description" || k == "timeout");
    } else if lower_target == "read"
        || lower_target == "view"
        || lower_target == "fileread"
        || lower_target == "file_read"
    {
        let target_param = if lower_target == "view" {
            "path"
        } else {
            "file_path"
        };
        let alt_param = if lower_target == "view" {
            "file_path"
        } else {
            "path"
        };

        if let Some(val) = take(
            obj,
            &[
                alt_param,
                "file",
                "filepath",
                "filename",
                "uri",
                "target",
                "path_to_file",
                "source_file",
            ],
        ) {
            if !obj.contains_key(target_param) {
                obj.insert(target_param.to_string(), val);
            }
        }

        if let Some(vr) = obj.remove("view_range").and_then(|v| v.as_array().cloned()) {
            if vr.len() >= 2 {
                if let (Some(start), Some(end)) = (vr[0].as_u64(), vr[1].as_u64()) {
                    obj.insert("offset".to_string(), json!(start));
                    obj.insert("limit".to_string(), json!(end.saturating_sub(start) + 1));
                }
            }
        }

        if !obj.contains_key("offset") {
            if let Some(l) = take(obj, &["start_line", "line", "from"]) {
                if let Some(s) = l.as_str() {
                    if let Ok(num) = s.parse::<u64>() {
                        obj.insert("offset".to_string(), json!(num));
                    }
                } else if l.is_number() {
                    obj.insert("offset".to_string(), l);
                }
            }
        }

        if let Some(el) = take(obj, &["end_line", "to"]) {
            if !obj.contains_key("limit") {
                let end_num = el
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .or_else(|| el.as_u64());
                if let Some(end) = end_num {
                    let offset = obj.get("offset").and_then(|v| v.as_u64()).unwrap_or(1);
                    obj.insert("limit".to_string(), json!(end.saturating_sub(offset) + 1));
                }
            }
        }

        if !obj.contains_key("limit") {
            if let Some(c) = take(obj, &["num_lines", "count", "lines", "length"]) {
                if let Some(s) = c.as_str() {
                    if let Ok(num) = s.parse::<u64>() {
                        obj.insert("limit".to_string(), json!(num));
                    }
                } else if c.is_number() {
                    obj.insert("limit".to_string(), c);
                }
            }
        }

        if let Some(Value::String(s)) = obj.get("offset") {
            if let Ok(num) = s.parse::<u64>() {
                obj.insert("offset".to_string(), json!(num));
            }
        }

        obj.retain(|k, _| k == "file_path" || k == "offset" || k == "limit");
    } else if lower_target == "edit" || lower_target == "fileedit" || lower_target == "file_edit" {
        if let Some(val) = take(
            obj,
            &[
                "path",
                "file",
                "filepath",
                "filename",
                "target",
                "path_to_file",
            ],
        ) {
            if !obj.contains_key("file_path") {
                obj.insert("file_path".to_string(), val);
            }
        }
        if let Some(ot) = take(
            obj,
            &[
                "old_text",
                "original_text",
                "search",
                "before",
                "replace",
                "target_string",
                "find",
            ],
        ) {
            if !obj.contains_key("old_string") {
                obj.insert("old_string".to_string(), ot);
            }
        }
        if let Some(nt) = take(
            obj,
            &[
                "new_text",
                "replacement",
                "after",
                "to",
                "replacement_string",
                "substitute",
                "replace_with",
            ],
        ) {
            if !obj.contains_key("new_string") {
                obj.insert("new_string".to_string(), nt);
            }
        }
        if let Some(ra) = obj.get("replace_all") {
            if let Some(s) = ra.as_str() {
                obj.insert(
                    "replace_all".to_string(),
                    json!(s.eq_ignore_ascii_case("true")),
                );
            }
        }
        obj.retain(|k, _| {
            k == "file_path" || k == "old_string" || k == "new_string" || k == "replace_all"
        });
    } else if lower_target == "write" || lower_target == "filewrite" || lower_target == "file_write"
    {
        if let Some(val) = take(
            obj,
            &[
                "path",
                "file",
                "filepath",
                "filename",
                "target",
                "path_to_file",
            ],
        ) {
            if !obj.contains_key("file_path") {
                obj.insert("file_path".to_string(), val);
            }
        }
        if let Some(t) = take(
            obj,
            &[
                "text",
                "code",
                "contents",
                "data",
                "body",
                "file_content",
                "new_content",
            ],
        ) {
            if !obj.contains_key("content") {
                obj.insert("content".to_string(), t);
            }
        }
        obj.retain(|k, _| k == "file_path" || k == "content");
    } else if lower_target == "glob" || lower_target == "globtool" {
        if let Some(pat) = take(obj, &["file_pattern", "glob", "query", "search"]) {
            if !obj.contains_key("pattern") {
                obj.insert("pattern".to_string(), pat);
            }
        }
        if let Some(p) = take(obj, &["directory", "dir"]) {
            if !obj.contains_key("path") {
                obj.insert("path".to_string(), p);
            }
        }
        obj.retain(|k, _| k == "pattern" || k == "path");
    } else if lower_target == "grep" || lower_target == "greptool" {
        if let Some(pat) = take(obj, &["regex", "search", "query", "term"]) {
            if !obj.contains_key("pattern") {
                obj.insert("pattern".to_string(), pat);
            }
        }
        if let Some(p) = take(obj, &["directory", "dir"]) {
            if !obj.contains_key("path") {
                obj.insert("path".to_string(), p);
            }
        }
        obj.retain(|k, _| k == "pattern" || k == "path" || k == "glob" || k == "case_sensitive");
    } else if lower_target == "websearch"
        || lower_target == "web_search"
        || lower_target.starts_with("web_search_")
    {
        if let Some(q) = take(
            obj,
            &["q", "search_query", "term", "keyword", "query_string"],
        ) {
            if !obj.contains_key("query") {
                obj.insert("query".to_string(), q);
            }
        }
    } else if lower_target == "webfetch" {
        if let Some(u) = take(obj, &["uri", "link", "address", "target_url", "endpoint"]) {
            if !obj.contains_key("url") {
                obj.insert("url".to_string(), u);
            }
        }
        if !obj.contains_key("prompt") {
            obj.insert("prompt".to_string(), json!("Fetch and return content"));
        }
    } else if lower_target == "notebookedit" {
        if let Some(p) = take(obj, &["path", "file", "file_path", "notebook"]) {
            if !obj.contains_key("notebook_path") {
                obj.insert("notebook_path".to_string(), p);
            }
        }
        if let Some(s) = take(obj, &["source", "code", "content", "text"]) {
            if !obj.contains_key("new_source") {
                obj.insert("new_source".to_string(), s);
            }
        }
    } else if lower_target == "croncreate" {
        if let Some(c) = take(obj, &["schedule", "cron_expression", "expression"]) {
            if !obj.contains_key("cron") {
                obj.insert("cron".to_string(), c);
            }
        }
        if let Some(p) = take(obj, &["message", "instruction", "task"]) {
            if !obj.contains_key("prompt") {
                obj.insert("prompt".to_string(), p);
            }
        }
    } else if lower_target == "sendmessage" {
        if let Some(t) = take(obj, &["recipient", "agent", "target"]) {
            if !obj.contains_key("to") {
                obj.insert("to".to_string(), t);
            }
        }
        if let Some(m) = take(obj, &["content", "text", "body"]) {
            if !obj.contains_key("message") {
                obj.insert("message".to_string(), m);
            }
        }
    } else if lower_target == "skill" {
        if let Some(s) = take(obj, &["name", "skill_name", "skill"]) {
            obj.insert("skill".to_string(), s);
        }
        // Preserve `args`/`arguments`: Claude Code's Skill schema accepts them.
    } else if lower_target == "agent" {
        if let Some(p) = take(obj, &["task", "instruction", "query", "message"]) {
            if !obj.contains_key("prompt") {
                obj.insert("prompt".to_string(), p);
            }
        }
        if let Some(t) = take(obj, &["subagent", "type", "agent_type", "agent"]) {
            if !obj.contains_key("subagent_type") {
                let t_str = t.as_str().unwrap_or("");
                let norm_t = if t_str == "general" || t_str == "default" || t_str.is_empty() {
                    "general-purpose"
                } else {
                    t_str
                };
                obj.insert("subagent_type".to_string(), json!(norm_t));
            }
        }
        if let Some(bg) = obj.remove("background") {
            if !obj.contains_key("run_in_background") {
                obj.insert("run_in_background".to_string(), bg);
            }
        }
        if !obj.contains_key("description") {
            let desc = obj
                .get("prompt")
                .and_then(|v| v.as_str())
                .map(|s| {
                    let cut = s.floor_char_boundary(40.min(s.len()));
                    s[..cut].replace('\n', " ")
                })
                .unwrap_or_else(|| "Subagent task".to_string());
            obj.insert("description".to_string(), json!(desc));
        }
    } else if lower_target == "taskcreate" {
        if let Some(s) = take(obj, &["task", "title", "name"]) {
            if !obj.contains_key("subject") {
                obj.insert("subject".to_string(), s);
            }
        }
    } else if lower_target == "taskupdate" {
        if let Some(id) = take(obj, &["task_id", "id"]) {
            if !obj.contains_key("taskId") {
                obj.insert("taskId".to_string(), id);
            }
        }
        if let Some(st) = obj
            .get_mut("status")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
        {
            let norm_status = match st.to_lowercase().as_str() {
                "running" | "in-progress" | "started" => "in_progress",
                "done" | "success" | "finished" | "passed" => "completed",
                "todo" | "queued" => "pending",
                _ => &st,
            };
            obj.insert("status".to_string(), json!(norm_status));
        }
    } else if lower_target == "workflow" {
        if let Some(c) = take(obj, &["code", "body", "content"]) {
            if !obj.contains_key("script") {
                obj.insert("script".to_string(), c);
            }
        }
        if let Some(wf) = obj.remove("workflow") {
            if let Some(wf_str) = wf.as_str() {
                if !obj.contains_key("script")
                    && (wf_str.contains("export const meta")
                        || wf_str.contains("agent(")
                        || wf_str.contains("parallel(")
                        || wf_str.contains(';'))
                {
                    obj.insert("script".to_string(), Value::String(wf_str.to_string()));
                } else if !obj.contains_key("name") {
                    obj.insert("name".to_string(), Value::String(wf_str.to_string()));
                }
            }
        }
        if let Some(p) = take(obj, &["prompt", "task", "instruction", "query"]) {
            if !obj.contains_key("script")
                && !obj.contains_key("name")
                && !obj.contains_key("scriptPath")
            {
                if let Some(p_str) = p.as_str() {
                    let escaped_prompt = p_str
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n");
                    let synth_script = format!(
                        "export const meta = {{\n  name: \"workflow-task\",\n  description: \"Execute requested workflow\",\n  phases: [{{ title: \"Execution\" }}]\n}};\n\nconst res = await agent(\"{}\", {{ label: \"task\", phase: \"Execution\" }});\nreturn res;",
                        escaped_prompt
                    );
                    obj.insert("script".to_string(), Value::String(synth_script));
                }
            }
        }
        if let Some(s) = obj
            .get_mut("script")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
        {
            let mut clean_script = s.trim().to_string();
            if clean_script.starts_with("```") {
                if let Some(first_nl) = clean_script.find('\n') {
                    clean_script = clean_script[first_nl + 1..].to_string();
                }
                if let Some(last_fence) = clean_script.rfind("```") {
                    clean_script = clean_script[..last_fence].trim().to_string();
                }
            }
            if !clean_script.contains("export const meta") {
                clean_script = format!(
                    "export const meta = {{\n  name: \"custom-workflow\",\n  description: \"Execute workflow routine\",\n  phases: [{{ title: \"Execution\" }}]\n}};\n\n{}",
                    clean_script
                );
            }
            obj.insert("script".to_string(), Value::String(clean_script));
        }

        let allowed = [
            "script",
            "name",
            "description",
            "title",
            "args",
            "scriptPath",
            "resumeFromRunId",
        ];
        obj.retain(|k, _| allowed.contains(&k.as_str()));
    }

    args
}

pub fn sanitize_and_validate(
    mut mapped_name: String,
    mut norm_args: Value,
    casing_map: &HashMap<String, String>,
) -> Option<RepairedToolCall> {
    let lower = mapped_name.to_ascii_lowercase();
    if lower == "read" || lower == "view" || lower == "fileread" || lower == "file_read" {
        let path_str = norm_args
            .get("file_path")
            .or_else(|| norm_args.get("path"))
            .and_then(|v| v.as_str())?;
        if path_str.trim().is_empty() {
            return None;
        }
        // Compatibility behaviour: a directory passed to Read becomes `ls`.
        // The strict engine never performs filesystem I/O.
        let p = std::path::Path::new(path_str);
        if p.is_dir() {
            mapped_name = casing_map
                .get("bash")
                .cloned()
                .unwrap_or_else(|| "Bash".to_string());
            norm_args = json!({"command": format!("ls -la {}", path_str)});
        }
    } else if lower == "bash" {
        let has_cmd = norm_args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !has_cmd {
            return None;
        }
    } else if lower == "write" || lower == "filewrite" || lower == "file_write" {
        let has_path = norm_args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !has_path {
            return None;
        }
    } else if lower == "edit" || lower == "fileedit" || lower == "file_edit" {
        let has_path = norm_args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_old = norm_args
            .get("old_string")
            .and_then(|v| v.as_str())
            .is_some();
        let has_new = norm_args
            .get("new_string")
            .and_then(|v| v.as_str())
            .is_some();
        if !has_path || !has_old || !has_new {
            return None;
        }
    } else if lower == "glob" || lower == "globtool" {
        if norm_args
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            return None;
        }
    } else if lower == "grep" || lower == "greptool" {
        if norm_args
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            return None;
        }
    } else if lower == "skill" {
        if norm_args
            .get("skill")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            return None;
        }
    } else if lower == "agent" {
        if norm_args
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            return None;
        }
    } else if lower == "taskcreate" {
        if norm_args
            .get("subject")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            return None;
        }
    } else if lower == "taskupdate" {
        if norm_args
            .get("taskId")
            .or_else(|| norm_args.get("task_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            return None;
        }
    } else if lower == "workflow" {
        let has_target = norm_args
            .get("script")
            .or_else(|| norm_args.get("name"))
            .or_else(|| norm_args.get("scriptPath"))
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !has_target {
            return None;
        }
    } else if lower == "taskget"
        || lower == "tasklist"
        || lower == "taskstop"
        || lower == "askuserquestion"
    {
        // Flexible schemas.
    } else if lower == "other" {
        let skill_name = norm_args
            .get("skill")
            .or_else(|| norm_args.get("name"))
            .and_then(|v| v.as_str())?;
        if skill_name.trim().is_empty() || !casing_map.contains_key("skill") {
            return None;
        }
        mapped_name = casing_map
            .get("skill")
            .cloned()
            .unwrap_or_else(|| "Skill".to_string());
        norm_args = json!({"skill": skill_name});
    }

    if !casing_map.is_empty() && !casing_map.contains_key(&mapped_name.to_ascii_lowercase()) {
        return None;
    }

    Some(RepairedToolCall {
        name: mapped_name,
        arguments: norm_args,
    })
}

/// Detect every legacy envelope in `text`.
pub fn detect_and_extract(
    text: &str,
    casing_map: &HashMap<String, String>,
) -> Vec<RepairedToolCall> {
    // 1-5. Standard XML/DSML envelopes.
    for family in ["tool_call", "function_call", "invoke", "action", "dsml"] {
        let mut found = Vec::new();
        for raw in scan_raw_blocks(text, family) {
            let candidates = ToolCallEngine::default().extract_candidates(&raw, false);
            found.extend(process_candidates(candidates, casing_map));
        }
        if !found.is_empty() {
            return found;
        }
    }

    // 5b. Unclosed <tool_call> at the end of the stream.
    if let Some(pos) = text.rfind("<tool_call") {
        let candidates = ToolCallEngine::default().extract_candidates(&text[pos..], true);
        let found = process_candidates(candidates, casing_map);
        if !found.is_empty() {
            return found;
        }
    }

    // 6. JSON code fence.
    if let Some(found) = extract_json_fences(text, casing_map) {
        if !found.is_empty() {
            return found;
        }
    }

    // 7. OpenCode Code Mode tools.<name>({...}) calls.
    let js_calls = extract_tools_js(text, casing_map);
    if !js_calls.is_empty() {
        return js_calls;
    }

    // 8. Workflow script fences.
    if let Some(found) = extract_workflow(text, casing_map) {
        return found;
    }

    // 9. Pseudo-subagent declarations.
    if let Some(found) = extract_pseudo_subagents(text, casing_map) {
        return found;
    }

    // 10. Markdown bash fences, only when Bash is declared.
    extract_bash_fences(text, casing_map)
}

fn scan_raw_blocks(text: &str, family: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let Some(tag) = scan_tool_tag_start(rest) else {
            break;
        };
        if tag.tag_name == family {
            if let Some(rel_end) = rest[tag.content_start..].find(tag.close_tag) {
                let end = tag.content_start + rel_end + tag.close_tag.len();
                found.push(rest[tag.start..end].to_string());
                cursor += end;
                continue;
            } else {
                found.push(rest[tag.start..].to_string());
                break;
            }
        }
        cursor += tag.start + 1;
    }
    found
}

fn process_candidates(
    candidates: Vec<ToolCallCandidate>,
    casing_map: &HashMap<String, String>,
) -> Vec<RepairedToolCall> {
    let mut out = Vec::new();
    for candidate in candidates {
        let mapped = map_tool_name(&candidate.raw_name, casing_map);
        let args =
            normalize_arguments_with_raw(&mapped, &candidate.raw_name, candidate.parsed_arguments);
        if let Some(call) = sanitize_and_validate(mapped, args, casing_map) {
            out.push(call);
        }
    }
    out
}

fn extract_json_fences(
    text: &str,
    casing_map: &HashMap<String, String>,
) -> Option<Vec<RepairedToolCall>> {
    let mut out = Vec::new();
    for (lang, body) in scan_fences(text) {
        if !lang.is_empty() && lang != "json" {
            continue;
        }
        let body = body.trim();
        if !body.starts_with('{') {
            continue;
        }
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&repair_tool_arguments(body))
        else {
            continue;
        };
        let raw_name = obj
            .get("name")
            .or_else(|| obj.get("tool"))
            .or_else(|| obj.get("tool_name"))
            .or_else(|| obj.get("action"))
            .and_then(|v| v.as_str());
        if let Some(raw_name) = raw_name {
            let mapped = map_tool_name(raw_name, casing_map);
            let mut args = obj
                .get("arguments")
                .or_else(|| obj.get("parameters"))
                .or_else(|| obj.get("args"))
                .or_else(|| obj.get("action_input"))
                .cloned()
                .unwrap_or_else(|| {
                    let mut copy = obj.clone();
                    for key in ["name", "tool", "tool_name", "action"] {
                        copy.remove(key);
                    }
                    Value::Object(copy)
                });
            if let Value::String(raw) = &args {
                args = serde_json::from_str(&repair_tool_arguments(raw))
                    .unwrap_or_else(|_| Value::String(raw.clone()));
            }
            let norm = normalize_arguments_with_raw(&mapped, raw_name, args);
            if let Some(call) = sanitize_and_validate(mapped, norm, casing_map) {
                out.push(call);
            }
        } else if obj.contains_key("command") {
            if let Some(mapped) = casing_map.get("bash") {
                let norm = normalize_arguments_with_raw(mapped, "bash", Value::Object(obj));
                if let Some(call) = sanitize_and_validate(mapped.clone(), norm, casing_map) {
                    out.push(call);
                }
            }
        }
    }
    Some(out)
}

fn extract_tools_js(text: &str, casing_map: &HashMap<String, String>) -> Vec<RepairedToolCall> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find("tools.") {
        let pos = cursor + rel + "tools.".len();
        let rest = &text[pos..];
        let name_len = rest
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if name_len == 0 {
            cursor = pos;
            continue;
        }
        let raw_name = &rest[..name_len];
        let after = rest[name_len..].trim_start();
        if !after.starts_with('(') {
            cursor = pos + name_len;
            continue;
        }
        let args_start = match after.find('{') {
            Some(i) => pos + name_len + (rest[name_len..].len() - after.len()) + i,
            None => break,
        };
        let Some(args_end) = find_matching_brace(text, args_start) else {
            break;
        };
        let raw_args = &text[args_start..=args_end];
        if let Ok(value) = serde_json::from_str::<Value>(&repair_tool_arguments(raw_args)) {
            let mapped = if raw_name.eq_ignore_ascii_case("subagent") {
                casing_map
                    .get("agent")
                    .cloned()
                    .unwrap_or_else(|| "Agent".to_string())
            } else {
                map_tool_name(raw_name, casing_map)
            };
            let norm = normalize_arguments_with_raw(&mapped, raw_name, value);
            if let Some(call) = sanitize_and_validate(mapped, norm, casing_map) {
                out.push(call);
            }
        }
        cursor = args_end + 1;
    }
    out
}

fn find_matching_brace(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string: Option<u8> = None;
    let mut escape = false;
    for (offset, &b) in bytes[start..].iter().enumerate() {
        if let Some(quote) = in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == quote {
                in_string = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => in_string = Some(b),
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_workflow(
    text: &str,
    casing_map: &HashMap<String, String>,
) -> Option<Vec<RepairedToolCall>> {
    let wf_name = casing_map.get("workflow")?;
    if !text.contains("export const meta") {
        return None;
    }
    let idx = text.find("export const meta")?;
    let tail = &text[idx..];
    let script = if let Some(end) = tail.find("```") {
        tail[..end].trim().to_string()
    } else {
        tail.trim().to_string()
    };
    if script.is_empty() {
        return None;
    }
    let args = json!({"script": script});
    let norm = normalize_arguments_with_raw(wf_name, "workflow", args);
    sanitize_and_validate(wf_name.clone(), norm, casing_map).map(|c| vec![c])
}

fn extract_pseudo_subagents(
    text: &str,
    casing_map: &HashMap<String, String>,
) -> Option<Vec<RepairedToolCall>> {
    let agent_name = casing_map.get("agent")?;
    let lower_text = text.to_lowercase();
    if !(lower_text.contains("subagent")
        || lower_text.contains("sub-agent")
        || lower_text.contains("spawn"))
    {
        return None;
    }

    let mut found = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let lower_l = trimmed.to_lowercase();
        let numbered = lower_l.starts_with("1. ")
            || lower_l.starts_with("2. ")
            || lower_l.starts_with("1) ")
            || lower_l.starts_with("2) ")
            || lower_l.starts_with("subagent 1")
            || lower_l.starts_with("subagent 2")
            || lower_l.starts_with("- subagent 1")
            || lower_l.starts_with("- subagent 2");
        if numbered
            && (lower_l.contains("agent")
                || lower_l.contains("rà soát")
                || lower_l.contains("kiểm tra")
                || lower_l.contains("task")
                || lower_l.contains("nhiệm vụ"))
        {
            found.push(trimmed.to_string());
        }
    }

    if found.len() < 2 {
        return None;
    }

    Some(
        found
            .into_iter()
            .enumerate()
            .map(|(i, prompt)| RepairedToolCall {
                name: agent_name.clone(),
                arguments: json!({
                    "description": format!("Subagent {}", i + 1),
                    "prompt": prompt,
                    "subagent_type": "fork"
                }),
            })
            .collect(),
    )
}

fn extract_bash_fences(text: &str, casing_map: &HashMap<String, String>) -> Vec<RepairedToolCall> {
    let Some(bash_name) = casing_map.get("bash") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (lang, body) in scan_fences(text) {
        if !matches!(lang.as_str(), "bash" | "sh" | "shell" | "zsh") {
            continue;
        }
        let cmd = body.trim();
        if cmd.is_empty() {
            continue;
        }
        if cmd.starts_with("const ")
            || cmd.starts_with("let ")
            || cmd.starts_with("var ")
            || cmd.contains("tools.")
            || cmd.contains("await ")
            || cmd.contains("search({")
        {
            continue;
        }
        out.push(RepairedToolCall {
            name: bash_name.clone(),
            arguments: json!({"command": cmd}),
        });
    }
    out
}

fn scan_fences(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find("```") {
        let open = cursor + rel + 3;
        let Some(nl_rel) = text[open..].find('\n') else {
            break;
        };
        let lang = text[open..open + nl_rel].trim().to_lowercase();
        let body_start = open + nl_rel + 1;
        let Some(close_rel) = text[body_start..].find("```") else {
            out.push((lang, text[body_start..].to_string()));
            break;
        };
        let body_end = body_start + close_rel;
        out.push((lang, text[body_start..body_end].to_string()));
        cursor = body_end + 3;
    }
    out
}

pub fn clean_user_visible_text(raw: &str) -> String {
    clean_with(raw, false)
}

pub fn strip_tool_tags(text: &str) -> String {
    clean_with(text, true)
}

fn clean_with(raw: &str, strip_only: bool) -> String {
    let mut text = strip_envelopes(raw);
    if !strip_only {
        // Remove a dangling unclosed opening tag.
        for marker in [
            "<tool_call",
            "<invoke",
            "<function_call",
            "<action",
            "<｜tool",
            "<|tool",
        ] {
            if let Some(pos) = text.rfind(marker) {
                let close = match marker {
                    "<tool_call" => "</tool_call>",
                    "<invoke" => "</invoke>",
                    "<function_call" => "</function_call>",
                    "<action" => "</action>",
                    "<｜tool" => "<｜tool call end｜>",
                    "<|tool" => "<|tool call end|>",
                    _ => "",
                };
                if !close.is_empty() && !text[pos..].contains(close) {
                    text.truncate(pos);
                }
            }
        }
    }
    text.trim().to_string()
}

pub fn get_streamable_text_end(text: &str) -> usize {
    streamable_text_end(text)
}

/// Public compatibility facade.
pub struct ToolCallRepairEngine;

impl ToolCallRepairEngine {
    pub fn clean_user_visible_text(raw: &str) -> String {
        clean_user_visible_text(raw)
    }

    pub fn get_streamable_text_end(s: &str) -> usize {
        get_streamable_text_end(s)
    }

    pub fn strip_tool_tags(text: &str) -> String {
        strip_tool_tags(text)
    }

    pub fn map_tool_name(raw_name: &str, casing_map: &HashMap<String, String>) -> String {
        map_tool_name(raw_name, casing_map)
    }

    pub fn normalize_arguments(tool_name: &str, args: Value) -> Value {
        normalize_arguments(tool_name, args)
    }

    pub fn normalize_arguments_with_raw(target_name: &str, raw_name: &str, args: Value) -> Value {
        normalize_arguments_with_raw(target_name, raw_name, args)
    }

    pub fn sanitize_and_validate(
        mapped_name: String,
        norm_args: Value,
        casing_map: &HashMap<String, String>,
    ) -> Option<RepairedToolCall> {
        sanitize_and_validate(mapped_name, norm_args, casing_map)
    }

    pub fn detect_and_extract(
        text: &str,
        casing_map: &HashMap<String, String>,
    ) -> Vec<RepairedToolCall> {
        detect_and_extract(text, casing_map)
    }

    /// Strict schema-driven extraction used by the production stream engine.
    pub fn validate_candidate(
        candidate: ToolCallCandidate,
        tools: &[crate::protocol::anthropic::AnthropicTool],
    ) -> Result<ValidatedToolCall, crate::protocol::tool_engine::ValidationError> {
        ToolCallEngine::new(tools).validate_and_normalize(candidate)
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
        assert_eq!(
            ToolCallRepairEngine::strip_tool_tags(text),
            "I will check the status."
        );
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
        let map = HashMap::new();
        let text = "```bash\nrm -rf /\n```";
        assert!(ToolCallRepairEngine::detect_and_extract(text, &map).is_empty());
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
        assert_eq!(
            calls[0].arguments["file_path"],
            "/home/user/project/file.txt"
        );
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

    #[test]
    fn test_repair_tool_arguments_unescaped_newlines() {
        let raw = "{\"command\": \"cat << 'EOF' > file.txt\nhello world\nEOF\"}";
        let repaired = repair_tool_arguments(raw);
        let val: Value = serde_json::from_str(&repaired).expect("Should parse");
        assert_eq!(val["command"], "cat << 'EOF' > file.txt\nhello world\nEOF");
    }

    #[test]
    fn test_repair_tool_arguments_python_dict() {
        let raw = "{'command': 'git status', 'replace_all': True}";
        let repaired = repair_tool_arguments(raw);
        let val: Value = serde_json::from_str(&repaired).expect("Should parse");
        assert_eq!(val["command"], "git status");
        assert_eq!(val["replace_all"], true);
    }

    #[test]
    fn test_repair_tool_arguments_invalid_escape() {
        let raw = r#"{"command": "grep -P '\d+' file.txt"}"#;
        let repaired = repair_tool_arguments(raw);
        let val: Value = serde_json::from_str(&repaired).expect("Should parse");
        assert_eq!(val["command"], r#"grep -P '\d+' file.txt"#);
    }

    #[test]
    fn test_extract_action_tag() {
        let mut map = HashMap::new();
        map.insert("bash".to_string(), "Bash".to_string());
        let text = r#"<action name="Bash">
{"command": "cargo build"}
</action>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "cargo build");
    }

    #[test]
    fn test_extract_dsml_tool_call() {
        let mut map = HashMap::new();
        map.insert("bash".to_string(), "Bash".to_string());
        let text = "<｜tool calls begin｜><｜tool call begin｜>function<｜tool sep｜>bash\n{\"command\": \"cargo check\"}<｜tool call end｜><｜tool calls end｜>";
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "cargo check");
    }

    #[test]
    fn test_extract_json_fence_tool_call() {
        let mut map = HashMap::new();
        map.insert("bash".to_string(), "Bash".to_string());
        let text = "Here is the command to run:\n```json\n{\n  \"tool\": \"bash\",\n  \"command\": \"npm test\"\n}\n```";
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], "npm test");
    }

    #[test]
    fn test_glob_and_grep_normalization() {
        let mut map = HashMap::new();
        map.insert("glob".to_string(), "Glob".to_string());
        map.insert("grep".to_string(), "Grep".to_string());

        let glob_text = r#"<tool_call>
{"name": "glob", "arguments": {"glob": "**/*.rs", "directory": "src"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(glob_text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Glob");
        assert_eq!(calls[0].arguments["pattern"], "**/*.rs");
        assert_eq!(calls[0].arguments["path"], "src");

        let grep_text = r#"<tool_call>
{"name": "grep", "arguments": {"regex": "fn test", "dir": "tests"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(grep_text, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Grep");
        assert_eq!(calls[0].arguments["pattern"], "fn test");
        assert_eq!(calls[0].arguments["path"], "tests");
    }

    #[test]
    fn test_repair_regex_dots_and_dollar() {
        let raw = r#"{"command": "grep 'foo\.bar\$' file.txt"}"#;
        let repaired = repair_tool_arguments(raw);
        let val: Value = serde_json::from_str(&repaired).expect("Should parse with doubled escape");
        assert_eq!(val["command"], r#"grep 'foo\.bar\$' file.txt"#);
    }

    #[test]
    fn test_repair_unquoted_keys() {
        let raw = "{command: \"ls -la\", timeout: 5000}";
        let repaired = repair_tool_arguments(raw);
        let val: Value = serde_json::from_str(&repaired).expect("Should parse unquoted keys");
        assert_eq!(val["command"], "ls -la");
        assert_eq!(val["timeout"], 5000);
    }

    #[test]
    fn test_websearch_casing_and_arg_normalization() {
        let mut map = HashMap::new();
        map.insert(
            "web_search_20250305".to_string(),
            "web_search_20250305".to_string(),
        );
        let mapped = ToolCallRepairEngine::map_tool_name("web_search", &map);
        assert_eq!(mapped, "web_search_20250305");
        let args = json!({"search_query": "ctf steganography"});
        let normalized = ToolCallRepairEngine::normalize_arguments(&mapped, args);
        assert_eq!(normalized["query"], "ctf steganography");
    }

    #[test]
    fn test_file_edit_write_read_casing_and_validation() {
        let mut map = HashMap::new();
        map.insert("fileedit".to_string(), "FileEdit".to_string());
        map.insert("filewrite".to_string(), "FileWrite".to_string());
        map.insert("fileread".to_string(), "FileRead".to_string());

        let edit_xml = r#"<tool_call>
{"name": "edit", "arguments": {"path": "src/main.rs", "old_text": "hello", "new_text": "world"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(edit_xml, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "FileEdit");
        assert_eq!(calls[0].arguments["file_path"], "src/main.rs");
        assert_eq!(calls[0].arguments["old_string"], "hello");
        assert_eq!(calls[0].arguments["new_string"], "world");

        let write_xml = r#"<tool_call>
{"name": "write", "arguments": {"path": "test.txt", "contents": "123"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(write_xml, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "FileWrite");
        assert_eq!(calls[0].arguments["file_path"], "test.txt");
        assert_eq!(calls[0].arguments["content"], "123");

        let read_xml = r#"<tool_call>
{"name": "read", "arguments": {"path": "Cargo.toml"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(read_xml, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "FileRead");
        assert_eq!(calls[0].arguments["file_path"], "Cargo.toml");
    }

    #[test]
    fn test_extract_subagents_from_text() {
        let mut map = HashMap::new();
        map.insert("agent".to_string(), "Agent".to_string());
        let text = "Tôi sẽ spawn 2 subagent chạy nền để làm việc:\n1. Explore agent: rà soát repo\n2. General agent: kiểm tra git status";
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "Agent");
        assert_eq!(calls[0].arguments["subagent_type"], "fork");
        assert_eq!(calls[0].arguments["description"], "Subagent 1");
        assert_eq!(calls[1].name, "Agent");
    }

    #[test]
    fn test_extract_tools_js_subagent_calls() {
        let mut map = HashMap::new();
        map.insert("agent".to_string(), "Agent".to_string());
        map.insert("bash".to_string(), "Bash".to_string());
        let text = r#"
```bash
const r = await tools.subagent({agent: "general", description: "Explore repo structure", prompt: "Explore the working directory"});
text(r);
const r2 = await tools.subagent({agent: "general", description: "Write README draft", prompt: "Create a new file"});
text(r2);
```
"#;
        let calls = ToolCallRepairEngine::detect_and_extract(text, &map);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "Agent");
        assert_eq!(calls[0].arguments["description"], "Explore repo structure");
        assert_eq!(calls[0].arguments["subagent_type"], "general-purpose");
        assert_eq!(calls[1].name, "Agent");
        assert_eq!(calls[1].arguments["description"], "Write README draft");
        assert_eq!(calls[1].arguments["subagent_type"], "general-purpose");
    }

    #[test]
    fn test_workflow_normalization_and_extraction() {
        let mut map = HashMap::new();
        map.insert("workflow".to_string(), "Workflow".to_string());

        let xml = r#"<tool_call>
{"name": "workflow", "arguments": {"workflow": "const res = await agent('audit'); return res;"}}
</tool_call>"#;
        let calls = ToolCallRepairEngine::detect_and_extract(xml, &map);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Workflow");
        assert!(calls[0].arguments.get("workflow").is_none());
        let script = calls[0].arguments["script"].as_str().unwrap();
        assert!(script.contains("export const meta ="));
        assert!(script.contains("const res = await agent('audit');"));

        let fence_text = r#"
```javascript
export const meta = {
  name: "demo-wf",
  description: "Test workflow",
  phases: [{ title: "Run" }]
};

const r = await agent("Audit project");
return r;
```
"#;
        let fence_calls = ToolCallRepairEngine::detect_and_extract(fence_text, &map);
        assert_eq!(fence_calls.len(), 1);
        assert_eq!(fence_calls[0].name, "Workflow");
        let s2 = fence_calls[0].arguments["script"].as_str().unwrap();
        assert!(s2.contains("export const meta ="));
        assert!(s2.contains("name: \"demo-wf\""));
    }
}
