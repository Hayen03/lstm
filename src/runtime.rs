//! Model-owned state captured by formula builtins.
//! Context queries read the input word; pose queries read the drawing in progress.

use crate::{context::Tree, dessin::Dessin, syntax::Token};
use formulas::{Env, Error, EvalValue as V, FormulaEngine, Function, Result};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Default)]
struct Live {
    pos: (f64, f64),
    heading: f64,
    color: u32,
    width: f64,
    size: (u32, u32),
    generation: i64,
    // Outer None means the target has not been resolved yet; inner None is endless.
    target: Option<Option<i64>>,
    // Marks are double-buffered: reads see the completed previous generation.
    previous: Arc<[V]>,
    current: Vec<V>,
    seq: Arc<[Token]>,
    at: usize,
    trees: [Option<Arc<Tree>>; 2],
    trace_queries: bool,
    queries: Vec<(String, V)>,
    provider: Option<Provider>,
}

pub type Neighborhood = (Arc<Tree>, Arc<[Token]>);
pub type Provider =
    Arc<dyn Fn(bool, &mut FormulaEngine) -> crate::err::Result<Neighborhood> + Send + Sync>;

/// Each engine's builtins capture this model's state, without an active-model global.
#[derive(Clone, Default)]
pub struct Runtime {
    live: Arc<Mutex<Live>>,
}
impl Runtime {
    pub fn target(&self, n: i64) {
        self.live.lock().unwrap().target = Some((n >= 0).then_some(n));
    }
    pub fn enable_query_trace(&self, on: bool) {
        self.live.lock().unwrap().trace_queries = on;
    }
    /// Drains queries recorded during formula evaluation for the caller's trace phase.
    pub fn queries(&self) -> Vec<(String, V)> {
        std::mem::take(&mut self.live.lock().unwrap().queries)
    }
    pub fn sync(&self, d: &Dessin) {
        let mut s = self.live.lock().unwrap();
        let pose = d.pose();
        (s.pos, s.heading) = (pose.p, pose.heading);
        s.size = (d.width, d.height);
        s.color = pose.color;
        s.width = pose.width;
    }
    /// Starts a generation, publishing collected marks and replacing the context word.
    pub fn generation(&self, n: i64, seq: Arc<[Token]>) {
        let mut s = self.live.lock().unwrap();
        s.generation = n;
        s.previous = std::mem::take(&mut s.current).into();
        s.seq = seq;
        s.trees = [None, None];
    }
    /// Selects the predecessor; cached neighborhoods belong only to this token index.
    pub fn token(&self, i: usize) {
        let mut s = self.live.lock().unwrap();
        s.at = i;
        s.trees = [None, None];
    }
    pub fn with_context<T>(&self, provider: Option<Provider>, f: impl FnOnce() -> T) -> T {
        // Restore the outer provider and query buffer on errors and unwinding too.
        let restore = {
            let mut s = self.live.lock().unwrap();
            ContextScope {
                runtime: self,
                provider: std::mem::replace(&mut s.provider, provider),
                queries: std::mem::take(&mut s.queries),
            }
        };
        let result = f();
        drop(restore);
        result
    }
    pub fn neighborhood(
        &self,
        left: bool,
        engine: &mut FormulaEngine,
    ) -> crate::err::Result<Neighborhood> {
        // Release the lock before invoking a provider: lookahead can evaluate
        // more formulas, which call back into this runtime.
        let provider = self.live.lock().unwrap().provider.clone();
        if let Some(resolve) = provider {
            return resolve(left, engine);
        }
        let mut s = self.live.lock().unwrap();
        let side = usize::from(left);
        if s.trees[side].is_none() {
            s.trees[side] = Some(Arc::new(Tree::build(&s.seq, s.at, left)));
        }
        Ok((s.trees[side].as_ref().unwrap().clone(), s.seq.clone()))
    }
    fn query(&self, name: &str, args: &[V], engine: &mut FormulaEngine) -> Result<V> {
        let value = self.query_inner(name, args, engine)?;
        let mut s = self.live.lock().unwrap();
        if s.trace_queries {
            s.queries.push((
                name.to_owned(),
                if name == "mark" {
                    args[0].clone()
                } else {
                    value.clone()
                },
            ));
        }
        Ok(value)
    }
    fn query_inner(&self, name: &str, args: &[V], engine: &mut FormulaEngine) -> Result<V> {
        let valid = match name {
            "mark" => args.len() == 1,
            "lctx" | "rctx" => args.len() <= 1,
            _ => args.is_empty(),
        };
        if !valid {
            return Err(Error::InvalidArgument(format!(
                "Wrong argument count for {name}"
            )));
        }
        if matches!(name, "lctx" | "rctx") {
            let d = match args.first() {
                None | Some(V::Null) => None,
                Some(V::Int(n)) => Some(*n),
                _ => {
                    return Err(Error::InvalidArgument(
                        "Context depth must be an integer".into(),
                    ));
                }
            };
            let (t, word) = self
                .neighborhood(name == "lctx", engine)
                // The formulas builtin API accepts only formulas::Error; retain
                // the LSTM diagnostic chain when crossing that boundary.
                .map_err(|e| Error::InvalidArgument(format!("{e:#}")))?;
            return Ok(t.value(&word, d));
        }
        let mut s = self.live.lock().unwrap();
        Ok(match name {
            "mark" => {
                s.current.push(args[0].clone());
                V::Null
            }
            "marked" => V::Array(s.previous.clone()),
            "pos" => V::Array(vec![V::Float(s.pos.0), V::Float(s.pos.1)].into()),
            "heading" => V::Float(s.heading),
            "gen" => V::Int(s.generation),
            "mxgen" => match s.target {
                Some(Some(n)) => V::Int(n),
                Some(None) => V::Null,
                None => return Err(Error::InvalidArgument(
                    "mxgen() is unavailable until recommended_iterations is resolved; use a literal target or a CLI -n override when calling it in globals".into()
                )),
            },
            "screen" => V::Array(vec![V::Int(s.size.0.into()), V::Int(s.size.1.into())].into()),
            "width" => V::Float(s.width),
            "color" => V::Int(s.color.into()),
            _ => unreachable!(),
        })
    }
    /// Adds L-system queries to the formula library's ordinary builtin environment.
    pub fn environment(&self) -> Env<'static> {
        let mut env = formulas::builtins::GLOBAL_ENV.clone();
        for name in [
            "pos", "heading", "gen", "mxgen", "screen", "mark", "marked", "lctx", "rctx", "width", "color",
        ] {
            let runtime = self.clone();
            env.insert(
                name,
                V::Func(Arc::new(Function::Builtin(
                    name.into(),
                    Arc::new(move |args, engine, _| runtime.query(name, args, engine)),
                ))),
            );
        }
        env.insert("hex", ENV_HEX.clone());
        env.insert("rgb", ENV_RGB.clone());
        env
    }
}

