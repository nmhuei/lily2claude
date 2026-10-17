use super::{Agent, AuthStatus};
use crate::adapters::acp_client::AcpClient;
use crate::discovery::catalog::ModelMetadata;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default)]
pub struct ClineAgent;

impl ClineAgent {
    pub fn providers_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/light".to_string());
        Path::new(&home).join(".cline/data/settings/providers.json")
    }
}

#[async_trait]
impl Agent for ClineAgent {
    fn id(&self) -> &'static str {
        "cline"
    }

    fn display_name(&self) -> &'static str {
        "Cline CLI"
    }

    fn detect_installation(&self) -> (bool, Option<String>, Option<String>) {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let p = Path::new(dir).join("cline");
                if p.is_file() {
                    let version = std::process::Command::new("cline")
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
        let path = Self::providers_path();
        if path.exists() {
            AuthStatus::Ready
        } else {
            AuthStatus::Missing(path.to_string_lossy().to_string())
        }
    }

    fn discover_models(&self) -> Vec<ModelMetadata> {
        let mut list = Vec::new();
        let path = Self::providers_path();
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(providers) = val.get("providers").and_then(|p| p.as_array()) {
                        for p in providers {
                            let p_id = p.get("id").and_then(|v| v.as_str()).unwrap_or("cline");
                            if let Some(models) = p.get("models").and_then(|m| m.as_array()) {
                                for m in models {
                                    let m_id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(m_id);
                                    let ctx = m.get("context_window").and_then(|v| v.as_u64()).unwrap_or(200000) as u32;
                                    let out = m.get("max_output_tokens").and_then(|v| v.as_u64()).unwrap_or(16384) as u32;
                                    list.push(ModelMetadata {
                                        id: format!("cline:{}:{}", p_id, m_id),
                                        name: name.to_string(),
                                        context_length: ctx,
                                        max_output_tokens: out,
                                        is_free: true,
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
            ("luna", "cline:openai-codex:gpt-5.6-luna".to_string()),
        ]
    }

    async fn execute_turn(
        &self,
        model: &str,
        prompt: &str,
        session_key: Option<&str>,
        session_mgr: Option<&crate::engine::session::SessionManager>,
        tx: mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let cmd = vec!["cline".to_string(), "--acp".to_string()];
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
