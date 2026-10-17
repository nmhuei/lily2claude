use lily2claude::config::Config;
use lily2claude::engine::router::EngineRouter;
use lily2claude::server::create_router;

#[tokio::test]
async fn test_server_routes_setup() {
    let config = Config::load_default();
    let router = std::sync::Arc::new(EngineRouter::new(config));
    let _app = create_router(router);
}
