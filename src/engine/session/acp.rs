use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

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

        // Isolate opencode data directory and inject WARP proxy if available
        if command[0].contains("opencode") {
            cmd.env("XDG_DATA_HOME", "/tmp/lily_opencode_data");

            if let Some(ref pool) = warp_pool {
                if let Some(proxy) = pool.acquire_proxy() {
                    cmd.env("HTTP_PROXY", &proxy);
                    cmd.env("HTTPS_PROXY", &proxy);
                    cmd.env("NO_PROXY", "localhost,127.0.0.1,::1");
                    proxy_used = Some(proxy);
                }
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

        // 1. initialize with 10-second timeout to prevent deadlocks
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

        let init_res = tokio::time::timeout(Duration::from_secs(10), async {
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

        let mut emitted_tool_calls: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(line) = self.reader.next_line().await? {
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
                        if text.contains("provider.quota")
                            || text.contains("Rate limit exceeded")
                            || text.contains("rate limit exceeded")
                        {
                            if let (Some(ref pool), Some(ref proxy)) = (&self.warp_pool, &self.proxy_used) {
                                pool.mark_rate_limited(proxy, 300);
                            }
                            anyhow::bail!("ACP rate limited: {}", text);
                        }
                        let _ = tx.send(crate::protocol::tokens::AcpOutputChunk::Text(text.to_string())).await;
                    }
                }

                if update_type == Some("tool_call") || update_type == Some("tool_call_update") {
                    let call_id = v.pointer("/params/update/toolCallId")
                        .and_then(|id| id.as_str())
                        .unwrap_or("");

                    if !call_id.is_empty() && emitted_tool_calls.contains(call_id) {
                        continue;
                    }

                    let input_val = v.pointer("/params/update/rawInput")
                        .or_else(|| v.pointer("/params/update/input"))
                        .unwrap_or(&serde_json::Value::Null);

                    if input_val.is_null() || input_val.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                        // Skip intermediate empty updates
                        continue;
                    }

                    let (tool_name, tool_input) = if let Some(kind) = v.pointer("/params/update/kind").and_then(|k| k.as_str()) {
                        match kind.to_lowercase().as_str() {
                            "bash" | "execute_bash" | "execute" => ("Bash".to_string(), input_val.clone()),
                            "read" | "file_read" | "view" => ("Read".to_string(), input_val.clone()),
                            "edit" | "file_edit" => ("Edit".to_string(), input_val.clone()),
                            "write" | "file_write" => ("Write".to_string(), input_val.clone()),
                            "skill" | "call_skill" => {
                                let s_name = input_val.get("skill").or_else(|| input_val.get("name")).and_then(|s| s.as_str()).unwrap_or("");
                                ("Skill".to_string(), serde_json::json!({"skill": s_name}))
                            }
                            "other" => {
                                if let Some(skill_name) = input_val.get("skill").or_else(|| input_val.get("name")).and_then(|s| s.as_str()) {
                                    ("Skill".to_string(), serde_json::json!({"skill": skill_name}))
                                } else if let Some(title) = v.pointer("/params/update/title").and_then(|t| t.as_str()) {
                                    (title.to_string(), input_val.clone())
                                } else {
                                    ("Bash".to_string(), input_val.clone())
                                }
                            }
                            "ls" | "dir" | "list_directory" => {
                                let path = input_val.get("path").or_else(|| input_val.get("directory")).and_then(|p| p.as_str()).unwrap_or(".");
                                ("Bash".to_string(), serde_json::json!({"command": format!("ls -la -- {}", shell_escape(path))}))
                            }
                            "grep" | "search_files" => {
                                let pat = input_val.get("pattern").or_else(|| input_val.get("query")).and_then(|p| p.as_str()).unwrap_or("");
                                let path = input_val.get("path").and_then(|p| p.as_str()).unwrap_or(".");
                                ("Bash".to_string(), serde_json::json!({"command": format!("grep -rn -- {} {}", shell_escape(pat), shell_escape(path))}))
                            }
                            "glob" | "find_files" => {
                                let pat = input_val.get("pattern").and_then(|p| p.as_str()).unwrap_or("*");
                                let path = input_val.get("path").and_then(|p| p.as_str()).unwrap_or(".");
                                ("Bash".to_string(), serde_json::json!({"command": format!("find {} -name {}", shell_escape(path), shell_escape(pat))}))
                            }
                            _ => (kind.to_string(), input_val.clone()),
                        }
                    } else if let Some(title) = v.pointer("/params/update/title").and_then(|t| t.as_str()) {
                        if title.to_lowercase().contains("bash") || title.to_lowercase().contains("shell") {
                            ("Bash".to_string(), input_val.clone())
                        } else {
                            (title.to_string(), input_val.clone())
                        }
                    } else {
                        ("Bash".to_string(), input_val.clone())
                    };

                    // Check that tool input is actually complete/usable before emitting to Claude Code
                    let is_ready = match tool_name.to_lowercase().as_str() {
                        "bash" => tool_input.get("command").and_then(|c| c.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false),
                        "read" => tool_input.get("file_path").or_else(|| tool_input.get("path")).and_then(|p| p.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false),
                        "edit" => tool_input.get("file_path").or_else(|| tool_input.get("path")).is_some() && tool_input.get("old_string").is_some(),
                        "write" => tool_input.get("file_path").or_else(|| tool_input.get("path")).is_some(),
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
            if err_str.contains("provider.quota")
                || err_str.contains("Rate limit exceeded")
                || err_str.contains("rate limit")
                || err_str.contains("429")
            {
                if let (Some(ref pool), Some(ref proxy)) = (&self.warp_pool, &self.proxy_used) {
                    pool.mark_rate_limited(proxy, 300);
                }
            }
            anyhow::bail!("ACP session/prompt error: {:?}", err);
        }

        let final_usage = captured_usage.unwrap_or_default();
        if final_usage.total() > 0 {
            let _ = tx.send(crate::protocol::tokens::AcpOutputChunk::Usage(final_usage)).await;
        }
        Ok(final_usage)
    }
}
