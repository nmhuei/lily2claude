use lily2claude::launcher::claude::build_settings_json;

#[test]
fn test_settings_json_generation() {
    let settings = build_settings_json(4001, 200000);
    assert!(settings.contains("127.0.0.1:4001"));
    assert!(settings.contains("200000"));
}
