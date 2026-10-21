use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub id: String,
    pub name: String,
    pub context_length: u32,
    pub max_output_tokens: u32,
    pub is_free: bool,
}

/// One Claude-facing alias this bridge advertises.
///
/// `l2c list`, `GET /v1/models` and the Claude Code model picker all render this
/// table, so an alias can no longer exist in one place and be missing from
/// another. Every `id` must resolve through
/// [`crate::engine::router::EngineRouter::resolve_alias`];
/// `tests/test_config_discovery.rs` enforces that, so adding an entry here
/// without a matching route in `Config::load_default` fails the suite.
pub struct AliasModel {
    /// Alias accepted by `POST /v1/messages`.
    pub id: &'static str,
    /// Short human name used by `/v1/models` and the CLI table.
    pub name: &'static str,
    /// Model picker label, e.g. `"DeepSeek V4.1 Flash (deepseek)"`.
    pub label: &'static str,
    pub description: &'static str,
    /// Claude model this alias emulates, for Claude Code's UI heuristics.
    pub behaves_as: &'static str,
    pub context_length: u32,
    pub max_output_tokens: u32,
    /// Token count at which Claude Code compacts a session on this alias.
    pub auto_compact_window: u32,
    /// Claude Code reasoning effort for this alias, when it wants one.
    pub effort_level: Option<&'static str>,
}

/// The aliases advertised to Claude Code, in display order.
pub const ALIAS_MODELS: &[AliasModel] = &[
    AliasModel {
        id: "deepseek",
        name: "DeepSeek V4.1 Flash",
        label: "DeepSeek V4.1 Flash (deepseek)",
        description: "DeepSeek V4.1 Flash Free via OpenCode (200k Context · Default Coding Model)",
        behaves_as: "claude-sonnet-4-5",
        context_length: 200_000,
        max_output_tokens: 32_000,
        auto_compact_window: 160_000,
        effort_level: Some("high"),
    },
    AliasModel {
        id: "glm",
        name: "GLM 5.3 Flash",
        label: "GLM 5.3 Flash (glm)",
        description: "GLM 5.3 Flash Free via OpenCode (200k Context · High Intelligence & Reasoning)",
        behaves_as: "claude-sonnet-4-5",
        context_length: 200_000,
        max_output_tokens: 32_000,
        auto_compact_window: 160_000,
        effort_level: Some("high"),
    },
    AliasModel {
        id: "claude-haiku-4-5-20251001",
        name: "Haiku 4.5 (remapped)",
        label: "Haiku 4.5 (haiku)",
        description: "Claude Haiku 4.5 · Fast Subagent Engine (backed by Free DeepSeek/MiMo)",
        behaves_as: "claude-haiku-4-5",
        context_length: 200_000,
        max_output_tokens: 32_000,
        auto_compact_window: 160_000,
        effort_level: None,
    },
    AliasModel {
        id: "mimo",
        name: "MiMo V2.6 Flash",
        label: "MiMo V2.6 Flash (mimo)",
        description: "Xiaomi MiMo V2.6 Flash Free via OpenCode (64k Context · Ultra Low Latency)",
        behaves_as: "claude-haiku-4-5",
        context_length: 64_000,
        max_output_tokens: 8_192,
        auto_compact_window: 50_000,
        effort_level: None,
    },
    AliasModel {
        id: "nemotron",
        name: "Nemotron 3.5 Lightning",
        label: "Nemotron 3 Nano (nemotron)",
        description: "NVIDIA Nemotron 3 Free via OpenCode (1M Context · Large Codebase)",
        behaves_as: "claude-opus-4-6",
        context_length: 1_000_000,
        max_output_tokens: 65_536,
        auto_compact_window: 800_000,
        effort_level: None,
    },
    AliasModel {
        id: "qwen",
        name: "Qwen 3.6 Plus",
        label: "Qwen 3.6 Plus (qwen)",
        description: "Alibaba Qwen 3.6 Plus Free via OpenCode (262k Context · Math & Code)",
        behaves_as: "claude-sonnet-4-5",
        context_length: 262_000,
        max_output_tokens: 32_768,
        auto_compact_window: 200_000,
        effort_level: None,
    },
    AliasModel {
        id: "minimax",
        name: "MiniMax M3.1 Flash",
        label: "MiniMax M3 (minimax)",
        description: "MiniMax M3 Free via OpenCode (200k Context · General Assistant)",
        behaves_as: "claude-sonnet-4-5",
        context_length: 200_000,
        max_output_tokens: 32_000,
        auto_compact_window: 160_000,
        effort_level: None,
    },
    AliasModel {
        id: "kimi",
        name: "Kimi K2.5",
        label: "Kimi K2.5 (kimi)",
        description: "Moonshot Kimi K2.5 Free via OpenCode (262k Context · Long Context Agent)",
        behaves_as: "claude-sonnet-4-5",
        context_length: 262_000,
        max_output_tokens: 32_768,
        auto_compact_window: 200_000,
        effort_level: None,
    },
    AliasModel {
        id: "kilo",
        name: "Kilo Auto Free",
        label: "Kilo Auto Free (kilo)",
        description: "Kilo Auto Free Router (262k Context · Dynamic Multi-Provider)",
        behaves_as: "claude-sonnet-4-5",
        context_length: 256_000,
        max_output_tokens: 32_768,
        auto_compact_window: 200_000,
        effort_level: None,
    },
    AliasModel {
        id: "luna",
        name: "GPT-5.6 Luna",
        label: "GPT-5.6 Luna (luna)",
        description: "GPT-5.6 Luna via Cline (400k Context · Frontier Reasoning)",
        behaves_as: "claude-opus-4-6",
        context_length: 400_000,
        max_output_tokens: 128_000,
        auto_compact_window: 320_000,
        effort_level: None,
    },
];

