use clap::Parser;
use lily2claude::cli::{Cli, Commands};
use lily2claude::config::Config;
use lily2claude::discovery::catalog::CatalogRegistry;
use lily2claude::engine::router::EngineRouter;
use lily2claude::launcher::claude::launch_claude;
use lily2claude::server::create_router;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Serve { port, host }) => {
            let config = Config::load_default();
            let router = Arc::new(EngineRouter::new(config));
            let app = create_router(router);
            let addr = format!("{}:{}", host, port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            println!("⚡ lily2claude server listening on http://{}", addr);
            axum::serve(listener, app).await?;
        }
        Some(Commands::Launch {
            model,
            port,
            extra_args,
        }) => {
            launch_claude(&model, port, &extra_args)?;
        }
        Some(Commands::List { free_only } | Commands::Models { free_only }) => {
            let registry =
                CatalogRegistry::load_from_system().unwrap_or_else(|_| CatalogRegistry::new());
            let mut list = if free_only {
                registry.filter_free_models()
            } else {
                registry.all_models()
            };
            list.sort_by(|a, b| a.id.cmp(&b.id));

            println!("\n=== Lily2Claude Available Models & Transparent Aliases ===");
            println!(
                "{:<35} {:<15} {:<12} {:<10}",
                "Model / Alias", "Context Window", "Max Output", "Tier"
            );
            println!("{:-<75}", "");
            for m in list {
                println!(
                    "{:<35} {:<15} {:<12} {:<10}",
                    m.id,
                    format!("{} tokens", m.context_length),
                    format!("{} tokens", m.max_output_tokens),
                    if m.is_free { "FREE" } else { "PAID" }
                );
            }
            println!("{:-<75}", "");
            println!("💡 To launch Claude Code with any model: l2c -m <alias>\n");
        }
        Some(Commands::Doctor) => {
            run_doctor().await;
        }
        None => {
            // Default when running `l2c`: Launch Claude Code with default or passed model!
            launch_claude(&cli.model, cli.port, &cli.extra_args)?;
        }
    }
    Ok(())
}

async fn run_doctor() {
    println!("\n🩺 === Lily2Claude Environment & DevOps Doctor ===\n");

    // 1. Check Docker WARP Nodes
    check_warp_node("WARP Node 1", 40001).await;
    check_warp_node("WARP Node 2", 40002).await;

    // 2. Dynamic Modular Agent Diagnostics (Zero Hardcoding)
    let agent_reg = lily2claude::agents::AgentRegistry::default();
    for diag in agent_reg.diagnose_all() {
        if diag.installed {
            let ver = diag.version.unwrap_or_else(|| "detected".to_string());
            let auth_str = match diag.auth_status {
                lily2claude::agents::AuthStatus::Ready => "auth: ready".to_string(),
                lily2claude::agents::AuthStatus::Missing(ref f) => format!("auth: missing ({})", f),
                lily2claude::agents::AuthStatus::NotRequired => "auth: none required".to_string(),
            };
            println!("  [✓] {:<15}: DETECTED ({}) [{}]", diag.display_name, ver, auth_str);
        } else {
            println!("  [✗] {:<15}: NOT FOUND", diag.display_name);
        }
    }

    // 3. Check Claude Code CLI
    check_agent_binary("Claude Code", "claude").await;

    println!("\nDoctor check completed.\n");
}

async fn check_warp_node(name: &str, port: u16) {
    let proxy_url = format!("socks5h://127.0.0.1:{}", port);
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy_url).unwrap())
        .timeout(Duration::from_secs(4))
        .build();

    match client {
        Ok(c) => match c.get("https://ifconfig.me/ip").send().await {
            Ok(resp) => {
                let ip = resp.text().await.unwrap_or_default().trim().to_string();
                println!("  [✓] {:<15} (port {:<5}): ACTIVE -> Egress IP: {}", name, port, ip);
            }
            Err(e) => {
                println!("  [✗] {:<15} (port {:<5}): ERROR ({})", name, port, e);
            }
        },
        Err(e) => {
            println!("  [✗] {:<15} (port {:<5}): FAILED TO BUILD CLIENT ({})", name, port, e);
        }
    }
}

async fn check_agent_binary(name: &str, path: &str) {
    match tokio::process::Command::new(path).arg("--version").output().await {
        Ok(out) => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let ver_clean = ver.lines().next().unwrap_or(&ver);
            println!("  [✓] {:<15}: DETECTED ({})", name, ver_clean);
        }
        Err(_) => {
            println!("  [✗] {:<15}: NOT FOUND ({})", name, path);
        }
    }
}
