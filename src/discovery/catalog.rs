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

        // Add standard transparent aliases
        registry.models.insert(
            "deepseek".to_string(),
            ModelMetadata {
                id: "deepseek".to_string(),
                name: "DeepSeek V4.1 Flash".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            },
        );
        registry.models.insert(
            "glm".to_string(),
            ModelMetadata {
                id: "glm".to_string(),
                name: "GLM-5.3-Flash".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            },
        );
        registry.models.insert(
            "kilo".to_string(),
            ModelMetadata {
                id: "kilo".to_string(),
                name: "Kilo Auto Free".to_string(),
                context_length: 256000,
                max_output_tokens: 32768,
                is_free: true,
            },
        );
        registry.models.insert(
            "luna".to_string(),
            ModelMetadata {
                id: "luna".to_string(),
                name: "GPT-5.6 Luna".to_string(),
                context_length: 400000,
                max_output_tokens: 128000,
                is_free: true,
            },
        );
        registry.models.insert(
            "mimo".to_string(),
            ModelMetadata {
                id: "mimo".to_string(),
                name: "MiMo 2.6 Flash".to_string(),
                context_length: 64000,
                max_output_tokens: 8192,
                is_free: true,
            },
        );
        registry.models.insert(
            "nemotron".to_string(),
            ModelMetadata {
                id: "nemotron".to_string(),
                name: "Nemotron 3.5 Lightning".to_string(),
                context_length: 1000000,
                max_output_tokens: 65536,
                is_free: true,
            },
        );

        Ok(registry)
    }

    pub fn filter_free_models(&self) -> Vec<ModelMetadata> {
        self.models.values().filter(|m| m.is_free).cloned().collect()
    }

    pub fn get_metadata(&self, model: &str) -> Option<ModelMetadata> {
        if let Some(m) = self.models.get(model) {
            return Some(m.clone());
        }
        if model.contains("deepseek") {
            return Some(ModelMetadata {
                id: model.to_string(),
                name: "DeepSeek".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            });
        }
        if model.contains("glm") {
            return Some(ModelMetadata {
                id: model.to_string(),
                name: "GLM".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            });
        }
        if model.contains("nemotron") {
            return Some(ModelMetadata {
                id: model.to_string(),
                name: "Nemotron".to_string(),
                context_length: 1000000,
                max_output_tokens: 65536,
                is_free: true,
            });
        }
        if model.contains("claude")
            || model.contains("haiku")
            || model.contains("sonnet")
            || model.contains("opus")
            || model == "default"
        {
            return Some(ModelMetadata {
                id: model.to_string(),
                name: "DeepSeek V4.1 Flash (Subagent Auto-Remap)".to_string(),
                context_length: 200000,
                max_output_tokens: 32000,
                is_free: true,
            });
        }
        None
    }
}
