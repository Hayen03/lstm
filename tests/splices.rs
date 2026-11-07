use serde_json::{Value, json};
use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Run {
    success: bool,
    stderr: String,
    png: Vec<u8>,
}

// Bound the subprocess lifetime: a regressed successor cursor used to loop forever.
// A file sink also avoids blocking on a full stderr pipe during verbose tracing.
fn run(mut model: Value, generation: usize, options: &[&str]) -> Result<Run> {
    let dir = tempfile::tempdir()?;
    model["width"] = json!(32);
    model["height"] = json!(32);
    let source = dir.path().join("model.lstm");
    let png = dir.path().join("frame.png");
    let stderr = dir.path().join("stderr");
    fs::write(&source, serde_json::to_vec(&model)?)?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_lstm"))
        .arg(source)
        .args([
            "-n",
            &generation.to_string(),
            "--seed",
            "splice-tests",
            "-o",
        ])
        .arg(&png)
        .args(options)
        .stdout(Stdio::null())
        .stderr(fs::File::create(&stderr)?)
        .spawn()?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(5) {
            child.kill()?;
            child.wait()?;
            panic!("LSTM did not terminate within five seconds");
        }
        thread::sleep(Duration::from_millis(10));
    };
    Ok(Run {
        success: status.success(),
        stderr: fs::read_to_string(stderr)?,
        png: if png.exists() { fs::read(png)? } else { vec![] },
    })
}

