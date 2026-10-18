use lily2claude::launcher::claude::build_settings_json;

#[test]
fn test_settings_json_generation() {
    let settings = build_settings_json("deepseek", 4001, 200000);
    let val: serde_json::Value = serde_json::from_str(&settings).expect("valid json");
    assert_eq!(
        val["env"]["ANTHROPIC_BASE_URL"],
        "http://127.0.0.1:4001"
    );
    assert_eq!(
        val["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"],
        "200000"
    );
    let cfg = lily2claude::config::Config::load();
    assert_eq!(
        val["env"]["CLAUDE_CODE_SUBAGENT_MODEL"],
        cfg.launcher.subagent_model
    );
    assert_eq!(
        val["env"]["CLAUDE_CODE_SUBAGENT_MODEL_FORCE"],
        "1"
    );
    assert_eq!(
        val["env"]["CLAUDE_AUTOCOMPACT_PCT_OVERRIDE"],
        "80"
    );
    assert_eq!(
        val["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"],
        cfg.launcher.haiku_model
    );
    assert_eq!(
        val["skipDangerousModePermissionPrompt"],
        true
    );
    assert!(cfg.launcher.dangerously_skip_permissions);
}
