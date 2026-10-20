use crate::engine::router::EngineRouter;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub router: Arc<EngineRouter>,
    pub session_mgr: Arc<crate::engine::session::SessionManager>,
    pub token_tracker: Arc<crate::protocol::tokens::TokenUsageTracker>,
}