fn events(output: &Run) -> Vec<Value> {
    assert!(output.success, "{}", output.stderr);
    output
        .stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

const TRACE: &[&str] = &["--trace-draw", "--trace-format", "jsonl"];

#[test]
fn growth_splices_keep_cursors_environments_and_empty_successors() -> Result {
    let output = run(
        json!({
            "axiom":"${[]}A{n:2}Z",
            "symbols":["B{x:k}"],
            "rules":[{
                "name":"expand", "from":"A{n:_}", "locals":{"k":"n+1"},
                "to":"${[]}${['B' for _ in range(n + gen() - 1)]}${{'symbol':'B','args':{'x':99}}}C"
            }]
        }),
        1,
        TRACE,
    )?;
    let all = events(&output);
    let emitted: Vec<_> = all
        .iter()
        .filter(|e| e["phase"] == "growth" && e["event"] == "token_emitted")
        .collect();
    assert_eq!(
        emitted
            .iter()
            .map(|e| e["output"]["symbol"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["B", "B", "B", "C"]
    );
    assert_eq!(
        emitted
            .iter()
            .take(3)
            .map(|e| e["output"]["args"]["x"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [3, 3, 99]
    );
    assert!(
        emitted
            .iter()
            .all(|e| e["rule"] == "expand" && e["token_index"] == 0 && e["symbol"] == "A")
    );
    assert_eq!(emitted[0]["successor_index"], 1);
    assert_eq!(emitted[1]["splice_index"], 1);
    assert!(all.iter().any(|e| e["event"] == "splice_evaluated"
        && e["phase"] == "growth"
        && e["token_count"] == 0));
    assert!(
        all.iter()
            .any(|e| e["event"] == "token_unchanged" && e["symbol"] == "Z")
    );
    let defaults: Vec<_> = all
        .iter()
        .filter(|e| {
            e["phase"] == "growth" && e["event"] == "argument_evaluated" && e["source"] == "default"
        })
        .collect();
    assert_eq!(defaults.len(), 2); // The explicit x=99 does not evaluate its default.
    assert_eq!(
        all.iter()
            .filter(|e| e["event"] == "runtime_query"
                && e["query"] == "gen"
                && e["phase"] == "growth")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn defaults_observe_live_state_in_all_three_phases() -> Result {
    for phase in ["axiom", "growth", "drawing"] {
        let mut model = json!({
            "axiom":"A", "symbols":["B{x:pos()[0]}"],
            "drawing":[{"name":"step","from":"B","to":"D{x:x+10,y:0}"}]
        });
        let expansion = json!({"name":"expand","from":"A","to":"${['B','B']}"});
        let generation = match phase {
            "axiom" => {
                model["axiom"] = json!("${['B','B']}");
                0
            }
            "growth" => {
                model["rules"] = json!([expansion]);
                1
            }
            _ => {
                model["drawing"].as_array_mut().unwrap().push(expansion);
                0
            }
        };
        let all = events(&run(model, generation, TRACE)?);
        let defaults: Vec<_> = all
            .iter()
            .filter(|e| {
                e["event"] == "argument_evaluated"
                    && e["source"] == "default"
                    && e["phase"] == phase
            })
            .collect();
        assert_eq!(defaults.len(), 2, "{phase}");
        assert_eq!(defaults[0]["value"], 0.0);
        assert_eq!(defaults[1]["value"], 10.0);
        let xs: Vec<_> = all
            .iter()
            .filter(|e| e["event"] == "instruction" && e["instruction"] == "D")
            .map(|e| e["arguments"]["x"].as_f64().unwrap())
            .collect();
        assert_eq!(xs, [10.0, 20.0], "{phase}");
        assert_eq!(
            all.iter()
                .filter(|e| e["event"] == "runtime_query"
                    && e["query"] == "pos"
                    && e["phase"] == phase)
                .count(),
            2
        );
    }
    Ok(())
}

#[test]
fn drawing_trace_restores_parent_rules_and_respects_filters() -> Result {
    let model = json!({
        "axiom":"A", "symbols":["X{x:width()}"],
        "drawing":[
            {"name":"parent","from":"A","to":"B${['X','X']}${[]}${'F'}"},
            {"name":"child","from":"B","to":"D{x:1,y:0}"},
            {"name":"leaf","from":"X","to":"W{width:width()+1}"}
        ]
    });
    let full = events(&run(model.clone(), 0, TRACE)?);
    let parent: Vec<_> = full
        .iter()
        .filter(|e| e["rule"] == "parent")
        .cloned()
        .collect();
    let filtered = events(&run(
        model,
        0,
        &[
            "--trace-draw",
            "--trace-format",
            "jsonl",
            "--trace-rule",
            "parent",
            "--trace-symbol",
            "A",
        ],
    )?);
    assert_eq!(filtered, parent);
    let splices: Vec<_> = parent
        .iter()
        .filter(|e| e["event"] == "splice_evaluated")
        .collect();
    assert_eq!(splices.len(), 3);
    assert!(
        splices
            .iter()
            .all(|e| e["symbol"] == "A" && e["token_index"] == 0)
    );
    let defaults: Vec<_> = parent
        .iter()
        .filter(|e| e["event"] == "argument_evaluated" && e["source"] == "default")
        .collect();
    assert_eq!(defaults.len(), 2);
    assert_eq!(defaults[0]["value"], 1.0);
    assert_eq!(defaults[1]["value"], 2.0);
    Ok(())
}

#[test]
fn axiom_trace_indexes_outputs_and_lookahead_does_not_repeat_evaluation() -> Result {
    let model = json!({
        "axiom":"A${['B','B']}C{n:1}", "symbols":["B{x:pos()[0]}"],
        "drawing":[
            {"from":"A","locals":{"next":"rctx(0)"},"to":"M{length:1}"},
            {"from":"B","to":"D{x:x+10,y:0}"}
        ]
    });
    let all = events(&run(model, 0, TRACE)?);
    let splices: Vec<_> = all
        .iter()
        .filter(|e| e["event"] == "splice_evaluated")
        .collect();
    assert_eq!(splices.len(), 1);
    assert_eq!(splices[0]["symbol"], "${}");
    let emitted: Vec<_> = all
        .iter()
        .filter(|e| e["event"] == "token_emitted" && e["phase"] == "axiom")
        .collect();
    assert_eq!(emitted.len(), 2);
    assert_eq!(emitted[0]["token_index"], 1);
    assert_eq!(emitted[1]["token_index"], 2);
    assert!(
        emitted
            .iter()
            .all(|e| e["symbol"] == "B" && e["output"]["args"]["x"] == 0.0)
    );
    assert!(all.iter().any(|e| e["phase"] == "axiom"
        && e["event"] == "argument_evaluated"
        && e["symbol"] == "C"
        && e["token_index"] == 3));
    Ok(())
}

#[test]
fn tracing_does_not_change_randomness_or_rendering() -> Result {
    let model = json!({
        "axiom":"A", "symbols":["B{x:2+random()*0.06}"],
        "rules":[{"from":"A","to":"${['B' for _ in range(4)]}"}],
        "drawing":[{"from":"B","to":"R{deg:random()*0.6-30}F{length:x}"}]
    });
    let plain = run(model.clone(), 1, &[])?;
    let traced = run(model, 1, TRACE)?;
    assert!(plain.success, "{}", plain.stderr);
    assert!(traced.success, "{}", traced.stderr);
    assert_eq!(plain.png, traced.png);
    assert!(!plain.png.is_empty());
    Ok(())
}

#[test]
fn splice_errors_include_phase_rule_successor_and_element() -> Result {
    for (phase, generation) in [("axiom", 0), ("growth", 1), ("drawing", 0)] {
        let mut model = json!({"axiom":"A"});
        if phase == "axiom" {
            model["axiom"] = json!("${['F',42]}");
        } else {
            model[if phase == "growth" {
                "rules"
            } else {
                "drawing"
            }] = json!([
                {"name":"broken","from":"A","to":"${['F',42]}"}
            ]);
        }
        let output = run(model, generation, &[])?;
        assert!(!output.success);
        for fragment in [
            phase,
            "successor 0",
            "splice element 1",
            "Expected a string or object",
            "42",
        ] {
            assert!(
                output.stderr.contains(fragment),
                "{fragment}: {}",
                output.stderr
            );
        }
        if phase != "axiom" {
            assert!(output.stderr.contains("rule broken"), "{}", output.stderr);
        }
        assert!(!output.stderr.contains("panicked"));
    }
    let output = run(
        json!({
            "axiom":"A", "symbols":["B{x:missing}"],
            "rules":[{"name":"buffered","from":"A","to":"${[{'symbol':'B','args':{'x':1}},'B']}"}]
        }),
        1,
        &[],
    )?;
    assert!(!output.success);
    for fragment in [
        "growth rule buffered",
        "generation 1",
        "token 0 (A)",
        "splice element 1",
        "default argument B.x",
        "Unknown name `missing`",
    ] {
        assert!(
            output.stderr.contains(fragment),
            "{fragment}: {}",
            output.stderr
        );
    }
    Ok(())
}

#[test]
fn evaluation_and_primitive_failures_keep_the_splice_origin() -> Result {
    for (to, fragments) in [
        (
            "${missing}",
            vec!["successor 0", "splice ${missing}", "Unknown name `missing`"],
        ),
        (
            "${{'symbol':'F','args':{'color':-1}}}",
            vec![
                "successor 0",
                "splice element 0",
                "instruction F",
                "Invalid argument for color",
            ],
        ),
    ] {
        let output = run(
            json!({"axiom":"A","drawing":[{"name":"broken","from":"A","to":to}]}),
            0,
            &[],
        )?;
        assert!(!output.success);
        assert!(
            output.stderr.contains("drawing rule broken"),
            "{}",
            output.stderr
        );
        for fragment in fragments {
            assert!(
                output.stderr.contains(fragment),
                "{fragment}: {}",
                output.stderr
            );
        }
    }
    Ok(())
}

#[test]
fn parsing_errors_identify_fields_and_accept_trailing_whitespace() -> Result {
    for field in ["global", "local", "cond", "to", "from", "axiom"] {
        let mut model = json!({"axiom":"A"});
        let fragments: Vec<&str> = match field {
            "global" => {
                model["globals"] = json!({"length":"1 garbage"});
                vec!["global length", "Trailing input"]
            }
            "axiom" => {
                model["axiom"] = json!("${'F'");
                vec!["Parsing axiom", "Unclosed argument block", "${'F'"]
            }
            other => {
                let mut rule = json!({"name":"broken","from":"A","to":"F"});
                let fragment = match other {
                    "local" => {
                        rule["locals"] = json!({"length":"1 garbage"});
                        "local length"
                    }
                    "cond" => {
                        rule["cond"] = json!("true garbage");
                        "field cond"
                    }
                    "to" => {
                        rule["to"] = json!("F{length:1+}");
                        "argument length"
                    }
                    _ => {
                        rule["from"] = json!("A{x:1+}");
                        "field from"
                    }
                };
                model["drawing"] = json!([rule]);
                vec!["Parsing drawing rule broken", fragment]
            }
        };
        let output = run(model, 0, &[])?;
        assert!(!output.success);
        for fragment in fragments {
            assert!(
                output.stderr.contains(fragment),
                "{fragment}: {}",
                output.stderr
            );
        }
    }
    let valid = run(
        json!({
            "globals":{"g":"1\u{a0}"}, "axiom":"${'A'\u{a0}}",
            "drawing":[{"from":"A","locals":{"x":"g+1 "},"cond":"true\n","to":"F{length:x}"}]
        }),
        0,
        &[],
    )?;
    assert!(valid.success, "{}", valid.stderr);
    Ok(())
}
