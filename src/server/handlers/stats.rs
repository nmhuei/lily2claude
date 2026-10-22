use crate::server::state::AppState;
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;

pub async fn stats_handler(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.token_tracker.snapshot().await;
    Json(snapshot)
}

pub async fn reset_usage_handler(State(state): State<AppState>) -> impl IntoResponse {
    state.token_tracker.reset().await;
    state.session_mgr.sweep_idle(std::time::Duration::from_secs(0)).await;
    Json(json!({
        "status": "ok",
        "message": "Token usage ledger, session history, and cached workers successfully cleared"
    }))
}
