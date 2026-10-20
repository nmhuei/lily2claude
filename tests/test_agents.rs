use lily2claude::agents::{
    commandcode::CommandCodeAgent,
    kilo::KiloAgent,
    cline::ClineAgent,
    opencode::OpenCodeAgent,
    generic_acp::GenericAcpAgent,
    Agent, AgentRegistry,
};
use std::sync::Arc;

#[test]
fn test_agent_registry_registration_and_diagnose() {
    let mut registry = AgentRegistry::new();
    let cc = Arc::new(CommandCodeAgent);
    let kilo = Arc::new(KiloAgent::default());
    let cline = Arc::new(ClineAgent);
    let opencode = Arc::new(OpenCodeAgent);

    registry.register(cc);
    registry.register(kilo);
    registry.register(cline);
    registry.register(opencode);

    assert_eq!(registry.all().len(), 4);
    assert!(registry.get("commandcode").is_some());
    assert!(registry.get("kilo").is_some());
    assert!(registry.get("cline").is_some());
    assert!(registry.get("opencode").is_some());

    let diags = registry.diagnose_all();
    assert_eq!(diags.len(), 4);
    for d in diags {
        println!("Agent: {} ({}) - installed: {}, auth: {:?}", d.display_name, d.id, d.installed, d.auth_status);
        assert!(!d.id.is_empty());
    }

    let models = registry.discover_all_models();
    assert!(!models.is_empty(), "Should discover models from local agent caches");

    let aliases = registry.default_aliases();
    assert!(aliases.contains_key("deepseek"));
    assert!(aliases.contains_key("glm"));
    assert!(aliases.contains_key("kilo"));
    assert!(aliases.contains_key("luna"));
}

#[test]
fn test_generic_acp_agent() {
    let agent = GenericAcpAgent::new("custom_agent", "Custom Agent", "echo", vec!["acp".to_string()]);
    assert_eq!(agent.id(), "custom_agent");
    assert_eq!(agent.display_name(), "Custom Agent");
}
