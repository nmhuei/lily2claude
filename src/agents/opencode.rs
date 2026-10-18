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
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/light".to_string());
        Path::new(&home).join(".local/share/opencode/auth.json")
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
            for attempt in 0..2 {
                let session = mgr.get_or_create(key, self.id(), &cmd).await?;
                let mut lock = session.lock().await;
                match lock.send_prompt(&qualified_model, prompt, tx.clone()).await {
                    Ok(usage) => return Ok(usage),
                    Err(e) => {
                        let err_str = e.to_string();
                        eprintln!("[OpenCode execute_turn] attempt {} failed: {}", attempt, err_str);
                        if err_str.contains("provider.invalid-output")
                            || err_str.contains("-32603")
                            || err_str.contains("OpenAI Chat tool call delta is missing id or name")
                        {
                            last_err = Some(e);
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            continue;
                        }
                        return Err(e);
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

