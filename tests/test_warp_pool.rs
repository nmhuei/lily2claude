use lily2claude::config::warp_pool::{
    parse_warp_trace, ContainerRuntime, WarpConfig, WarpPool,
};
use std::time::{Duration, Instant};

#[test]
fn test_default_pool_has_two_nodes() {
    let config = WarpConfig::default();
    assert_eq!(config.nodes.len(), 2);
    assert_eq!(config.nodes[0].url, "http://127.0.0.1:40001");
    assert_eq!(config.nodes[1].url, "http://127.0.0.1:40002");
}

#[test]
fn test_candidates_skip_cooling_nodes() {
    let pool = WarpPool::for_test(vec![
        "http://127.0.0.1:40001".to_string(),
        "http://127.0.0.1:40002".to_string(),
    ]);

    pool.report_network_error("http://127.0.0.1:40001");

    let candidates = pool.candidates(2);
    assert_eq!(candidates, vec!["http://127.0.0.1:40002".to_string()]);
}

#[test]
fn test_all_cooling_returns_earliest_expiry() {
    let pool = WarpPool::for_test(vec![
        "http://127.0.0.1:40001".to_string(),
        "http://127.0.0.1:40002".to_string(),
    ]);

    pool.mark_rate_limited("http://127.0.0.1:40001", 60);
    pool.mark_rate_limited("http://127.0.0.1:40002", 5);

    assert!(pool.is_exhausted());
    assert_eq!(
        pool.acquire_proxy().as_deref(),
        Some("http://127.0.0.1:40002")
    );
}

#[test]
fn test_report_success_clears_cooldown() {
    let pool = WarpPool::for_test(vec!["http://127.0.0.1:40001".to_string()]);
    let proxy = "http://127.0.0.1:40001";

    pool.report_network_error(proxy);
    assert!(pool.is_exhausted());

    pool.report_success(proxy);
    assert!(!pool.is_exhausted());
    assert_eq!(pool.acquire_proxy().as_deref(), Some(proxy));
}

#[test]
fn test_preemptive_rotation_counter_on_success() {
    let pool = WarpPool::for_test(vec!["http://127.0.0.1:40001".to_string()]);
    let proxy = "http://127.0.0.1:40001";

    for _ in 0..5 {
        pool.report_success(proxy);
    }
    let statuses = pool.node_statuses();
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].healthy);
}

#[tokio::test]
async fn test_acquire_wait_returns_healthy_node_immediately() {
    let pool = WarpPool::for_test(vec![
        "http://127.0.0.1:40001".to_string(),
        "http://127.0.0.1:40002".to_string(),
    ]);

    let started = Instant::now();
    let proxy = pool.acquire_proxy_wait(Duration::from_secs(5)).await;
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(proxy.is_some());
}

#[tokio::test]
async fn test_acquire_wait_returns_last_resort_after_timeout() {
    let proxy = "http://127.0.0.1:40001";
    let pool = WarpPool::for_test(vec![proxy.to_string()]);
    pool.mark_rate_limited(proxy, 60);

    let started = Instant::now();
    let acquired = pool.acquire_proxy_wait(Duration::from_millis(300)).await;
    assert_eq!(acquired.as_deref(), Some(proxy));
    assert!(started.elapsed() >= Duration::from_millis(250));
}

#[test]
fn test_parse_warp_trace() {
    assert_eq!(
        parse_warp_trace("ip=104.28.1.2\nwarp=on\n"),
        Some("104.28.1.2".to_string())
    );
    assert_eq!(parse_warp_trace("ip=104.28.1.2\nwarp=off\n"), None);
    assert_eq!(parse_warp_trace("warp=on\n"), None);
}

#[test]
fn test_warp_config_from_toml() {
    let config: WarpConfig = toml::from_str(
        r#"
probe_interval_secs = 30
max_attempts = 2
require_warp = true

[[nodes]]
url = "http://127.0.0.1:40001"
container = "test-warp-1"
runtime = "podman"
"#,
    )
    .expect("parse warp config");

    assert_eq!(config.probe_interval_secs, 30);
    assert_eq!(config.nodes.len(), 1);
    assert_eq!(config.nodes[0].container.as_deref(), Some("test-warp-1"));
    assert_eq!(config.nodes[0].runtime, ContainerRuntime::Podman);
}

#[test]
fn test_is_upstream_rate_limited_detection() {
    use lily2claude::config::warp_pool::is_upstream_rate_limited;
    assert!(is_upstream_rate_limited("provider.quota"));
    assert!(is_upstream_rate_limited("Rate limit exceeded"));
    assert!(is_upstream_rate_limited("OpenCode's free models aren't available from your current network because we've detected abusive traffic from this IP address"));
    assert!(is_upstream_rate_limited("HTTP 429 Too Many Requests"));
    assert!(is_upstream_rate_limited("Resource has been exhausted (e.g. check quota)."));
    assert!(is_upstream_rate_limited("FreeTierError: quota exceeded"));
    assert!(!is_upstream_rate_limited("Hello, how can I help you today?"));
    assert!(!is_upstream_rate_limited("SyntaxError: unexpected token"));
}

#[test]
fn test_continuous_failover_cycle_and_fast_recovery() {
    let node1 = "http://127.0.0.1:40001";
    let node2 = "http://127.0.0.1:40002";
    let pool = WarpPool::for_test(vec![node1.to_string(), node2.to_string()]);

    // Initial state: both are healthy
    assert!(!pool.is_proxy_cooling(node1));
    assert!(!pool.is_proxy_cooling(node2));

    // Turn 1: Node 1 hits rate limit -> put in cooling
    pool.mark_rate_limited(node1, 60);
    assert!(pool.is_proxy_cooling(node1));
    assert!(!pool.is_proxy_cooling(node2));

    // Priority check: Node 2 is prioritized as the only healthy candidate
    let candidates = pool.candidates(1);
    assert_eq!(candidates, vec![node2.to_string()]);

    // Fast recovery simulation: Node 1 rotates and reports success with new IP
    pool.report_success(node1);
    assert!(!pool.is_proxy_cooling(node1));

    // Turn 2: Node 2 hits rate limit -> put in cooling
    pool.mark_rate_limited(node2, 60);
    assert!(!pool.is_proxy_cooling(node1));
    assert!(pool.is_proxy_cooling(node2));

    // Priority check: Node 1 is prioritized as the healthy candidate
    let candidates = pool.candidates(1);
    assert_eq!(candidates, vec![node1.to_string()]);

    // Fast recovery simulation: Node 2 rotates and reports success with new IP
    pool.report_success(node2);
    assert!(!pool.is_proxy_cooling(node2));

    // Turn 3: Node 1 hits rate limit again -> cycle continues seamlessly
    pool.mark_rate_limited(node1, 60);
    let candidates = pool.candidates(1);
    assert_eq!(candidates, vec![node2.to_string()]);
}
