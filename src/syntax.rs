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
            V::String(s) => Ok(Token {
                ch: token_symbol(&s)?,
                args: IndexMap::new(),
            }),
            V::Object(data) => {
                let ch = match data.get("symbol") {
                    Some(V::String(s)) => token_symbol(s).context("token field symbol")?,
                    Some(other) => {
                        return Err(crate::Error::InvalidToken(format!(
                            "Expected a string for 'symbol', got {}",
                            other.repr()
                        )));
                    }
                    None => {
                        return Err(crate::Error::InvalidToken("Missing 'symbol' field".into()));
                    }
                };
                let args = match data.get("args") {
                    Some(V::Object(map)) => map.as_ref().clone(),
                    Some(other) => {
                        return Err(crate::Error::InvalidToken(format!(
                            "Expected an object for 'args', got {}",
                            other.repr()
                        )));
                    }
                    None => IndexMap::new(),
                };
                Ok(Token { ch, args })
            }
            other => Err(crate::Error::InvalidToken(format!(
                "Expected a string or object, got {}",
                other.repr()
            ))),
        }
    }
}

/// Formula-produced symbols obey the same restrictions as literal word symbols.
/// Validate before NFC normalization, which can change the UTF-8 byte length.
fn token_symbol(source: &str) -> Result<Arc<str>> {
    let mut graphemes = source.graphemes(true);
    let raw = graphemes.next().ok_or_else(|| {
        crate::Error::InvalidToken(
            "Expected exactly one grapheme cluster, got an empty symbol".into(),
        )
    })?;
    ensure!(
        graphemes.next().is_none(),
        crate::Error::InvalidToken(format!(
            "Expected exactly one grapheme cluster, got {source:?}"
        ))
    );
    let ch: Arc<str> = raw.nfc().collect::<String>().into();
    ensure!(
        !reserved(&ch),
        SyntaxError::ReservedSymbol { symbol: ch.clone() }
    );
    Ok(ch)
}

fn reserved(ch: &str) -> bool {
    META.contains(ch) || r#"{}:,"'"#.contains(ch)
}

/// Parse one complete field, unlike `parse_next`, which also serves word boundaries.
/// Reject trailing tokens but allow whitespace, including multibyte whitespace.
pub fn expression(source: &str) -> Result<Expr> {
    let (expr, end) = parse_next(source).with_context(|| format!("Parsing formula {source:?}"))?;
    ensure!(
        source[end..].trim().is_empty(),
        crate::Error::TrailingInput(source.into())
    );
    Ok(expr)
}

/// A literal token or a formula evaluated once when this word position is reached.
/// A splice can produce zero, one, or several tokens; it is never reparsed as a word.
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

/// Parses ordered `name: expression` pairs, including standalone `_` constraints.
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
        // parse_next returns a byte offset, safe for slicing the original UTF-8.
        let (expr, end) =
            parse_next(rest).with_context(|| format!("Parsing argument {k}: {rest:?}"))?;
        result.insert(k.into(), expr.into());
        s = rest[end..].trim_start();
        if !s.is_empty() {
            s = s.strip_prefix(',').ok_or(SyntaxError::ExpectedComma)?
        }
    }
    Ok(result)
}
/// Consumes one symbol and its optional argument block, leaving the suffix.
pub fn atom(s: &str) -> Result<(DeferredToken, &str)> {
    if s.starts_with("${") {
        let (expr, idx) =
            formulas::parse_next(&s[2..]).with_context(|| format!("Parsing splice {s:?}"))?;
        // The expression parser stops before the delimiter; validate it explicitly.
        let end_idx = 2 + idx;
        let rest = s[end_idx..].trim_start();
        if !rest.starts_with('}') {
            return Err(SyntaxError::UnclosedArguments { input: s.into() }.into());
        }
        return Ok((DeferredToken::Func(expr), &rest[1..]));
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
                ch: ch.clone(),
                args: args(&rest[1..end - 1]).with_context(|| format!("symbol {ch}"))?,
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
                !reserved(&lit.ch),
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
    fn splice_delimiters_and_unicode_argument_boundaries() -> Result<()> {
        let (token, rest) = atom("${{'symbol':'é', 'args':{'text':'}'}}\u{a0}}B")?;
        assert!(matches!(token, DeferredToken::Func(_)));
        assert_eq!(rest, "B");
        for source in ["${'F'", "${'F'X", "${}"] {
            assert!(atom(source).is_err(), "{source}");
        }
        let values = args("text: 'é👩‍🔬', n: 2, present: _")?;
        assert_eq!(values.len(), 3);
        assert_eq!(values["present"], Arg::Joker);
        assert!(matches!(values["n"], Arg::Expr(_)));
        Ok(())
    }

    #[test]
    fn token_values_share_symbol_validation_and_preserve_arguments() -> Result<()> {
        for source in ["e\u{301}", "👩‍🔬", "F", "[", "]"] {
            let literal = Token::try_from(V::String(source.into()))?;
            let object = V::Object(Arc::new(IndexMap::from([
                ("symbol".into(), V::String(source.into())),
                (
                    "args".into(),
                    V::Object(Arc::new(IndexMap::from([("x".into(), V::Int(7))]))),
                ),
            ])));
            let token = Token::try_from(object)?;
            assert_eq!(literal.ch, token.ch);
            assert_eq!(token.ch.as_ref(), source.nfc().collect::<String>());
            assert_eq!(token.args["x"], V::Int(7));
        }
        for source in ["", "AB", "+", "$", "{", "}", ":", ",", "\"", "'"] {
            for value in [
                V::String(source.into()),
                V::Object(Arc::new(IndexMap::from([(
                    "symbol".into(),
                    V::String(source.into()),
                )]))),
            ] {
                assert!(Token::try_from(value).is_err(), "{source:?}");
            }
        }
        assert!(Token::try_from(V::Int(3)).is_err());
        assert!(Token::try_from(V::Object(Arc::new(IndexMap::new()))).is_err());
        Ok(())
    }

    #[test]
    fn complete_fields_allow_whitespace_but_reject_trailing_tokens() -> Result<()> {
        for source in ["1 ", "true\n", "'é'\u{a0}", "fn x -> x + 1 "] {
            expression(source)?;
        }
        for source in ["1 garbage", "true false", "1, 2"] {
            let error = expression(source).unwrap_err();
            assert!(matches!(
                error.without_context(),
                crate::Error::TrailingInput(_)
            ));
        }
        Ok(())
    }

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
            assert_eq!(
                token.args["text"],
                formulas::parse_next("'e\u{301}'")?.0.into()
            );
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
