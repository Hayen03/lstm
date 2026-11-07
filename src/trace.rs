//! Optional text or JSONL tracing with generation, symbol, rule, and phase filters.
//! Callers supply event details lazily so rejected events need no JSON allocation.

use crate::err::{Error, Result, ensure};
use clap::Args;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};
use unicode_normalization::UnicodeNormalization;

#[derive(Args, Default, Clone)]
pub struct Options {
    #[arg(long)]
    pub trace: bool,
    #[arg(long)]
    pub trace_output: Option<PathBuf>,
    #[arg(long,default_value="text",value_parser=["text","jsonl"])]
    trace_format: String,
    #[arg(long)]
    trace_symbol: Vec<String>,
    #[arg(long)]
    trace_rule: Vec<String>,
    #[arg(long,value_parser=range)]
    trace_generations: Option<(i64, i64)>,
    #[arg(long)]
    trace_draw: bool,
}
/// Parses one generation or an inclusive `START:END` interval.
fn range(s: &str) -> Result<(i64, i64)> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let a = a.parse().map_err(|source| Error::InvalidTraceRange {
        input: s.into(),
        source: Some(source),
    })?;
    let b = b.parse().map_err(|source| Error::InvalidTraceRange {
        input: s.into(),
        source: Some(source),
    })?;
    ensure!(
        a >= 0 && b >= a,
        Error::InvalidTraceRange {
            input: s.into(),
            source: None
        }
    );
    Ok((a, b))
}
/// Output sink plus the current predecessor's identity, attached to every event.
#[derive(Clone)]
pub struct Trace {
    options: Options,
    sink: Option<std::sync::Arc<std::sync::Mutex<Box<dyn Write + Send>>>>,
    pub generation: i64,
    pub index: usize,
    pub symbol: Arc<str>,
    pub rule: Option<(usize, String)>,
}
impl Trace {
    pub fn new(mut options: Options) -> Result<Self> {
        // Match the NFC representation stored by the symbol parser.
        for symbol in &mut options.trace_symbol {
            *symbol = symbol.nfc().collect();
        }
        // A filter or drawing switch enables tracing on its own. An explicit file
        // also enables output; otherwise enabled traces go to stderr.
        let enabled = options.trace
            || options.trace_draw
            || !options.trace_symbol.is_empty()
            || !options.trace_rule.is_empty()
            || options.trace_generations.is_some();
        let sink: Option<Box<dyn Write + Send>> = if let Some(p) = &options.trace_output {
            Some(Box::new(File::create(p)?))
        } else if enabled {
            Some(Box::new(io::stderr()))
        } else {
            None
        };
        Ok(Self {
            options,
            sink: sink.map(|out| std::sync::Arc::new(std::sync::Mutex::new(out))),
            generation: 0,
            index: 0,
            symbol: Arc::from(" "),
            rule: None,
        })
    }
    pub fn enabled(&self) -> bool {
        self.sink.is_some()
    }
    /// Diagnostic identity is available even when trace output is disabled.
    /// During drawing, the token index and symbol identify the growth anchor.
    pub fn location(&self, phase: &str) -> String {
        let rule = self.rule.as_ref().map_or(String::new(), |(i, name)| {
            format!(" rule {name} (index {i})")
        });
        format!(
            "{phase}{rule}, generation {}, token {} ({})",
            self.generation, self.index, self.symbol
        )
    }
    /// Writes one accepted event, adding the current generation and rule metadata.
    pub fn event(&mut self, phase: &str, event: &str, data: impl FnOnce() -> Value) -> Result<()> {
        let Some(out) = &mut self.sink else {
            return Ok(());
        };
        let o = &self.options;
        if phase == "drawing" && !o.trace_draw
            || o.trace_generations
                .is_some_and(|(a, b)| self.generation < a || self.generation > b)
            || !o.trace_symbol.is_empty()
                && !o.trace_symbol.iter().any(|s| s == self.symbol.as_ref())
            || !o.trace_rule.is_empty()
                && !self.rule.as_ref().is_some_and(|(i, r)| {
                    o.trace_rule.contains(r) || o.trace_rule.contains(&i.to_string())
                })
        {
            return Ok(());
        }
        let mut v = data();
        // Metadata belongs to the active predecessor even for nested drawing events.
        v["event"] = json!(event);
        v["phase"] = json!(phase);
        v["symbol"] = json!(self.symbol);
        v["generation"] = json!(self.generation);
        v["token_index"] = json!(self.index);
        if let Some((i, r)) = &self.rule {
            v["rule"] = json!(r);
            v["rule_index"] = json!(i)
        }
        let mut out = out.lock().unwrap();
        if o.trace_format == "jsonl" {
            writeln!(out, "{v}")?
        } else {
            writeln!(
                out,
                "[g={} {} token={} {}] {} {}",
                self.generation, phase, self.index, self.symbol, event, v
            )?
        }
        Ok(())
    }
}
