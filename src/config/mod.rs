pub mod warp_pool;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: String,
    pub endpoint: Option<String>,
    pub command: Option<Vec<String>>,
    pub auth_file: Option<String>,
    pub auth_token: Option<String>,
    pub warp_proxies: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherConfig {
    pub default_model: String,
    pub anthropic_model: String,
    pub subagent_model: String,
    pub haiku_model: String,
    pub sonnet_model: String,
    pub opus_model: String,
    #[serde(default = "default_true")]
    pub dangerously_skip_permissions: bool,
}

fn default_true() -> bool {
    true
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            default_model: "opencode:deepseek-v4.1-flash-free".to_string(),
            anthropic_model: "claude-sonnet-5-5".to_string(),
            subagent_model: "claude-haiku-4-5-20251001".to_string(),
            haiku_model: "claude-haiku-4-5-20251001".to_string(),
            sonnet_model: "claude-sonnet-5-5".to_string(),
            opus_model: "claude-opus-5-5".to_string(),
            dangerously_skip_permissions: true,
        }
    }
}

fn default_target_fallback() -> String {
    "deepseek".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_port: u16,
    pub server_host: String,
    #[serde(default = "default_target_fallback")]
    pub default_target: String,
    #[serde(default)]
    pub launcher: LauncherConfig,
    pub aliases: HashMap<String, Vec<String>>,
    pub providers: HashMap<String, ProviderConfig>,
}

impl Config {
    pub fn config_path() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("LILY2CLAUDE_CONFIG") {
            return std::path::PathBuf::from(p);
        }
        let local = std::path::PathBuf::from("lily2claude.toml");
        if local.exists() {
            return local;
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home).join(".config/lily2claude/config.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(cfg) = toml::from_str::<Config>(&content) {
                    return cfg;
                }
            }
        }
        let default_cfg = Self::load_default();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(serialized) = toml::to_string_pretty(&default_cfg) {
            let _ = std::fs::write(&path, serialized);
        }
        default_cfg
    }

    pub fn load_default() -> Self {
        let agent_reg = crate::agents::AgentRegistry::default();
        let mut aliases = agent_reg.default_aliases();

        let deepseek_targets = vec![
            "opencode:deepseek-v4.1-flash-free".to_string(),
            "commandcode:deepseek/deepseek-v4.1-flash".to_string(),
        ];

        // Ensure transparent aliases have full fallbacks
        aliases.insert("deepseek".to_string(), deepseek_targets.clone());
        aliases.insert("deepseek-v4.1-flash".to_string(), deepseek_targets.clone());
        aliases.insert("deepseek/deepseek-v4.1-flash".to_string(), deepseek_targets.clone());

        // Map Anthropic subagent models transparently to DeepSeek V4.1 Flash
        for anthropic_model in &[
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-sonnet-4-6",
            "claude-opus-4-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-3-7-sonnet-20250219",
            "claude-3-7-sonnet",
            "claude-3-5-sonnet-20241022",
            "claude-3-5-haiku-20241022",
            "claude-3-haiku-20240307",
            "claude-3-opus-20240229",
            "haiku-4-5",
            "sonnet-4-5",
            "sonnet-4-6",
            "opus-4-5",
            "haiku",
            "sonnet",
            "opus",
            "default",
        ] {
            aliases.entry(anthropic_model.to_string()).or_insert_with(|| deepseek_targets.clone());
        }

        aliases.entry("glm".to_string()).or_insert_with(|| vec![
            "commandcode:z-ai/glm-5.3-flash".to_string(),
            "opencode:glm-5.3-flash-free".to_string(),
        ]);
        aliases.entry("kilo".to_string()).or_insert_with(|| vec![
            "kilo:kilo-auto/free".to_string(),
        ]);
        aliases.entry("luna".to_string()).or_insert_with(|| vec![
            "cline:openai-codex:gpt-5.6-luna".to_string(),
        ]);
        aliases.entry("mimo".to_string()).or_insert_with(|| vec![
            "commandcode:xiaomi/mimo-v2.6-flash".to_string(),
            "opencode:mimo-v2.6-flash-free".to_string(),
        ]);
        aliases.entry("nemotron".to_string()).or_insert_with(|| vec![
            "commandcode:nvidia/nemotron-3-ultra-550b-a55b".to_string(),
            "opencode:nvidia/nemotron-3-nano-free".to_string(),
        ]);

        let mut providers = HashMap::new();
        providers.insert(
            "opencode".to_string(),
            ProviderConfig {
                name: "opencode".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["opencode".to_string(), "acp".to_string()]),
                auth_file: Some("/home/light/.local/share/opencode/auth.json".to_string()),
                auth_token: None,
                warp_proxies: None,
            },
        );
        providers.insert(
            "commandcode".to_string(),
            ProviderConfig {
                name: "commandcode".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["commandcode".to_string(), "acp".to_string()]),
                auth_file: Some("/home/light/.commandcode/auth.json".to_string()),
                auth_token: None,
                warp_proxies: None,
            },
        );
        providers.insert(
            "kilo".to_string(),
            ProviderConfig {
                name: "kilo".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["kilo".to_string(), "acp".to_string()]),
                auth_file: None,
                auth_token: None,
                warp_proxies: None,
            },
        );
        providers.insert(
            "cline".to_string(),
            ProviderConfig {
                name: "cline".to_string(),
                kind: "acp_stdio".to_string(),
                endpoint: None,
                command: Some(vec!["cline".to_string(), "--acp".to_string()]),
                auth_file: Some("/home/light/.cline/data/settings/providers.json".to_string()),
                auth_token: None,
                warp_proxies: None,
            },
        );

        Self {
            server_port: 4001,
            server_host: "127.0.0.1".to_string(),
            default_target: "deepseek".to_string(),
            launcher: LauncherConfig::default(),
            aliases,
            providers,
        }
    }
}
