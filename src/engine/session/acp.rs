use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tracing::warn;

/// Escapes a shell argument safely using POSIX single-quote escaping
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub struct AcpSession {
    pub session_id: String,
    pub child: Child,
    pub child_pid: Option<u32>,
    pub stdin: ChildStdin,
    pub reader: Lines<BufReader<ChildStdout>>,
    pub last_used: Instant,
    pub rpc_id: u64,
    pub warp_pool: Option<Arc<crate::config::warp_pool::WarpPool>>,
    pub proxy_used: Option<String>,
}

impl AcpSession {
    pub async fn spawn(
        command: &[String],
        warp_pool: Option<Arc<crate::config::warp_pool::WarpPool>>,
    ) -> anyhow::Result<Self> {
        let mut cmd = Command::new(&command[0]);
        cmd.args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut proxy_used = None;

        // Isolate opencode data directory, but preserve existing auth token so the provider
        // doesn't fail with "Authentication required".
        if command[0].contains("opencode") {
            let data_dir = std::path::Path::new("/tmp/lily_opencode_data");
            let opencode_dir = data_dir.join("opencode");
            let _ = std::fs::create_dir_all(&opencode_dir);
            let src_auth = crate::agents::opencode::OpenCodeAgent::auth_path();
            let dst_auth = opencode_dir.join("auth.json");
            if src_auth.exists()
                && (!dst_auth.exists()
                    || std::fs::metadata(&src_auth).map(|m| m.len()).unwrap_or(0)
                        != std::fs::metadata(&dst_auth).map(|m| m.len()).unwrap_or(0))
            {
                let _ = std::fs::copy(&src_auth, &dst_auth);
            }
            cmd.env("XDG_DATA_HOME", "/tmp/lily_opencode_data");
        }

        if let Some(ref pool) = warp_pool {
            let wait = Duration::from_secs(pool.config().acquire_wait_secs.max(1));
            if let Some(proxy) = pool.acquire_proxy_wait(wait).await {
                cmd.env("HTTP_PROXY", &proxy);
                cmd.env("HTTPS_PROXY", &proxy);
                cmd.env("ALL_PROXY", &proxy);
                cmd.env("http_proxy", &proxy);
                cmd.env("https_proxy", &proxy);
                cmd.env("all_proxy", &proxy);
                cmd.env("NO_PROXY", "localhost,127.0.0.1,::1");
                cmd.env("no_proxy", "localhost,127.0.0.1,::1");
                proxy_used = Some(proxy);
            }
        }

        let mut child = cmd.spawn()?;
        let child_pid = child.id();

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let stderr = child.stderr.take().expect("Failed to open stderr");

        // Drain stderr concurrently to avoid deadlocks
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if std::env::var_os("L2C_DEBUG_LOG").is_some() {
                    eprintln!("[ACP STDERR] {}", line);
                }
            }
        });

        let mut reader = BufReader::new(stdout).lines();

        // 1. initialize with 35-second timeout to allow cold daemon initialization
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientInfo": {"name": "lily2claude", "version": "0.1.0"},
                "capabilities": {}
            }
        });
        stdin.write_all(format!("{}\n", init_req).as_bytes()).await?;
        stdin.flush().await?;

        let init_res = tokio::time::timeout(Duration::from_secs(35), async {
            while let Some(line) = reader.next_line().await? {
                if let Ok(val) = serde_json::from_str::<Value>(&line) {
                    if val.get("id").and_then(|id| id.as_u64()) == Some(1) {
                        return Ok(());
                    }
                }
            }
            anyhow::bail!("ACP reader closed during initialize handshake")
        })
        .await;

        match init_res {
            Ok(Ok(())) => {},
            _ => {
                let _ = child.kill().await;
                anyhow::bail!("ACP initialize handshake timed out or failed for {:?}", command);
            }
        }

        Ok(Self {
            session_id: "s_warm".to_string(),
            child,
            child_pid,
            stdin,
            reader,
            last_used: Instant::now(),
            rpc_id: 1,
            warp_pool,
            proxy_used,
        })
    }

    pub async fn send_prompt(
        &mut self,
        model: &str,
        prompt: &str,
        tx: mpsc::Sender<crate::protocol::tokens::AcpOutputChunk>,
    ) -> anyhow::Result<crate::protocol::tokens::TokenUsage> {
        self.last_used = Instant::now();

        // 1. Create a fresh session for this turn to guarantee clean context
        self.rpc_id += 1;
        let session_call_id = self.rpc_id;
        let mut cwd = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("/home/light"))
            .to_string_lossy()
            .to_string();

        for line in prompt.lines() {
            if let Some(rest) = line.trim().strip_prefix("- Primary working directory:") {
                let p = rest.trim();
                let dir = if let Some(idx) = p.find(" (was ") {
                    &p[..idx]
                } else {
                    p
                };
                if std::path::Path::new(dir).is_dir() {
                    cwd = dir.to_string();
                    break;
                }
            }
        }

        let session_req = json!({
            "jsonrpc": "2.0",
            "id": session_call_id,
            "method": "session/new",
            "params": {"cwd": cwd, "mcpServers": []}
        });
        self.stdin.write_all(format!("{}\n", session_req).as_bytes()).await?;
        self.stdin.flush().await?;

        let is_debug = std::env::var_os("L2C_DEBUG_LOG").is_some();
        let session_new_res = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(line) = self.reader.next_line().await? {
                if is_debug {
                    let cut = line.floor_char_boundary(line.len().min(120));
                    eprintln!("[ACP session/new raw] {}", &line[..cut]);
                }
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v.get("id").and_then(|id| id.as_u64()) == Some(session_call_id) {
                        if let Some(err) = v.get("error") {
                            if is_debug {
                                eprintln!("[ACP session/new error] {:?}", err);
                            }
                            anyhow::bail!("ACP session/new failed: {:?}", err);
                        }
                        if let Some(sid) = v.pointer("/result/sessionId").and_then(|s| s.as_str()) {
                            return Ok(sid.to_string());
                        }
                        anyhow::bail!("ACP session/new result missing sessionId");
                    }
                }
            }
            anyhow::bail!("ACP stdout closed while waiting for session/new response")
        }).await;

        let current_session_id = match session_new_res {
            Ok(Ok(sid)) => sid,
            Ok(Err(e)) => anyhow::bail!("ACP session/new error: {}", e),
            Err(_) => anyhow::bail!("ACP session/new timed out after 15s"),
        };
        if is_debug {
            eprintln!("[ACP session/new acquired] sid={}", current_session_id);
        }

        // 2. Set model via session/set_config_option
        if !model.is_empty() {
            self.rpc_id += 1;
            let cfg_call_id = self.rpc_id;
            let cfg_req = json!({
                "jsonrpc": "2.0",
                "id": cfg_call_id,
                "method": "session/set_config_option",
                "params": {
                    "sessionId": current_session_id,
                    "configId": "model",
                    "value": model
                }
            });
            self.stdin.write_all(format!("{}\n", cfg_req).as_bytes()).await?;
            self.stdin.flush().await?;

            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(line) = self.reader.next_line().await? {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v.get("id").and_then(|id| id.as_u64()) == Some(cfg_call_id) {
                            if is_debug {
                                eprintln!("[ACP session/set_config_option response] {:?}", v);
                            }
                            return Ok(());
                        }
                    }
                }
                anyhow::bail!("ACP stdout closed during set_config_option")
            }).await;
        }

        // 3. session/prompt
        self.rpc_id += 1;
        let prompt_call_id = self.rpc_id;
        let prompt_req = json!({
            "jsonrpc": "2.0",
            "id": prompt_call_id,
            "method": "session/prompt",
            "params": {
                "sessionId": current_session_id,
                "model": model,
                "prompt": [{"type": "text", "text": prompt}]
            }
        });
        self.stdin.write_all(format!("{}\n", prompt_req).as_bytes()).await?;
        self.stdin.flush().await?;
        if is_debug {
            eprintln!("[ACP session/prompt sent] id={}", prompt_call_id);
        }

        if is_debug {
            let _ = std::fs::write("/tmp/last_acp_prompt.txt", prompt);
        }
        let mut captured_usage: Option<crate::protocol::tokens::TokenUsage> = None;
        let mut prompt_err = None;

        let mut seen_tool_calls: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut emitted_tool_calls: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            let next_line_future = self.reader.next_line();
            let line_opt = if !emitted_tool_calls.is_empty() && emitted_tool_calls.len() >= seen_tool_calls.len() {
                // All announced tool calls have been emitted to Claude Code.
                // Wait with a short 250ms debounce window to catch any additional
                // parallel tool calls before concluding this turn.
                match tokio::time::timeout(Duration::from_millis(250), next_line_future).await {
                    Ok(res) => res?,
                    Err(_) => {
                        if is_debug {
                            eprintln!(
                                "[ACP tool_call debounce] 250ms window elapsed. Drained {} tools.",
                                emitted_tool_calls.len()
                            );
                        }
                        let cancel_req = json!({
                            "jsonrpc": "2.0",
                            "method": "session/cancel",
                            "params": {
                                "sessionId": current_session_id
                            }
                        });
                        let _ = self.stdin.write_all(format!("{}\n", cancel_req).as_bytes()).await;
                        let _ = self.stdin.flush().await;
                        break;
                    }
                }
            } else if !emitted_tool_calls.is_empty() {
                // There are still announced tool calls waiting for their argument updates.
                // Wait up to 1500ms for arguments to arrive.
                match tokio::time::timeout(Duration::from_millis(1500), next_line_future).await {
                    Ok(res) => res?,
                    Err(_) => {
                        if is_debug {
                            eprintln!(
                                "[ACP tool_call debounce] Timed out waiting for arguments of pending tool calls. Drained {} tools.",
                                emitted_tool_calls.len()
                            );
                        }
                        let cancel_req = json!({
                            "jsonrpc": "2.0",
                            "method": "session/cancel",
                            "params": {
                                "sessionId": current_session_id
                            }
                        });
                        let _ = self.stdin.write_all(format!("{}\n", cancel_req).as_bytes()).await;
                        let _ = self.stdin.flush().await;
                        break;
                    }
                }
            } else {
                next_line_future.await?
            };

            let Some(line) = line_opt else {
                break;
            };

            if is_debug {
                let cut = line.floor_char_boundary(line.len().min(120));
                eprintln!("[ACP prompt raw] {}", &line[..cut]);
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/acp_raw.log") {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", line);
                }
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                // Reject internal ACP tool execution: OpenCode is purely an LLM inference provider,
                // and all tool execution belongs strictly to the Claude Code harness.
                if let (Some(method), Some(req_id)) = (v.get("method").and_then(|m| m.as_str()), v.get("id")) {
                    if method == "session/request_permission" {
                        let reply = json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "outcome": {
                                    "outcome": "rejected"
                                }
                            }
                        });
                        self.stdin.write_all(format!("{}\n", reply).as_bytes()).await?;
                        self.stdin.flush().await?;
                        if is_debug {
                            eprintln!("[ACP request_permission rejected for internal tool execution] id={:?}", req_id);
                        }
                        if !emitted_tool_calls.is_empty() {
                            let cancel_req = json!({
                                "jsonrpc": "2.0",
                                "method": "session/cancel",
                                "params": {
                                    "sessionId": current_session_id
                                }
                            });
                            let _ = self.stdin.write_all(format!("{}\n", cancel_req).as_bytes()).await;
                            let _ = self.stdin.flush().await;
                            break;
                        }
                        continue;
                    }
                }

                // Filter notifications: if line contains a sessionId, verify it matches current_session_id!
                if let Some(sid) = v.pointer("/params/sessionId")
                    .or_else(|| v.pointer("/params/update/sessionId"))
                    .and_then(|s| s.as_str())
                {
                    if sid != current_session_id {
                        continue;
                    }
                }

                let update_type = v.pointer("/params/update/sessionUpdate").and_then(|u| u.as_str());
                if update_type == Some("agent_thought_chunk") {
                    if let Some(thought) = v
                        .pointer("/params/update/content/text")
                        .or_else(|| v.pointer("/params/update/content/0/text"))
                        .and_then(|t| t.as_str())
                    {
                        if !thought.is_empty() {
                            let _ = tx.send(crate::protocol::tokens::AcpOutputChunk::Thinking(thought.to_string())).await;
                        }
                    }
                    continue;
                }

                if let Some(usage) = crate::protocol::tokens::extract_acp_usage(&v) {
                    captured_usage = Some(usage);
                }

                // Parse text chunks from either object or array
                if let Some(text) = v
                    .pointer("/params/update/content/text")
                    .or_else(|| v.pointer("/params/update/content/0/text"))
                    .or_else(|| v.pointer("/params/content/text"))
                    .or_else(|| v.pointer("/params/content"))
                    .and_then(|t| t.as_str())
                {
                    if !text.is_empty() {
                        if crate::config::warp_pool::is_upstream_rate_limited(text) {
                            if let (Some(ref pool), Some(ref proxy)) = (&self.warp_pool, &self.proxy_used) {
                                warn!(
                                    proxy = %proxy,
                                    "ACP upstream rate limited; cooling this WARP node and triggering fast rotation"
                                );
                                pool.mark_rate_limited(proxy, 300);
                            }
                            anyhow::bail!("ACP rate limited: {}", text);
                        }
                        let _ = tx.send(crate::protocol::tokens::AcpOutputChunk::Text(text.to_string())).await;
                    }
                }

                // Post-tool boundary guard: if tools have already been emitted and OpenCode moves on
                // to agent text or completion, conclude the turn.
                if !emitted_tool_calls.is_empty() && (update_type == Some("agent_message_chunk") || update_type == Some("completed")) {
                    if is_debug {
                        eprintln!("[ACP tool_call drain] Post-tool chunk arrived (update={:?}) - concluding turn", update_type);
                    }
                    let cancel_req = json!({
                        "jsonrpc": "2.0",
                        "method": "session/cancel",
                        "params": {
                            "sessionId": current_session_id
                        }
                    });
                    let _ = self.stdin.write_all(format!("{}\n", cancel_req).as_bytes()).await;
                    let _ = self.stdin.flush().await;
                    break;
                }

                if update_type == Some("tool_call") || update_type == Some("tool_call_update") {
                    let call_id = v.pointer("/params/update/toolCallId")
                        .and_then(|id| id.as_str())
                        .unwrap_or("");

                    if !call_id.is_empty() {
                        seen_tool_calls.insert(call_id.to_string());
                    }

                    if !call_id.is_empty() && emitted_tool_calls.contains(call_id) {
                        continue;
                    }

                    let input_val = v.pointer("/params/update/rawInput")
                        .or_else(|| v.pointer("/params/update/input"))
                        .unwrap_or(&serde_json::Value::Null);

                    let raw_name = v.pointer("/params/update/name")
                        .or_else(|| v.pointer("/params/update/title"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    let kind = v.pointer("/params/update/kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("");
                    let status = v.pointer("/params/update/status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");

                    let input_is_empty = input_val.is_null()
                        || input_val.as_object().map(|o| o.is_empty()).unwrap_or(true);

                    if input_is_empty {
                        let name_l = raw_name.to_lowercase();
                        let kind_l = kind.to_lowercase();
                        let expects_input = matches!(
                            name_l.as_str(),
                            "bash" | "execute_bash" | "shell" | "read" | "file_read" | "view"
                                | "edit" | "file_edit" | "write" | "file_write" | "glob"
                                | "find_files" | "grep" | "search_files" | "skill" | "call_skill"
                                | "ls" | "dir" | "list_directory"
                        ) || matches!(
                            kind_l.as_str(),
                            "bash" | "execute" | "read" | "edit" | "write" | "skill"
                        );

                        // Argument-bearing tools send an empty placeholder first; ignore it
                        // and wait for the populated `tool_call_update`. A genuinely
                        // parameterless tool (e.g. CronList) stays empty forever, so emit it
                        // once ACP confirms the call is starting instead of swallowing it.
                        let started = matches!(status, "in_progress" | "completed" | "failed");
                        if expects_input || !started {
                            continue;
                        }
                    }

                    let input_val = if input_is_empty {
                        serde_json::json!({})
                    } else {
                        input_val.clone()
                    };

                    let (tool_name, tool_input) = match (raw_name.to_lowercase().as_str(), kind.to_lowercase().as_str()) {
                        ("bash", _) | ("execute_bash", _) | ("shell", _) | (_, "bash") | (_, "execute") => {
                            ("Bash".to_string(), input_val.clone())
                        }
                        ("read", _) | ("file_read", _) | ("view", _) | (_, "read") => {
                            let p = input_val.get("file_path").or_else(|| input_val.get("path")).and_then(|p| p.as_str()).unwrap_or("");
                            if !p.is_empty() && std::path::Path::new(p).is_dir() {
                                ("Bash".to_string(), serde_json::json!({"command": format!("ls -la -- {}", shell_escape(p))}))
                            } else {
                                ("Read".to_string(), input_val.clone())
                            }
                        }
                        ("edit", _) | ("file_edit", _) | (_, "edit") => {
                            ("Edit".to_string(), input_val.clone())
                        }
                        ("write", _) | ("file_write", _) | (_, "write") => {
                            ("Write".to_string(), input_val.clone())
                        }
                        ("glob", _) | ("find_files", _) => {
                            ("Glob".to_string(), input_val.clone())
                        }
                        ("grep", _) | ("search_files", _) => {
                            ("Grep".to_string(), input_val.clone())
                        }
                        ("skill", _) | ("call_skill", _) | (_, "skill") => {
                            let s_name = input_val.get("skill").or_else(|| input_val.get("name")).and_then(|s| s.as_str()).unwrap_or("");
                            ("Skill".to_string(), serde_json::json!({"skill": s_name}))
                        }
                        ("ls" | "dir" | "list_directory", _) => {
                            let path = input_val.get("path").or_else(|| input_val.get("directory")).and_then(|p| p.as_str()).unwrap_or(".");
                            ("Bash".to_string(), serde_json::json!({"command": format!("ls -la -- {}", shell_escape(path))}))
                        }
                        _ => {
                            let t_name = if !raw_name.is_empty() {
                                raw_name.to_string()
                            } else if !kind.is_empty() {
                                kind.to_string()
                            } else {
                                "Bash".to_string()
                            };
                            (t_name, input_val.clone())
                        }
                    };

                    // Check that tool input is actually complete/usable before emitting to Claude Code
                    let is_ready = match tool_name.to_lowercase().as_str() {
                        "bash" => tool_input.get("command").and_then(|c| c.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false),
                        "read" => tool_input.get("file_path").or_else(|| tool_input.get("path")).and_then(|p| p.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false),
                        "edit" => tool_input.get("file_path").or_else(|| tool_input.get("path")).is_some() && (tool_input.get("old_string").is_some() || tool_input.get("new_string").is_some()),
                        "write" => tool_input.get("file_path").or_else(|| tool_input.get("path")).is_some(),
                        "glob" => tool_input.get("pattern").is_some(),
                        "grep" => tool_input.get("pattern").is_some() || tool_input.get("query").is_some(),
                        "skill" => tool_input.get("skill").or_else(|| tool_input.get("name")).is_some(),
                        _ => true,
                    };

                    if !is_ready {
                        continue;
                    }

                    if !call_id.is_empty() {
                        emitted_tool_calls.insert(call_id.to_string());
                    }

                    let tool_str = format!("\n<tool_call>\n{{\"name\": \"{}\", \"arguments\": {}}}\n</tool_call>\n", tool_name, tool_input);
                    let _ = tx.send(crate::protocol::tokens::AcpOutputChunk::Text(tool_str)).await;

                    if is_debug {
                        eprintln!(
                            "[ACP tool_call guard] Intercepted tool call '{}' (id={}) - delegated to Claude Code (emitted: {}/{})",
                            tool_name, call_id, emitted_tool_calls.len(), seen_tool_calls.len()
                        );
                    }
                    continue;
                }

                if let Some(err) = v.get("error") {
                    if v.get("id").and_then(|id| id.as_u64()) == Some(prompt_call_id) {
                        if is_debug {
                            eprintln!(">>> ACP RPC ERROR for prompt id {}: {:?}", prompt_call_id, err);
                        }
                        prompt_err = Some(err.clone());
                    } else {
                        if is_debug {
                            eprintln!(">>> ACP RPC ERROR ignoring mismatched id {:?} (expected {}): {:?}", v.get("id"), prompt_call_id, err);
                        }
                    }
                }

                // Check stop conditions
                let is_matching_result = v.get("id").and_then(|id| id.as_u64()) == Some(prompt_call_id);
                let is_stop_notification = v.pointer("/params/update/stopReason").is_some()
                    || v.pointer("/params/stopReason").is_some()
                    || v.pointer("/params/update/finish").is_some()
                    || v.pointer("/params/finish").is_some()
                    || v.pointer("/params/update/error").is_some()
                    || update_type == Some("turn_end")
                    || update_type == Some("completed")
                    || update_type == Some("error");
                let is_stop_result = is_matching_result
                    && (v.pointer("/result/stopReason").is_some()
                        || v.get("result").is_some()
                        || v.get("error").is_some());

                if is_stop_notification || is_stop_result {
                    if is_debug {
                        eprintln!(
                            "[ACP prompt broke] is_stop_notification={} is_stop_result={}",
                            is_stop_notification, is_stop_result
                        );
                    }
                    if !emitted_tool_calls.is_empty() {
                        let cancel_req = json!({
                            "jsonrpc": "2.0",
                            "method": "session/cancel",
                            "params": {
                                "sessionId": current_session_id
                            }
                        });
                        let _ = self.stdin.write_all(format!("{}\n", cancel_req).as_bytes()).await;
                        let _ = self.stdin.flush().await;
                    }
                    break;
                }
            }
        }
        if is_debug {
            eprintln!("[ACP prompt reader ended] final_usage={:?} prompt_err={:?}", captured_usage, prompt_err);
            eprintln!("[ACP child exit status] {:?}", self.child.try_wait());
        }

        if let Some(err) = prompt_err {
            let err_str = err.to_string();
            if crate::config::warp_pool::is_upstream_rate_limited(&err_str) {
                if let (Some(ref pool), Some(ref proxy)) = (&self.warp_pool, &self.proxy_used) {
                    warn!(
                        proxy = %proxy,
                        "ACP session/prompt rate limited; cooling this WARP node and triggering fast rotation"
                    );
                    pool.mark_rate_limited(proxy, 300);
                }
            }
            anyhow::bail!("ACP session/prompt error: {:?}", err);
        }

        let final_usage = captured_usage.unwrap_or_default();
        if final_usage.total() > 0 {
            let _ = tx.send(crate::protocol::tokens::AcpOutputChunk::Usage(final_usage)).await;
        }
        if let (Some(ref pool), Some(ref proxy)) = (&self.warp_pool, &self.proxy_used) {
            pool.report_success(proxy);
        }
        Ok(final_usage)
    }
}
