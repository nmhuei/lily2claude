use lily2claude::adapters::http_upstream::HttpUpstreamAdapter;
use lily2claude::config::warp_pool::WarpPool;

#[test]
fn test_http_adapter_config() {
    let pool = WarpPool::new(vec!["socks5h://127.0.0.1:40001".to_string()]);
    let adapter = HttpUpstreamAdapter::new(
        "https://opencode.ai/inference/openai/v1",
        Some("dummy-token"),
        Some(pool),
    );
    assert_eq!(adapter.base_url(), "https://opencode.ai/inference/openai/v1");
}
