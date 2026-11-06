use std::{fs, process::Command};
use tempfile::tempdir;

#[test]
fn cli_reports_context_and_cause_without_debug_output() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let model = dir.path().join("invalid.lstm");
    fs::write(&model, "{globals:{length:'missing'}}")?;
    let output = Command::new(env!("CARGO_BIN_EXE_lstm"))
        .arg(&model)
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.starts_with("Error: Loading "), "{stderr}");
    assert!(stderr.contains("global length"), "{stderr}");
    assert!(stderr.contains("Unknown name `missing`"), "{stderr}");
    assert!(!stderr.contains("Context {"), "{stderr}");

    let missing = dir.path().join("missing.lstm");
    let output = Command::new(env!("CARGO_BIN_EXE_lstm"))
        .arg(&missing)
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.starts_with("Error: Reading "), "{stderr}");
    assert!(stderr.contains("missing.lstm"), "{stderr}");
    assert!(stderr.contains("I/O operation failed:"), "{stderr}");
    Ok(())
}

#[test]
fn invalid_trace_ranges_are_reported_by_clap() -> Result<(), Box<dyn std::error::Error>> {
    for range in ["invalid", "3:2", "-1", "99999999999999999999999"] {
        let output = Command::new(env!("CARGO_BIN_EXE_lstm"))
            .args(["unused.lstm", &format!("--trace-generations={range}")])
            .output()?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("Expected N or START:END"), "{stderr}");
    }
    Ok(())
}
