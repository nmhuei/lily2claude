use crate::server::state::AppState;
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;

pub async fn stats_handler(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.token_tracker.snapshot().await;
    Json(snapshot)
}

pub async fn reset_usage_handler(State(state): State<AppState>) -> impl IntoResponse {
    state.token_tracker.reset().await;
    Json(json!({
        "status": "ok",
        "message": "Token usage ledger and session history successfully reset to 0"
    }))
}
