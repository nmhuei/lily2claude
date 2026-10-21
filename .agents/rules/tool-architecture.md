# Tool Execution & Compilation Architecture Rules

## Summary
- Upstream agent (OpenCode) has native tools DENIED. It is solely an LLM completion bridge.
- Claude Code is the sole executor of all tools on the host.
- `req.tools` must ALWAYS be transformed into XML tool calling schemas via `compile_tools` in `src/protocol/prompt.rs`.
- Do not create native ACP tool modes that skip tool schema prompt injection.
- Reference commit: `5c3fba5`.
