use crate::discovery::catalog::CatalogRegistry;
use serde_json::json;
use std::process::Command;

pub fn build_settings_json(model: &str, port: u16, context_tokens: u32) -> String {
    let cfg = crate::config::Config::load();
    let l = &cfg.launcher;
    let main_model = if model.contains("claude") {
        model
    } else {
        &l.anthropic_model
    };

    json!({
        "env": {
            "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{}", port),
            "ANTHROPIC_API_KEY": "dummy",
            "ANTHROPIC_MODEL": main_model,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": &l.haiku_model,
            "ANTHROPIC_SMALL_FAST_MODEL": &l.haiku_model,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": &l.sonnet_model,
            "ANTHROPIC_DEFAULT_OPUS_MODEL": &l.opus_model,
            "CLAUDE_CODE_SUBAGENT_MODEL": &l.subagent_model,
            "CLAUDE_CODE_SUBAGENT_MODEL_FORCE": "1",
            "CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL": &l.subagent_model,
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1",
            "CLAUDE_CODE_WORKFLOWS": "1",
            "CLAUDE_CODE_FORK_SUBAGENT": "0",
            "CLAUDE_AUTO_BACKGROUND_TASKS": "1",
            "CLAUDE_CODE_ENABLE_TASKS": "1",
            "CLAUDE_CODE_AGENT_PROGRESS_SUMMARIES": "1",
            "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT": "1",
            "CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK": "1",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": context_tokens.to_string(),
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW": (context_tokens.saturating_mul(8) / 10).to_string(),
            "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "80",
            "API_TIMEOUT_MS": "600000",
            "CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS": "600000",
            "CLAUDE_STREAM_IDLE_TIMEOUT_MS": "600000",
            "CLAUDE_BYTE_STREAM_IDLE_TIMEOUT_MS": "600000",
        },
        "modelPicker": {
            "options": [
                {
                    "model": "deepseek",
                    "label": "DeepSeek V4.1 Flash (deepseek)",
                    "description": "DeepSeek V4.1 Flash Free via OpenCode (200k Context · Default Coding Model)",
                    "behavesAs": "claude-sonnet-4-5"
                },
                {
                    "model": "glm",
                    "label": "GLM 5.3 Flash (glm)",
                    "description": "GLM 5.3 Flash Free via OpenCode (200k Context · High Intelligence & Reasoning)",
                    "behavesAs": "claude-sonnet-4-5"
                },
                {
                    "model": "claude-haiku-4-5-20251001",
                    "label": "Haiku 4.5 (haiku)",
                    "description": "Claude Haiku 4.5 · Fast Subagent Engine (backed by Free DeepSeek/MiMo)",
                    "behavesAs": "claude-haiku-4-5"
                },
                {
                    "model": "mimo",
                    "label": "MiMo V2.6 Flash (mimo)",
                    "description": "Xiaomi MiMo V2.6 Flash Free via OpenCode (64k Context · Ultra Low Latency)",
                    "behavesAs": "claude-haiku-4-5"
                },
                {
                    "model": "nemotron",
                    "label": "Nemotron 3 Nano (nemotron)",
                    "description": "NVIDIA Nemotron 3 Free via OpenCode (1M Context · Large Codebase)",
                    "behavesAs": "claude-opus-4-6"
                },
                {
                    "model": "qwen",
                    "label": "Qwen 3.6 Plus (qwen)",
                    "description": "Alibaba Qwen 3.6 Plus Free via OpenCode (262k Context · Math & Code)",
                    "behavesAs": "claude-sonnet-4-5"
                },
                {
                    "model": "minimax",
                    "label": "MiniMax M3 (minimax)",
                    "description": "MiniMax M3 Free via OpenCode (200k Context · General Assistant)",
                    "behavesAs": "claude-sonnet-4-5"
                },
                {
                    "model": "kimi",
                    "label": "Kimi K2.5 (kimi)",
                    "description": "Moonshot Kimi K2.5 Free via OpenCode (262k Context · Long Context Agent)",
                    "behavesAs": "claude-sonnet-4-5"
                },
                {
                    "model": "kilo",
                    "label": "Kilo Auto Free (kilo)",
                    "description": "Kilo Auto Free Router (262k Context · Dynamic Multi-Provider)",
                    "behavesAs": "claude-sonnet-4-5"
                }
            ],
            "replaceBuiltInOptions": true
        },
        "skipDangerousModePermissionPrompt": true,
        "enableWorkflows": true,
        "disableWorkflows": false,
        "skipWorkflowUsageWarning": true,
        "autoCompactEnabled": true,
        "autoCompactWindow": context_tokens.saturating_mul(8) / 10,
        "modelSettings": {
            "deepseek": { "autoCompactWindow": 160000, "effortLevel": "high" },
            "glm": { "autoCompactWindow": 160000, "effortLevel": "high" },
            "mimo": { "autoCompactWindow": 50000 },
            "nemotron": { "autoCompactWindow": 800000 },
            "qwen": { "autoCompactWindow": 200000 },
            "minimax": { "autoCompactWindow": 160000 },
            "kimi": { "autoCompactWindow": 200000 },
            "kilo": { "autoCompactWindow": 200000 }
        }
    })
    .to_string()
}

