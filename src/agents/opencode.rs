use super::{Agent, AuthStatus};
use crate::adapters::http_upstream::HttpUpstreamAdapter;
use crate::config::warp_pool::WarpPool;
use crate::discovery::catalog::ModelMetadata;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct OpenCodeAgent {
    warp_pool: Arc<WarpPool>,
    endpoint: String,
}

impl Default for OpenCodeAgent {
    fn default() -> Self {
        let pool = Arc::new(WarpPool::new(vec![
            "socks5h://127.0.0.1:40001".to_string(),
            "socks5h://127.0.0.1:40002".to_string(),
        ]));
        Self {
            warp_pool: pool,
            endpoint: "https://opencode.ai/inference/openai/v1".to_string(),
        }
    }
}

impl OpenCodeAgent {
    pub fn auth_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/light".to_string());
        Path::new(&home).join(".local/share/opencode/auth.json")
    }

    pub fn warp_pool(&self) -> Arc<WarpPool> {
        self.warp_pool.clone()
    }
}

#[async_trait]
impl Agent for OpenCodeAgent {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode Cloud"
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
        let path = Self::auth_path();
        if path.exists() {
            AuthStatus::Ready
        } else {
            AuthStatus::Missing(path.to_string_lossy().to_string())
        }
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
        _session_mgr: Option<&crate::engine::session::SessionManager>,
        tx: mpsc::Sender<String>,
    ) -> anyhow::Result<()> {
        let adapter = HttpUpstreamAdapter::new(
            &self.endpoint,
            None,
            Some((*self.warp_pool).clone()),
        );
        let req = crate::protocol::openai::ChatCompletionsRequest {
            model: model.to_string(),
            messages: vec![crate::protocol::openai::OpenAiMessage {
                role: "user".to_string(),
                content: Some(prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            stream: true,
            max_tokens: None,
            temperature: None,
        };
        let response = adapter.send_request_with_session(&req, session_key).await?;
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;
        while let Some(item) = stream.next().await {
            if let Ok(bytes) = item {
                if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                    let _ = tx.send(text).await;
                }
            }
        }
        Ok(())
    }
}
