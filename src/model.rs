//! Loads JSON5 models and coordinates rewriting, formula evaluation, and drawing.
//! Rule matching reads the previous word, but successors are evaluated and drawn
//! in order so later expressions can observe the turtle's updated position.

use crate::err::{Error, Result, ResultContext, SyntaxError, ensure};
use crate::{
    context::{Pattern, predecessor},
    dessin::Dessin,
    runtime::{self, Runtime},
    syntax::*,
    trace::Trace,
};
use formulas::{Env, EvalValue as V, FormulaEngine, Function};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

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
struct Symbol {
    defaults: Args,
}
struct Rule {
    locals: Args,
    core: Template,
    left: Option<Pattern>,
    right: Option<Pattern>,
    cond: Option<String>,
    out: Vec<Template>,
    weight: f64,
    specificity: i64,
    name: String,
}
/// Parsed model definitions and the formula engine shared by all generations.
pub struct Model {
    pub name: String,
    pub iterations: i64,
    pub bg: u32,
    pub width: u32,
    pub height: u32,
    axiom: Arc<[Template]>,
    symbols: Arc<HashMap<char, Symbol>>,
    rules: Arc<[Rule]>,
    drawing: Vec<Rule>,
    engine: FormulaEngine,
    runtime: Runtime,
}
impl Model {
    /// Parses definitions and applies CLI overrides before evaluating canvas settings.
    pub fn load(
        text: &str,
        override_axiom: Option<&str>,
        globals: &str,
        user_seed: Option<&str>,
    ) -> Result<Self> {
        let s: Source = json5::from_str(text)?;
        let seed_arg = s.seed.map(formula);

        let mut defs: Args = s
            .globals
            .into_iter()
            .map(|(k, v)| (k.into(), formula(v)))
            .collect();
        let g = globals.trim();
        defs.extend(args(
            g.strip_prefix('{')
                .and_then(|v| v.strip_suffix('}'))
                .unwrap_or(g),
        )?);
        let runtime = Runtime::default();
        let mut env = runtime.environment();
        let mut engine = FormulaEngine::new(user_seed.or(seed_arg.as_deref()).map(seed))
            .with_globals(env.clone());
        // Evaluate each definition once, in declaration order. Function bodies
        // run only when called and resolve globals through the engine normally.
        for (name, source) in defs {
            let value = engine
                .evaluate(&source, &Env::new())
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
        let mut symbols = HashMap::new();
        for declaration in s.symbols {
            let core = single(&declaration)?;
            ensure!(
                !DRAW.contains(core.ch) && !"[]".contains(core.ch),
                SyntaxError::ReservedSymbol { symbol: core.ch }
            );
            symbols.insert(
                core.ch,
                Symbol {
                    defaults: core.args,
                },
            );
        }
        Ok(Self {
            name: s.name,
            iterations: s.recommended_iterations.unwrap_or(5),
            bg,
            axiom: word(override_axiom.unwrap_or(&s.axiom))?.into(),
            symbols: Arc::new(symbols),
            rules: parse_rules(s.rules)?.into(),
            drawing: parse_rules(s.drawing)?,
            width,
            height,
            engine,
            runtime,
        })
    }
    pub fn frame(
        &mut self,
        prev: Arc<[Token]>,
        n: i64,
        d: &mut Dessin,
        tr: &mut Trace,
        _animate: bool,
    ) -> Result<Arc<[Token]>> {
        d.reset();
        self.runtime.sync(d);
        self.runtime.generation(n, prev.clone());
        self.runtime.enable_query_trace(tr.enabled());
        tr.generation = n;
        let word = Arc::new(Mutex::new(FrameWord {
            runtime: self.runtime.clone(),
            prev,
            axiom: (n == 0).then(|| self.axiom.clone()),
            symbols: self.symbols.clone(),
            rules: self.rules.clone(),
            trace: tr.clone(),
            index: 0,
            pending: None,
            tokens: Vec::new(),
        }));
        let mut at = 0;
        loop {
            let token = word.lock().unwrap().get(at, &mut self.engine)?;
            let Some(token) = token else { break };
            tr.index = at;
            tr.symbol = token.ch;
            tr.rule = None;
            let context = drawing_context(self.runtime.clone(), word.clone(), at);
            self.runtime.with_context(Some(context), || {
                draw(
                    &self.runtime,
                    &self.symbols,
                    &self.drawing,
                    &mut self.engine,
                    &token,
                    d,
                    tr,
                    &mut Vec::new(),
                )
            })?;
            at += 1;
        }
        let result = std::mem::take(&mut word.lock().unwrap().tokens);
        Ok(result.into())
    }
}

fn parse_rules(source: Vec<RuleSource>) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    for (i, r) in source.into_iter().enumerate() {
        let (l, core, h) = predecessor(&r.from)?;
        let left = l.map(Pattern::parse).transpose()?;
        let right = h.map(Pattern::parse).transpose()?;
        let specificity = r
            .specificity
            .unwrap_or(core.args.len() as i64 + left.is_some() as i64 + right.is_some() as i64);
        let weight = r.weight.unwrap_or(1.);
        ensure!(
            weight >= 0. && weight.is_finite(),
            Error::InvalidRuleWeight { weight }
        );
        rules.push(Rule {
            locals: r
                .locals
                .into_iter()
                .map(|(k, v)| (k.into(), formula(v)))
                .collect(),
            core,
            left,
            right,
            weight,
            specificity,
            cond: r.cond.map(formula),
            name: r.name.unwrap_or_else(|| format!("rule[{i}]")),
            out: word(&r.to)?,
        })
    }
    Ok(rules)
}

