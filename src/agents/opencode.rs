use super::{Agent, AuthStatus};
use crate::adapters::acp_client::AcpClient;
use crate::discovery::catalog::ModelMetadata;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default)]
pub struct OpenCodeAgent;

impl OpenCodeAgent {
    pub fn auth_path() -> PathBuf {
        crate::config::home_dir().join(".local/share/opencode/auth.json")
    }

    /// `XDG_DATA_HOME` handed to every opencode child process.
    ///
    /// opencode keeps a SQLite database here, so it has to be persistent
    /// storage. `std::env::temp_dir()` is not good enough: on Linux it resolves
    /// to `/tmp`, which is RAM-backed on most distros and wiped on reboot.
    pub fn isolated_data_dir() -> PathBuf {
        match std::env::var_os("XDG_DATA_HOME").filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir).join("lily2claude/opencode-data"),
            None => crate::config::home_dir().join(".local/share/lily2claude/opencode-data"),
        }
    }

    /// Isolate `cmd`'s opencode data directory, carrying the user's existing
    /// auth token across so the provider does not fail with
    /// "Authentication required".
    ///
    /// Both ACP spawn paths (the pooled session and the one-shot client) must
    /// agree on this directory, otherwise they fight over the same SQLite file.
    pub fn prepare_child_env(cmd: &mut tokio::process::Command) {
        let data_dir = Self::isolated_data_dir();
        let opencode_dir = data_dir.join("opencode");
        let _ = std::fs::create_dir_all(&opencode_dir);

        let src_auth = Self::auth_path();
        let dst_auth = opencode_dir.join("auth.json");
        if src_auth.exists()
            && (!dst_auth.exists()
                || std::fs::metadata(&src_auth).map(|m| m.len()).unwrap_or(0)
                    != std::fs::metadata(&dst_auth).map(|m| m.len()).unwrap_or(0))
        {
            let _ = std::fs::copy(&src_auth, &dst_auth);
        }

        cmd.env("XDG_DATA_HOME", &data_dir);
    }
}

#[async_trait]
impl Agent for OpenCodeAgent {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn detect_installation(&self) -> (bool, Option<String>, Option<String>) {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let p = Path::new(dir).join("opencode");
                if p.is_file() {
                    let version = std::process::Command::new("opencode")
                        .arg("--version")
                        .output()
                        .ok()
                        .and_then(|o| {
                            if o.status.success() {
                                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                            } else {
                                None
                            }
                        });
                    return (true, Some(p.to_string_lossy().to_string()), version);
                }
            }
        }
        (false, None, None)
    }

    fn check_auth(&self) -> AuthStatus {
        AuthStatus::Ready
    }

    fn discover_models(&self) -> Vec<ModelMetadata> {
        vec![
            ModelMetadata {
                id: "opencode:deepseek-v4.1-flash-free".to_string(),
                name: "DeepSeek V4.1 Flash (Free)".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            },
            ModelMetadata {
                id: "opencode:glm-5.3-flash-free".to_string(),
                name: "GLM 5.3 Flash (Free)".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            },
            ModelMetadata {
                id: "opencode:mimo-v2.6-flash-free".to_string(),
                name: "MiMo 2.6 Flash (Free)".to_string(),
                context_length: 64000,
                max_output_tokens: 8192,
                is_free: true,
            },
            ModelMetadata {
                id: "opencode:nvidia/nemotron-3-nano-free".to_string(),
                name: "Nemotron 3 Nano (Free)".to_string(),
                context_length: 1000000,
                max_output_tokens: 65536,
                is_free: true,
            },
        ]
    }

    fn default_aliases(&self) -> Vec<(&'static str, String)> {
        vec![
            ("deepseek", "opencode:deepseek-v4.1-flash-free".to_string()),
            ("glm", "opencode:glm-5.3-flash-free".to_string()),
            ("mimo", "opencode:mimo-v2.6-flash-free".to_string()),
            ("nemotron", "opencode:nvidia/nemotron-3-nano-free".to_string()),
        ]
    }

    async fn execute_turn(
        &self,
        model: &str,
        prompt: &str,
        session_key: Option<&str>,
        session_mgr: Option<&crate::engine::session::SessionManager>,
        tx: mpsc::Sender<crate::protocol::tokens::AcpOutputChunk>,
    ) -> anyhow::Result<crate::protocol::tokens::TokenUsage> {
        let clean_model = model.strip_prefix("opencode:").unwrap_or(model);
        let qualified_model = if clean_model.contains('/') {
            clean_model.to_string()
        } else {
            format!("opencode/{}", clean_model)
        };
        let cmd = vec!["opencode".to_string(), "acp".to_string()];
        if let (Some(mgr), Some(key)) = (session_mgr, session_key) {
            let mut last_err = None;
            for attempt in 0..4 {
                let session = mgr.get_or_create(key, self.id(), &cmd).await?;
                let mut lock = session.lock().await;
                match lock.send_prompt(&qualified_model, prompt, tx.clone()).await {
                    Ok(usage) => return Ok(usage),
                    Err(e) => {
                        let err_str = e.to_string();
                        if crate::config::debug_enabled() {
                            eprintln!("[OpenCode execute_turn] attempt {} failed: {}", attempt, err_str);
                        }
                        mgr.evict_session(key, self.id()).await;
                        last_err = Some(e);
                        if crate::config::warp_pool::is_upstream_rate_limited(&err_str) {
                            if crate::config::debug_enabled() {
                                eprintln!("[OpenCode execute_turn] rate limited on attempt {}, failing over to next proxy: {}", attempt, err_str);
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            continue;
                        }
                        if err_str.contains("provider.invalid-output")
                            || err_str.contains("-32603")
                            || err_str.contains("OpenAI Chat tool call delta is missing id or name")
                        {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            continue;
                        }
                        return Err(last_err.unwrap());
                    }
                }
            }
            if let Some(e) = last_err {
                return Err(e);
            }
            anyhow::bail!("OpenCode turn failed after retries")
        } else {
            let client = AcpClient::new(cmd);
            client.run_prompt(&qualified_model, prompt, tx).await
        }
    }
}

