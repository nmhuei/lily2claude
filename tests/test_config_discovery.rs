use lily2claude::config::warp_pool::WarpPool;
use lily2claude::discovery::catalog::CatalogRegistry;

#[test]
fn test_warp_pool_failover_and_cooldown() {
    let pool = WarpPool::new(vec![
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