pub fn ensure_server_running(port: u16) {
    crate::launcher::daemon::ensure_server_running(port);
}

pub fn launch_claude(model: &str, port: u16, extra_args: &[String]) -> anyhow::Result<()> {
    ensure_server_running(port);

    let registry = CatalogRegistry::load_from_system().unwrap_or_else(|_| CatalogRegistry::new());
    let ctx = registry
        .get_metadata(model)
        .map(|m| m.context_length)
        .unwrap_or(200000);

    let settings = build_settings_json(model, port, ctx);
    let cfg = crate::config::Config::load();
    let l = &cfg.launcher;
    let cli_model = if model.contains("claude") {
        model.to_string()
    } else {
        l.anthropic_model.clone()
    };

    println!("⚡ lily2claude: Launching Claude Code with model '{}' (context limit: {} tokens)...", model, ctx);

    let mut cmd = Command::new("claude");
    cmd.env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", port));
    cmd.env("ANTHROPIC_API_KEY", "dummy");
    cmd.env("ANTHROPIC_MODEL", &cli_model);
    cmd.env("ANTHROPIC_DEFAULT_HAIKU_MODEL", &l.haiku_model);
    cmd.env("ANTHROPIC_SMALL_FAST_MODEL", &l.haiku_model);
    cmd.env("ANTHROPIC_DEFAULT_SONNET_MODEL", &l.sonnet_model);
    cmd.env("ANTHROPIC_DEFAULT_OPUS_MODEL", &l.opus_model);
    cmd.env("CLAUDE_CODE_SUBAGENT_MODEL", &l.subagent_model);
    cmd.env("CLAUDE_CODE_SUBAGENT_MODEL_FORCE", "1");
    cmd.env("CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL", &l.subagent_model);
    cmd.env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
    cmd.env("CLAUDE_CODE_WORKFLOWS", "1");
    cmd.env("CLAUDE_CODE_FORK_SUBAGENT", "0");
    cmd.env("CLAUDE_AUTO_BACKGROUND_TASKS", "1");
    cmd.env("CLAUDE_CODE_ENABLE_TASKS", "1");
    cmd.env("CLAUDE_CODE_AGENT_PROGRESS_SUMMARIES", "1");
    cmd.env("CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT", "1");
    cmd.env("CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK", "1");
    cmd.env("CLAUDE_CODE_NO_MODEL_FALLBACK", "1");
    cmd.env("CLAUDE_CODE_MAX_CONTEXT_TOKENS", ctx.to_string());
    cmd.env("CLAUDE_CODE_AUTO_COMPACT_WINDOW", (ctx.saturating_mul(8) / 10).to_string());
    cmd.env("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE", "80");
    cmd.env("API_TIMEOUT_MS", "600000");
    cmd.env("CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS", "600000");
    cmd.env("CLAUDE_STREAM_IDLE_TIMEOUT_MS", "600000");
    cmd.env("CLAUDE_BYTE_STREAM_IDLE_TIMEOUT_MS", "600000");
    cmd.env("CLAUDE_CODE_SKIP_DANGEROUS_MODE_PERMISSION_PROMPT", "1");

    cmd.arg("--settings").arg(&settings);
    cmd.arg("--model").arg(&cli_model);
    if !extra_args.iter().any(|a| a == "--autocompact" || a.starts_with("--autocompact=")) {
        cmd.arg("--autocompact").arg("auto");
    }

    // Default to --dangerously-skip-permissions unless user explicitly passed safe/permission flags
    let has_permission_arg = extra_args.iter().any(|a| {
        a == "--dangerously-skip-permissions"
            || a == "--safe-mode"
            || a.starts_with("--permission-mode")
            || a.starts_with("--permission-prompts")
    });
    if !has_permission_arg && l.dangerously_skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }

    let has_session_arg = extra_args.iter().any(|a| {
        a == "--session-id"
            || a == "-r"
            || a == "--resume"
            || a.starts_with("--session-id=")
            || a.starts_with("-r=")
            || a.starts_with("--resume=")
    });
    if !has_session_arg {
        let fresh_id = uuid::Uuid::new_v4().to_string();
        cmd.arg("--session-id").arg(fresh_id);
    }

    let mut i = 0;
    while i < extra_args.len() {
        let arg = &extra_args[i];
        if arg == "--effort" && i + 1 < extra_args.len() && extra_args[i + 1].eq_ignore_ascii_case("ultra") {
            cmd.arg("--effort").arg("max");
            i += 2;
        } else if arg.eq_ignore_ascii_case("--effort=ultra") {
            cmd.arg("--effort=max");
            i += 1;
        } else {
            cmd.arg(arg);
            i += 1;
        }
    }

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("Claude Code exited with status {}", status);
    }
    Ok(())
}
