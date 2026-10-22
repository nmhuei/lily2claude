use crate::discovery::catalog::{CatalogRegistry, ALIAS_MODELS};
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

    // Same table as `GET /v1/models`, so the picker never offers an alias the
    // router cannot resolve.
    let picker_options: Vec<serde_json::Value> = ALIAS_MODELS
        .iter()
        .map(|m| {
            json!({
                "model": m.id,
                "label": m.label,
                "description": m.description,
                "behavesAs": m.behaves_as,
            })
        })
        .collect();

    // Per-alias limits, keyed by the same ids as the picker.
    let model_settings: serde_json::Map<String, serde_json::Value> = ALIAS_MODELS
        .iter()
        .map(|m| {
            let mut entry = serde_json::Map::new();
            entry.insert("autoCompactWindow".to_string(), json!(m.auto_compact_window));
            if let Some(effort) = m.effort_level {
                entry.insert("effortLevel".to_string(), json!(effort));
            }
            (m.id.to_string(), serde_json::Value::Object(entry))
        })
        .collect();

    let (compact_window, compact_pct) = if context_tokens == 200_000 {
        (context_tokens.saturating_mul(95) / 100, "95")
    } else {
        (context_tokens.saturating_mul(8) / 10, "80")
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
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW": compact_window.to_string(),
            "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": compact_pct,
            "API_TIMEOUT_MS": "600000",
            "CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS": "600000",
            "CLAUDE_STREAM_IDLE_TIMEOUT_MS": "600000",
            "CLAUDE_BYTE_STREAM_IDLE_TIMEOUT_MS": "600000",
        },
        "modelPicker": {
            "options": picker_options,
            "replaceBuiltInOptions": true
        },
        "skipDangerousModePermissionPrompt": true,
        "enableWorkflows": true,
        "disableWorkflows": false,
        "skipWorkflowUsageWarning": true,
        "autoCompactEnabled": true,
        "autoCompactWindow": compact_window,
        "modelSettings": model_settings
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

    let (compact_window, compact_pct) = if ctx == 200_000 {
        (ctx.saturating_mul(95) / 100, "95")
    } else {
        (ctx.saturating_mul(8) / 10, "80")
    };

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
    cmd.env("CLAUDE_CODE_AUTO_COMPACT_WINDOW", compact_window.to_string());
    cmd.env("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE", compact_pct);
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
