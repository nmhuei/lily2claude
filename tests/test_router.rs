use lily2claude::config::Config;
use lily2claude::engine::router::EngineRouter;

#[test]
fn test_router_prioritizes_local_acp_over_cloud_for_deepseek() {
    let config = Config::load_default();
    let router = EngineRouter::new(config);

    let candidates = router.resolve_alias("deepseek");
    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].provider, "commandcode");
    assert_eq!(candidates[1].provider, "opencode");
}
