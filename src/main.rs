use clap::Parser;
use lily2claude::cli::{Cli, Commands, ServerAction};
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
            #[cfg(unix)]
            unsafe {
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
            }
            lily2claude::launcher::daemon::write_pid_file(port, std::process::id());
            let config = Config::load();
            let router = Arc::new(EngineRouter::new(config));
            let app = create_router(router);
            let addr = format!("{}:{}", host, port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            println!("⚡ lily2claude server listening on http://{}", addr);

            let shutdown_signal = async {
                let ctrl_c = async {
                    tokio::signal::ctrl_c()
                        .await
                        .expect("failed to install Ctrl+C handler");
                };

                #[cfg(unix)]
                let terminate = async {
                    if let Ok(mut stream) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                        stream.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                };

                #[cfg(not(unix))]
                let terminate = std::future::pending::<()>();

                tokio::select! {
                    _ = ctrl_c => {},
                    _ = terminate => {},
                }
            };

            axum::serve(listener, app).with_graceful_shutdown(shutdown_signal).await?;
            lily2claude::launcher::daemon::remove_pid_file(port);
        }
        Some(Commands::Server { action, port }) => {
            match action {
                Some(ServerAction::Start) | None => {
                    lily2claude::launcher::daemon::start_daemon(port, true)?;
                }
                Some(ServerAction::Stop) => {
                    lily2claude::launcher::daemon::stop_daemon(port, true)?;
                }
                Some(ServerAction::Restart) => {
                    lily2claude::launcher::daemon::stop_daemon(port, true)?;
                    lily2claude::launcher::daemon::start_daemon(port, true)?;
                }
                Some(ServerAction::Status) => {
                    lily2claude::launcher::daemon::daemon_status(port);
                }
            }
        }
        Some(Commands::Launch {
            model,
            port,
            extra_args,
        }) => {
            launch_claude(&model, port, &extra_args)?;
        }
        Some(Commands::List { free_only: _, all } | Commands::Models { free_only: _, all }) => {
            let registry =
                CatalogRegistry::load_from_system().unwrap_or_else(|_| CatalogRegistry::new());
            let show_free_only = !all;
            let mut list = if show_free_only {
                registry.filter_free_models()
            } else {
                registry.all_models()
            };
            list.sort_by(|a, b| a.id.cmp(&b.id));

            println!(
                "\n=== Lily2Claude Available Models {} ===",
                if show_free_only { "(Free Tier)" } else { "(All Models)" }
            );
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
            if show_free_only {
                println!("💡 Tip: Showing free tier models only. Use 'l2c models --all' to include paid models.");
            }
            println!("💡 To launch Claude Code with any model: l2c -m <alias>\n");
        }
        Some(Commands::Doctor) => {
            run_doctor().await;
        }
        Some(Commands::Stats { port, session, json, reset }) => {
            if reset {
                run_reset_usage(port).await?;
            } else {
                run_stats(port, session, json).await?;
            }
        }
        None => {
            // Default when running `l2c`: Launch Claude Code with default or passed model!
            launch_claude(&cli.model, cli.port, &cli.extra_args)?;
        }
    }
    Ok(())
}

async fn run_reset_usage(port: u16) -> anyhow::Result<()> {
    let url = format!("http://127.0.0.1:{}/v1/usage/reset", port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    let resp = match client.post(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\n❌ Could not connect to lily2claude daemon on port {}.", port);
            eprintln!("   Make sure the server is running: `l2c serve --port {}`\n", port);
            return Err(e.into());
        }
    };

    if resp.status().is_success() {
        println!("\n✅ Successfully reset Lily2Claude token usage ledger and session stats to 0.\n");
    } else {
        eprintln!("\n❌ Failed to reset usage stats: status {}\n", resp.status());
    }
    Ok(())
}

async fn run_stats(port: u16, session_filter: Option<String>, json_output: bool) -> anyhow::Result<()> {
    let url = format!("http://127.0.0.1:{}/v1/stats", port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\n❌ Could not connect to lily2claude daemon on port {}.", port);
            eprintln!("   Make sure the server is running: `l2c serve --port {}`\n", port);
            return Err(e.into());
        }
    };

    if !resp.status().is_success() {
        eprintln!("\n❌ Daemon returned error status: {}\n", resp.status());
        return Ok(());
    }

    let stats: lily2claude::protocol::tokens::TokenStatsSummary = resp.json().await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&stats)?);
        return Ok(());
    }

    let hours = stats.uptime_seconds / 3600;
    let mins = (stats.uptime_seconds % 3600) / 60;
    let secs = stats.uptime_seconds % 60;

    println!("\n📊 === Lily2Claude Token Usage & Performance Dashboard ===");
    println!("  Server: {} v{} | Uptime: {}h {}m {}s | Total Requests: {}",
        stats.service, stats.version, hours, mins, secs, stats.total_requests
    );
    println!("{:-<75}", "");
    println!("📈 Cumulative Token Ledger:");
    println!("  Input Tokens:        {:>10}", stats.cumulative.input_tokens);
    println!("  Output Tokens:       {:>10}", stats.cumulative.output_tokens);
    println!("  Cache Read Tokens:   {:>10}", stats.cumulative.cached_read_tokens);
    println!("  Total Tokens:        {:>10}", stats.cumulative.total_tokens);
    println!("{:-<75}", "");

    // Models Table
    println!("🤖 Model Consumption Breakdown:");
    println!("{:<30} {:<10} {:<14} {:<14} {:<14}", "Model Name", "Requests", "Input", "Output", "Total");
    println!("{:-<86}", "");
    let mut model_list: Vec<_> = stats.models.values().collect();
    model_list.sort_by_key(|b| std::cmp::Reverse(b.total_tokens));
    for m in model_list {
        println!("{:<30} {:<10} {:<14} {:<14} {:<14}",
            m.model_name, m.request_count, m.input_tokens, m.output_tokens, m.total_tokens
        );
    }
    println!("{:-<86}", "");

    // Sessions Table
    println!("🧵 Active Conversation Sessions ({} total):", stats.sessions.len());
    println!("{:<38} {:<10} {:<12} {:<12} {:<12}", "Session ID", "Requests", "Input", "Output", "Total");
    println!("{:-<88}", "");
    let mut session_list: Vec<_> = stats.sessions.iter().filter(|s| {
        if let Some(ref filter) = session_filter {
            s.session_id.contains(filter)
        } else {
            true
        }
    }).collect();
    session_list.sort_by_key(|b| std::cmp::Reverse(b.last_activity_epoch_secs));

    for s in session_list {
        println!("{:<38} {:<10} {:<12} {:<12} {:<12}",
            s.session_id, s.request_count, s.input_tokens, s.output_tokens, s.total_tokens
        );
    }
    println!("{:-<88}\n", "");

    Ok(())
}

