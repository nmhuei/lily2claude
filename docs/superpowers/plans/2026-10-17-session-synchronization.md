# Session Synchronization & Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement stateful session persistence and synchronization across conversation turns in `lily2claude`, avoiding process recreation and context loss between prompts.

**Architecture:** 
1. Deterministic Conversation Fingerprinting: Compute invariant `session_key` from the conversation root message (`req.messages[0]`).
2. Persistent `AcpSessionPool`: Maintain long-lived ACP child processes (`commandcode`, `kilo`, `cline`) per `(session_key, provider)`. Re-use active ACP `sessionId` for subsequent `session/prompt` calls.
3. OpenCode Session Correlation: Pass consistent `x-opencode-session: ses_<session_key>` across requests.
4. Auto-reaping & Lifecycle Management: 15-minute idle timeout eviction with graceful process termination.

**Tech Stack:** Rust 2021, Tokio Process, Tokio Mutex, SHA-256 (or fast hasher), Serde JSON.

---

### Task 1: Session Fingerprinting and `SessionManager` Core

**Files:**
- Create: `src/engine/session.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/lib.rs`
- Test: `tests/test_session_sync.rs`

**Interfaces:**
- Produces: `pub struct SessionManager`, `pub struct AcpSession`, `pub fn extract_session_key(req: &MessagesRequest) -> String`

- [x] **Step 1: Write unit test for session key derivation and session reuse**
- [x] **Step 2: Implement `src/engine/session.rs` with `AcpSession` and `SessionManager`**
- [x] **Step 3: Export in `src/engine/mod.rs` and `src/lib.rs`**
- [x] **Step 4: Run `cargo test --test test_session_sync` and verify pass**

---

### Task 2: Integrate `SessionManager` into `AcpClient` and Agent Modules

**Files:**
- Modify: `src/adapters/acp_client.rs`
- Modify: `src/agents/mod.rs`
- Modify: `src/agents/commandcode.rs`
- Modify: `src/agents/kilo.rs`
- Modify: `src/agents/cline.rs`
- Test: `tests/test_session_sync.rs`

**Interfaces:**
- Consumes: `SessionManager`
- Produces: Persistent ACP session prompt execution

- [x] **Step 1: Update `Agent::execute_turn` signature to accept optional `session_key`**
- [x] **Step 2: Update `AcpClient` to support persistent session reuse via `SessionManager`**
- [x] **Step 3: Update `commandcode.rs`, `kilo.rs`, `cline.rs` to delegate to persistent session**
- [x] **Step 4: Verify with `cargo test --test test_session_sync`**

---

### Task 3: Refactor Server Dispatching & OpenCode Session Binding

**Files:**
- Modify: `src/server.rs`
- Modify: `src/adapters/http_upstream.rs`
- Test: `tests/test_server.rs`

**Interfaces:**
- Consumes: `SessionManager`, `extract_session_key`
- Produces: Consistent session routing across turns

- [x] **Step 1: Extract `session_key` in `messages_handler`**
- [x] **Step 2: Pass `session_key` to `agent.execute_turn` and `http_adapter.send_request`**
- [x] **Step 3: Set `x-opencode-session` header to `ses_{session_key}`**
- [x] **Step 4: Run full `cargo test` suite**

---

### Task 4: Empirical Multi-Turn Memory Verification

**Files:**
- Binary: `/home/light/GitHub/lily2claude/target/release/lily2claude`
- Binary: `~/.local/bin/l2c`

- [x] **Step 1: Rebuild release binary and reinstall to `~/.local/bin/l2c`**
- [x] **Step 2: Restart daemon server (`l2c serve --port 4001`)**
- [x] **Step 3: Verify multi-turn context retention through real Claude Code interaction**
- [x] **Step 4: Commit all changes to git**
- [x] **Step 5: Conclude goal with `<!-- GOAL_COMPLETE -->`**
