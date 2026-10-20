use crate::protocol::tokens::{extract_acp_usage, AcpOutputChunk, TokenUsage};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

pub struct AcpClient {
    command: Vec<String>,
}

impl AcpClient {
    pub fn new(command: Vec<String>) -> Self {
        Self { command }
    }

    pub fn command(&self) -> &[String] {
        &self.command
    }

    pub async fn run_prompt(
        &self,
        model: &str,
        prompt: &str,
        tx: mpsc::Sender<AcpOutputChunk>,
    ) -> anyhow::Result<TokenUsage> {

        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Isolate opencode data directory
        if self.command[0].contains("opencode") {
            cmd.env("XDG_DATA_HOME", "/tmp/lily_opencode_data");

            if let Ok(proxy) = std::env::var("L2C_PROXY") {
                cmd.env("HTTP_PROXY", &proxy);
                cmd.env("HTTPS_PROXY", &proxy);
                cmd.env("NO_PROXY", "localhost,127.0.0.1,::1");
            }
        }

        let mut child = cmd.spawn()?;

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let stderr = child.stderr.take().expect("Failed to open stderr");

        // Concurrently drain stderr to avoid pipe buffer deadlocks
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });

        let mut reader = BufReader::new(stdout).lines();

        // 1. initialize
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
        let init_res = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(line) = reader.next_line().await? {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v.get("id").and_then(|id| id.as_u64()) == Some(1) {
                        return Ok(());
                    }
                }
            }
            anyhow::bail!("ACP initialize stream closed")
        }).await;

        match init_res {
            Ok(Ok(())) => {},
            _ => {
                let _ = child.kill().await;
                anyhow::bail!("ACP initialize handshake timed out or failed");
            }
        }

        // 2. session/new
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
            "id": 2,
            "method": "session/new",
            "params": {"cwd": cwd, "mcpServers": []}
        });
        stdin.write_all(format!("{}\n", session_req).as_bytes()).await?;
        stdin.flush().await?;

        let session_res = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while let Some(line) = reader.next_line().await? {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v.get("id").and_then(|id| id.as_u64()) == Some(2) {
                        if let Some(err) = v.get("error") {
                            anyhow::bail!("ACP session/new error: {:?}", err);
                        }
                        if let Some(sid) = v.pointer("/result/sessionId").and_then(|v| v.as_str()) {
                            return Ok(sid.to_string());
                        }
                        anyhow::bail!("Missing sessionId from ACP session/new");
                    }
                }
            }
            anyhow::bail!("ACP session/new stream closed")
        }).await;

        let session_id = match session_res {
            Ok(Ok(sid)) => sid,
            _ => {
                let _ = child.kill().await;
                anyhow::bail!("ACP session/new timed out or failed");
            }
        };

        // 3. session/set_config_option
        let mut rpc_id = 3;
        if !model.is_empty() {
            let cfg_call_id = rpc_id;
            rpc_id += 1;
            let cfg_req = json!({
                "jsonrpc": "2.0",
                "id": cfg_call_id,
                "method": "session/set_config_option",
                "params": {
                    "sessionId": session_id,
                    "configId": "model",
                    "value": model
                }
            });
            stdin.write_all(format!("{}\n", cfg_req).as_bytes()).await?;
            stdin.flush().await?;

            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while let Some(line) = reader.next_line().await? {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v.get("id").and_then(|id| id.as_u64()) == Some(cfg_call_id) {
                            return Ok(());
                        }
                    }
                }
                anyhow::bail!("ACP stream closed during set_config_option")
            }).await;
        }

        // 4. session/prompt
        let prompt_req = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "model": model,
                "prompt": [{"type": "text", "text": prompt}]
            }
        });
        stdin.write_all(format!("{}\n", prompt_req).as_bytes()).await?;
        stdin.flush().await?;

        let is_debug = std::env::var_os("L2C_DEBUG_LOG").is_some();
        let mut captured_usage: Option<TokenUsage> = None;
        let mut prompt_err = None;

        while let Some(line) = reader.next_line().await? {
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
                        stdin.write_all(format!("{}\n", reply).as_bytes()).await?;
                        stdin.flush().await?;
                        if is_debug {
                            eprintln!("[ACP request_permission rejected for internal tool execution] id={:?}", req_id);
                        }
                        continue;
                    }
                }

                // Filter notifications: if line contains a sessionId, verify it matches session_id!
                if let Some(sid) = v.pointer("/params/sessionId")
                    .or_else(|| v.pointer("/params/update/sessionId"))
                    .and_then(|s| s.as_str())
                {
                    if sid != session_id {
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
                            let _ = tx.send(AcpOutputChunk::Thinking(thought.to_string())).await;
                        }
                    }
                    continue;
                }

                if let Some(usage) = extract_acp_usage(&v) {
                    captured_usage = Some(usage);
                }

                if let Some(text) = v
                    .pointer("/params/update/content/text")
                    .or_else(|| v.pointer("/params/update/content/0/text"))
                    .or_else(|| v.pointer("/params/content/text"))
                    .or_else(|| v.pointer("/params/content"))
                    .and_then(|t| t.as_str())
                {
                    if !text.is_empty() {
                        let _ = tx.send(AcpOutputChunk::Text(text.to_string())).await;
                    }
                }

                if update_type == Some("tool_call") || update_type == Some("tool_call_update") {
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
                    let input_val = v.pointer("/params/update/rawInput")
                        .or_else(|| v.pointer("/params/update/input"))
                        .unwrap_or(&serde_json::Value::Null);

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

                        // Empty placeholder for an argument-bearing tool: wait for the
                        // populated update. A truly parameterless tool stays empty, so
                        // emit it once ACP confirms the call has started.
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
                                ("Bash".to_string(), serde_json::json!({"command": format!("ls -la -- {}", crate::engine::session::acp::shell_escape(p))}))
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

                    let is_ready = match tool_name.to_lowercase().as_str() {
                        "bash" => tool_input.get("command").and_then(|c| c.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false),
                        "read" => tool_input.get("file_path").or_else(|| tool_input.get("path")).and_then(|p| p.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false),
                        "edit" => tool_input.get("file_path").or_else(|| tool_input.get("path")).is_some(),
                        "write" => tool_input.get("file_path").or_else(|| tool_input.get("path")).is_some(),
                        _ => true,
                    };

                    if is_ready {
                        let tool_str = format!("\n<tool_call>\n{{\"name\": \"{}\", \"arguments\": {}}}\n</tool_call>\n", tool_name, tool_input);
                        let _ = tx.send(AcpOutputChunk::Text(tool_str)).await;

                        if is_debug {
                            eprintln!("[AcpClient tool_call guard] Intercepted tool call '{}' - delegating to Claude Code", tool_name);
                        }
                        let cancel_req = json!({
                            "jsonrpc": "2.0",
                            "method": "session/cancel",
                            "params": { "sessionId": session_id }
                        });
                        let _ = stdin.write_all(format!("{}\n", cancel_req).as_bytes()).await;
                        let _ = stdin.flush().await;
                        break;
                    }
                }

                if let Some(err) = v.get("error") {
                    if v.get("id").and_then(|id| id.as_u64()) == Some(3) {
                        if is_debug {
                            eprintln!(">>> ACP RPC ERROR for prompt id 3: {:?}", err);
                        }
                        prompt_err = Some(err.clone());
                    } else {
                        if is_debug {
                            eprintln!(">>> ACP RPC ERROR ignoring mismatched id {:?}: {:?}", v.get("id"), err);
                        }
                    }
                }

                let is_matching_result = v.get("id").and_then(|id| id.as_u64()) == Some(3);
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
                    break;
                }
            }
        }

        if let Some(err) = prompt_err {
            anyhow::bail!("ACP session/prompt error: {:?}", err);
        }

        let _ = child.kill().await;
        let _ = child.wait().await;
        let final_usage = captured_usage.unwrap_or_default();
        if final_usage.total() > 0 {
            let _ = tx.send(AcpOutputChunk::Usage(final_usage)).await;
        }
        Ok(final_usage)
    }
}

