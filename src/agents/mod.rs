pub mod commandcode;
pub mod kilo;
pub mod cline;
pub mod opencode;
pub mod generic_acp;
pub mod registry;

pub use registry::AgentRegistry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use crate::discovery::catalog::ModelMetadata;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthStatus {
    Ready,
    Missing(String),
    NotRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDiagnostic {
    pub id: String,
    pub display_name: String,
    pub installed: bool,
    pub binary_path: Option<String>,
    pub version: Option<String>,
    pub auth_status: AuthStatus,
    pub error: Option<String>,
}

#[async_trait]
pub trait Agent: Send + Sync {
    /// Machine-readable identifier (e.g. "commandcode", "kilo", "cline", "opencode")
    fn id(&self) -> &'static str;

    /// Human-friendly display name (e.g. "CommandCode", "Kilo Agent")
    fn display_name(&self) -> &'static str;

    /// Detect if binary is installed, its path and version
    fn detect_installation(&self) -> (bool, Option<String>, Option<String>);

    /// Check authentication status
    fn check_auth(&self) -> AuthStatus;

    /// Discover available models from this agent's local catalog/cache
    fn discover_models(&self) -> Vec<ModelMetadata>;

    /// Default model aliases provided by this agent: (alias, qualified_model_id)
    fn default_aliases(&self) -> Vec<(&'static str, String)>;

    /// Run diagnostic health check
    fn diagnose(&self) -> AgentDiagnostic {
        let (installed, binary_path, version) = self.detect_installation();
        let auth_status = self.check_auth();
        AgentDiagnostic {
            id: self.id().to_string(),
            display_name: self.display_name().to_string(),
            installed,
            binary_path,
            version,
            auth_status,
            error: None,
        }
    }

    /// Execute a single turn / prompt with optional session persistence
    async fn execute_turn(
        &self,
        model: &str,
        prompt: &str,
        session_key: Option<&str>,
        session_mgr: Option<&crate::engine::session::SessionManager>,
        tx: mpsc::Sender<String>,
    ) -> anyhow::Result<()>;
}
