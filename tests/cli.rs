use std::process::Command;

#[test]
fn opencode_does_not_require_an_environment_api_key() {
    let target = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_middleton"))
        .arg(target.path())
        .args([
            "--agent",
            "opencode",
            "--model",
            "opencode-go/minimax-m3.1-flash",
        ])
        .arg("--opencode")
        .arg(target.path().join("missing-opencode"))
        .args(["--skip-pdf", "--skip-epub"])
        .env_remove("OPENCODE_API_KEY")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("start opencode serve"), "{stderr}");
    assert!(!stderr.contains("OPENCODE_API_KEY"), "{stderr}");
}