async fn run_doctor() {
    println!("\n🩺 === Lily2Claude Environment & DevOps Doctor ===\n");

    // 1. Verify the host WARP client is not nesting container WARP traffic.
    check_host_warp_bypass();

    // 2. Check WARP egress nodes with a real `warp=on` probe.
    check_warp_node("WARP Node 1", 40001).await;
    check_warp_node("WARP Node 2", 40002).await;

    // 3. Dynamic Modular Agent Diagnostics (Zero Hardcoding)
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

    // 4. Check Claude Code CLI
    check_agent_binary("Claude Code", "claude").await;

    println!("\nDoctor check completed.\n");
}

/// The host WARP client must let traffic to the WARP edge bypass its own tunnel.
/// Otherwise container WARP gets nested inside host WARP and dies with
/// `sendmmsg: message too long` because the outer tunnel MTU is 1280.
fn check_host_warp_bypass() {
    const ENDPOINTS: &[(&str, &str)] = &[
        ("ip", "162.159.192.1"),
        ("ip", "162.159.195.1"),
        ("ip", "188.114.96.1"),
        ("ip", "188.114.99.1"),
        ("ip -6", "2606:4700:d0::a29f:c001"),
        ("ip -6", "2606:4700:d1::a29f:c001"),
    ];

    let mut captured = Vec::new();
    let mut checked = 0usize;

    for &(family, endpoint) in ENDPOINTS {
        let args: Vec<&str> = if family == "ip -6" {
            vec!["-6", "route", "get", endpoint]
        } else {
            vec!["route", "get", endpoint]
        };

        if let Ok(output) = std::process::Command::new("ip").args(&args).output() {
            if output.status.success() {
                checked += 1;
                if String::from_utf8_lossy(&output.stdout).contains("CloudflareWARP") {
                    captured.push(endpoint);
                }
            }
        }
    }

    if !captured.is_empty() {
        println!(
            "  [✗] Host WARP       : {} endpoint(s) still routed into CloudflareWARP (nested VPN): {}",
            captured.len(),
            captured.join(", ")
        );
        println!("      Fix: scripts/setup-warp.sh   (or add these exclusions manually)");
        println!("           warp-cli --accept-tos tunnel ip add-range 162.159.192.0/24");
        println!("           warp-cli --accept-tos tunnel ip add-range 162.159.193.0/24");
        println!("           warp-cli --accept-tos tunnel ip add-range 162.159.195.0/24");
        println!("           warp-cli --accept-tos tunnel ip add-range 188.114.96.0/24 ... 99.0/24");
        println!("           warp-cli --accept-tos tunnel ip add-range 2606:4700:d0::/48");
        println!("           warp-cli --accept-tos tunnel ip add-range 2606:4700:d1::/48");
    } else if checked == 0 {
        println!("  [✗] Host WARP       : cannot inspect routes (`ip` unavailable or no route)");
    } else {
        println!(
            "  [✓] Host WARP       : {} WARP edge endpoints bypass the host tunnel",
            checked
        );
    }
}

async fn check_warp_node(name: &str, port: u16) {
    let proxy_url = format!("socks5h://127.0.0.1:{}", port);
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy_url).unwrap())
        .timeout(Duration::from_secs(6))
        .build();

    match client {
        Ok(client) => match client
            .get("https://www.cloudflare.com/cdn-cgi/trace")
            .send()
            .await
        {
            Ok(response) => {
                let body = response.text().await.unwrap_or_default();
                let ip = body
                    .lines()
                    .find_map(|line| line.strip_prefix("ip="))
                    .unwrap_or("unknown");

                match lily2claude::config::warp_pool::parse_warp_trace(&body) {
                    Some(egress_ip) => println!(
                        "  [✓] {:<15} (port {:<5}): ACTIVE -> Egress IP: {} (warp=on)",
                        name, port, egress_ip
                    ),
                    None => println!(
                        "  [✗] {:<15} (port {:<5}): ERROR (egress is not WARP, ip={})",
                        name, port, ip
                    ),
                }
            }
            Err(err) => {
                println!("  [✗] {:<15} (port {:<5}): ERROR ({})", name, port, err);
            }
        },
        Err(err) => {
            println!(
                "  [✗] {:<15} (port {:<5}): FAILED TO BUILD CLIENT ({})",
                name, port, err
            );
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
