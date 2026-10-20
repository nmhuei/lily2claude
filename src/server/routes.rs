use super::handlers::{
    count_tokens_handler, health_handler, messages_handler, models_handler, reset_usage_handler,
    stats_handler,
};
use super::state::AppState;
use crate::engine::router::EngineRouter;
use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub fn create_router(router: Arc<EngineRouter>) -> Router {
    let session_mgr = Arc::new(crate::engine::session::SessionManager::new());
    let token_tracker = Arc::new(crate::protocol::tokens::TokenUsageTracker::new());
    let state = AppState {
        router,
        session_mgr: session_mgr.clone(),
        token_tracker,
    };

    // Background idle cleanup: evicts sessions inactive for > 5 minutes
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            session_mgr.sweep_idle(std::time::Duration::from_secs(300)).await;
        }
    });

    Router::new()
        .route("/health", get(health_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/stats", get(stats_handler))
        .route("/v1/usage", get(stats_handler))
        .route("/v1/usage/reset", post(reset_usage_handler).get(reset_usage_handler))
        .route("/v1/reset", post(reset_usage_handler).get(reset_usage_handler))
        .route("/v1/messages/count_tokens", post(count_tokens_handler))
        .route("/v1/messages", post(messages_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
