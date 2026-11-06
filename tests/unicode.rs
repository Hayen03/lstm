use serde_json::Value;
use std::{fs, process::Command};
use tempfile::tempdir;

#[test]
fn cli_axiom_and_trace_filters_use_canonical_symbols() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let model = dir.path().join("unicode.lstm");
    fs::write(
        &model,
        r#"{width:16,height:16,axiom:"B",symbols:["é{x:3}"],
             rules:[{from:"é",to:"e\u0301"}],
             drawing:[{from:"e\u0301",to:"F{length:x}"}]}"#,
    )?;
    for axiom in ["éB", "e\u{301}B"] {
        for filter in ["é", "e\u{301}"] {
            let output = Command::new(env!("CARGO_BIN_EXE_lstm"))
                .arg(&model)
                .args([
                    "--axiom",
                    axiom,
                    "-n",
                    "1",
                    "--trace-symbol",
                    filter,
                    "--trace-draw",
                    "--trace-format",
                    "jsonl",
                    "-o",
                ])
                .arg(dir.path().join("frame.png"))
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let trace = String::from_utf8(output.stderr)?;
            let events = trace
                .lines()
                .map(serde_json::from_str)
                .collect::<Result<Vec<Value>, _>>()?;
            assert!(!events.is_empty());
            assert!(events.iter().all(|e| e["symbol"] == "é"));
            for phase in ["growth", "drawing"] {
                assert!(
                    events
                        .iter()
                        .any(|e| e["event"] == "rule_selected" && e["phase"] == phase)
                );
            }
        }
    }
    Ok(())
}
