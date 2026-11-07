//! Deserialize JSON5 source and compile it into an executable model.
//! JSON-specific schema and value coercion stay here; the model only sees parsed
//! rules, token templates, and the initialized formula engine/runtime pair.

use crate::{
    context::{Pattern, predecessor},
    err::{Error, Result, ResultContext, SyntaxError, ensure},
    model::{Model, Rule, Symbol, argument_value},
    runtime::Runtime,
    syntax::*,
};
use formulas::{Env, FormulaEngine};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Source {
    name: String,
    axiom: String,
    globals: IndexMap<String, Value>,
    bg: Option<Value>,
    width: Option<Value>,
    height: Option<Value>,
    seed: Option<Value>,
    recommended_iterations: Option<i64>,
    symbols: Vec<String>,
    drawing: Vec<RuleSource>,
    rules: Vec<RuleSource>,
    reclim: Option<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSource {
    #[serde(default)]
    locals: IndexMap<String, Value>,
    from: String,
    #[serde(default)]
    to: String,
    name: Option<String>,
    weight: Option<f64>,
    specificity: Option<i64>,
    cond: Option<Value>,
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

/// Builds a model from JSON5 text, including source defaults and CLI overrides.
/// Each load creates its own runtime and engine so models never share live state.
pub struct Json5Loader;

impl Json5Loader {
    /// Parses definitions and applies CLI overrides before evaluating canvas settings.
    pub fn load(
        text: &str,
        override_axiom: Option<&str>,
        globals: &str,
        user_seed: Option<&str>,
        freclim: Option<usize>,
    ) -> Result<Model> {
        let s: Source = ::json5::from_str(text)?;
        let seed_arg = s.seed.map(formula);

        let runtime = Runtime::default();
        let mut env = runtime.environment();
        let mut engine = FormulaEngine::new(user_seed.or(seed_arg.as_deref()).map(seed))
            .with_globals(env.clone());
        if let Some(freclim) = freclim {
            engine = engine.with_max_rec(freclim);
        }

        let mut defs: Args = IndexMap::new();
        for (name, value) in s.globals {
            let expr =
                expression(&formula(value)).with_context(|| format!("Parsing global {name}"))?;
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
        let mut setting = |field: &'static str, value: Option<Value>, default| -> Result<u32> {
            let Some(value) = value else {
                return Ok(default);
            };
            let value = engine.evaluate(&formula(value), &Env::new())?.to_i64()?;
            u32::try_from(value).map_err(|source| Error::InvalidUnsignedSetting {
                field,
                value,
                source,
            })
        };
        let bg = setting("bg", s.bg, 0xffffff)?;
        let width = setting("width", s.width, 1000)?;
        let height = setting("height", s.height, 800)?;
        let rec_lim = setting("reclim", s.reclim, 128)? as usize;
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
            name: s.name,
            iterations: s.recommended_iterations.unwrap_or(5),
            bg,
            axiom: axiom.into(),
            symbols: Arc::new(symbols),
            rules: parse_rules(s.rules, "growth")?.into(),
            drawing: parse_rules(s.drawing, "drawing")?,
            width,
            height,
            rec_lim,
            engine,
            runtime,
        })
    }
}

/// Attach field and rule identities at load time, before any generation runs.
fn parse_rules(source: Vec<RuleSource>, phase: &str) -> Result<Vec<Rule>> {
    source
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let name = r.name.unwrap_or_else(|| format!("rule[{i}]"));
            (|| -> Result<Rule> {
                let (l, core, h) = predecessor(&r.from).context("field from")?;
                let left = l.map(Pattern::parse).transpose().context("left context")?;
                let right = h.map(Pattern::parse).transpose().context("right context")?;
                let specificity = r.specificity.unwrap_or(
                    core.args.len() as i64 + left.is_some() as i64 + right.is_some() as i64,
                );
                let weight = r.weight.unwrap_or(1.);
                ensure!(
                    weight >= 0. && weight.is_finite(),
                    Error::InvalidRuleWeight { weight }
                );
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