fn metadata_from_alias(alias: &AliasModel, id: String) -> ModelMetadata {
    ModelMetadata {
        id,
        name: alias.name.to_string(),
        context_length: alias.context_length,
        max_output_tokens: alias.max_output_tokens,
        is_free: true,
    }
}

#[derive(Debug, Clone)]
pub struct CatalogRegistry {
    models: HashMap<String, ModelMetadata>,
}

impl Default for CatalogRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogRegistry {
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
        }
    }

    pub fn all_models(&self) -> Vec<ModelMetadata> {
        self.models.values().cloned().collect()
    }

    pub fn load_from_system() -> anyhow::Result<Self> {
        let mut registry = Self::new();

        // Dynamically discover models from all registered agent modules
        let agent_reg = crate::agents::AgentRegistry::default();
        for m in agent_reg.discover_all_models() {
            registry.models.insert(m.id.clone(), m.clone());
            if let Some(stripped) = m.id.strip_prefix("kilo:") {
                let mut stripped_m = m.clone();
                stripped_m.id = stripped.to_string();
                registry.models.insert(stripped.to_string(), stripped_m);
            }
        }

        // Advertised aliases. Inserted after discovery so a hard-coded alias
        // always wins over an agent cache entry with the same name.
        for alias in ALIAS_MODELS {
            registry
                .models
                .insert(alias.id.to_string(), metadata_from_alias(alias, alias.id.to_string()));
        }

        Ok(registry)
    }

    pub fn filter_free_models(&self) -> Vec<ModelMetadata> {
        self.models.values().filter(|m| m.is_free).cloned().collect()
    }

    pub fn get_metadata(&self, model: &str) -> Option<ModelMetadata> {
        if let Some(m) = self.models.get(model) {
            return Some(m.clone());
        }
        let lower = model.to_ascii_lowercase();
        if let Some(alias) = ALIAS_MODELS.iter().find(|m| lower.contains(m.id)) {
            return Some(metadata_from_alias(alias, model.to_string()));
        }
        // Claude-family names, including every dated variant, are remapped onto
        // the default upstream model by `EngineRouter::resolve_alias_raw`.
        if lower.contains("claude")
            || lower.contains("haiku")
            || lower.contains("sonnet")
            || lower.contains("opus")
            || lower.contains("fable")
            || lower == "default"
        {
            let alias = &ALIAS_MODELS[0];
            let mut meta = metadata_from_alias(alias, model.to_string());
            meta.name = "DeepSeek V4.1 Flash (Subagent Auto-Remap)".to_string();
            return Some(meta);
        }
        None
    }
}