struct ContextScope<'a> {
    runtime: &'a Runtime,
    provider: Option<Provider>,
    queries: Vec<(String, V)>,
}
impl Drop for ContextScope<'_> {
    fn drop(&mut self) {
        let mut s = self.runtime.live.lock().unwrap();
        s.provider = self.provider.take();
        s.queries = std::mem::take(&mut self.queries);
    }
}

fn env_hex(arguments: &[V], _formulas: &mut FormulaEngine, _env: &Env) -> Result<V> {
    if arguments.len() != 3 {
        return Err(Error::Arity { function: "hex".into(), expected: formulas::err::ArgumentCount::Exactly(3), actual: arguments.len() });
    }
    let r = match &arguments[0] {
        V::Int(1) | V::Bool(true) => 0xFFu32,
        V::Int(0) | V::Bool(false) => 0x00u32,
        V::Float(f) => {
            if !f.is_finite() {
                return Err(Error::Type(format!("r: Expected a finite number between 0 and 1, got {}", f).into()));
            }
            (f.clamp(0.0, 1.0) * 255.0).round() as u32
        },
        other => return Err(Error::Type(format!("r: Expected a number between 0 and 1, got {}", other.type_name()).into())),
    };
    let g = match &arguments[1] {
        V::Int(1) | V::Bool(true) => 0xFFu32,
        V::Int(0) | V::Bool(false) => 0x00u32,
        V::Float(f) => {
            if !f.is_finite() {
                return Err(Error::Type(format!("g: Expected a finite number between 0 and 1, got {}", f).into()));
            }
            (f.clamp(0.0, 1.0) * 255.0).round() as u32
        },
        other => return Err(Error::Type(format!("g: Expected a number between 0 and 1, got {}", other.type_name()).into())),
    };
    let b = match &arguments[2] {
        V::Int(1) | V::Bool(true) => 0xFFu32,
        V::Int(0) | V::Bool(false) => 0x00u32,
        V::Float(f) => {
            if !f.is_finite() {
                return Err(Error::Type(format!("b: Expected a finite number between 0 and 1, got {}", f).into()));
            }
            (f.clamp(0.0, 1.0) * 255.0).round() as u32
        },
        other => return Err(Error::Type(format!("b: Expected a number between 0 and 1, got {}", other.type_name()).into())),
    };
    Ok(V::Int(((r << 16) | (g << 8) | b) as i64))
}
fn env_rgb(arguments: &[V], _formulas: &mut FormulaEngine, _env: &Env) -> Result<V> {
    if arguments.len() != 1 {
        return Err(Error::Arity { function: "rgb".into(), expected: formulas::err::ArgumentCount::Exactly(1), actual: arguments.len() });
    }
    let i = match &arguments[0] {
        V::Int(i) => {
            if (0..=0xFFFFFF).contains(i) { 
                *i as u32 
            } else { 
                return Err(Error::Type(format!("rgb: Expected an integer color between 0 and 0xFFFFFF, got {}", i).into())) 
            } 
        },
        other => return Err(Error::Type(format!("rgb: Expected an integer color, got {}", other.type_name()).into())),
    };
    let r = ((i >> 16) & 0xFF) as f64 / 255.0;
    let g = ((i >> 8) & 0xFF) as f64 / 255.0;
    let b = (i & 0xFF) as f64 / 255.0;
    Ok(V::Array(Arc::from(vec![V::Float(r), V::Float(g), V::Float(b)])))
}
static ENV_HEX: LazyLock<V> = LazyLock::new(|| V::Func(Arc::new(formulas::Function::Builtin(Arc::from("hex"), Arc::new(env_hex)))));
static ENV_RGB: LazyLock<V> = LazyLock::new(|| V::Func(Arc::new(formulas::Function::Builtin(Arc::from("rgb"), Arc::new(env_rgb)))));

