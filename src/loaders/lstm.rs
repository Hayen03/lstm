//! Native v3 file grammar. The host parser owns fields, lists and rule clauses;
//! the formulas parser owns embedded expression boundaries. No JSON5 round trip
//! or formula evaluation is involved in reading a native document.

use super::{
    common::{Definition, RuleDefinition, build},
    content_start, diagnostic,
};
use crate::{
    context::{Pattern, predecessor},
    err::{Error, Result, ResultContext},
    model::Model,
    syntax,
};
use formulas::{EvalValue as V, Expr};
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashSet;
use unicode_segmentation::UnicodeSegmentation;

pub struct LstmLoader;

impl LstmLoader {
    pub fn load(
        text: &str,
        override_axiom: Option<&str>,
        globals: &str,
        user_seed: Option<&str>,
        freclim: Option<usize>,
    ) -> Result<Model> {
        let source = Parser::new(text)?.document()?;
        // The same evaluation order, overrides and runtime initialization apply
        // to both formats, regardless of the textual order of native fields.
        build(source, override_axiom, globals, user_seed, freclim, None)
    }

    pub(super) fn definition(text: &str) -> Result<Definition> {
        Parser::new(text)?.document()
    }
}

struct Parser<'a> {
    original: &'a str,
    text: String,
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Result<Self> {
        let start = content_start(source)?;
        let mut bytes = source.as_bytes().to_vec();
        let mut quote = None;
        let mut escaped = false;
        let mut line_start = false;
        // Blank whole-line comments without moving any subsequent source offset.
        // A quoted multiline string suppresses comment recognition until it ends.
        for (offset, ch) in source[start..].char_indices() {
            let i = start + offset;
            if bytes[i] != source.as_bytes()[i] {
                continue;
            }
            if let Some(q) = quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == q {
                    quote = None;
                }
            } else if line_start && ch == '/' && source[i..].starts_with("//") {
                let end = source[i..].find('\n').map_or(source.len(), |n| i + n);
                bytes[i..end].fill(b' ');
            } else if ch == '\'' || ch == '"' {
                quote = Some(ch);
            }
            if ch == '\n' {
                line_start = true;
            } else if !ch.is_whitespace() {
                line_start = false;
            }
        }
        Ok(Self {
            original: source,
            text: String::from_utf8(bytes).unwrap(),
            at: start,
        })
    }

    fn error(&self, message: impl Into<String>) -> Error {
        diagnostic(self.original, self.at, message)
    }
    fn location(&self, at: usize) -> String {
        diagnostic(self.original, at, "")
            .to_string()
            .trim_end_matches(": ")
            .to_owned()
    }
    fn rest(&self) -> &str {
        &self.text[self.at..]
    }
    fn ws(&mut self) {
        self.at += self.rest().len() - self.rest().trim_start().len();
    }
    fn eat(&mut self, token: &str) -> bool {
        self.ws();
        if self.rest().starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, token: &str) -> Result<()> {
        if self.eat(token) {
            Ok(())
        } else {
            Err(self.error(format!("Expected {token:?}")))
        }
    }
    fn ident(&mut self) -> Result<String> {
        self.ws();
        let mut end = 0;
        for (i, c) in self.rest().char_indices() {
            if (i == 0 && (c.is_alphabetic() || c == '_'))
                || (i > 0 && (c.is_alphanumeric() || c == '_'))
            {
                end = i + c.len_utf8();
            } else {
                break;
            }
        }
        if end == 0 {
            return Err(self.error("Expected an identifier"));
        }
        let name = self.rest()[..end].to_owned();
        self.at += end;
        Ok(name)
    }
    fn expr(&mut self) -> Result<(Expr, String)> {
        self.ws();
        let start = self.at;
        let (expr, end) =
            formulas::parse_next(self.rest()).with_context(|| self.location(start))?;
        self.at += end;
        Ok((expr, self.text[start..self.at].to_owned()))
    }
    fn string(&mut self) -> Result<String> {
        self.ws();
        let start = self.at;
        let (expr, _) = self.expr()?;
        match expr {
            Expr::Literal(V::String(value)) => Ok(value.to_string()),
            _ => Err(diagnostic(
                self.original,
                start,
                "Expected a string literal",
            )),
        }
    }
    fn integer(&mut self) -> Result<i64> {
        self.ws();
        let start = self.at;
        let (_, raw) = self.expr()?;
        raw.parse()
            .map_err(|_| diagnostic(self.original, start, "Expected an integer literal"))
    }
    fn collection(&mut self) -> Result<char> {
        self.expect("{")?;
        Ok('}')
    }
    fn separator(&mut self, close: char) -> Result<bool> {
        if self.eat(",") {
            Ok(false)
        } else if self.eat(&close.to_string()) {
            Ok(true)
        } else {
            Err(self.error(format!("Expected ',' or '{close}'")))
        }
    }

    fn bindings(&mut self) -> Result<IndexMap<String, Value>> {
        self.expect("{")?;
        let mut bindings = IndexMap::new();
        loop {
            if self.eat("}") {
                break;
            }
            let name = self.ident()?;
            self.expect(":")?;
            let (_, raw) = self.expr().with_context(|| format!("binding {name}"))?;
            bindings.insert(name, Value::String(raw));
            if self.separator('}')? {
                break;
            }
        }
        Ok(bindings)
    }
    fn metadata(&mut self) -> Result<IndexMap<String, Value>> {
        self.expect("{")?;
        let mut values = IndexMap::new();
        loop {
            if self.eat("}") {
                break;
            }
            self.ws();
            let key = if self.rest().starts_with(['\'', '"']) {
                self.string()?
            } else {
                self.ident()?
            };
            self.expect(":")?;
            let value = Value::String(self.expr()?.1);
            if values.insert(key.clone(), value).is_some() {
                return Err(self.error(format!("Repeated metadata key {key}")));
            }
            if self.separator('}')? {
                break;
            }
        }
        Ok(values)
    }

    /// Decode one scalar in a symbol, keeping offsets in the original source.
    /// Other backslashes remain literal; formula escapes belong to `expr`.
    fn symbol_scalar(&self, at: usize) -> Result<(char, usize)> {
        let rest = &self.text[at..];
        if !rest.starts_with("\\u") {
            let ch = rest.chars().next().unwrap();
            return Ok((ch, ch.len_utf8()));
        }
        let invalid = || {
            diagnostic(
                self.original,
                at,
                "Invalid Unicode symbol escape; expected \\uXXXX or \\u{H…} (1–6 hex digits)",
            )
        };
        let (digits, end) = if rest.starts_with("\\u{") {
            let count = rest[3..].bytes().take_while(u8::is_ascii_hexdigit).count();
            if !(1..=6).contains(&count) || rest.as_bytes().get(3 + count) != Some(&b'}') {
                return Err(invalid());
            }
            (&rest[3..3 + count], 4 + count)
        } else {
            let digits = rest.get(2..6).ok_or_else(invalid)?;
            if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            (digits, 6)
        };
        let value = u32::from_str_radix(digits, 16).unwrap();
        let ch = char::from_u32(value).ok_or_else(|| {
            diagnostic(self.original, at, "Unicode symbol escape must be a Unicode scalar value (no surrogates; maximum U+10FFFF)")
        })?;
        Ok((ch, end))
    }

    /// Decode before segmentation so escaped combining marks and ZWJ sequences
    /// join their neighbors. Only consume the first decoded grapheme, and never
    /// feed the decoded text back through escape decoding.
    fn symbol(&mut self) -> Result<String> {
        let raw = self
            .rest()
            .graphemes(true)
            .next()
            .ok_or_else(|| self.error("Expected a symbol"))?;
        if !self.rest().starts_with("\\u") && !self.rest()[raw.len()..].starts_with("\\u") {
            let symbol = raw.to_owned();
            self.at += raw.len();
            return Ok(symbol);
        }
        let mut symbol = String::new();
        let mut at = self.at;
        while at < self.text.len() {
            let (ch, len) = self.symbol_scalar(at)?;
            let previous = symbol.len();
            symbol.push(ch);
            if symbol.graphemes(true).nth(1).is_some() {
                symbol.truncate(previous);
                break;
            }
            at += len;
        }
        self.at = at;
        Ok(symbol)
    }

    /// Decode symbols and normalize host whitespace before their arguments.
    /// Formula strings and whitespace within their expressions are preserved.
    fn token(&mut self) -> Result<String> {
        self.ws();
        let start = self.at;
        if self.eat("${") {
            let (_, raw) = self.expr()?;
            self.expect("}")?;
            return Ok(format!("${{{raw}}}"));
        }
        let symbol = self.symbol()?;
        syntax::single(&symbol).with_context(|| self.location(start))?;
        self.ws();
        if self.rest().starts_with('{') {
            let bindings = self.bindings()?;
            let args = bindings
                .into_iter()
                .map(|(k, v)| format!("{k}:{}", v.as_str().unwrap()))
                .collect::<Vec<_>>()
                .join(",");
            Ok(format!("{symbol}{{{args}}}"))
        } else {
            Ok(symbol.to_owned())
        }
    }

    fn word(&mut self, close: char) -> Result<String> {
        let mut output = String::new();
        loop {
            self.ws();
            if self.rest().is_empty() {
                return Err(self.error(format!("Unclosed word; expected '{close}'")));
            }
            if close == ';' && self.rest().starts_with(';') {
                break;
            }
            if close != ';' && self.rest().starts_with(',') {
                break;
            }
            if close == '}' && self.rest().starts_with('}') {
                break;
            }

            let token = self.token()?;
            output.push_str(&token);
            output.push(' ');
        }
        Ok(output.trim_end().to_owned())
    }

    fn symbols(&mut self) -> Result<Vec<String>> {
        let close = self.collection()?;
        let mut symbols = Vec::new();
        loop {
            if self.eat(&close.to_string()) {
                break;
            }
            let at = self.at;
            let symbol = self.token()?;
            syntax::single(&symbol).with_context(|| self.location(at))?;
            symbols.push(symbol);
            if self.separator(close)? {
                break;
            }
        }
        Ok(symbols)
    }

    fn clause_at(&self) -> bool {
        ["when", "weight", "specificity", "locals"]
            .iter()
            .any(|key| {
                self.rest().strip_prefix(key).is_some_and(|rest| {
                    rest.chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                })
            })
    }
    fn pattern(&mut self) -> Result<String> {
        self.ws();
        let start = self.at;
        let mut depth = 0i64;
        let mut repetition = false;
        let mut output = String::new();
        loop {
            if self.rest().is_empty() {
                return Err(self.error("Expected '=>' after predecessor"));
            }
            let boundary = self.at == start || self.text[..self.at].ends_with(char::is_whitespace);
            if depth == 0 && (self.rest().starts_with("=>") || (boundary && self.clause_at())) {
                break;
            }
            let ch = self.rest().chars().next().unwrap();
            if depth == 0 && matches!(ch, ',' | ';' | '}') {
                return Err(self.error("Expected a rule clause or '=>'"));
            }
            if ch == '{' {
                // Skip argument blocks as units: comparison operators and lambda
                // arrows inside them do not split context patterns or clauses.
                let end = syntax::block(self.rest()).with_context(|| self.location(self.at))?;
                output.truncate(output.trim_end().len());
                output.push_str(&self.rest()[..end]);
                self.at += end;
                continue;
            }
            let symbol = if repetition
                || ch.is_whitespace()
                || syntax::META.contains(ch)
                || "()[]{}:,\"'".contains(ch)
            {
                self.at += ch.len_utf8();
                ch.to_string()
            } else {
                let at = self.at;
                let symbol = self.symbol()?;
                syntax::single(&symbol).with_context(|| self.location(at))?;
                symbol
            };
            if matches!(symbol.as_str(), "(" | "[") {
                depth += 1;
            }
            if symbol == "[" {
                repetition = true;
            } else if symbol == "]" {
                repetition = false;
            }
            if matches!(symbol.as_str(), ")" | "]") {
                depth -= 1;
                if depth < 0 {
                    return Err(self.error("Unmatched closing delimiter in predecessor"));
                }
            }
            output.push_str(&symbol);
        }
        let pattern = output.trim().to_owned();
        // Validate here to attach the native location before common construction.
        let (left, _, right) = predecessor(&pattern).with_context(|| self.location(start))?;
        for side in [left, right].into_iter().flatten() {
            Pattern::parse(side).with_context(|| self.location(start))?;
        }
        Ok(pattern)
    }

    fn rules(&mut self) -> Result<Vec<RuleDefinition>> {
        let close = self.collection()?;
        let mut rules = Vec::new();
        loop {
            if self.eat(&close.to_string()) {
                break;
            }
            self.ws();
            let start = self.at;
            let name = if self.rest().starts_with(['\'', '"']) {
                let name = self.string()?;
                self.expect(":")?;
                Some(name)
            } else {
                let name = self.ident().ok();
                if name.is_some() && self.eat(":") {
                    name
                } else {
                    self.at = start;
                    None
                }
            };
            let from = self.pattern()?;
            let mut rule = RuleDefinition {
                from,
                name,
                to: String::new(),
                locals: IndexMap::new(),
                cond: None,
                weight: None,
                specificity: None,
            };
            let mut clauses = HashSet::new();
            while !self.eat("=>") {
                let at = self.at;
                let clause = self.ident()?;
                if !clauses.insert(clause.clone()) {
                    return Err(diagnostic(
                        self.original,
                        at,
                        format!("Repeated rule clause {clause}"),
                    ));
                }
                match clause.as_str() {
                    "when" => rule.cond = Some(Value::String(self.expr()?.1)),
                    "locals" => rule.locals = self.bindings()?,
                    "specificity" => rule.specificity = Some(Value::String(self.expr()?.1)),
                    "weight" => rule.weight = Some(Value::String(self.expr()?.1)),
                    _ => {
                        return Err(diagnostic(
                            self.original,
                            at,
                            format!("Unknown rule clause {clause}"),
                        ));
                    }
                }
            }
            rule.to = self.word(close)?;
            rules.push(rule);
            if self.separator(close)? {
                break;
            }
        }
        Ok(rules)
    }

    fn document(mut self) -> Result<Definition> {
        let mut source = Definition {
            native: true,
            ..Definition::default()
        };
        let mut fields = HashSet::new();
        loop {
            self.ws();
            if self.rest().is_empty() {
                break;
            }
            let at = self.at;
            let field = self.ident()?;
            if !fields.insert(field.clone()) {
                return Err(diagnostic(
                    self.original,
                    at,
                    format!("Repeated field {field}"),
                ));
            }
            self.expect(":")?;
            match field.as_str() {
                "version" => {
                    let version = self.integer()?;
                    if version != 3 {
                        return Err(diagnostic(
                            self.original,
                            at,
                            format!("Unsupported native version {version}; supported version is 3"),
                        ));
                    }
                }
                "name" => source.name = Some(Value::String(self.expr()?.1)),
                "description" => source.description = Some(Value::String(self.expr()?.1)),
                "meta" => source.meta = self.metadata()?,
                "recommended_iterations" => {
                    source.recommended_iterations = Some(Value::String(self.expr()?.1));
                }
                "seed" => {
                    source.seed = Some(Value::String(self.expr()?.1));
                }
                "width" | "height" | "bg" | "reclim" => {
                    let value = Some(Value::String(self.expr()?.1));
                    match field.as_str() {
                        "width" => source.width = value,
                        "height" => source.height = value,
                        "bg" => source.bg = value,
                        _ => source.reclim = value,
                    }
                }
                "globals" => source.globals = self.bindings()?,
                "axiom" => source.axiom = self.word(';')?,
                "symbols" => source.symbols = self.symbols()?,
                "growth" => source.rules = self.rules()?,
                "draw" => source.drawing = self.rules()?,
                _ => {
                    return Err(diagnostic(
                        self.original,
                        at,
                        format!("Unknown native field {field}"),
                    ));
                }
            }
            self.expect(";")?;
        }
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dessin::Dessin, syntax::Token, trace::Trace};
    use std::sync::Arc;

    fn load(text: &str) -> Result<Model> {
        LstmLoader::load(text, None, "", None, None)
    }
    fn frames(mut model: Model, n: i64) -> Result<Vec<Arc<[Token]>>> {
        let mut turtle = Dessin::new(model.width, model.height);
        let mut trace = Trace::new(Default::default())?;
        let mut seq = Arc::from([]);
        let mut result = Vec::new();
        for generation in 0..=n {
            seq = model.frame(seq, generation, &mut turtle, &mut trace, false, None)?;
            result.push(seq.clone());
        }
        Ok(result)
    }
    fn word(tokens: &[Token]) -> String {
        tokens.iter().map(|t| t.ch.as_ref()).collect()
    }

    #[test]
    fn complete_example_grows_and_erases_its_bud() -> Result<()> {
        let model = load(include_str!("../../tests/fixtures/counting_sprig.lstm"))?;
        assert_eq!((model.width, model.height, model.rec_lim), (1200, 780, 8));
        assert!(model.description.contains('\n'));
        let all = frames(model, 5)?;
        assert_eq!(word(&all[1]), "IA");
        assert_eq!(word(&all[2]), "IBBIA");
        assert_eq!(all[2][1].args["turn"], V::Int(-33));
        assert_eq!(all[2][2].args["turn"], V::Int(33));
        assert_eq!(word(&all[5]), "IBBIBBIBBI");
        Ok(())
    }

    #[test]
    fn native_and_json5_have_identical_semantics_and_overrides() -> Result<()> {
        let native = r##"
            width: step*20; height: 40; bg: #fff9eb;
            globals: {unit: 2, step: unit*3}; seed: 'same';
            symbols: {A {n:2}}; axiom: A;
            growth: {A{n:_} when n>0 locals {next:n-1} => ${['B']}A{n:next}, A{n:0} =>};
            draw: {B => F{length:step}, A =>};
        "##;
        let json5 = r##"{width:'step*20',height:40,bg:'#fff9eb',globals:{unit:2,step:'unit*3'},seed:'same',
            symbols:['A{n:2}'],axiom:'A',rules:[{from:'A{n:_}',cond:'n>0',locals:{next:'n-1'},to:"${['B']}A{n:next}"},{from:'A{n:0}'}],
            drawing:[{from:'B',to:'F{length:step}'},{from:'A'}]}"##;
        let left = LstmLoader::load(
            native,
            Some("A{n:3}"),
            "unit:4",
            Some("override"),
            Some(100),
        )?;
        let right = crate::loaders::json5::Json5Loader::load(
            json5,
            Some("A{n:3}"),
            "unit:4",
            Some("override"),
            Some(100),
        )?;
        assert_eq!((left.width, left.height), (right.width, right.height));
        assert_eq!(left.width, 240);
        for (a, b) in frames(left, 4)?.iter().zip(frames(right, 4)?) {
            assert_eq!(
                a.iter().map(Token::value).collect::<Vec<_>>(),
                b.iter().map(Token::value).collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn comments_strings_and_floor_division_have_distinct_boundaries() -> Result<()> {
        let text = "\u{feff}// file header\n/* leading block */\nname: 'a\n// literal string line\nb';\nmeta: {author: \"${not_a_splice}\", 'long key': 'ok'};\nglobals: {x: 12 //\n  3, add: fn a,b -> a+b, data: {'k':[1,2]}};\n// comment\naxiom: A {n:add(x,2)} ${['B', {'symbol':'C','args':{'text':'};,'}}]};\n";
        let model = load(text)?;
        assert_eq!(model.name, "a\n// literal string line\nb");
        assert_eq!(model.meta["author"], "${not_a_splice}");
        let all = frames(model, 0)?;
        assert_eq!(word(&all[0]), "ABC");
        assert_eq!(all[0][0].args["n"], V::Int(6));
        assert_eq!(all[0][2].args["text"], V::String("};,".into()));
        Ok(())
    }

    #[test]
    fn context_clauses_repetitions_and_predicates_are_not_split() -> Result<()> {
        let model = load(
            r#"
            axiom: BBA{x:3}Bwhen;
            growth: {
                named: B[2,4] < A {x:fn v -> v<5} > B(when)
                    locals {value:x+1} specificity -1 weight 0.5 when x<5 => Z{v:value},
            };
        "#,
        )?;
        let all = frames(model, 1)?;
        assert_eq!(word(&all[1]), "BBZBwhen");
        assert_eq!(all[1][2].args["v"], V::Int(4));
        let model = load("axiom: AB; growth: { A > B when true => Z }; ")?;
        assert_eq!(word(&frames(model, 1)?[1]), "ZB");
        Ok(())
    }

    #[test]
    fn brace_blocks_do_not_confuse_branch_tokens_or_empty_successors() -> Result<()> {
        for (rule, expected) in [
            ("A => [BB]", "[BB]"),
            ("A => ],", "]"),
            ("A =>", ""),
            ("A => ${[]}Z", "Z"),
        ] {
            let text = format!("axiom: A; growth: {{ {rule} }}\n// before semicolon\n;");
            assert_eq!(word(&frames(load(&text)?, 1)?[1]), expected);
        }
        for text in [
            "symbols: [A];",
            "growth: [A => B];",
            "draw: [];",
            "globals: [];",
            "meta: [];",
        ] {
            assert!(load(text).is_err(), "{text}");
        }
        Ok(())
    }

    #[test]
    fn malformed_fields_clauses_and_delimiters_are_diagnosed() {
        for (text, fragment) in [
            ("version:2;", "Unsupported native version"),
            ("version:4;", "Unsupported native version"),
            ("version:3.0;", "integer literal"),
            ("name:'a'; name:'b';", "Repeated field name"),
            ("rules:{};", "Unknown native field rules"),
            (
                "draw:{A when true when false =>};",
                "Repeated rule clause when",
            ),
            ("axiom: ${'F';", "Expected"),
            ("axiom: A", "Unclosed word"),
        ] {
            let error = load(text)
                .err()
                .unwrap_or_else(|| panic!("accepted {text}"));
            let message = format!("{error:#}");
            assert!(message.contains(fragment), "{text}: {message}");
            assert!(message.contains("Native syntax at"), "{message}");
        }
        let error = load("// header\nname:'é';\nunknown: 1;").err().unwrap();
        assert!(format!("{error:#}").contains("3:1"));
    }

    #[test]
    fn native_settings_are_strict_and_defaults_match_json5() -> Result<()> {
        let model = load("")?;
        assert_eq!(
            (
                model.width,
                model.height,
                model.bg,
                model.iterations,
                model.rec_lim
            ),
            (1000, 800, 0xffffff, 5, 128)
        );
        for text in [
            "width:0;",
            "height:-1;",
            "width:1.5;",
            "reclim:-1;",
            "width:true;",
            "name: 42;",
            "description: null;",
            "meta: {author:42};",
            "recommended_iterations:-1;",
            "recommended_iterations:1.5;",
            "draw:{A specificity 1.5 =>};",
            "seed: 1.5;",
            "seed: true;",
        ] {
            assert!(load(text).is_err(), "{text}");
        }
        assert_eq!(load("reclim:0;")?.rec_lim, 0);
        for text in ["bg:-1;", "bg:#ffffffff;", "bg:true;"] {
            let error = frames(load(text)?, 0).unwrap_err();
            assert!(format!("{error:#}").contains("field bg, generation 0"));
        }
        assert!(load("{axiom:'A'}").is_err());
        Ok(())
    }

    #[test]
    fn unicode_and_reserved_symbols_apply_to_literals_and_splices() -> Result<()> {
        let model = load("symbols: {é{x:2}}; axiom: ${['é']}👩‍🔬;")?;
        let seq = &frames(model, 0)?[0];
        assert_eq!(seq[0].ch.as_ref(), "é");
        assert_eq!(seq[0].args["x"], V::Int(2));
        for c in [",", ";", "=", ":", " "] {
            for expr in [format!("'{c}'"), format!("{{'symbol':'{c}'}}")] {
                let model = load(&format!("axiom: ${{{expr}}};"))?;
                assert!(frames(model, 0).is_err(), "{expr}");
            }
            assert!(syntax::single(c).is_err(), "{c}");
        }
        Ok(())
    }

    #[test]
    fn symbol_escapes_are_decoded_before_graphemes_and_normalization() -> Result<()> {
        let model = load(
            r#"
            symbols: {e\u0301{x:2}};
            axiom: \u0065́{x:5} e\u{301} E\u0301 \u{1F469}\u200D🔬 🇨\u{1f1e6};
        "#,
        )?;
        let seq = &frames(model, 0)?[0];
        assert_eq!(
            seq.iter().map(|t| t.ch.as_ref()).collect::<Vec<_>>(),
            ["é", "é", "É", "👩‍🔬", "🇨🇦"]
        );
        assert_eq!(seq[0].args["x"], V::Int(5));
        assert_eq!(seq[1].args["x"], V::Int(2));
        Ok(())
    }

    #[test]
    fn escaped_symbols_match_in_growth_drawing_and_contexts() -> Result<()> {
        let model = load(
            r#"
            axiom: ééA{x:3}👩‍🔬;
            growth: {
                (e\u0301)[2] < \u0041{x:fn v -> v<5} > \u{1f469}\u200d🔬
                    when x<5 => \u005a{x:x+1},
            };
            draw: {\u005a => \u0046{length:x}};
        "#,
        )?;
        let all = frames(model, 1)?;
        assert_eq!(word(&all[1]), "ééZ👩‍🔬");
        assert_eq!(all[1][2].args["x"], V::Int(4));
        Ok(())
    }

    #[test]
    fn symbol_decoding_leaves_formula_escapes_and_strings_alone() -> Result<()> {
        let model = load(
            r#"
            name: "\u0041";
            symbols: {e\u0301{text:"\\u0041"}};
            axiom: e\u0301 A{text:"\u007d\u0022"} ${[{"symbol":"B","args":{"text":"\\u0041"}}]};
            growth: {e\u0301{text:"\\u0041"} when text=="\\u0041"
                locals {value:"\\u0042"} => é{text:value}};
        "#,
        )?;
        assert_eq!(model.name, "A");
        let all = frames(model, 1)?;
        assert_eq!(word(&all[0]), "éAB");
        assert_eq!(all[0][0].args["text"], V::String(r"\u0041".into()));
        assert_eq!(all[0][1].args["text"], V::String("}\"".into()));
        assert_eq!(all[0][2].args["text"], V::String(r"\u0041".into()));
        assert_eq!(all[1][0].args["text"], V::String(r"\u0042".into()));
        Ok(())
    }

    #[test]
    fn symbol_escapes_do_not_redecode_backslashes_or_change_json5() -> Result<()> {
        assert_eq!(
            word(&frames(load(r"axiom: \u005cu0041; ")?, 0)?[0]),
            r"\u0041"
        );
        assert_eq!(word(&frames(load(r"axiom: \n; ")?, 0)?[0]), r"\n");
        let json =
            crate::loaders::json5::Json5Loader::load(r#"{axiom:"\\u0041"}"#, None, "", None, None)?;
        assert_eq!(word(&frames(json, 0)?[0]), r"\u0041");
        Ok(())
    }

    #[test]
    fn invalid_symbol_escapes_report_original_source_locations() {
        for escape in [
            r"\u",
            r"\u12",
            r"\uZZZZ",
            r"\ué123",
            r"\u{}",
            r"\u{123;",
            r"\u{1234567}",
            r"\u{xyz}",
            r"\uD800",
            r"\uDC00",
            r"\u{110000}",
        ] {
            for source in [
                format!("axiom: é{escape};"),
                format!("symbols: {{{escape}}};"),
                format!("growth: {{{escape} => A}};"),
                format!("draw: {{A > {escape} => B}};"),
            ] {
                let error = format!(
                    "{:#}",
                    load(&source)
                        .err()
                        .unwrap_or_else(|| panic!("accepted {source}"))
                );
                assert!(error.contains("Unicode symbol escape"), "{source}: {error}");
                let column = source
                    .find('\\')
                    .map(|i| source[..i].chars().count() + 1)
                    .unwrap();
                assert!(error.contains(&format!("1:{column}")), "{source}: {error}");
            }
        }
        for escape in [
            r"\u007b", r"\u007d", r"\u0022", r"\u003b", r"\u0020", r"\u003c",
        ] {
            assert!(load(&format!("axiom: {escape};")).is_err(), "{escape}");
            assert!(
                load(&format!("growth: {{A > {escape} => B}};")).is_err(),
                "{escape}"
            );
        }
    }

    #[test]
    fn startup_properties_use_globals_and_are_evaluated_once() -> Result<()> {
        let model = load(
            r#"
            seed: 20+22;
            globals: {step: let _=mark('globals') for 8};
            recommended_iterations: let _=mark('iterations') for step//2;
            width: let _=mark('width') for step*2;
            height: let _=mark('height') for step;
            reclim: let _=mark('reclim') for step+1;
            name: let _=mark('name') for 'Model '+str(mxgen());
            description: let _=mark('description') for str(step);
            meta: {author: let _=mark('meta') for 'author '+str(step)};
            axiom: A{calls:marked()};
            growth: {named: A specificity (let _=mark('specificity') for step) => A{calls:marked()}};
        "#,
        )?;
        assert_eq!(
            (model.width, model.height, model.rec_lim, model.iterations),
            (16, 8, 9, 4)
        );
        assert_eq!(model.name, "Model 4");
        assert_eq!(model.description, "8");
        assert_eq!(model.meta["author"], "author 8");
        let all = frames(model, 1)?;
        let expected = [
            "globals",
            "iterations",
            "width",
            "height",
            "reclim",
            "name",
            "description",
            "meta",
            "specificity",
        ]
        .map(|s| V::String(s.into()))
        .to_vec();
        assert_eq!(all[0][0].args["calls"], V::Array(expected.into()));
        assert_eq!(all[1][0].args["calls"], V::Array(vec![].into()));
        assert!(load(r#"growth: {"name"+"suffix": A => B};"#).is_err());
        Ok(())
    }

    #[test]
    fn seed_formulas_run_before_globals_and_cli_seed_bypasses_them() -> Result<()> {
        for (expression, literal) in [("6*7", "42"), ("'spr'+'ing'", "'spring'")] {
            let source = "globals: {sample:random()}; axiom: A{x:sample};";
            let a = frames(load(&format!("seed:{expression}; {source}"))?, 0)?;
            let b = frames(load(&format!("seed:{literal}; {source}"))?, 0)?;
            assert_eq!(a[0][0].value(), b[0][0].value());
        }
        let error = load("seed:base; globals:{base:42};").err().unwrap();
        assert!(format!("{error:#}").contains("field seed"));
        for seed in ["gen()", "mxgen()", "screen()"] {
            assert!(load(&format!("seed:{seed};")).is_err());
        }
        let a = LstmLoader::load(
            "seed:error('unused'); globals:{x:random()}; axiom:A{x:x};",
            None,
            "",
            Some("42"),
            None,
        )?;
        let b = load("seed:42; globals:{x:random()}; axiom:A{x:x};")?;
        assert_eq!(frames(a, 0)?[0][0].value(), frames(b, 0)?[0][0].value());
        Ok(())
    }

    #[test]
    fn mxgen_uses_recommendations_overrides_and_endless_mode() -> Result<()> {
        let text = "recommended_iterations:3; globals:{target:mxgen()}; name:str(mxgen()); axiom:A{target:target};";
        for (override_n, expected) in [(None, V::Int(3)), (Some(7), V::Int(7)), (Some(-1), V::Null)]
        {
            let model =
                crate::loaders::load_with_iterations(text, None, "", None, None, override_n)?;
            let all = frames(model, 0)?;
            assert_eq!(all[0][0].args["target"], expected);
        }
        let model =
            load("globals:{step:3}; recommended_iterations:step*2; axiom:A{target:mxgen()};")?;
        assert_eq!(frames(model, 0)?[0][0].args["target"], V::Int(6));
        let source = "globals:{target:mxgen()}; recommended_iterations:target+1;";
        let error = load(source).err().unwrap();
        assert!(format!("{error:#}").contains("unavailable until recommended_iterations"));
        assert!(
            crate::loaders::load_with_iterations(source, None, "", None, None, Some(4)).is_ok()
        );
        let model = load("axiom:A{target:mxgen()};")?;
        assert_eq!(frames(model, 0)?[0][0].args["target"], V::Int(5));
        let model = crate::loaders::json5::Json5Loader::load(
            r#"{recommended_iterations:-1,globals:{target:"mxgen()"},axiom:"A{target:target}"}"#,
            None,
            "",
            None,
            None,
        )?;
        assert_eq!(frames(model, 0)?[0][0].args["target"], V::Null);
        Ok(())
    }

    #[test]
    fn dynamic_weights_run_once_per_match_before_selection_without_locals() -> Result<()> {
        let model = load(
            r#"
            globals:{factor:2}; axiom:A{x:0}A{x:1};
            growth:{
                A{x:99} weight error('constraint failed') => X,
                A when false weight error('guard failed') => X,
                A specificity 100 weight 0 => X,
                A weight (let _=mark('first') for x*factor*gen()) => B,
                A weight (let _=mark('second') for (1-x)*factor) => C,
                B => B{calls:marked()}, C => C{calls:marked()},
            };
        "#,
        )?;
        let all = frames(model, 2)?;
        assert_eq!(word(&all[1]), "CB");
        let expected = ["first", "second", "first", "second"]
            .map(|s| V::String(s.into()))
            .to_vec();
        assert_eq!(all[2][0].args["calls"], V::Array(expected.into()));
        let model = load("axiom:A; growth:{A weight local locals {local:1} => B};")?;
        let error = frames(model, 1).unwrap_err();
        assert!(format!("{error:#}").contains("weight local"));
        Ok(())
    }

    #[test]
    fn drawing_weights_and_background_observe_live_state_each_frame() -> Result<()> {
        let mut model = load(
            r#"
            width:16; height:8; globals:{base:256};
            axiom:A{x:2}A{x:5};
            draw:{
                A weight (let _=mark('weight') for 1 if pos()[0]<x else 0) => P{x:x,y:0}H{rot:30},
                A weight 0 => P{x:99,y:99},
            };
            bg: let _=mark('bg') for base+int(pos()[0])+int(heading())+screen()[0]+gen();
        "#,
        )?;
        let mut d = Dessin::new(24, 12);
        let mut trace = Trace::new(Default::default())?;
        let mut seq = Arc::from([]);
        for generation in 0..=2 {
            d.capture = generation == 2;
            seq = model.frame(seq, generation, &mut d, &mut trace, false, None)?;
            assert_eq!(d.background, 256 + 5 + 30 + 24 + generation as u32);
            assert_eq!(d.pose().p, (5., 0.));
            assert_eq!(
                model.engine.evaluate("marked()", &formulas::Env::new())?,
                if generation == 0 {
                    V::Array(vec![].into())
                } else {
                    V::Array(
                        vec![
                            V::String("weight".into()),
                            V::String("weight".into()),
                            V::String("bg".into()),
                        ]
                        .into(),
                    )
                }
            );
        }
        Ok(())
    }

    #[test]
    fn invalid_dynamic_weights_and_backgrounds_have_runtime_context() -> Result<()> {
        for expr in ["-1", "true", "'bad'", "[1]", "float('inf')"] {
            for (phase, source, generation) in [
                (
                    "growth",
                    format!("axiom:A; growth:{{named:A weight {expr} => B}};"),
                    1,
                ),
                (
                    "drawing",
                    format!("axiom:A; draw:{{named:A weight {expr} => B}};"),
                    0,
                ),
            ] {
                let error = frames(load(&source)?, generation).unwrap_err();
                let message = format!("{error:#}");
                assert!(
                    message.contains(&format!("{phase} rule named")),
                    "{message}"
                );
                assert!(message.contains("weight"), "{message}");
            }
        }
        let model = load("bg: 0 if gen()==0 else -1;")?;
        let error = frames(model, 1).unwrap_err();
        assert!(format!("{error:#}").contains("field bg, generation 1"));
        let model =
            load("axiom:A; growth:{A weight float('1e308')=>B, A weight float('1e308')=>C};")?;
        assert!(frames(model, 1).is_err());
        Ok(())
    }
}
