use crate::protocol::anthropic::{AnthropicContent, ContentBlock, MessagesRequest};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};

/// Extracts a deterministic session key from conversation root message
pub fn extract_session_key(req: &MessagesRequest) -> String {
    let mut hasher = DefaultHasher::new();
    if let Some(first_msg) = req.messages.first() {
        first_msg.role.hash(&mut hasher);
        match &first_msg.content {
            AnthropicContent::Text(t) => t.hash(&mut hasher),
            AnthropicContent::Blocks(blocks) => {
                for b in blocks {
                    if let ContentBlock::Text { text } = b {
                        text.hash(&mut hasher);
                    }
                }
            }
        }
    } else {
        "empty_session".hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

pub struct AcpSession {
    pub session_id: String,
    pub child: Child,
    pub stdin: ChildStdin,
    pub reader: Lines<BufReader<ChildStdout>>,
    pub last_used: Instant,
    pub rpc_id: u64,
}

impl AcpSession {
    pub async fn spawn(command: &[String]) -> anyhow::Result<Self> {
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let stderr = child.stderr.take().expect("Failed to open stderr");

        // Drain stderr concurrently to avoid deadlocks
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
        let _init_resp = reader.next_line().await?;

        // 2. session/new
        let session_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": std::env::current_dir()?.to_string_lossy(), "mcpServers": []}
        });
        stdin.write_all(format!("{}\n", session_req).as_bytes()).await?;
        stdin.flush().await?;

        let session_resp = reader.next_line().await?.unwrap_or_default();
        let session_val: Value = serde_json::from_str(&session_resp)?;
        let session_id = session_val
            .pointer("/result/sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("s1")
            .to_string();

        Ok(Self {
            session_id,
            child,
            stdin,
            reader,
            last_used: Instant::now(),
            rpc_id: 2,
        })
    }

    pub async fn send_prompt(
        &mut self,
        model: &str,
        prompt: &str,
        tx: mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        self.last_used = Instant::now();
        self.rpc_id += 1;
        let call_id = self.rpc_id;

        let prompt_req = json!({
            "jsonrpc": "2.0",
            "id": call_id,
            "method": "session/prompt",
            "params": {
                "sessionId": self.session_id,
                "model": model,
                "prompt": [{"type": "text", "text": prompt}]
            }
        });
        self.stdin.write_all(format!("{}\n", prompt_req).as_bytes()).await?;
        self.stdin.flush().await?;

        while let Some(line) = self.reader.next_line().await? {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                // Parse text chunks from either object or array
                if let Some(text) = v
                    .pointer("/params/update/content/text")
                    .or_else(|| v.pointer("/params/update/content/0/text"))
                    .or_else(|| v.pointer("/params/content/text"))
                    .and_then(|t| t.as_str())
                {
                    if !text.is_empty() {
                        let _ = tx.send(text.to_string()).await;
                    }
                }

                // Check stop conditions
                let is_stop = v.pointer("/result/stopReason").is_some()
                    || v.pointer("/params/update/stopReason").is_some()
                    || v.pointer("/params/stopReason").is_some();

                let is_matching_result = v.get("id").and_then(|id| id.as_u64()) == Some(call_id);

                if is_stop || (is_matching_result && v.get("result").is_some()) {
                    break;
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<AcpSession>>>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn active_count(&self) -> usize {
        let lock = self.sessions.lock().await;
        lock.len()
    }

    pub async fn get_or_create(
        &self,
        session_key: &str,
        provider: &str,
        command: &[String],
    ) -> anyhow::Result<Arc<Mutex<AcpSession>>> {
        let full_key = format!("{}:{}", provider, session_key);
        let mut lock = self.sessions.lock().await;

        if let Some(existing) = lock.get(&full_key) {
            // Check if process is still alive
            let mut session_lock = existing.lock().await;
            match session_lock.child.try_wait() {
                Ok(None) => {
                    // Still running! Reuse existing session!
                    drop(session_lock);
                    return Ok(existing.clone());
                }
                _ => {
                    // Exited, drop and recreate
                }
            }
        }

        // Spawn new session
        let new_session = AcpSession::spawn(command).await?;
        let handle = Arc::new(Mutex::new(new_session));
        lock.insert(full_key, handle.clone());
        Ok(handle)
    }

    pub async fn sweep_idle(&self, max_idle: Duration) {
        let mut lock = self.sessions.lock().await;
        let mut to_remove = Vec::new();

        for (k, sess) in lock.iter() {
            let mut s = sess.lock().await;
            if s.last_used.elapsed() > max_idle {
                let _ = s.child.kill().await;
                to_remove.push(k.clone());
            }
        }

        for k in to_remove {
            lock.remove(&k);
        }
    }
}
