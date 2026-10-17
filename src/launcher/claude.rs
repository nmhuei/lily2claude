use crate::discovery::catalog::CatalogRegistry;
use serde_json::json;
use std::process::Command;
use std::time::Duration;
use tracing::info;

pub fn build_settings_json(model: &str, port: u16, context_tokens: u32) -> String {
    json!({
        "env": {
            "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{}", port),
            "ANTHROPIC_API_KEY": "dummy",
            "ANTHROPIC_MODEL": model,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": model,
            "ANTHROPIC_SMALL_FAST_MODEL": model,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
            "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
            "CLAUDE_CODE_SUBAGENT_MODEL": model,
            "CLAUDE_CODE_SUBAGENT_MODEL_FORCE": "1",
            "CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL": model,
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

    println!("⚡ lily2claude: Launching Claude Code with model '{}' (context limit: {} tokens)...", model, ctx);

    let mut cmd = Command::new("claude");
    cmd.env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", port));
    cmd.env("ANTHROPIC_API_KEY", "dummy");
    cmd.env("ANTHROPIC_MODEL", model);
    cmd.env("ANTHROPIC_DEFAULT_HAIKU_MODEL", model);
    cmd.env("ANTHROPIC_SMALL_FAST_MODEL", model);
    cmd.env("ANTHROPIC_DEFAULT_SONNET_MODEL", model);
    cmd.env("ANTHROPIC_DEFAULT_OPUS_MODEL", model);
    cmd.env("CLAUDE_CODE_SUBAGENT_MODEL", model);
    cmd.env("CLAUDE_CODE_SUBAGENT_MODEL_FORCE", "1");
    cmd.env("CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL", model);
    cmd.env("CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT", "1");
    cmd.env("CLAUDE_CODE_DISABLE_MODEL_ACCESS_FALLBACK", "1");
    cmd.env("CLAUDE_CODE_NO_MODEL_FALLBACK", "1");
    cmd.env("CLAUDE_CODE_MAX_CONTEXT_TOKENS", ctx.to_string());

    cmd.arg("--settings").arg(&settings);
    cmd.arg("--model").arg(model);
    for arg in extra_args {
        cmd.arg(arg);
    }

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("Claude Code exited with status {}", status);
    }
    Ok(())
}
