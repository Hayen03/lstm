//! Parses L-system words into templates whose argument formulas are evaluated later.
//! Formula parsing owns expression boundaries, so nested commas and braces can
//! appear inside an argument without splitting the surrounding symbol.

use crate::err::{Result, ResultContext, SyntaxError, ensure};
use formulas::{EvalValue as V, Expr, parse_next};
use indexmap::IndexMap;
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Joker,
    Expr(Expr),
}
impl From<formulas::Expr> for Arg {
    fn from(expr: formulas::Expr) -> Self {
        if expr == Expr::Joker {
            return Arg::Joker;
        }
        Arg::Expr(expr)
    }
}

// Preserve declaration order: evaluating arguments may consume randomness or marks.
pub type Values = IndexMap<Arc<str>, V>;
pub type Args = IndexMap<Arc<str>, Arg>;
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
impl TryFrom<V> for Token {
    type Error = crate::Error;
    fn try_from(value: V) -> std::result::Result<Self, Self::Error> {
        match value {
            V::String(s) => {
                let raw = s.graphemes(true).next().ok_or_else(|| crate::Error::InvalidToken(s.to_string()))?;
                let rest = &s[raw.len()..];
                if !rest.is_empty() {
                    return Err(crate::Error::InvalidToken(s.to_string()));
                }
                let ch: Arc<str> = if raw.is_ascii() {
                    Arc::from(raw)
                } else {
                    raw.nfc().collect::<String>().into()
                };
                if META.contains(&*ch) || r#"{}:,"'"#.contains(ch.as_ref()) {
                    return Err(crate::Error::InvalidToken(s.to_string()));
                }
                Ok(Token { ch, args: IndexMap::new() })
            },
            V::Object(data) => {
                let ch = match data.get("symbol") {
                    Some(V::String(s)) => {
                        let raw = s.graphemes(true).next().ok_or_else(|| crate::Error::InvalidToken(s.to_string()))?;
                        let rest = &s[raw.len()..];
                        if !rest.is_empty() {
                            return Err(crate::Error::InvalidToken(s.to_string()));
                        }
                        let ch: Arc<str> = if raw.is_ascii() {
                            Arc::from(raw)
                        } else {
                            raw.nfc().collect::<String>().into()
                        };
                        if META.contains(&*ch) || r#"{}:,"'"#.contains(ch.as_ref()) {
                            return Err(crate::Error::InvalidToken(s.to_string()));
                        }
                        ch
                    },
                    Some(other) => return Err(crate::Error::InvalidToken(format!("Expected a string for 'symbol', got {}", other.repr()))),
                    None => return Err(crate::Error::InvalidToken("Missing 'symbol' field".into())),
                };
                let args = match data.get("args") {
                    Some(V::Object(map)) => map.as_ref().clone(),
                    Some(other) => return Err(crate::Error::InvalidToken(format!("Expected an object for 'args', got {}", other.repr()))),
                    None => IndexMap::new(),
                };
                Ok(Token { ch, args })
            },
            other => Err(crate::Error::InvalidToken(format!("Expected a string or object, got {}", other.repr()))),
        }
    }
}

pub enum DeferredToken {
    Literal(Template),
    Func(formulas::Expr),
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
        let end=  {
            let (expr, n) = parse_next(rest)
                .with_context(|| "Error parsing token arguments".into())?;
            
            result.insert(k.into(), expr.into());
            n
        };
        s = rest[end..].trim_start();
        if !s.is_empty() {
            s = s.strip_prefix(',').ok_or(SyntaxError::ExpectedComma)?
        }
    }
    Ok(result)
}
/// Consumes one symbol and its optional argument block, leaving the suffix.
pub fn atom(s: &str) -> Result<(DeferredToken, &str)> {
    if s.trim().starts_with("${") {
        // spliced formula
        let (expr, idx) = formulas::parse_next(&s[2..])?;
        // skip the "${" prefix and the "}" suffix
        let end_idx = 2 + idx;
        let s = s[end_idx..].trim_start();
        if !s.starts_with('}') {
            return Err(SyntaxError::UnclosedArguments { input: s.into() }.into());
        }
        return Ok((
            DeferredToken::Func(expr),
            &s[1..],
        ));
    }
    let raw = s.graphemes(true).next().ok_or(SyntaxError::MissingSymbol)?;
    // Slice the original input before normalization, which may change byte length.
    let rest = &s[raw.len()..];
    let ch: Arc<str> = if raw.is_ascii() {
        Arc::from(raw)
    } else {
        raw.nfc().collect::<String>().into()
    };
    let tmplt = if rest.starts_with('{') {
        let end = block(rest)?;
        (
            Template {
                ch,
                args: args(&rest[1..end - 1])?,
            },
            &rest[end..],
        )
    } else {
        (
            Template {
                ch,
                args: Args::new(),
            },
            rest,
        )
    };
    Ok((DeferredToken::Literal(tmplt.0), tmplt.1))
}
/// Parses a whole axiom, successor, or drawing word; whitespace separates symbols.
pub fn word(mut s: &str) -> Result<Vec<DeferredToken>> {
    let mut out = Vec::new();
    while !s.trim_start().is_empty() {
        let (t, rest) = atom(s.trim_start())?;
        if let DeferredToken::Literal(lit) = &t {
            ensure!(
                !META.contains(lit.ch.as_ref()) && !r#"{}:,"'"#.contains(lit.ch.as_ref()),
                SyntaxError::ReservedSymbol {
                    symbol: lit.ch.clone()
                }
            );
        }
        out.push(t);
        s = rest;
    }
    Ok(out)
}

/// ensure the input contains exactly one symbol and return it as a Template
/// Does not accept spliced expressions like `${...}`
pub fn single(s: &str) -> Result<Template> {
    let mut w = word(s)?;
    ensure!(
        w.len() == 1,
        SyntaxError::ExpectedSingleSymbol { input: s.into() }
    );
    let tok = w.remove(0);
    match tok {
        DeferredToken::Literal(lit) => Ok(lit),
        DeferredToken::Func(expr) => Err(crate::Error::ExprForTok(expr.to_string())),
    }
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
            let DeferredToken::Literal(token) = token else {
                unreachable!("Expected a literal token");
            };
            assert_eq!(token.ch.as_ref(), normalized);
            assert_eq!(token.args["text"], formulas::parse_next("'e\u{301}'")?.0.into());
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
