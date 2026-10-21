use lily2claude::config::warp_pool::WarpPool;
use lily2claude::config::Config;
use lily2claude::discovery::catalog::{CatalogRegistry, ALIAS_MODELS};
use lily2claude::engine::router::EngineRouter;

#[test]
fn test_warp_pool_failover_and_cooldown() {
    let pool = WarpPool::for_test(vec![
        "socks5h://127.0.0.1:40001".to_string(),
        "socks5h://127.0.0.1:40002".to_string(),
    ]);

    let p1 = pool.acquire_proxy().expect("Proxy 1");
    let p2 = pool.acquire_proxy().expect("Proxy 2");
    assert_ne!(p1, p2);

    pool.mark_rate_limited(&p1, 60);
    let p3 = pool.acquire_proxy().expect("Should get non-cooldown proxy");
    assert_eq!(p3, p2);
}

#[test]
fn test_in_memory_catalog_discovery() {
    let catalog = CatalogRegistry::load_from_system().expect("Load local catalogs");
    let free_models = catalog.filter_free_models();
    assert!(!free_models.is_empty(), "Should discover local free models in <15ms");

    let deepseek_meta = catalog.get_metadata("deepseek");
    assert!(deepseek_meta.is_some());
    assert!(deepseek_meta.unwrap().context_length >= 200000);
}

/// `ALIAS_MODELS` is what `/v1/models` and the Claude Code picker advertise, so
/// every entry must be routable. Without this, adding a display row without a
/// matching alias in `Config::load_default` would ship a model that 404s.
#[test]
fn test_advertised_aliases_are_routable() {
    let router = EngineRouter::new(Config::load_default());

    for alias in ALIAS_MODELS {
        let candidates = router.resolve_alias(alias.id);
        assert!(
            !candidates.is_empty(),
            "advertised alias '{}' has no route in Config::load_default",
            alias.id
        );
    }
}

/// The picker's per-alias settings and `/v1/models` share ids, so the CLI table
/// (`l2c models`) must show the same context window the picker advertises.
#[test]
fn test_alias_table_matches_catalog_metadata() {
    let catalog = CatalogRegistry::load_from_system().expect("Load local catalogs");

    for alias in ALIAS_MODELS {
        let meta = catalog
            .get_metadata(alias.id)
            .unwrap_or_else(|| panic!("catalog is missing advertised alias '{}'", alias.id));
        assert_eq!(
            meta.context_length, alias.context_length,
            "context window mismatch for '{}'",
            alias.id
        );
        assert_eq!(
            meta.max_output_tokens, alias.max_output_tokens,
            "max output mismatch for '{}'",
            alias.id
        );
    }
}
