use super::{Agent, AuthStatus};
use crate::adapters::acp_client::AcpClient;
use crate::discovery::catalog::ModelMetadata;
use async_trait::async_trait;
use std::path::Path;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct GenericAcpAgent {
    id: String,
    display_name: String,
    binary: String,
    args: Vec<String>,
}

impl GenericAcpAgent {
    pub fn new(id: &str, display_name: &str, binary: &str, args: Vec<String>) -> Self {
        Self {
            id: id.to_string(),
            display_name: display_name.to_string(),
            binary: binary.to_string(),
            args,
        }
    }
}

#[async_trait]
impl Agent for GenericAcpAgent {
    fn id(&self) -> &'static str {
        Box::leak(self.id.clone().into_boxed_str())
    }

    fn display_name(&self) -> &'static str {
        Box::leak(self.display_name.clone().into_boxed_str())
    }

    fn detect_installation(&self) -> (bool, Option<String>, Option<String>) {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let p = Path::new(dir).join(&self.binary);
                if p.is_file() {
                    let version = std::process::Command::new(&self.binary)
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
        AuthStatus::NotRequired
    }

    fn discover_models(&self) -> Vec<ModelMetadata> {
        Vec::new()
    }

    fn default_aliases(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    async fn execute_turn(
        &self,
        model: &str,
        prompt: &str,
        session_key: Option<&str>,
        session_mgr: Option<&crate::engine::session::SessionManager>,
        tx: mpsc::Sender<crate::protocol::tokens::AcpOutputChunk>,
    ) -> anyhow::Result<crate::protocol::tokens::TokenUsage> {
        let mut cmd = vec![self.binary.clone()];
        cmd.extend(self.args.clone());
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

