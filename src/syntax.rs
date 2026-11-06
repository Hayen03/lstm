//! Parses L-system words into templates whose argument formulas are evaluated later.
//! Formula parsing owns expression boundaries, so nested commas and braces can
//! appear inside an argument without splitting the surrounding symbol.

use crate::err::{Result, SyntaxError, ensure};
use formulas::{EvalValue as V, parse_next};
use indexmap::IndexMap;
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

// Preserve declaration order: evaluating arguments may consume randomness or marks.
pub type Values = IndexMap<Arc<str>, V>;
pub type Args = IndexMap<Arc<str>, String>;
pub const DRAW: &str = "FRL()MCWPDH";
pub const META: &str = "+.-*^$|?><";
#[derive(Clone, Debug)]
/// A symbol with unevaluated argument expressions, as written in a model.
pub struct Template {
    pub ch: Arc<str>,
    pub args: Args,
}
#[derive(Clone, Debug)]
/// A symbol in a generation, with its arguments already resolved to values.
pub struct Token {
    pub ch: Arc<str>,
    pub args: Values,
}
impl Token {
    /// Exposes a token to formulas as `{symbol: ..., args: ...}`.
    pub fn value(&self) -> V {
        V::Object(Arc::new(IndexMap::from([
            ("symbol".into(), V::String(self.ch.to_string().into())),
            ("args".into(), V::Object(self.args.clone().into())),
        ])))
    }
}
/// Returns the UTF-8 byte length of a block starting with `{`, including `}`.
/// Quoted braces and escaped quotes do not change the nesting depth.
pub fn block(s: &str) -> Result<usize> {
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if c == '\\' {
                escaped = true
            } else if c == q {
                quote = None
            }
        } else {
            match c {
                '\'' | '"' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i + 1);
                    }
                }
                _ => (),
            }
        }
    }
    Err(SyntaxError::UnclosedArguments { input: s.into() }.into())
}
/// Splits `name: expression` pairs while retaining the original formula text.
pub fn args(mut s: &str) -> Result<Args> {
    let mut result = Args::new();
    while !s.trim().is_empty() {
        let (k, rest) = s
            .trim_start()
            .split_once(':')
            .ok_or(SyntaxError::ExpectedArgument)?;
        let k = k.trim();
        ensure!(
            !k.is_empty() && k.chars().all(|c| c == '_' || c.is_alphanumeric()),
            SyntaxError::InvalidArgumentName { name: k.into() }
        );
        let rest = rest.trim_start();
        let (_, n) = parse_next(rest)?;
        // The formula parser counts characters; Rust string slices require bytes.
        let end = rest.char_indices().nth(n).map_or(rest.len(), |(i, _)| i);
        ensure!(end > 0, SyntaxError::EmptyFormula { name: k.into() });
        result.insert(k.into(), rest[..end].to_owned());
        s = rest[end..].trim_start();
        if !s.is_empty() {
            s = s.strip_prefix(',').ok_or(SyntaxError::ExpectedComma)?
        }
    }
    Ok(result)
}
/// Consumes one symbol and its optional argument block, leaving the suffix.
pub fn atom(s: &str) -> Result<(Template, &str)> {
    let raw = s.graphemes(true).next().ok_or(SyntaxError::MissingSymbol)?;
    // Slice the original input before normalization, which may change byte length.
    let rest = &s[raw.len()..];
    let ch: Arc<str> = if raw.is_ascii() {
        Arc::from(raw)
    } else {
        raw.nfc().collect::<String>().into()
    };
    if rest.starts_with('{') {
        let end = block(rest)?;
        Ok((
            Template {
                ch,
                args: args(&rest[1..end - 1])?,
            },
            &rest[end..],
        ))
    } else {
        Ok((
            Template {
                ch,
                args: Args::new(),
            },
            rest,
        ))
    }
}
/// Parses a whole axiom, successor, or drawing word; whitespace separates symbols.
pub fn word(mut s: &str) -> Result<Vec<Template>> {
    let mut out = Vec::new();
    while !s.trim_start().is_empty() {
        let (t, rest) = atom(s.trim_start())?;
        ensure!(
            !META.contains(t.ch.as_ref()) && !r#"{}:,"'"#.contains(t.ch.as_ref()),
            SyntaxError::ReservedSymbol {
                symbol: t.ch.clone()
            }
        );
        out.push(t);
        s = rest;
    }
    Ok(out)
}
pub fn single(s: &str) -> Result<Template> {
    let mut w = word(s)?;
    ensure!(
        w.len() == 1,
        SyntaxError::ExpectedSingleSymbol { input: s.into() }
    );
    Ok(w.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_preserves_source_offsets_and_argument_text() -> Result<()> {
        for (raw, normalized) in [
            ("e\u{301}", "é"),
            ("é", "é"),
            ("\u{344}", "\u{308}\u{301}"),
            ("👩‍🔬", "👩‍🔬"),
        ] {
            let source = format!("{raw}{{text: 'e\u{301}'}}B");
            let (token, rest) = atom(&source)?;
            assert_eq!(token.ch.as_ref(), normalized);
            assert_eq!(token.args["text"], "'e\u{301}'");
            assert_eq!(rest, "B");
        }
        Ok(())
    }

    #[test]
    fn normalization_preserves_case_and_compatibility_distinctions() -> Result<()> {
        for (a, b) in [("é", "É"), ("①", "1"), ("ﬁ", "f"), ("Ａ", "A")] {
            assert_ne!(single(a)?.ch, single(b)?.ch);
        }
        assert_eq!(word("e\u{301}👩‍🔬①")?.len(), 3);
        Ok(())
    }
}
