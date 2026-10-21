# CLAUDE.md - Rules for Claude Code & Subagents in lily2claude

> **CRITICAL ARCHITECTURAL CONTRACT (DO NOT MODIFY TOOL COMPILATION LOGIC)**

## Project Overview
`lily2claude` bridges Claude Code CLI to OpenCode (ACP) and local agent backends (DeepSeek, MiniMax, Qwen), with Docker WARP proxy failover and streaming translation.

## Core Rules & Invariants

### 1. Tool Injection (NON-NEGOTIABLE)
- Claude Code owns all tool executions. OpenCode native tools are strictly configured to `deny` in `~/.config/opencode/opencode.jsonc`.
- `req.tools` from Claude Code **MUST ALWAYS** be compiled into XML `<tool_call>` definitions (`compile_tools` / `[Tools Instruction]`) in `src/protocol/prompt.rs`.
- **NEVER** strip, omit, or replace XML tool injection with an ACP "native tools" mode. Doing so strips tool definitions, blinds the upstream LLM, causes monologue halting ("Let me check..."), and kills the autonomous loop.

### 2. Error Handling & Rate Limits
- `FreeUsageLimitError`: Account token exhaustion in `auth.json`. WARP IP rotation CANNOT fix this. Do not trigger proxy rotation on account quota errors.
- HTTP 429 / Cloudflare 403 / `FreeTierError`: Egress IP throttling. Handled automatically by WARP pool failover.

### 3. Build & Deploy Protocol
- **Build**: Always set `CARGO_TARGET_DIR=/home/light/Downloads/` to avoid filling `/tmp` or the root filesystem:
  ```bash
  CARGO_TARGET_DIR=/home/light/Downloads/ cargo build --release
  ```
- **Install**: Always use `install -m 755` instead of `cp` to avoid `Text file busy` errors:
  ```bash
  install -m 755 /home/light/Downloads/release/lily2claude /home/light/.cargo/bin/lily2claude
  ```
- **Restart Daemon**:
  ```bash
  lily2claude server restart --port 4001
  ```
- **Run Tests**:
  ```bash
  CARGO_TARGET_DIR=/home/light/Downloads/ cargo test
  ```

### 4. Baseline Reference
- Git commit `5c3fba5` is the verified stable baseline.
