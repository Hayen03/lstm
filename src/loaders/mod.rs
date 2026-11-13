//! Source-format detection and loaders for the shared executable model.

mod common;
pub mod json5;
pub mod lstm;

use crate::{
    err::{Error, Result},
    model::Model,
};

/// Select a format by content, so existing JSON5 `.lstm` files remain usable.
/// Individual loaders are strict about their own format and do not fall back.
pub fn load(
    text: &str,
    override_axiom: Option<&str>,
    globals: &str,
    user_seed: Option<&str>,
    freclim: Option<usize>,
) -> Result<Model> {
    load_with_iterations(text, override_axiom, globals, user_seed, freclim, None)
}

/// Resolve the effective target before evaluating globals and startup properties.
pub fn load_with_iterations(
    text: &str,
    override_axiom: Option<&str>,
    globals: &str,
    user_seed: Option<&str>,
    freclim: Option<usize>,
    iterations: Option<i64>,
) -> Result<Model> {
    let start = content_start(text)?;
    let source = if text[start..].starts_with('{') {
        ::json5::from_str(text.trim_start_matches('\u{feff}'))?
    } else {
        lstm::LstmLoader::definition(text)?
    };
    common::build(
        source,
        override_axiom,
        globals,
        user_seed,
        freclim,
        iterations,
    )
}

pub(super) fn diagnostic(text: &str, offset: usize, message: impl Into<String>) -> Error {
    let prefix = &text[..offset];
    Error::NativeSyntax {
        line: prefix.bytes().filter(|&b| b == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
        message: message.into(),
    }
}

/// Skip only the file prelude; `//` elsewhere may be formula floor division.
/// Offsets always refer to the original UTF-8 source, including a possible BOM.
pub(super) fn content_start(text: &str) -> Result<usize> {
    let mut at = if text.starts_with('\u{feff}') { 3 } else { 0 };
    loop {
        at += text[at..].len() - text[at..].trim_start().len();
        if text[at..].starts_with("//") {
            at = text[at..].find('\n').map_or(text.len(), |i| at + i);
        } else if text[at..].starts_with("/*") {
            let end = text[at + 2..]
                .find("*/")
                .ok_or_else(|| diagnostic(text, at, "Unclosed leading block comment"))?;
            at += end + 4;
        } else {
            return Ok(at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_skips_the_prelude_and_keeps_loaders_strict() -> Result<()> {
        for prefix in ["", "\u{feff}", " \n// header\n/* comment */\n"] {
            for source in ["{name:'example'}", "name: 'example';"] {
                let model = load(&format!("{prefix}{source}"), None, "", None, None)?;
                assert_eq!(model.name, "example");
            }
        }
        for source in ["", "\u{feff} \n// only a comment", "/* only a comment */"] {
            assert_eq!(load(source, None, "", None, None)?.name, "");
        }
        for (source, fragment) in [
            ("// header\n[", "2:1"),
            ("/* unclosed", "Unclosed leading block comment"),
            ("{name:}", "JSON5"),
        ] {
            let error = load(source, None, "", None, None).err().unwrap();
            assert!(format!("{error:#}").contains(fragment), "{error:#}");
        }
        assert!(json5::Json5Loader::load("name:'native';", None, "", None, None).is_err());
        Ok(())
    }
}
