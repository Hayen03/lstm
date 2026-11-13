//! Common source representation and construction, independent of file delimiters.
//! Native and JSON5 parsers fill this representation without evaluating globals;
//! overrides and eager evaluation are applied exactly once, here.

use crate::{
    context::{Pattern, predecessor},
    err::{Error, Result, ResultContext, SyntaxError, ensure},
    model::{Model, Rule, Symbol, argument_value},
    runtime::Runtime,
    syntax::*,
};
use formulas::{Env, EvalValue as V, Expr, FormulaEngine};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Definition {
    pub name: Option<Value>,
    pub description: Option<Value>,
    pub meta: IndexMap<String, Value>,
    #[serde(skip)]
    pub native: bool,
    pub axiom: String,
    pub globals: IndexMap<String, Value>,
    pub bg: Option<Value>,
    pub width: Option<Value>,
    pub height: Option<Value>,
    pub seed: Option<Value>,
    pub recommended_iterations: Option<Value>,
    pub symbols: Vec<String>,
    pub drawing: Vec<RuleDefinition>,
    pub rules: Vec<RuleDefinition>,
    pub reclim: Option<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuleDefinition {
    #[serde(default)]
    pub locals: IndexMap<String, Value>,
    pub from: String,
    #[serde(default)]
    pub to: String,
    pub name: Option<String>,
    pub weight: Option<Value>,
    pub specificity: Option<Value>,
    pub cond: Option<Value>,
}
// JSON strings contain formula source; scalar JSON values become literal formulas.
fn formula(v: Value) -> String {
    match v {
        Value::String(s) => s,
        _ => v.to_string(),
    }
}
fn seed(s: &str) -> u64 {
    if let Ok(n) = s.parse::<u64>() {
        return n;
    }
    if let Ok(n) = s.parse::<i64>() {
        return n as u64;
    }
    // FNV-1a gives text seeds a repeatable mapping without a randomized Rust hasher.
    s.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

fn evaluate(value: Value, engine: &mut FormulaEngine, field: &str) -> Result<V> {
    engine
        .evaluate(&formula(value), &Env::new())
        .map_err(Error::from)
        .with_context(|| format!("field {field}"))
}

fn text_setting(
    value: Option<Value>,
    native: bool,
    engine: &mut FormulaEngine,
    field: &str,
) -> Result<String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let value = if native {
        evaluate(value, engine, field)?
    } else if let Value::String(text) = value {
        return Ok(text);
    } else {
        evaluate(value, engine, field)?
    };
    match value {
        V::String(text) => Ok(text.to_string()),
        got => Err(Error::InvalidArgument {
            field: field.into(),
            expected: "string".into(),
            got,
        }),
    }
}

fn integer_setting(
    value: Value,
    native: bool,
    engine: &mut FormulaEngine,
    field: &str,
) -> Result<i64> {
    if !native {
        return value.as_i64().ok_or_else(|| Error::InvalidArgument {
            field: field.into(),
            expected: "integer literal".into(),
            got: V::String(value.to_string().into()),
        });
    }
    match evaluate(value, engine, field)? {
        V::Int(n) => Ok(n),
        got => Err(Error::InvalidArgument {
            field: field.into(),
            expected: "integer".into(),
            got,
        }),
    }
}

pub(super) fn build(
    s: Definition,
    override_axiom: Option<&str>,
    globals: &str,
    user_seed: Option<&str>,
    freclim: Option<usize>,
    target: Option<i64>,
) -> Result<Model> {
    // A CLI seed bypasses the model seed. Native seed formulas run before any
    // globals, using only ordinary formula builtins and their own temporary RNG.
    let seed_arg = if let Some(value) = user_seed {
        Some(seed(value))
    } else if let Some(value) = s.seed {
        if s.native {
            let mut bootstrap = FormulaEngine::new(None);
            if let Some(limit) = freclim {
                bootstrap = bootstrap.with_max_rec(limit);
            }
            let value = evaluate(value, &mut bootstrap, "seed")?;
            Some(match value {
                V::Int(n) => seed(&n.to_string()),
                V::String(text) => seed(&text),
                got => {
                    return Err(Error::InvalidArgument {
                        field: "seed".into(),
                        expected: "integer or string".into(),
                        got,
                    });
                }
            })
        } else {
            Some(seed(&formula(value)))
        }
    } else {
        None
    };

    let runtime = Runtime::default();
    let iterations_expr = s.recommended_iterations.unwrap_or(Value::from(5));
    // Literal targets (the usual case) and CLI overrides are already available
    // to globals. A computed recommendation is resolved after globals instead.
    if let Some(n) = target {
        runtime.target(n);
    } else if let Some(n) = iterations_expr.as_i64() {
        runtime.target(n);
    } else if let Expr::Literal(V::Int(n)) =
        expression(&formula(iterations_expr.clone())).context("Parsing recommended_iterations")?
    {
        runtime.target(n);
    }
    let mut env = runtime.environment();
    let mut engine = FormulaEngine::new(seed_arg).with_globals(env.clone());
    if let Some(freclim) = freclim {
        engine = engine.with_max_rec(freclim);
    }

    let mut defs: Args = IndexMap::new();
    for (name, value) in s.globals {
        let expr = expression(&formula(value)).with_context(|| format!("Parsing global {name}"))?;
        defs.insert(name.into(), expr.into());
    }
    let g = globals.trim();
    defs.extend(
        args(
            g.strip_prefix('{')
                .and_then(|v| v.strip_suffix('}'))
                .unwrap_or(g),
        )
        .context("Parsing CLI globals")?,
    );
    // Evaluate each definition once, in declaration order. Function bodies
    // run only when called and resolve globals through the engine normally.
    for (name, source) in defs {
        let value = argument_value(&source, &mut engine, &Env::new())
            .with_context(|| format!("global {name}"))?;
        env.insert(name, value);
        engine = engine.with_globals(env.clone());
    }
    let iterations = integer_setting(
        iterations_expr,
        s.native,
        &mut engine,
        "recommended_iterations",
    )?;
    ensure!(
        !s.native || iterations >= 0,
        Error::InvalidArgument {
            field: "recommended_iterations".into(),
            expected: "nonnegative integer".into(),
            got: V::Int(iterations),
        }
    );
    runtime.target(target.unwrap_or(iterations));
    let mut setting = |field: &'static str, value: Option<Value>, default| -> Result<u32> {
        let Some(value) = value else {
            return Ok(default);
        };
        let evaluated = evaluate(value, &mut engine, field)?;
        if s.native {
            let valid = match &evaluated {
                formulas::EvalValue::Int(n) => match field {
                    "width" | "height" => *n > 0 && *n <= u32::MAX as i64,
                    "bg" => (0..=0xFFFFFF).contains(n),
                    _ => *n >= 0 && *n <= u32::MAX as i64,
                },
                _ => false,
            };
            ensure!(
                valid,
                Error::InvalidArgument {
                    field: field.into(),
                    expected: match field {
                        "width" | "height" => "positive 32-bit integer",
                        "bg" => "RGB integer in 0..=0xFFFFFF",
                        _ => "nonnegative 32-bit integer",
                    }
                    .into(),
                    got: evaluated.clone(),
                }
            );
        }
        let value = evaluated.to_i64()?;
        u32::try_from(value).map_err(|source| Error::InvalidUnsignedSetting {
            field,
            value,
            source,
        })
    };
    let (bg, background) = if s.native {
        (
            0xffffff,
            s.bg.map(|v| expression(&formula(v)))
                .transpose()
                .context("Parsing field bg")?,
        )
    } else {
        (setting("bg", s.bg, 0xffffff)?, None)
    };
    let width = setting("width", s.width, 1000)?;
    let height = setting("height", s.height, 800)?;
    let rec_lim = setting("reclim", s.reclim, 128)? as usize;
    let name = text_setting(s.name, s.native, &mut engine, "name")?;
    let description = text_setting(s.description, s.native, &mut engine, "description")?;
    let meta = s
        .meta
        .into_iter()
        .map(|(key, value)| {
            let text = text_setting(Some(value), s.native, &mut engine, &format!("meta.{key}"))?;
            Ok((key, text))
        })
        .collect::<Result<_>>()?;
    let mut symbols = HashMap::new();
    for declaration in s.symbols {
        let core = single(&declaration)
            .with_context(|| format!("Parsing symbol declaration {declaration:?}"))?;
        ensure!(
            !DRAW.contains(core.ch.as_ref()) && !"[]".contains(core.ch.as_ref()),
            SyntaxError::ReservedSymbol { symbol: core.ch }
        );
        symbols.insert(
            core.ch,
            Symbol {
                defaults: core.args,
            },
        );
    }
    let axiom = word(override_axiom.unwrap_or(&s.axiom)).context("Parsing axiom")?;
    Ok(Model {
        name,
        description,
        meta,
        iterations,
        bg,
        background,
        axiom: axiom.into(),
        symbols: Arc::new(symbols),
        rules: parse_rules(s.rules, "growth", s.native, &mut engine)?.into(),
        drawing: parse_rules(s.drawing, "drawing", s.native, &mut engine)?,
        width,
        height,
        rec_lim,
        engine,
        runtime,
    })
}

