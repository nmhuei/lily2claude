# Unified State Machine & Schema-Driven Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-architect `lily2claude`'s prompt compiler, streaming engine, and tool repair layer into a unified, deterministic, Claude Code-compliant system meeting all 19 Definition of Done criteria.

**Architecture:** A 3-pillar design consisting of:
1. A clean Prompt Compiler that mirrors Claude Code's native role separation and environment reminders.
2. A single Deterministic Finite State Machine (`AnthropicStreamEngine`) for all stream sources (ACP chunks and OpenAI HTTP SSE) with proven chunk invariance.
3. A 3-Tier Schema-Driven Tool Engine (Parse -> Normalize -> Validate) with resilient stateful JSON parsing and zero regex soup.

**Tech Stack:** Rust (Tokio, Axum, Serde JSON, Reqwest), Claude Code 2.1.303 wire protocol, Anthropic Messages API SSE specification.

**Spec:** [`docs/superpowers/specs/2026-10-20-unified-state-machine-and-schema-engine-design.md`](file:///home/light/GitHub/lily2claude/docs/superpowers/specs/2026-10-20-unified-state-machine-and-schema-engine-design.md)

## Global Constraints

- Always build and test with `CARGO_TARGET_DIR=/home/light/Downloads`
- All 83 existing tests (49 unit in `src/lib.rs` + 34 integration in `tests/`) must pass with zero regressions
- The active daemon on port 4001 must NEVER be replaced until candidate binary passes full staging verification on port 4002
- Backup existing binary to `/home/light/.cargo/bin/lily2claude.bak` prior to port 4001 replacement
- Strict chunk invariance: arbitrary byte chunking of identical input must yield identical Anthropic SSE event sequences

---

### Task 1: Prompt Compiler Standardization (`src/protocol/prompt.rs`)

**Files:**
- Modify: `src/protocol/prompt.rs`
- Test: `tests/test_server.rs`, `tests/test_protocol_transform.rs`

**Interfaces:**
- Consumes: `crate::protocol::anthropic::MessagesRequest`
- Produces: `pub fn build_acp_prompt(req: &MessagesRequest) -> String`

- [ ] **Step 1: Write unit tests for prompt compiler data preservation and compaction**
Add tests in `src/protocol/prompt.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::anthropic::*;

    #[test]
    fn test_prompt_compiler_role_separation_and_reminders() {
        let req = MessagesRequest {
            model: "deepseek".to_string(),
            system: Some(SystemPrompt::Single("You are a Claude agent.".to_string())),
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("<system-reminder>cwd: /tmp</system-reminder>\nHello".to_string()),
                },
                Message {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(vec![
                        ContentBlock::ToolUse {
                            id: "call_1".to_string(),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "pwd"}),
                        }
                    ]),
                },
                Message {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![
                        ContentBlock::ToolResult {
                            tool_use_id: "call_1".to_string(),
                            content: serde_json::Value::String("/tmp".to_string()),
                            is_error: Some(false),
                        }
                    ]),
                }
            ],
            tools: Some(vec![ToolDefinition {
                name: "Bash".to_string(),
                description: "Run shell command".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"]
                }),
                resolved_name: None,
            }]),
            max_tokens: 1024,
            stream: Some(true),
            temperature: None,
        };

        let prompt = build_acp_prompt(&req);
        assert!(prompt.contains("[System Instructions]\nYou are a Claude agent."));
        assert!(prompt.contains("Bash(command)"));
        assert!(prompt.contains("<system-reminder>cwd: /tmp</system-reminder>"));
        assert!(prompt.contains("<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\":\"pwd\"}}\n</tool_call>"));
        assert!(prompt.contains("<tool_result>\n/tmp\n</tool_result>"));
    }
}
```

- [ ] **Step 2: Run test to verify it compiles and checks current behavior**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --lib protocol::prompt`

- [ ] **Step 3: Refactor `build_acp_prompt` in `src/protocol/prompt.rs`**
Ensure:
- System context preserves Claude Code instructions without injecting raw JSON schemas.
- Clean compact tool signatures: `ToolName(req_args, opt_args?)`.
- Compaction mode strips tool declarations and injects the concise summary directive.
- Pre-turn reminders `<system-reminder>` preserved verbatim inside User turns.
- Proper escaping and serialization of multi-turn tool calls and tool results.

- [ ] **Step 4: Verify prompt compiler tests and existing server tests pass**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_server`

- [ ] **Step 5: Commit**
```bash
git add src/protocol/prompt.rs
git commit -m "refactor(prompt): standardize prompt compiler with role preservation and clean tool signatures"
```

---

### Task 2: Resilient JSON Parser & Tag Lexer (`src/protocol/tool_engine/`)

**Files:**
- Create: `src/protocol/tool_engine/tag_lexer.rs`
- Create: `src/protocol/tool_engine/json_parser.rs`
- Modify: `src/protocol/mod.rs`
- Test: `tests/test_tool_engine.rs`

**Interfaces:**
- Consumes: Raw text fragments and unclosed JSON strings
- Produces:
  - `tag_lexer::scan_tool_tag_start(text: &str) -> Option<TagMatch>`
  - `tag_lexer::scan_tool_tag_end(text: &str, open_tag: &str) -> Option<usize>`
  - `json_parser::parse_resilient_json(input: &str) -> ParseResult`

- [ ] **Step 1: Write tests for Tag Lexer & Resilient JSON Parser**
Create `tests/test_tool_engine.rs`:
```rust
use lily2claude::protocol::tool_engine::tag_lexer::*;
use lily2claude::protocol::tool_engine::json_parser::*;
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
```

- [ ] **Step 2: Run test to verify it fails**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_tool_engine`
Expected: Compilation failure (module does not exist yet).

- [ ] **Step 3: Implement `tag_lexer.rs` and `json_parser.rs`**
Implement stateful scanner without regex soup:
- `tag_lexer.rs`: Exact byte scanning for `<tool_call>`, `</tool_call>`, `<invoke name="...">`, `</invoke>`.
- `json_parser.rs`: Strict `serde_json::from_str` first. On failure, run stateful tokenizer handling missing closing quotes, braces, trailing commas, unescaped control chars, with recursion depth cap of 16.

- [ ] **Step 4: Run tests to verify pass**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_tool_engine`

- [ ] **Step 5: Commit**
```bash
git add src/protocol/tool_engine/tag_lexer.rs src/protocol/tool_engine/json_parser.rs src/protocol/mod.rs tests/test_tool_engine.rs
git commit -m "feat(tool_engine): add deterministic tag lexer and resilient JSON parser"
```

---

### Task 3: Schema-Driven Tool Normalizer & Validator (`src/protocol/tool_engine/`)

**Files:**
- Create: `src/protocol/tool_engine/schema_normalizer.rs`
- Create: `src/protocol/tool_engine/validation.rs`
- Create: `src/protocol/tool_engine/mod.rs`
- Modify: `src/protocol/repair/mod.rs` (forward calls to `tool_engine` to preserve backward compatibility for existing tests)
- Test: `tests/test_tool_engine.rs`

**Interfaces:**
- Consumes: `ToolCallCandidate`, declared `tools: &[ToolDefinition]`, `casing_map: &HashMap<String, String>`
- Produces: `ToolCallEngine::validate_and_normalize(candidate, tools) -> Result<ValidatedToolCall, ValidationError>`

- [ ] **Step 1: Write tests for 3-Tier Schema Normalization and Validation**
Add to `tests/test_tool_engine.rs`:
```rust
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
```

- [ ] **Step 2: Run test to verify it fails**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_tool_engine`

- [ ] **Step 3: Implement `schema_normalizer.rs` and `validation.rs`**
Implement:
- Tier 2: Non-destructive alias mapping against declared schema (`cmd` -> `command`, `file` -> `file_path`, `dir` -> `path`). Reject with `AmbiguityConflict` if both target and alias are provided.
- Tier 3: Validation checking `required` properties, non-empty values, primitive types.
- Facade `ToolCallEngine` in `src/protocol/tool_engine/mod.rs`.
- Wire `src/protocol/repair/mod.rs` to delegate to `tool_engine` to maintain 100% compatibility with all existing unit tests.

- [ ] **Step 4: Run tests to verify pass**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --lib protocol::repair && CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_tool_engine`

- [ ] **Step 5: Commit**
```bash
git add src/protocol/tool_engine/ src/protocol/repair/ tests/test_tool_engine.rs
git commit -m "feat(tool_engine): implement 3-tier schema normalizer and validation engine"
```

---

### Task 4: Unified Streaming FSM Core Engine (`src/protocol/stream/engine.rs`)

**Files:**
- Create: `src/protocol/stream/engine.rs`
- Modify: `src/protocol/stream/mod.rs`
- Test: `tests/test_stream_engine.rs`

**Interfaces:**
- Consumes: `StreamInput` (`ThinkingDelta`, `TextDelta`, `UsageUpdate`, `Finish`, `Cancel`, `UpstreamError`)
- Produces: `Vec<SseEvent>`

- [ ] **Step 1: Write property-based chunk invariance test and FSM transition tests**
Create `tests/test_stream_engine.rs`:
```rust
use lily2claude::protocol::stream::engine::*;
use lily2claude::protocol::stream::events::SseEvent;

#[test]
fn test_chunk_invariance_property() {
    let full_text = "I will check the directory.\n<tool_call>\n{\"name\": \"Bash\", \"arguments\": {\"command\": \"pwd\"}}\n</tool_call>\nDone!";
    
    // Test Case A: fed as a single chunk
    let mut engine_a = AnthropicStreamEngine::new("msg_a", "deepseek", 100, HashMap::new());
    let mut events_a = engine_a.feed_text(full_text);
    events_a.extend(engine_a.finish(100, 10, 0));

    // Test Case B: fed character-by-character (1 byte per chunk)
    let mut engine_b = AnthropicStreamEngine::new("msg_a", "deepseek", 100, HashMap::new());
    let mut events_b = Vec::new();
    for ch in full_text.chars() {
        let mut s = String::new();
        s.push(ch);
        events_b.extend(engine_b.feed_text(&s));
    }
    events_b.extend(engine_b.finish(100, 10, 0));

    // Logical events must match exactly
    assert_eq!(extract_event_types(&events_a), extract_event_types(&events_b));
    assert_eq!(extract_tool_calls(&events_a), extract_tool_calls(&events_b));
}

#[test]
fn test_interleaved_thinking_text_tool_text() {
    let mut engine = AnthropicStreamEngine::new("msg_1", "deepseek", 100, HashMap::new());
    let ev1 = engine.feed_thinking("Planning steps...");
    assert!(ev1.iter().any(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking"));
    
    let ev2 = engine.feed_text("Let me run pwd: <tool_call>{\"name\":\"Bash\",\"arguments\":{\"command\":\"pwd\"}}</tool_call>\nWaiting for output.");
    // Verify signature delta emitted closing thinking before text
    assert!(ev2.iter().any(|e| e.data.pointer("/delta/type").and_then(|t| t.as_str()) == Some("signature_delta")));
    // Verify tool_use block emitted
    assert!(ev2.iter().any(|e| e.data.pointer("/content_block/type").and_then(|t| t.as_str()) == Some("tool_use")));
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_stream_engine`

- [ ] **Step 3: Implement `AnthropicStreamEngine` in `src/protocol/stream/engine.rs`**
Implement the FSM following Section 3.3 of the spec:
- States: `Idle`, `Thinking`, `Text`, `ToolTagScan`, `ToolArguments`, `Finished`.
- Maintain monotonic `block_index`.
- Use holdback buffer for partial tags `<tool_call`, `<invoke`, `<think`.
- Paced 256-byte streaming for `text_delta`.
- Exact Anthropic SSE block lifecycle.

- [ ] **Step 4: Run tests to verify pass**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_stream_engine`

- [ ] **Step 5: Commit**
```bash
git add src/protocol/stream/engine.rs src/protocol/stream/mod.rs tests/test_stream_engine.rs
git commit -m "feat(stream): implement AnthropicStreamEngine FSM with chunk invariance"
```

---

### Task 5: Stream Adapters & Server Integration

**Files:**
- Create: `src/protocol/stream/acp_adapter.rs`
- Create: `src/protocol/stream/openai_adapter.rs`
- Modify: `src/server/handlers/messages.rs`
- Test: `tests/test_stream.rs`, `tests/test_acp_client.rs`

**Interfaces:**
- Consumes: `AcpOutputChunk` from ACP child process and SSE `data: {...}` lines from HTTP upstream
- Produces: Anthropic SSE events sent to `sse_tx` channel

- [ ] **Step 1: Write adapter tests**
Add adapter tests to `tests/test_stream.rs` validating that both ACP input chunks and OpenAI chat completions streaming deltas feed into `AnthropicStreamEngine` identically.

- [ ] **Step 2: Run test to verify it fails**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_stream`

- [ ] **Step 3: Implement `acp_adapter.rs` and `openai_adapter.rs`**
- `acp_adapter.rs`: Bridges `AcpOutputChunk` (`Thinking`, `Text`, `Usage`) into `StreamInput` for `AnthropicStreamEngine`.
- `openai_adapter.rs`: Parses OpenAI SSE json chunks (`choices[0].delta.content`, `choices[0].delta.reasoning_content`) and translates them into `StreamInput`.
- Wire `src/server/handlers/messages.rs` at line 193 (OpenAI stream) and line 472 (ACP stream) to use the new unified engine.

- [ ] **Step 4: Run full stream test suite**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test --test test_stream && CARGO_TARGET_DIR=/home/light/Downloads cargo test --lib protocol::stream`

- [ ] **Step 5: Commit**
```bash
git add src/protocol/stream/acp_adapter.rs src/protocol/stream/openai_adapter.rs src/server/handlers/messages.rs tests/test_stream.rs
git commit -m "refactor(server): integrate unified stream engine for ACP and OpenAI upstreams"
```

---

### Task 6: Full Regression Verification & Staging Port 4002 Verification

**Files:**
- Test: All 83 existing test suites

- [ ] **Step 1: Run full regression test suite**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo test`
Expected: 100% pass on all 49 unit tests and all integration tests.

- [ ] **Step 2: Build release candidate binary**
Run: `CARGO_TARGET_DIR=/home/light/Downloads cargo build --release`

- [ ] **Step 3: Launch staging daemon on port 4002**
Run in background:
`env L2C_DEBUG_LOG=1 /home/light/Downloads/release/lily2claude serve --port 4002`

- [ ] **Step 4: Execute staging health check and smoke test on port 4002**
Send test Anthropic Messages request to `http://127.0.0.1:4002/v1/messages` and verify:
- HTTP 200 OK
- `event: message_start` with valid usage
- `event: content_block_start`
- `event: content_block_delta`
- `event: message_stop`

- [ ] **Step 5: Kill staging daemon on port 4002**
Clean up staging process.

- [ ] **Step 6: Commit test results and staging notes**
```bash
git commit --allow-empty -m "test(staging): verify staging daemon on port 4002 with 100% test pass"
```

---

### Task 7: Controlled Port 4001 Swap & Live Claude Code CLI Verification

**Files:**
- Binary: `/home/light/.cargo/bin/lily2claude`

- [ ] **Step 1: Backup current production binary**
```bash
cp /home/light/.cargo/bin/lily2claude /home/light/.cargo/bin/lily2claude.bak
```

- [ ] **Step 2: Install candidate binary**
```bash
cp /home/light/Downloads/release/lily2claude /home/light/.cargo/bin/lily2claude
```

- [ ] **Step 3: Controlled restart of port 4001 daemon**
- Identify PID from `/tmp/lily2claude_4001.pid` or running background task.
- Terminate old daemon gracefully.
- Start new daemon on port 4001.

- [ ] **Step 4: Live Claude Code CLI Verification**
Execute in terminal:
1. `Bash: pwd` verification:
   `claude --safe-mode --dangerously-skip-permissions --model deepseek -p "Run 'pwd' using Bash and output only the directory."`
2. Subagent spawning via `Agent`:
   `claude --safe-mode --dangerously-skip-permissions --model deepseek -p "Use Agent tool to spawn a subagent that runs 'git status' and summarizes."`
3. Scheduled task via `CronCreate`:
   `claude --safe-mode --dangerously-skip-permissions --model deepseek -p "Use CronCreate to schedule a one-off task in 10 minutes."`
4. Context ledger verification:
   Verify `/context` output metrics.

- [ ] **Step 5: Deliver comprehensive release verification report**
Update `tasks.md` and report release notes to user.
