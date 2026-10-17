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

#[test]
fn test_router_resolves_claude_subagent_models_to_deepseek() {
    let config = Config::load_default();
    let router = EngineRouter::new(config);

    for subagent_model in &[
        "claude-3-5-haiku-20241022",
        "claude-3-haiku-20240307",
        "claude-3-5-sonnet-20241022",
        "claude-3-7-sonnet-20250219",
        "haiku",
        "sonnet",
        "opus",
        "default",
    ] {
        let candidates = router.resolve_alias(subagent_model);
        assert!(!candidates.is_empty(), "Failed to resolve subagent model: {}", subagent_model);
        assert_eq!(candidates[0].provider, "commandcode");
        assert_eq!(candidates[0].model, "deepseek/deepseek-v4.1-flash");
    }
}
