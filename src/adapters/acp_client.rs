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

        // Isolate opencode data directory and inject WARP proxy if available
        if self.command[0].contains("opencode") {
            cmd.env("XDG_DATA_HOME", "/tmp/lily_opencode_data");

            if std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], 40001)),
                std::time::Duration::from_millis(100),
            )
            .is_ok()
            {
                cmd.env("HTTP_PROXY", "http://127.0.0.1:40001");
                cmd.env("HTTPS_PROXY", "http://127.0.0.1:40001");
                cmd.env("NO_PROXY", "localhost,127.0.0.1,::1");
            } else if std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], 40002)),
                std::time::Duration::from_millis(100),
            )
            .is_ok()
            {
                cmd.env("HTTP_PROXY", "http://127.0.0.1:40002");
                cmd.env("HTTPS_PROXY", "http://127.0.0.1:40002");
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
        stdin.flush().await?;
        while let Some(line) = reader.next_line().await? {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v.get("id").and_then(|id| id.as_u64()) == Some(1) {
                    break;
                }
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

        let mut session_id = None;
        while let Some(line) = reader.next_line().await? {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v.get("id").and_then(|id| id.as_u64()) == Some(2) {
                    if let Some(err) = v.get("error") {
                        anyhow::bail!("ACP session/new error: {:?}", err);
                    }
                    if let Some(sid) = v.pointer("/result/sessionId").and_then(|v| v.as_str()) {
                        session_id = Some(sid.to_string());
                    }
                    break;
                }
            }
        }

        let session_id = session_id.ok_or_else(|| anyhow::anyhow!("Missing sessionId from ACP session/new"))?;

        // 3. session/prompt
        let prompt_req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "model": model,
                "prompt": [{"type": "text", "text": prompt}]
            }
        });
        stdin.write_all(format!("{}\n", prompt_req).as_bytes()).await?;
        stdin.flush().await?;

        let mut captured_usage: Option<TokenUsage> = None;
        let mut prompt_err = None;

        while let Some(line) = reader.next_line().await? {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                // Auto-approve session/request_permission to bypass permission prompts automatically
                if let (Some(method), Some(req_id)) = (v.get("method").and_then(|m| m.as_str()), v.get("id")) {
                    if method == "session/request_permission" {
                        let option_id = v.pointer("/params/options")
                            .and_then(|opts| opts.as_array())
                            .and_then(|arr| {
                                arr.iter().find_map(|opt| {
                                    let kind = opt.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                    let opt_id = opt.get("optionId").and_then(|id| id.as_str())?;
                                    if kind == "allow_always" || opt_id == "always" {
                                        Some(opt_id.to_string())
                                    } else {
                                        None
                                    }
                                }).or_else(|| {
                                    arr.iter().find_map(|opt| {
                                        let kind = opt.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                        let opt_id = opt.get("optionId").and_then(|id| id.as_str())?;
                                        if kind == "allow_once" || opt_id == "once" {
                                            Some(opt_id.to_string())
                                        } else {
                                            None
                                        }
                                    })
                                })
                            })
                            .unwrap_or_else(|| "always".to_string());

                        let reply = json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "outcome": {
                                    "outcome": "selected",
                                    "optionId": option_id
                                }
                            }
                        });
                        stdin.write_all(format!("{}\n", reply).as_bytes()).await?;
                        stdin.flush().await?;
                        eprintln!("[ACP request_permission auto-approved] id={:?} optionId={}", req_id, option_id);
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

                if let Some(err) = v.get("error") {
                    if v.get("id").and_then(|id| id.as_u64()) == Some(3) {
                        eprintln!(">>> ACP RPC ERROR for prompt id 3: {:?}", err);
                        prompt_err = Some(err.clone());
                    } else {
                        eprintln!(">>> ACP RPC ERROR ignoring mismatched id {:?}: {:?}", v.get("id"), err);
                    }
                }

                let is_matching_result = v.get("id").and_then(|id| id.as_u64()) == Some(3);
                let is_stop_notification = v.pointer("/params/update/stopReason").is_some()
                    || v.pointer("/params/stopReason").is_some();
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