/// Attach field and rule identities at load time, before any generation runs.
fn parse_rules(
    source: Vec<RuleDefinition>,
    phase: &str,
    native: bool,
    engine: &mut FormulaEngine,
) -> Result<Vec<Rule>> {
    source
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let name = r.name.unwrap_or_else(|| format!("rule[{i}]"));
            (|| -> Result<Rule> {
                let (l, core, h) = predecessor(&r.from).context("field from")?;
                let left = l.map(Pattern::parse).transpose().context("left context")?;
                let right = h.map(Pattern::parse).transpose().context("right context")?;
                let specificity = match r.specificity {
                    Some(value) => integer_setting(value, native, engine, "specificity")?,
                    None => core.args.len() as i64 + left.is_some() as i64 + right.is_some() as i64,
                };
                let weight_value = r.weight.unwrap_or(Value::from(1));
                // Legacy JSON5 weights are still static numeric literals.
                let weight = if native {
                    expression(&formula(weight_value)).context("field weight")?
                } else {
                    let number = weight_value
                        .as_f64()
                        .ok_or_else(|| Error::InvalidArgument {
                            field: "weight".into(),
                            expected: "numeric literal".into(),
                            got: V::String(weight_value.to_string().into()),
                        })?;
                    crate::model::rule_weight(V::Float(number))?;
                    Expr::Literal(V::Float(number))
                };
                let mut locals = Args::new();
                for (key, value) in r.locals {
                    let expr =
                        expression(&formula(value)).with_context(|| format!("local {key}"))?;
                    locals.insert(key.into(), expr.into());
                }
                let cond = r
                    .cond
                    .map(|value| expression(&formula(value)))
                    .transpose()
                    .context("field cond")?;
                let out = word(&r.to).context("field to")?;
                Ok(Rule {
                    locals,
                    core,
                    left,
                    right,
                    cond,
                    out,
                    weight,
                    specificity,
                    name: name.clone(),
                })
            })()
            .with_context(|| format!("Parsing {phase} rule {name} (index {i})"))
        })
        .collect()
}
