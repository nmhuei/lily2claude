use super::{
    commandcode::CommandCodeAgent,
    kilo::KiloAgent,
    cline::ClineAgent,
    opencode::OpenCodeAgent,
    Agent, AgentDiagnostic,
};
use crate::discovery::catalog::ModelMetadata;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct AgentRegistry {
    agents: HashMap<String, Arc<dyn Agent>>,
    order: Vec<String>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        let mut reg = Self {
            agents: HashMap::new(),
            order: Vec::new(),
        };
        reg.register(Arc::new(CommandCodeAgent));
        reg.register(Arc::new(OpenCodeAgent));
        reg.register(Arc::new(KiloAgent::default()));
        reg.register(Arc::new(ClineAgent));
        reg
    }
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn register(&mut self, agent: Arc<dyn Agent>) {
        let id = agent.id().to_string();
        if !self.order.contains(&id) {
            self.order.push(id.clone());
        }
        self.agents.insert(id, agent);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Agent>> {
        self.agents.get(id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<dyn Agent>> {
        self.order
            .iter()
            .filter_map(|id| self.agents.get(id).cloned())
            .collect()
    }

    pub fn discover_all_models(&self) -> Vec<ModelMetadata> {
        let mut all = Vec::new();
        for agent in self.all() {
            all.extend(agent.discover_models());
        }
        all
    }

    pub fn diagnose_all(&self) -> Vec<AgentDiagnostic> {
        self.all().into_iter().map(|a| a.diagnose()).collect()
    }

    pub fn default_aliases(&self) -> HashMap<String, Vec<String>> {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for agent in self.all() {
            for (alias, model_id) in agent.default_aliases() {
                map.entry(alias.to_string()).or_default().push(model_id);
            }
        }
        map
    }
}
