use crate::config::warp_pool::WarpPool;
use crate::engine::router::EngineRouter;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub router: Arc<EngineRouter>,
    pub session_mgr: Arc<crate::engine::session::SessionManager>,
    pub token_tracker: Arc<crate::protocol::tokens::TokenUsageTracker>,
}

impl AppState {
    /// Every pool that can currently serve traffic, de-duplicated by Arc identity.
    pub fn warp_pools(&self) -> Vec<Arc<WarpPool>> {
        let mut pools = self.router.warp_pools();
        if let Some(pool) = self.session_mgr.warp_pool() {
            pools.push(pool);
        }

        let mut unique: Vec<Arc<WarpPool>> = Vec::new();
        for pool in pools {
            if !unique.iter().any(|existing| Arc::ptr_eq(existing, &pool)) {
                unique.push(pool);
            }
        }
        unique
    }
}
