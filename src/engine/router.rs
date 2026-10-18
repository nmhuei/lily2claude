use crate::adapters::acp_client::AcpClient;
use crate::adapters::http_upstream::HttpUpstreamAdapter;
use crate::config::warp_pool::WarpPool;
use crate::config::Config;
use std::collections::HashMap;
use std::sync::Arc;

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct ProviderHealthRegistry {
    tripped_until: Arc<Mutex<HashMap<String, Instant>>>,
}

impl ProviderHealthRegistry {
    pub fn new() -> Self {
        Self {
            tripped_until: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn mark_failed(&self, provider: &str, cooldown_secs: u64) {
        let mut map = self.tripped_until.lock().unwrap();
        map.insert(provider.to_string(), Instant::now() + Duration::from_secs(cooldown_secs));
    }

    pub fn is_healthy(&self, provider: &str) -> bool {
        let mut map = self.tripped_until.lock().unwrap();
        let now = Instant::now();
        map.retain(|_, expiry| *expiry > now);
        !map.contains_key(provider)
    }

    pub fn prioritize_candidates(&self, candidates: Vec<TargetCandidate>) -> Vec<TargetCandidate> {
        let mut healthy = Vec::new();
        let mut degraded = Vec::new();
        for c in candidates {
            if self.is_healthy(&c.provider) {
                healthy.push(c);
            } else {
                degraded.push(c);
            }
        }
        healthy.extend(degraded);
        healthy
    }
}

#[derive(Debug, Clone)]
pub struct TargetCandidate {
    pub provider: String,
    pub model: String,
}

pub struct EngineRouter {
    config: Config,
    http_adapters: HashMap<String, Arc<HttpUpstreamAdapter>>,
    acp_adapters: HashMap<String, Arc<AcpClient>>,
    health: ProviderHealthRegistry,
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
            health: ProviderHealthRegistry::new(),
        }
    }

    pub fn mark_provider_degraded(&self, provider: &str, cooldown_secs: u64) {
        self.health.mark_failed(provider, cooldown_secs);
    }

    fn resolve_targets(&self, targets: &[String], depth: usize) -> Vec<TargetCandidate> {
        if depth > 5 {
            return Vec::new();
        }
        let mut candidates = Vec::new();
        for t in targets {
            if t.contains(':') {
                let parts: Vec<&str> = t.splitn(2, ':').collect();
                if parts.len() == 2 {
                    candidates.push(TargetCandidate {
                        provider: parts[0].to_string(),
                        model: parts[1].to_string(),
                    });
                }
            } else if let Some(sub_targets) = self.config.aliases.get(t) {
                candidates.extend(self.resolve_targets(sub_targets, depth + 1));
            }
        }
        candidates
    }

    pub fn resolve_alias(&self, alias: &str) -> Vec<TargetCandidate> {
        let raw = self.resolve_alias_raw(alias);
        self.health.prioritize_candidates(raw)
    }

    pub fn resolve_alias_raw(&self, alias: &str) -> Vec<TargetCandidate> {
        let clean = alias.trim().to_lowercase();

        // 1. Direct alias match
        if let Some(targets) = self.config.aliases.get(alias) {
            let res = self.resolve_targets(targets, 0);
            if !res.is_empty() {
                return res;
            }
        }
        if let Some(targets) = self.config.aliases.get(&clean) {
            let res = self.resolve_targets(targets, 0);
            if !res.is_empty() {
                return res;
            }
        }

        // 2. Claude / Anthropic subagent model names -> transparently map to default target
        if clean.contains("claude")
            || clean.contains("haiku")
            || clean.contains("sonnet")
            || clean.contains("opus")
            || clean.contains("fable")
            || clean.contains("anthropic")
            || clean.contains("subagent")
            || clean.contains("explore")
            || clean.contains("general")
            || clean.contains("plan")
            || clean.contains("fast")
            || clean.contains("small")
            || clean.contains("auto")
            || clean == "default"
            || clean.is_empty()
        {
            if let Some(targets) = self.config.aliases.get(&self.config.default_target) {
                let res = self.resolve_targets(targets, 0);
                if !res.is_empty() {
                    return res;
                }
            }
            if let Some(targets) = self.config.aliases.get("deepseek") {
                let res = self.resolve_targets(targets, 0);
                if !res.is_empty() {
                    return res;
                }
            }
        }

        // 3. Family fuzzy matching
        if clean.contains("deepseek") {
            if let Some(targets) = self.config.aliases.get("deepseek") {
                let res = self.resolve_targets(targets, 0);
                if !res.is_empty() {
                    return res;
                }
            }
        }
        if clean.contains("glm") {
            if let Some(targets) = self.config.aliases.get("glm") {
                let res = self.resolve_targets(targets, 0);
                if !res.is_empty() {
                    return res;
                }
            }
        }
        if clean.contains("nemotron") {
            if let Some(targets) = self.config.aliases.get("nemotron") {
                let res = self.resolve_targets(targets, 0);
                if !res.is_empty() {
                    return res;
                }
            }
        }
        if clean.contains("mimo") {
            if let Some(targets) = self.config.aliases.get("mimo") {
                let res = self.resolve_targets(targets, 0);
                if !res.is_empty() {
                    return res;
                }
            }
        }
        if clean.contains("kilo") {
            if let Some(targets) = self.config.aliases.get("kilo") {
                let res = self.resolve_targets(targets, 0);
                if !res.is_empty() {
                    return res;
                }
            }
        }

        // 4. Provider:model explicit specification
        if alias.contains(':') {
            let parts: Vec<&str> = alias.splitn(2, ':').collect();
            if parts.len() == 2 {
                let mut res = vec![TargetCandidate {
                    provider: parts[0].to_string(),
                    model: parts[1].to_string(),
                }];
                if let Some(ds) = self.config.aliases.get("deepseek") {
                    res.extend(self.resolve_targets(ds, 0));
                }
                return res;
            }
        }

        // 5. Safe fallback: attempt opencode model, with guaranteed DeepSeek fallback candidates
        let mut candidates = vec![TargetCandidate {
            provider: "opencode".to_string(),
            model: alias.to_string(),
        }];
        if let Some(ds) = self.config.aliases.get("deepseek") {
            candidates.extend(self.resolve_targets(ds, 0));
        }
        candidates
    }

    pub fn get_http_adapter(&self, provider: &str) -> Option<Arc<HttpUpstreamAdapter>> {
        self.http_adapters.get(provider).cloned()
    }

    pub fn get_acp_adapter(&self, provider: &str) -> Option<Arc<AcpClient>> {
        self.acp_adapters.get(provider).cloned()
    }
}
