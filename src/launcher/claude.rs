use crate::discovery::catalog::CatalogRegistry;
use serde_json::json;
use std::process::Command;
use std::time::Duration;
use tracing::info;

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
            "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT": "1",
            "CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK": "1",
            "CLAUDE_CODE_NO_MODEL_FALLBACK": "1",
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS": context_tokens.to_string(),
        }
    })
    .to_string()
}

pub fn ensure_server_running(port: u16) {
    if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
        return; // Server is already listening
    }

    info!(port, "Server not detected on port, starting background daemon...");
    let exe = std::env::current_exe().unwrap_or_else(|_| "lily2claude".into());
    let _ = Command::new(exe)
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .spawn();

    // Small wait for server bind
    std::thread::sleep(Duration::from_millis(500));
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
    cmd.env("CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT", "1");
    cmd.env("CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK", "1");
    cmd.env("CLAUDE_CODE_NO_MODEL_FALLBACK", "1");
    cmd.env("CLAUDE_CODE_MAX_CONTEXT_TOKENS", ctx.to_string());

    cmd.arg("--settings").arg(&settings);
    cmd.arg("--model").arg(&cli_model);

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

    for arg in extra_args {
        cmd.arg(arg);
    }

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("Claude Code exited with status {}", status);
    }
    Ok(())
}