#[cfg(test)]
mod tests {
    use unicode_segmentation::UnicodeSegmentation;

use super::*;

    fn tokens(s: &str) -> Arc<[Token]> {
        s.graphemes(true)
            .map(|ch| Token {
                ch: Arc::from(ch),
                args: Default::default(),
            })
            .collect()
    }

    #[test]
    fn nested_context_restores_provider_and_trace_after_errors() -> crate::err::Result<()> {
        let runtime = Runtime::default();
        let mut engine = FormulaEngine::new(Some(1)).with_globals(runtime.environment());
        runtime.generation(3, tokens("AB"));
        runtime.enable_query_trace(true);
        engine.evaluate("gen()", &Env::new())?;
        let provider: Provider = Arc::new(|left, engine| {
            // A provider may reenter the same runtime through another builtin.
            assert_eq!(engine.evaluate("gen()", &Env::new())?, V::Int(3));
            let word = tokens("XY");
            Ok((Arc::new(Tree::build(&word, 0, left)), word))
        });
        let query = "rctx(0)[0].symbol";
        runtime.with_context(Some(provider.clone()), || -> crate::err::Result<()> {
            assert_eq!(engine.evaluate(query, &Env::new())?, V::String("Y".into()));
            let error = runtime.with_context(None, || -> crate::err::Result<()> {
                assert_eq!(engine.evaluate(query, &Env::new())?, V::String("B".into()));
                Err(crate::err::Error::UnclosedBranch)
            });
            assert!(error.is_err());
            assert_eq!(engine.evaluate(query, &Env::new())?, V::String("Y".into()));
            Ok(())
        })?;
        assert_eq!(runtime.queries(), vec![("gen".into(), V::Int(3))]);
        assert_eq!(engine.evaluate(query, &Env::new())?, V::String("B".into()));

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.with_context(Some(provider), || panic!("provider scope failed"));
        }));
        assert!(panic.is_err());
        assert_eq!(engine.evaluate(query, &Env::new())?, V::String("B".into()));
        Ok(())
    }
}
