use super::{Agent, AuthStatus};
use crate::adapters::acp_client::AcpClient;
use crate::discovery::catalog::ModelMetadata;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default)]
pub struct CommandCodeAgent;

impl CommandCodeAgent {
    pub fn auth_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/light".to_string());
        Path::new(&home).join(".commandcode/auth.json")
    }

    pub fn catalog_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/light".to_string());
        Path::new(&home).join(".commandcode/cache/model-catalog/v1")
    }
}

#[async_trait]
impl Agent for CommandCodeAgent {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn display_name(&self) -> &'static str {
        "CommandCode"
    }

    fn detect_installation(&self) -> (bool, Option<String>, Option<String>) {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let p = Path::new(dir).join("commandcode");
                if p.is_file() {
                    let version = std::process::Command::new("commandcode")
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
        let path = Self::auth_path();
        if path.exists() {
            AuthStatus::Ready
        } else {
            AuthStatus::Missing(path.to_string_lossy().to_string())
        }
    }

    fn discover_models(&self) -> Vec<ModelMetadata> {
        let mut models = Vec::new();
        let cat_dir = Self::catalog_dir();
        if cat_dir.exists() && cat_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(cat_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                                if let Some(arr) = val.as_array() {
                                    for m in arr {
                                        let raw_id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                        if raw_id.is_empty() {
                                            continue;
                                        }
                                        let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(raw_id);
                                        let ctx = m.get("context_length").and_then(|v| v.as_u64()).unwrap_or(128000) as u32;
                                        let out = m.get("max_output_tokens").and_then(|v| v.as_u64()).unwrap_or(8192) as u32;
                                        let qualified_id = format!("commandcode:{}", raw_id);
                                        models.push(ModelMetadata {
                                            id: qualified_id,
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
        }
        models
    }

    fn default_aliases(&self) -> Vec<(&'static str, String)> {
        vec![
            ("deepseek", "commandcode:deepseek/deepseek-v4.1-flash".to_string()),
            ("glm", "commandcode:z-ai/glm-5.3-flash".to_string()),
            ("mimo", "commandcode:xiaomi/mimo-v2.6-flash".to_string()),
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
        let cmd = vec!["commandcode".to_string(), "acp".to_string()];
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
