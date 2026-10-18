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

/// Extracts persistent session key prioritizing Claude Code's native session headers
pub fn extract_session_key_from_headers_or_req(
    headers: &axum::http::HeaderMap,
    req: &MessagesRequest,
) -> String {
    // 1. Claude Code native session header
    if let Some(val) = headers.get("x-claude-code-session-id").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // 2. Custom lily session header
    if let Some(val) = headers.get("x-lily-session").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // 3. Generic session header
    if let Some(val) = headers.get("x-session-id").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // 4. Fallback to conversation root message hash
    extract_session_key(req)
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
            .stderr(Stdio::piped());

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
                eprintln!("[ACP STDERR] {}", line);
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

        let mut current_session_id = None;
        while let Some(line) = self.reader.next_line().await? {
            eprintln!("[ACP session/new raw] {}", &line[..line.len().min(120)]);
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v.get("id").and_then(|id| id.as_u64()) == Some(session_call_id) {
                    if let Some(err) = v.get("error") {
                        eprintln!("[ACP session/new error] {:?}", err);
                        anyhow::bail!("ACP session/new failed: {:?}", err);
                    }
                    if let Some(sid) = v.pointer("/result/sessionId").and_then(|s| s.as_str()) {
                        current_session_id = Some(sid.to_string());
                    }
                    break;
                }
            }
        }

        let current_session_id = current_session_id.ok_or_else(|| {
            anyhow::anyhow!("Failed to acquire sessionId from ACP session/new")
        })?;
        eprintln!("[ACP session/new acquired] sid={}", current_session_id);

        // 2. session/prompt
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
        eprintln!("[ACP session/prompt sent] id={}", prompt_call_id);

        let _ = std::fs::write("/tmp/last_acp_prompt.txt", prompt);
        let mut captured_usage: Option<crate::protocol::tokens::TokenUsage> = None;
        let mut prompt_err = None;

        let mut emitted_tool_calls: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(line) = self.reader.next_line().await? {
            eprintln!("[ACP prompt raw] {}", &line[..line.len().min(120)]);
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/acp_raw.log") {
                use std::io::Write;
                let _ = writeln!(f, "{}", line);
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
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
                                ("Bash".to_string(), serde_json::json!({"command": format!("ls -la {}", path)}))
                            }
                            "grep" | "search_files" => {
                                let pat = input_val.get("pattern").or_else(|| input_val.get("query")).and_then(|p| p.as_str()).unwrap_or("");
                                let path = input_val.get("path").and_then(|p| p.as_str()).unwrap_or(".");
                                ("Bash".to_string(), serde_json::json!({"command": format!("grep -rn '{}' {}", pat, path)}))
                            }
                            "glob" | "find_files" => {
                                let pat = input_val.get("pattern").and_then(|p| p.as_str()).unwrap_or("*");
                                let path = input_val.get("path").and_then(|p| p.as_str()).unwrap_or(".");
                                ("Bash".to_string(), serde_json::json!({"command": format!("find {} -name '{}'", path, pat)}))
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
                    eprintln!(">>> ACP RPC ERROR: {:?}", err);
                    prompt_err = Some(err.clone());
                }

                // Check stop conditions
                let is_stop = v.pointer("/result/stopReason").is_some()
                    || v.pointer("/params/update/stopReason").is_some()
                    || v.pointer("/params/stopReason").is_some()
                    || update_type == Some("turn_end");

                let is_matching_result = v.get("id").and_then(|id| id.as_u64()) == Some(prompt_call_id);

                if is_stop || (is_matching_result && (v.get("result").is_some() || v.get("error").is_some())) {
                    eprintln!("[ACP prompt broke] is_stop={} is_matching_result={}", is_stop, is_matching_result);
                    break;
                }
            }
        }
        eprintln!("[ACP prompt reader ended] final_usage={:?} prompt_err={:?}", captured_usage, prompt_err);
        eprintln!("[ACP child exit status] {:?}", self.child.try_wait());

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


#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, (Option<u32>, Arc<Mutex<AcpSession>>)>>>,
    warp_pool: Option<Arc<crate::config::warp_pool::WarpPool>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            warp_pool: Some(Arc::new(crate::config::warp_pool::WarpPool::default_pool())),
        }
    }

    pub fn with_pool(warp_pool: Option<Arc<crate::config::warp_pool::WarpPool>>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            warp_pool,
        }
    }

    pub fn warp_pool(&self) -> Option<Arc<crate::config::warp_pool::WarpPool>> {
        self.warp_pool.clone()
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
        let base_key = format!("{}:{}", provider, session_key);

        // 1. Try to find an existing idle worker without spawning
        {
            let lock = self.sessions.lock().await;
            for worker_idx in 0..4 {
                let full_key = if worker_idx == 0 {
                    base_key.clone()
                } else {
                    format!("{}:worker_{}", base_key, worker_idx)
                };

                if let Some((_, existing)) = lock.get(&full_key) {
                    if let Ok(mut session_lock) = existing.try_lock() {
                        match session_lock.child.try_wait() {
                            Ok(None) => {
                                drop(session_lock);
                                return Ok(existing.clone());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // 2. Spawn new session OUTSIDE the lock to eliminate any possibility of deadlock
        let new_session = AcpSession::spawn(command, self.warp_pool.clone()).await?;
        let child_pid = new_session.child_pid;
        let handle = Arc::new(Mutex::new(new_session));

        // 3. Insert into sessions map with lock
        let mut lock = self.sessions.lock().await;
        for worker_idx in 0..4 {
            let full_key = if worker_idx == 0 {
                base_key.clone()
            } else {
                format!("{}:worker_{}", base_key, worker_idx)
            };
            if !lock.contains_key(&full_key) {
                lock.insert(full_key, (child_pid, handle.clone()));
                return Ok(handle);
            }
        }
        lock.insert(base_key, (child_pid, handle.clone()));
        Ok(handle)
    }

    pub async fn sweep_idle(&self, max_idle: Duration) {
        let mut lock = self.sessions.lock().await;
        let mut to_remove = Vec::new();

        for (k, (_pid, sess)) in lock.iter() {
            if let Ok(mut s) = sess.try_lock() {
                if s.last_used.elapsed() > max_idle {
                    let _ = s.child.kill().await;
                    to_remove.push(k.clone());
                }
            }
            // If try_lock() fails, session is currently processing a request - NOT idle, do not kill!
        }

        for k in to_remove {
            lock.remove(&k);
        }
    }

    pub async fn evict_session(&self, session_key: &str, provider: &str) {
        let base_prefix = format!("{}:{}", provider, session_key);
        let mut lock = self.sessions.lock().await;
        let mut to_remove = Vec::new();

        for (k, (pid, sess)) in lock.iter() {
            if k == &base_prefix || k.starts_with(&format!("{}:", base_prefix)) {
                if let Ok(mut s) = sess.try_lock() {
                    let _ = s.child.kill().await;
                }
                if let Some(p) = pid {
                    let _ = tokio::process::Command::new("kill").args(["-9", &p.to_string()]).output().await;
                }
                to_remove.push(k.clone());
            }
        }

        for k in to_remove {
            lock.remove(&k);
        }
    }
}
