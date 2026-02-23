use std::process::Command;

#[test]
fn test_version_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_vaprs"))
        .arg("--version")
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("vaprs"));
}

#[test]
fn test_help_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_vaprs"))
        .arg("--help")
        .output()
        .expect("failed to execute");
    assert!(output.status.success());
}

#[test]
fn test_missing_config_exits_with_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_vaprs"))
        .arg("-f")
        .arg("/nonexistent/config.toml")
        .output()
        .expect("failed to execute");
    assert!(!output.status.success());
}
