use crate::server::state::AppState;
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;

pub async fn health_handler() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "lily2claude", "version": "0.1.0"}))
}

/// Real data-plane health for the WARP egress pool.
///
/// A published port being open is not evidence that the tunnel works; each
/// node's `egress_ip` only gets populated after a successful `warp=on` probe.
pub async fn warp_health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let pools = state.warp_pools();
    let nodes: Vec<_> = pools
        .iter()
        .flat_map(|pool| pool.node_statuses())
        .collect();

    let distinct_egress_ips = nodes
        .iter()
        .filter_map(|node| node.egress_ip.clone())
        .collect::<std::collections::HashSet<_>>()
        .len();

    let healthy = nodes.iter().filter(|node| node.healthy).count();
    let status = if nodes.is_empty() {
        "not_configured"
    } else if healthy == nodes.len() {
        "ok"
    } else if healthy == 0 {
        "down"
    } else {
        "degraded"
    };

    Json(json!({
        "status": status,
        "pools": pools.len(),
        "nodes_total": nodes.len(),
        "nodes_healthy": healthy,
        "distinct_egress_ips": distinct_egress_ips,
        "nodes": nodes,
    }))
}
