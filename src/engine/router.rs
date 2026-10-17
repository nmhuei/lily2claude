use crate::adapters::acp_client::AcpClient;
use crate::adapters::http_upstream::HttpUpstreamAdapter;
use crate::config::warp_pool::WarpPool;
use crate::config::Config;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TargetCandidate {
    pub provider: String,
    pub model: String,
}

pub struct EngineRouter {
    config: Config,
    http_adapters: HashMap<String, Arc<HttpUpstreamAdapter>>,
    acp_adapters: HashMap<String, Arc<AcpClient>>,
}

impl EngineRouter {
    pub fn new(config: Config) -> Self {
        let mut http_adapters = HashMap::new();
        let mut acp_adapters = HashMap::new();

        for (name, p_cfg) in &config.providers {
            if p_cfg.kind == "opencode_free" || p_cfg.kind == "openai_http" {
                let pool = p_cfg
                    .warp_proxies
                    .as_ref()
                    .map(|proxies| WarpPool::new(proxies.clone()));
                let token = p_cfg.auth_token.clone().or_else(|| {
                    p_cfg.auth_file.as_ref().and_then(|path| {
                        std::fs::read_to_string(path).ok().and_then(|data| {
                            serde_json::from_str::<serde_json::Value>(&data)
                                .ok()
                                .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(|s| s.to_string()))
                        })
                    })
                });
                let endpoint = p_cfg
                    .endpoint
                    .clone()
                    .unwrap_or_else(|| "https://opencode.ai/inference/openai/v1".to_string());
                http_adapters.insert(
                    name.clone(),
                    Arc::new(HttpUpstreamAdapter::new(&endpoint, token.as_deref(), pool)),
                );
            } else if p_cfg.kind == "acp_stdio" {
                if let Some(cmd) = &p_cfg.command {
                    acp_adapters.insert(name.clone(), Arc::new(AcpClient::new(cmd.clone())));
                }
            }
        }

        Self {
            config,
            http_adapters,
            acp_adapters,
        }
    }

    pub fn resolve_alias(&self, alias: &str) -> Vec<TargetCandidate> {
        if let Some(targets) = self.config.aliases.get(alias) {
            targets
                .iter()
                .filter_map(|t| {
                    let parts: Vec<&str> = t.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        Some(TargetCandidate {
                            provider: parts[0].to_string(),
                            model: parts[1].to_string(),
                        })
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            vec![TargetCandidate {
                provider: "opencode".to_string(),
                model: alias.to_string(),
            }]
        }
    }

    pub fn get_http_adapter(&self, provider: &str) -> Option<Arc<HttpUpstreamAdapter>> {
        self.http_adapters.get(provider).cloned()
    }

    pub fn get_acp_adapter(&self, provider: &str) -> Option<Arc<AcpClient>> {
        self.acp_adapters.get(provider).cloned()
    }
}
