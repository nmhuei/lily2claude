use std::process::Command;

#[test]
fn test_cli_help_displays_core_commands() {
    let output = Command::new("cargo")
        .args(["run", "--", "--help"])
        .output()
        .expect("Failed to execute cargo run");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("serve"));
    assert!(stdout.contains("launch"));
    assert!(stdout.contains("list"));
    assert!(stdout.contains("doctor"));
}
