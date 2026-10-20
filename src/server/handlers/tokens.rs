use crate::protocol::anthropic::MessagesRequest;
use axum::{response::IntoResponse, Json};
use serde_json::json;

pub async fn count_tokens_handler(Json(req_val): Json<serde_json::Value>) -> impl IntoResponse {
    // Attempt exact calculation by deserializing into MessagesRequest
    let tokens = if let Ok(req) = serde_json::from_value::<MessagesRequest>(req_val.clone()) {
        crate::protocol::tokens::TokenCalculator::count_request_tokens(&req)
    } else {
        // High-precision BPE fallback on raw payload string
        let raw = req_val.to_string();
        crate::protocol::tokens::TokenCalculator::count_text(&raw).max(1)
    };
    Json(json!({"input_tokens": tokens}))
}
