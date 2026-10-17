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
pub struct Config {
    pub server_port: u16,
    pub server_host: String,
    pub aliases: HashMap<String, Vec<String>>,
    pub providers: HashMap<String, ProviderConfig>,
}

impl Config {
    pub fn load_default() -> Self {
        let agent_reg = crate::agents::AgentRegistry::default();
        let mut aliases = agent_reg.default_aliases();

        // Ensure transparent aliases have full fallbacks
        aliases.entry("deepseek".to_string()).or_insert_with(|| vec![
            "commandcode:deepseek/deepseek-v4.1-flash".to_string(),
            "opencode:deepseek-v4.1-flash-free".to_string(),
            "kilo:kilo-auto/free".to_string(),
        ]);
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
            "opencode:nvidia/nemotron-3-nano-free".to_string(),
            "kilo:nvidia/nemotron-3.5-lightning:free".to_string(),
        ]);

        let mut providers = HashMap::new();
        providers.insert(
            "opencode".to_string(),
            ProviderConfig {
                name: "opencode".to_string(),
                kind: "opencode_free".to_string(),
                endpoint: Some("https://opencode.ai/inference/openai/v1".to_string()),
                command: None,
                auth_file: Some("/home/light/.local/share/opencode/auth.json".to_string()),
                auth_token: None,
                warp_proxies: Some(vec![
                    "socks5h://127.0.0.1:40001".to_string(),
                    "socks5h://127.0.0.1:40002".to_string(),
                ]),
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
            aliases,
            providers,
        }
    }
}
