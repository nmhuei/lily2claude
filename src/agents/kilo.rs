use super::{Agent, AuthStatus};
use crate::adapters::acp_client::AcpClient;
use crate::discovery::catalog::ModelMetadata;
use async_trait::async_trait;
use regex::Regex;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct KiloAgent {
    free_regex: Regex,
}

impl Default for KiloAgent {
    fn default() -> Self {
        Self {
            free_regex: Regex::new(r"(?i)(-free|:free|/free)$").unwrap(),
        }
    }
}

impl KiloAgent {
    pub fn cache_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/light".to_string());
        Path::new(&home).join(".cache/kilo/models.json")
    }

    pub fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/light".to_string());
        Path::new(&home).join(".config/kilo/kilo.jsonc")
    }
}

#[async_trait]
impl Agent for KiloAgent {
    fn id(&self) -> &'static str {
        "kilo"
    }

    fn display_name(&self) -> &'static str {
        "Kilo"
    }

    fn detect_installation(&self) -> (bool, Option<String>, Option<String>) {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let p = Path::new(dir).join("kilo");
                if p.is_file() {
                    let version = std::process::Command::new("kilo")
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
        // Kilo Auto / Free tier does not strictly require auth file, but config may exist
        let config = Self::config_path();
        if config.exists() {
            AuthStatus::Ready
        } else {
            AuthStatus::NotRequired
        }
    }

    fn discover_models(&self) -> Vec<ModelMetadata> {
        let mut list = Vec::new();
        let cache_file = Self::cache_path();
        if cache_file.exists() {
            if let Ok(data) = std::fs::read_to_string(cache_file) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(obj) = val.as_object() {
                        for (prov_name, p_data) in obj {
                            if let Some(models) = p_data.get("models").and_then(|m| m.as_object()) {
                                for (m_id, m_info) in models {
                                    let ctx = m_info
                                        .pointer("/limit/context")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| m_info.get("context_length").and_then(|v| v.as_u64()))
                                        .unwrap_or(128000) as u32;
                                    let out = m_info
                                        .pointer("/limit/output")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| m_info.get("max_tokens").and_then(|v| v.as_u64()))
                                        .unwrap_or(8192) as u32;
                                    let name = m_info.get("name").and_then(|v| v.as_str()).unwrap_or(m_id);
                                    let is_free = self.free_regex.is_match(m_id);

                                    let qualified_id = format!("kilo:{}/{}", prov_name, m_id);
                                    list.push(ModelMetadata {
                                        id: qualified_id,
                                        name: name.to_string(),
                                        context_length: ctx,
                                        max_output_tokens: out,
                                        is_free,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        list
    }

    fn default_aliases(&self) -> Vec<(&'static str, String)> {
        vec![
            ("kilo", "kilo:kilo-auto/free".to_string()),
            ("nemotron", "kilo:nvidia/nemotron-3.5-lightning:free".to_string()),
            ("deepseek", "kilo:kilo-auto/free".to_string()),
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
        let cmd = vec!["kilo".to_string(), "acp".to_string()];
        if let (Some(mgr), Some(key)) = (session_mgr, session_key) {
            let session = mgr.get_or_create(key, self.id(), &cmd).await?;
            let mut lock = session.lock().await;
            lock.send_prompt(model, prompt, tx).await
        } else {
            let client = AcpClient::new(cmd);
            client.run_prompt(model, prompt, tx).await
        }
    }
}

