use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::info;

pub fn pid_file_path(port: u16) -> PathBuf {
    crate::config::ensure_state_dir().join(format!("lily2claude_{}.pid", port))
}

pub fn log_file_path() -> PathBuf {
    crate::config::ensure_state_dir().join("daemon.log")
}

pub fn is_server_listening(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

pub fn read_pid_file(port: u16) -> Option<u32> {
    let path = pid_file_path(port);
    let pid = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())?;

    #[cfg(unix)]
    {
        // Check if process is still alive with signal 0
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            Some(pid)
        } else {
            // Stale PID file from crashed/killed process, clean it up
            let _ = std::fs::remove_file(&path);
            None
        }
    }
    #[cfg(not(unix))]
    Some(pid)
}

pub fn write_pid_file(port: u16, pid: u32) {
    let path = pid_file_path(port);
    let _ = std::fs::write(path, pid.to_string());
}

pub fn remove_pid_file(port: u16) {
    let path = pid_file_path(port);
    let _ = std::fs::remove_file(path);
}

/// Starts the server daemon completely detached from the controlling terminal.
/// Uses setsid() and ignores SIGHUP so it survives terminal closing / SSH disconnect.
pub fn start_daemon(port: u16, verbose: bool) -> anyhow::Result<u32> {
    if is_server_listening(port) {
        let pid = read_pid_file(port);
        if verbose {
            if let Some(p) = pid {
                println!("⚡ lily2claude daemon is ALREADY running on http://127.0.0.1:{} (PID: {})", port, p);
            } else {
                println!("⚡ lily2claude daemon is ALREADY running on http://127.0.0.1:{}", port);
            }
        }
        return Ok(pid.unwrap_or(0));
    }

    if verbose {
        println!("🚀 Starting lily2claude daemon on http://127.0.0.1:{} (detached background)...", port);
    } else {
        info!(port, "Server not detected on port, starting background daemon...");
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| "lily2claude".into());
    let log_file = crate::config::open_private_append(&log_file_path());

    let (stdout_cfg, stderr_cfg) = match log_file {
        Ok(file) => {
            let stderr_file = file.try_clone().ok();
            (
                Stdio::from(file),
                stderr_file.map(Stdio::from).unwrap_or_else(Stdio::null),
            )
        }
        Err(_) => (Stdio::null(), Stdio::null()),
    };

    let mut cmd = Command::new(exe);
    cmd.arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(stdout_cfg)
        .stderr(stderr_cfg);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                // Detach into new independent session leader
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Ignore SIGHUP so terminal exit never terminates daemon
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    }

    let child = cmd.spawn()?;
    let pid = child.id();
    write_pid_file(port, pid);

    // Poll up to 3 seconds for server bind
    for _ in 0..30 {
        if is_server_listening(port) {
            if verbose {
                println!("✅ Daemon successfully started on http://127.0.0.1:{} (PID: {})", port, pid);
                println!("   Log: {}", log_file_path().display());
            }
            return Ok(pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    anyhow::bail!(
        "Server failed to bind to port {} within 3 seconds. Check log at {}",
        port,
        log_file_path().display()
    );
}

pub fn ensure_server_running(port: u16) {
    if !is_server_listening(port) {
        let _ = start_daemon(port, false);
    }
}

pub fn stop_daemon(port: u16, verbose: bool) -> anyhow::Result<()> {
    let pid = read_pid_file(port).or_else(|| {
        Command::new("lsof")
            .args(["-t", "-i", &format!(":{}", port)])
            .output()
            .ok()
            .and_then(|out| {
                let s = String::from_utf8_lossy(&out.stdout);
                s.lines().next().and_then(|line| line.trim().parse::<u32>().ok())
            })
    });

    if !is_server_listening(port) && pid.is_none() {
        if verbose {
            println!("ℹ️  No lily2claude daemon is running on port {}.", port);
        }
        return Ok(());
    }

    if verbose {
        println!("🛑 Stopping lily2claude daemon on port {}...", port);
    }

    #[cfg(unix)]
    {
        if let Some(p) = pid {
            unsafe {
                let _ = libc::kill(p as i32, libc::SIGTERM);
            }
        }
    }

    // Wait up to 2 seconds for server to exit
    for _ in 0..20 {
        if !is_server_listening(port) {
            remove_pid_file(port);
            if verbose {
                println!("✅ Daemon stopped successfully.");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Force kill if still listening
    #[cfg(unix)]
    {
        if let Some(p) = pid {
            unsafe {
                let _ = libc::kill(p as i32, libc::SIGKILL);
            }
        }
    }

    remove_pid_file(port);
    if verbose {
        println!("✅ Daemon terminated.");
    }
    Ok(())
}

pub fn daemon_status(port: u16) {
    let listening = is_server_listening(port);
    let pid = read_pid_file(port);

    println!("\n=== Lily2Claude Daemon Status ===");
    println!("  Port:      {}", port);
    println!("  Status:    {}", if listening { "🟢 ACTIVE (Listening)" } else { "🔴 STOPPED" });
    if let Some(p) = pid {
        println!("  PID:       {}", p);
    }
    println!("  Log File:  {}", log_file_path().display());
    println!("{:-<35}\n", "");
}