fn trace_queries(runtime: &Runtime, tr: &mut Trace, phase: &str) -> Result<()> {
    if tr.enabled() {
        for (name, value) in runtime.queries() {
            tr.event(
                phase,
                "runtime_query",
                || json!({"query":name,"value":display(&value)}),
            )?;
        }
    }
    Ok(())
}
fn display(v: &V) -> Value {
    match v {
        V::Null => Value::Null,
        V::Bool(x) => json!(x),
        V::Int(x) => json!(x),
        V::Float(x) => json!(x),
        V::String(x) => json!(x.as_ref()),
        V::Array(xs) => Value::Array(xs.iter().map(display).collect()),
        V::Object(xs) => Value::Object(
            xs.iter()
                .map(|(k, v)| (k.to_string(), display(v)))
                .collect(),
        ),
        V::Func(_) => json!("<function>"),
    }
}
fn boolean(v: V) -> Result<bool> {
    if let V::Bool(b) = v {
        Ok(b)
    } else {
        Err(Error::ExpectedBoolean {
            actual: v.diagnostic().to_string(),
        })
    }
}
/// Checks required arguments: `_` accepts any present value, functions act as
/// predicates, and all other expressions must equal the token's argument value.
fn constraints(
    runtime: &Runtime,
    engine: &mut FormulaEngine,
    pattern: &Template,
    t: &Token,
    tr: &mut Trace,
    phase: &str,
) -> Result<bool> {
    let env = Env::from_index_map(&t.args);
    for (k, expr) in &pattern.args {
        let Some(actual) = t.args.get(k) else {
            tr.event(
                phase,
                "constraint_checked",
                || json!({"argument":k.as_ref(),"matched":false,"reason":"missing_argument"}),
            )?;
            return Ok(false);
        };
        if expr.trim() == "_" {
            continue;
        }
        let expected = engine.evaluate(expr, &env)?;
        trace_queries(runtime, tr, phase)?;
        tr.event(
            phase,
            "constraint_evaluated",
            || json!({"argument":k.as_ref(),"formula":expr,"value":display(&expected)}),
        )?;
        if let V::Func(f) = &expected {
            if let Function::User(c) = f.as_ref() {
                ensure!(
                    c.definition
                        .parameters
                        .as_ref()
                        .is_some_and(|p| p.len() == 1),
                    Error::InvalidPredicate
                )
            }
            let matched = boolean(expected.call(std::slice::from_ref(actual), engine, &env)?)?;
            trace_queries(runtime, tr, phase)?;
            tr.event(
                phase,
                "predicate_evaluated",
                || json!({"argument":k.as_ref(),"formula":expr,"value":matched}),
            )?;
            if !matched {
                return Ok(false);
            }
        } else {
            let matched = actual == &expected;
            tr.event(phase,"constraint_checked",||json!({"argument":k.as_ref(),"actual":display(actual),"expected":display(&expected),"matched":matched}))?;
            if !matched {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
/// Resolves a template using symbol defaults, with explicit arguments overriding them.
fn actualize(
    runtime: &Runtime,
    symbols: &HashMap<char, Symbol>,
    engine: &mut FormulaEngine,
    t: &Template,
    env: &Env,
    tr: &mut Trace,
    phase: &str,
) -> Result<Token> {
    let mut values = Values::new();
    let defaults = symbols.get(&t.ch).map(|s| &s.defaults);
    // Defaults determine the first keys' order, followed by extra explicit keys.
    // Every expression sees the incoming environment, not earlier output arguments.
    let keys = defaults.into_iter().flat_map(|d| d.keys()).chain(
        t.args
            .keys()
            .filter(|k| !defaults.is_some_and(|d| d.contains_key(*k))),
    );
    for k in keys {
        let expr = t
            .args
            .get(k)
            .or_else(|| defaults.and_then(|d| d.get(k)))
            .unwrap();
        let v = engine
            .evaluate(expr, env)
            .with_context(|| format!("argument {}.{k}, generation {}", t.ch, tr.generation))?;
        trace_queries(runtime, tr, phase)?;
        tr.event(phase,"argument_evaluated",||json!({"argument":k.as_ref(),"formula":expr,"value":display(&v),"target_symbol":t.ch}))?;
        values.insert(k.clone(), v);
    }
    Ok(Token {
        ch: t.ch,
        args: values,
    })
}
fn select(
    runtime: &Runtime,
    rules: &[Rule],
    token: &Token,
    engine: &mut FormulaEngine,
    tr: &mut Trace,
    phase: &str,
) -> Result<Option<usize>> {
    let env = Env::from_index_map(&token.args);
    let mut candidates = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        if rule.core.ch != token.ch {
            continue;
        }
        tr.rule = Some((index, rule.name.clone()));
        let matches = (|| -> Result<bool> {
            if !constraints(runtime, engine, &rule.core, token, tr, phase)? {
                return Ok(false);
            }
            for (pattern, left) in [(&rule.left, true), (&rule.right, false)] {
                if let Some(pattern) = pattern {
                    let (tree, seq) = runtime.neighborhood(left, engine)?;
                    let ok = pattern.matches(&tree, &seq, left, |a, t| {
                        constraints(runtime, engine, a, t, tr, phase)
                    })?;
                    tr.event(
                        phase,
                        "context_checked",
                        || json!({"side":if left{"left"}else{"right"},"matched":ok}),
                    )?;
                    if !ok {
                        return Ok(false);
                    }
                }
            }
            if let Some(cond) = &rule.cond {
                let ok = boolean(engine.evaluate(cond, &env)?)?;
                trace_queries(runtime, tr, phase)?;
                tr.event(
                    phase,
                    "condition_evaluated",
                    || json!({"formula":cond,"value":ok}),
                )?;
                return Ok(ok);
            }
            Ok(true)
        })()
        .with_context(|| {
            format!(
                "{phase} rule {}, generation {}, token {}",
                rule.name, tr.generation, tr.index
            )
        })?;
        tr.event(
            phase,
            "rule_checked",
            || json!({"matched":matches,"specificity":rule.specificity,"weight":rule.weight}),
        )?;
        if matches && rule.weight > 0. {
            candidates.push(index)
        }
    }
    let Some(best) = candidates.iter().map(|&i| rules[i].specificity).max() else {
        tr.rule = None;
        return Ok(None);
    };
    candidates.retain(|&i| rules[i].specificity == best);
    let total = candidates.iter().map(|&i| rules[i].weight).sum::<f64>();
    if !total.is_finite() {
        return Err(Error::InvalidTotalWeight { total });
    }
    let value = if phase == "drawing" && candidates.len() == 1 {
        0.
    } else {
        engine.random_percent() / 100. * total
    };
    let mut sum = 0.;
    let i = candidates
        .iter()
        .copied()
        .find(|&i| {
            sum += rules[i].weight;
            sum > value
        })
        .unwrap_or(*candidates.last().unwrap());
    tr.rule = Some((i, rules[i].name.clone()));
    tr.event(
        phase,
        "rule_selected",
        || json!({"specificity":best,"draw":value,"total_weight":total}),
    )?;
    Ok(Some(i))
}

fn bindings(
    runtime: &Runtime,
    rule: &Rule,
    token: &Token,
    engine: &mut FormulaEngine,
    tr: &mut Trace,
    phase: &str,
) -> Result<Env<'static>> {
    let mut env = Env::from_index_map(&token.args);
    for (name, expr) in &rule.locals {
        let value = engine
            .evaluate(expr, &env)
            .with_context(|| format!("{phase} rule {} local {name}", rule.name))?;
        trace_queries(runtime, tr, phase)?;
        tr.event(
            phase,
            "local_evaluated",
            || json!({"local":name.as_ref(),"formula":expr,"value":display(&value)}),
        )?;
        env.insert(name.clone(), value);
    }
    Ok(env)
}

struct Pending {
    rule: usize,
    next: usize,
    env: Env<'static>,
}

struct FrameWord {
    runtime: Runtime,
    prev: Arc<[Token]>,
    axiom: Option<Arc<[Template]>>,
    symbols: Arc<HashMap<char, Symbol>>,
    rules: Arc<[Rule]>,
    trace: Trace,
    index: usize,
    pending: Option<Pending>,
    tokens: Vec<Token>,
}
impl FrameWord {
    fn get(&mut self, at: usize, engine: &mut FormulaEngine) -> Result<Option<Token>> {
        while self.tokens.len() <= at {
            let Some(t) = self.produce(engine)? else {
                return Ok(None);
            };
            self.tokens.push(t);
        }
        Ok(Some(self.tokens[at].clone()))
    }

    fn produce(&mut self, engine: &mut FormulaEngine) -> Result<Option<Token>> {
        if let Some(axiom) = &self.axiom {
            let Some(t) = axiom.get(self.index) else {
                return Ok(None);
            };
            self.trace.index = self.index;
            self.trace.symbol = t.ch;
            self.index += 1;
            return actualize(
                &self.runtime,
                &self.symbols,
                engine,
                t,
                &Env::new(),
                &mut self.trace,
                "axiom",
            )
            .map(Some);
        }
        loop {
            if let Some(p) = &mut self.pending {
                if let Some(template) = self.rules[p.rule].out.get(p.next) {
                    p.next += 1;
                    let t = actualize(
                        &self.runtime,
                        &self.symbols,
                        engine,
                        template,
                        &p.env,
                        &mut self.trace,
                        "growth",
                    )?;
                    self.trace.event(
                        "growth",
                        "token_emitted",
                        || json!({"output":display(&t.value())}),
                    )?;
                    return Ok(Some(t));
                }
                self.pending = None;
            }
            let Some(token) = self.prev.get(self.index) else {
                return Ok(None);
            };
            self.runtime.token(self.index);
            self.trace.index = self.index;
            self.trace.symbol = token.ch;
            self.trace.rule = None;
            self.index += 1;
            self.trace.event(
                "growth",
                "token_started",
                || json!({"input":display(&token.value())}),
            )?;
            if let Some(i) = select(
                &self.runtime,
                &self.rules,
                token,
                engine,
                &mut self.trace,
                "growth",
            )? {
                let env = bindings(
                    &self.runtime,
                    &self.rules[i],
                    token,
                    engine,
                    &mut self.trace,
                    "growth",
                )?;
                self.pending = Some(Pending {
                    rule: i,
                    next: 0,
                    env,
                });
            } else {
                self.trace
                    .event("growth", "token_unchanged", || json!({}))?;
                return Ok(Some(token.clone()));
            }
        }
    }
}

/// All recursive drawing children share this anchor. Only right-context reads
/// advance the growth cursor, and then only to the end of the current branch.
fn drawing_context(runtime: Runtime, word: Arc<Mutex<FrameWord>>, at: usize) -> runtime::Provider {
    let cache: Mutex<[Option<runtime::Neighborhood>; 2]> = Mutex::new([None, None]);
    Arc::new(move |left, engine| {
        let side = usize::from(left);
        if let Some(pair) = &cache.lock().unwrap()[side] {
            return Ok(pair.clone());
        }
        let mut word = word.lock().unwrap();
        let end = if left {
            at + 1
        } else {
            runtime.with_context(None, || -> Result<usize> {
                let mut depth = 0;
                let mut i = at + 1;
                while let Some(token) = word.get(i, engine)? {
                    i += 1;
                    match token.ch {
                        '[' => depth += 1,
                        ']' if depth == 0 => break,
                        ']' => depth -= 1,
                        _ => (),
                    }
                }
                ensure!(depth == 0, Error::UnclosedBranch);
                Ok(i)
            })?
        };
        let seq: Arc<[Token]> = word.tokens[..end].into();
        let tree = Arc::new(crate::context::Tree::build(&seq, at, left));
        cache.lock().unwrap()[side] = Some((tree.clone(), seq.clone()));
        Ok((tree, seq))
    })
}

fn draw(
    runtime: &Runtime,
    symbols: &HashMap<char, Symbol>,
    rules: &[Rule],
    engine: &mut FormulaEngine,
    token: &Token,
    d: &mut Dessin,
    tr: &mut Trace,
    path: &mut Vec<char>,
) -> Result<()> {
    let Some(i) = select(runtime, rules, token, engine, tr, "drawing")? else {
        if DRAW.contains(token.ch) || "[]".contains(token.ch) {
            tr.event("drawing","instruction",||json!({"instruction":token.ch,"arguments":display(&V::Object(token.args.clone().into()))}))?;
            d.apply(token)?;
            runtime.sync(d);
        }
        return Ok(());
    };
    path.push(token.ch);
    ensure!(
        path.len() <= 128,
        Error::DrawingRecursion {
            limit: 128,
            path: path
                .iter()
                .map(char::to_string)
                .collect::<Vec<_>>()
                .join(" -> "),
        }
    );
    let rule = &rules[i];
    let env = bindings(runtime, rule, token, engine, tr, "drawing")?;
    tr.event(
        "drawing",
        "symbol_expanded",
        || json!({"drawing_path":path.iter().collect::<String>()}),
    )?;
    for child in &rule.out {
        tr.rule = Some((i, rule.name.clone()));
        let t = actualize(runtime, symbols, engine, child, &env, tr, "drawing")?;
        draw(runtime, symbols, rules, engine, &t, d, tr, path)?;
    }
    path.pop();
    Ok(())
}

#[cfg(test)]
mod tests {
    // Exercise full generation transitions, including each model’s live state.
    use super::*;
    fn generations(src: &str, n: i64, animate: bool) -> Result<Vec<Arc<[Token]>>> {
        let mut d = Dessin::new(800, 600);
        let mut m = Model::load(src, None, "", Some("1"))?;
        let mut trace = Trace::new(Default::default())?;
        let mut frames = Vec::new();
        let mut seq: Arc<[Token]> = Vec::new().into();
        for i in 0..=n {
            seq = m.frame(seq, i, &mut d, &mut trace, animate)?;
            frames.push(seq.clone());
        }
        Ok(frames)
    }
    fn final_pose(src: &str, n: i64) -> Result<f64> {
        let mut model = Model::load(src, None, "", Some("fixture"))?;
        let mut turtle = Dessin::new(800, 600);
        let mut trace = Trace::new(Default::default())?;
        let mut seq = Arc::from([]);
        for g in 0..=n {
            seq = model.frame(seq, g, &mut turtle, &mut trace, true)?;
        }
        Ok(turtle.pose().0.0)
    }

    #[test]
    fn globals_are_eager_and_lexically_scoped() -> Result<()> {
        let src = r#"{axiom:"A{x:3}",
          globals:{x:"99",b:"4",a:"b+1",
                   read_x:"fn x -> x",recurse:"fn n -> 1 if n==0 else n*recurse(n-1)"},
          drawing:[{from:"A",locals:{shadow:"let x=6 for read_x(x)",
                                     count:"[x for x in [1,2,3]].sum()"},
                    to:"P{x:x+shadow+count+a+recurse(3),y:0}"}]}"#;
        assert_eq!(final_pose(src, 0)?, 26.);
        for src in [
            r#"{globals:{bad:"error('unused global')"}}"#,
            r#"{globals:{bad:"later+1",later:"4"}}"#,
            r#"{globals:{bad:"other",other:"bad"}}"#,
        ] {
            let error = Model::load(src, None, "", None)
                .err()
                .expect("invalid global");
            assert!(format!("{error:#}").contains("global bad"));
        }
        Ok(())
    }

    #[test]
    fn model_validation_errors_are_typed() {
        let load_error = |src| {
            Model::load(src, None, "", None)
                .err()
                .expect("invalid model")
        };
        assert!(matches!(load_error("{"), Error::Json5(_)));
        assert!(matches!(
            load_error("{axiom:'+'}"),
            Error::Syntax(SyntaxError::ReservedSymbol { symbol: '+' })
        ));
        assert!(matches!(
            load_error("{rules:[{from:'A',weight:-1}]}"),
            Error::InvalidRuleWeight { weight: -1.0 }
        ));
        assert!(matches!(
            load_error("{width:4294967297}"),
            Error::InvalidUnsignedSetting {
                field: "width",
                value: 4294967297,
                ..
            }
        ));
        let error = final_pose("{axiom:'A',drawing:[{from:'A',cond:'1'}]}", 0).unwrap_err();
        assert!(matches!(
            error.without_context(),
            Error::ExpectedBoolean { .. }
        ));
    }

    #[test]
    fn declarations_and_rules_reject_unsupported_fields() -> Result<()> {
        for src in [
            "{symbols:[{symbol:'A'}]}",
            "{symbols:[{symbol:'A{x:2}',drawing:'F',locals:{step:2}}]}",
        ] {
            assert!(matches!(
                Model::load(src, None, "", None),
                Err(Error::Json5(_))
            ));
        }
        for family in ["rules", "drawing"] {
            for field in ["left-ctx", "right-ctx"] {
                for value in ["'B'", "null"] {
                    let src = format!("{{{family}:[{{from:'A','{field}':{value}}}]}}");
                    let error = Model::load(&src, None, "", None)
                        .err()
                        .expect("invalid rule");
                    assert!(matches!(error, Error::Json5(_)));
                    assert!(format!("{error:#}").contains(field));
                }
            }
        }
        let frames = generations(
            "{axiom:'BAQ',symbols:['A{x:2}'],rules:[{from:'B < A > Q',locals:{y:'x+1'},to:'Z{x:y}'}],drawing:[{from:'B < Z > Q',to:'F{length:x}'}]}",
            1,
            false,
        )?;
        assert_eq!(frames[1][1].ch, 'Z');
        assert_eq!(frames[1][1].args["x"], V::Int(3));
        Ok(())
    }

    #[test]
    fn dynamic_formulas_and_fmt_use_global_values() -> Result<()> {
        let src = r#"{axiom:"A",globals:{v:"2+3", f:"fn x -> x+v"},
          drawing:[{from:"A", locals:{calc:"eval('f(v)')",text:"'${v}=${v}'.fmt()",
                    stored:"let fmt=''.fmt for fmt('v=${v}')", other:"{'fmt':fn n -> n+1}.fmt(3)"},
                    to:"P{x:calc+other if text=='5=5' && stored=='v=5' else 0,y:0}"}]}"#;
        assert_eq!(final_pose(src, 0)?, 14.);
        Ok(())
    }

    #[test]
    fn globals_are_sampled_once_and_models_are_isolated() -> Result<()> {
        let src = r#"{globals:{roll:"random()",twice:"roll+roll"}}"#;
        let mut model = Model::load(src, None, "", Some("1"))?;
        let mut reference = FormulaEngine::new(Some(1));
        let env = Env::new();
        let first = reference.evaluate("random()", &env)?;
        let second = reference.evaluate("random()", &env)?;
        let _other = Model::load("{globals:{roll:42}}", None, "", None)?;
        for _ in 0..3 {
            assert_eq!(model.engine.evaluate("roll", &env)?, first);
            assert_eq!(
                model.engine.evaluate("twice == roll+roll", &env)?,
                V::Bool(true)
            );
        }
        assert_eq!(model.engine.evaluate("random()", &env)?, second);
        Ok(())
    }

    #[test]
    fn interleaved_models_keep_independent_runtime_state() -> Result<()> {
        let src = r#"{globals:{id:11,initial:"pos()[0]",position:"pos"},
          axiom:"A B{id:id}",
          rules:[{from:"A",to:"A{seen:marked()[0],neighbor:rctx(0)[0].args.id,g:gen()}"}],
          drawing:[{from:"A",locals:{record:"mark(id)"},to:"P{x:id,y:0}H{rot:id}"}]}"#;
        let mut first = Model::load(src, None, "", Some("1"))?;
        let mut d1 = Dessin::new(100, 200);
        let mut tr = Trace::new(Default::default())?;
        let word1 = first.frame(Arc::from([]), 0, &mut d1, &mut tr, true)?;
        // Loading another model must start from its own initial runtime state.
        let mut second = Model::load(src, None, "id:22", Some("1"))?;
        assert_eq!(second.engine.evaluate("initial", &Env::new())?, V::Int(0));
        let mut d2 = Dessin::new(300, 400);
        let word2 = second.frame(Arc::from([]), 0, &mut d2, &mut tr, true)?;
        for (model, position, width) in [(&mut first, 11, 100), (&mut second, 22, 300)] {
            assert_eq!(
                model.engine.evaluate("position()[0]", &Env::new())?,
                V::Int(position)
            );
            assert_eq!(
                model.engine.evaluate("heading()", &Env::new())?,
                V::Int(position)
            );
            assert_eq!(
                model.engine.evaluate("screen()[0]", &Env::new())?,
                V::Int(width)
            );
        }
        let word1 = first.frame(word1, 1, &mut d1, &mut tr, true)?;
        let word2 = second.frame(word2, 2, &mut d2, &mut tr, true)?;
        for (word, id, generation) in [(word1, 11, 1), (word2, 22, 2)] {
            assert_eq!(word[0].args["seen"], V::Int(id));
            assert_eq!(word[0].args["neighbor"], V::Int(id));
            assert_eq!(word[0].args["g"], V::Int(generation));
        }
        assert_eq!(first.engine.evaluate("gen()", &Env::new())?, V::Int(1));
        Ok(())
    }

    #[test]
    fn global_overrides_precede_evaluation_and_functions_read_live_state() -> Result<()> {
        let src = r#"{globals:{base:"error('overridden')",derived:"base*2",
                              current:"fn -> gen()+later",later:4}}"#;
        let mut model = Model::load(src, None, "base:3", None)?;
        assert_eq!(model.engine.evaluate("derived", &Env::new())?, V::Int(6));
        for n in [0, 7] {
            model.runtime.generation(n, Arc::from([]));
            assert_eq!(
                model.engine.evaluate("current()", &Env::new())?,
                V::Int(n + 4)
            );
        }
        Ok(())
    }
    #[test]
    fn anchored_context_across_branches_and_recursive_helpers() -> Result<()> {
        let src = r#"{axiom:"S",rules:[{from:"S",to:"P{x:0,y:0}H{rot:0}A[B]Z"}],
          drawing:[
            {from:"A > B",to:"XF{length:2}Y"},
            {from:"^ < X > Z",to:"F{length:3}"},
            {from:"Y > B",cond:"rctx(0)[0].symbol == 'B'",to:"F{length:pos()[0]}"},
            {from:"A < B > $",to:"F{length:123}"},
            {from:"A < Z",to:"F{length:1}"}]}"#;
        assert_eq!(final_pose(src, 1)?, 11.);
        Ok(())
    }

    #[test]
    fn formula_lookahead_preserves_global_values() -> Result<()> {
        let src = r#"{axiom:"S",globals:{v:"pos()[0]+1"},
          rules:[{from:"S",to:"P{x:0,y:0}H{rot:0}A B{x:v}"}],
          drawing:[
            {from:"A",to:"F{length:9}X"},
            {from:"X",locals:{w:"v+rctx(0)[0].args.x+v"},to:"F{length:w}"},
            {from:"B",to:"F{length:x}"}]}"#;
        // v is fixed at 1 during loading, including in the looked-ahead token.
        assert_eq!(final_pose(src, 1)?, 13.);
        Ok(())
    }

    #[test]
    fn defaults_locals_selection_and_terminating_recursion() -> Result<()> {
        let src = r#"{axiom:"P{x:0,y:0}H{rot:0}A A{x:4}",symbols:["A{x:2}"],
          rules:[
            {from:"A",locals:{bad:"error('unselected')"}},
            {from:"A{x:_}",locals:{a:"x+1",b:"a+1"},to:"B{x:b}"}],
          drawing:[
            {from:"B",cond:"false",locals:{bad:"error('false')"}},
            {from:"B{x:_}",weight:0,locals:{bad:"error('zero')"}},
            {from:"B",cond:"x>0",locals:{v:"1"},to:"F{length:v}B{x:x-1}"}]}"#;
        assert_eq!(final_pose(src, 1)?, 10.);
        Ok(())
    }

    #[test]
    fn weighted_drawing_and_depth_guard() -> Result<()> {
        let src = format!(
            r#"{{axiom:"P{{x:0,y:0}}H{{rot:0}}{}",drawing:[
          {{from:"A",to:"F{{length:1}}",weight:1}},
          {{from:"A",to:"F{{length:2}}",weight:3}}]}}"#,
            "A".repeat(100)
        );
        let value = final_pose(&src, 0)?;
        assert!(value > 150. && value < 200.);
        assert_eq!(value, final_pose(&src, 0)?);
        let error = final_pose(
            r#"{axiom:"A",drawing:[{from:"A",to:"B"},{from:"B",to:"A"}]}"#,
            0,
        )
        .unwrap_err();
        assert!(error.to_string().contains("A -> B -> A"));
        Ok(())
    }
    #[test]
    fn reference_words() -> Result<()> {
        for (model, words) in [
            (
                include_str!("../lstms/lindenmayer_algae.lstm"),
                ["S", "MRMA", "MRMAB", "MRMABA"],
            ),
            (
                include_str!("../lstms/context_signal.lstm"),
                [
                    "MABB[RBBB][RBBB]BBB",
                    "MAAB[RBBB][RBBB]BBB",
                    "MAAA[RBBB][RBBB]BBB",
                    "MAAA[RABB][RABB]ABB",
                ],
            ),
        ] {
            for (tokens, word) in generations(model, 3, true)?.iter().zip(words) {
                assert_eq!(tokens.iter().map(|t| t.ch).collect::<String>(), word);
            }
        }
        Ok(())
    }
    #[test]
    fn previous_generation_marks() -> Result<()> {
        for animate in [false, true] {
            let frames = generations(
                include_str!("../lstms/marks_mode_difference.lstm"),
                1,
                animate,
            )?;
            assert_eq!(frames[1][0].args["x"], V::Int(1));
        }
        Ok(())
    }
    #[test]
    fn bundled_examples_run_through_first_rewrite() -> Result<()> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lstms");
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "lstm") {
                generations(&std::fs::read_to_string(&path)?, 1, true)
                    .with_context(|| format!("model {}", path.display()))?;
            }
        }
        Ok(())
    }
    #[test]
    fn apple_seasons_completes_its_lifecycle() -> Result<()> {
        let src = include_str!("../lstms/apple_seasons.lstm");
        let iterations = Model::load(src, None, "", Some("1"))?.iterations;
        let frames = generations(src, iterations, true)?;
        // Exercise the falling-fruit rules that only become active after generation 16.
        for stage in ['T', 'G', 'W'] {
            assert!(
                frames
                    .iter()
                    .any(|frame| frame.iter().any(|t| t.ch == stage)),
                "orchard never reached stage {stage}"
            );
        }
        Ok(())
    }
    #[test]
    fn live_position_changes_between_successors() -> Result<()> {
        let frames = generations(
            r#"{axiom:'A',rules:[{from:'A',to:'F{length:8}B{y:pos()[1]}'}]}"#,
            1,
            true,
        )?;
        assert_eq!(frames[1][1].args["y"], V::Float(-292.));
        Ok(())
    }
}
