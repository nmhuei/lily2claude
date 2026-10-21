use crate::discovery::catalog::ALIAS_MODELS;
use axum::{response::IntoResponse, Json};
use serde_json::json;

/// Fixed timestamp for the model list. The aliases mirror upstream models that
/// this bridge does not control, so a per-boot time would make the list churn
/// for no reason.
const LIST_CREATED_AT: &str = "2026-01-01T00:00:00Z";

/// Anthropic-shaped model list.
///
/// Rendered from [`ALIAS_MODELS`] — the same table that drives the Claude Code
/// picker — so the catalogue can never advertise a model this server cannot
/// route, or omit one it can.
pub async fn models_handler() -> impl IntoResponse {
    let data: Vec<serde_json::Value> = ALIAS_MODELS
        .iter()
        .map(|m| {
            json!({
                "type": "model",
                "id": m.id,
                "display_name": m.name,
                "created_at": LIST_CREATED_AT,
            })
        })
        .collect();

    Json(json!({
        "data": data,
        "has_more": false,
        "first_id": ALIAS_MODELS.first().map(|m| m.id),
        "last_id": ALIAS_MODELS.last().map(|m| m.id),
    }))
}
