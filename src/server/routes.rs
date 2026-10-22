use super::handlers::{
    count_tokens_handler, health_handler, messages_handler, models_handler, reset_usage_handler,
    stats_handler, warp_health_handler,
};
use super::state::AppState;
use crate::engine::router::EngineRouter;
use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

pub fn create_router(router: Arc<EngineRouter>) -> Router {
    let session_mgr = Arc::new(crate::engine::session::SessionManager::new());
    let token_tracker = Arc::new(crate::protocol::tokens::TokenUsageTracker::new());
    let state = AppState {
        router,
        session_mgr: session_mgr.clone(),
        token_tracker,
    };

    // Start one real-egress prober per configured pool.
    if tokio::runtime::Handle::try_current().is_ok() {
        for pool in state.warp_pools() {
            pool.spawn_prober();
        }
    }

    // Background idle cleanup: evicts sessions inactive for > 2 minutes (default) or L2C_IDLE_TIMEOUT_SECS
    tokio::spawn(async move {
        let idle_timeout = std::env::var("L2C_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs)
            .unwrap_or(std::time::Duration::from_secs(120));
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            session_mgr.sweep_idle(idle_timeout).await;
        }
    });

    Router::new()
        .route("/health", get(health_handler))
        .route("/health/warp", get(warp_health_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/stats", get(stats_handler))
        .route("/v1/usage", get(stats_handler))
        // State-mutating endpoints are POST-only. Exposing them via GET let any
        // page on the machine wipe the usage ledger with `<img src=...>`.
        .route("/v1/usage/reset", post(reset_usage_handler))
        .route("/v1/reset", post(reset_usage_handler))
        .route("/v1/messages/count_tokens", post(count_tokens_handler))
        .route("/v1/messages", post(messages_handler))
        // No CORS layer: the only clients are the Claude Code CLI and `l2c`,
        // which are not browsers. A permissive policy made this local server
        // reachable from any web page the user had open.
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
