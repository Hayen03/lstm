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
        build(source, override_axiom, globals, user_seed, freclim)
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
    fn metadata(&mut self) -> Result<IndexMap<String, String>> {
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
            let value = self.string()?;
            if values.insert(key.clone(), value).is_some() {
                return Err(self.error(format!("Repeated metadata key {key}")));
            }
            if self.separator('}')? {
                break;
            }
        }
        Ok(values)
    }

    /// Normalize only the host whitespace between a symbol and its arguments.
    /// Formula strings and whitespace within their expressions are preserved.
    fn token(&mut self) -> Result<String> {
        self.ws();
        let start = self.at;
        if self.eat("${") {
            let (_, raw) = self.expr()?;
            self.expect("}")?;
            return Ok(format!("${{{raw}}}"));
        }
        let symbol = self
            .rest()
            .graphemes(true)
            .next()
            .ok_or_else(|| self.error("Expected a symbol"))?
            .to_owned();
        syntax::single(&symbol).with_context(|| self.location(start))?;
        self.at += symbol.len();
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
            Ok(symbol)
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
            if matches!(ch, '(' | '[') {
                depth += 1;
            }
            if matches!(ch, ')' | ']') {
                depth -= 1;
                if depth < 0 {
                    return Err(self.error("Unmatched closing delimiter in predecessor"));
                }
            }
            output.push(ch);
            self.at += ch.len_utf8();
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
                    "specificity" => rule.specificity = Some(self.integer()?),
                    "weight" => {
                        let (_, raw) = self.expr()?;
                        let weight: f64 = raw
                            .parse()
                            .map_err(|_| self.error("Weight must be a numeric literal"))?;
                        if !weight.is_finite() || weight < 0. {
                            return Err(self.error("Weight must be finite and nonnegative"));
                        }
                        rule.weight = Some(weight);
                    }
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
                "name" => source.name = self.string()?,
                "description" => source.description = self.string()?,
                "meta" => source.meta = self.metadata()?,
                "recommended_iterations" => {
                    let n = self.integer()?;
                    if n < 0 {
                        return Err(self.error("recommended_iterations must be nonnegative"));
                    }
                    source.recommended_iterations = Some(n);
                }
                "seed" => {
                    self.ws();
                    source.seed = Some(if self.rest().starts_with(['\'', '"']) {
                        Value::String(self.string()?)
                    } else {
                        Value::from(self.integer()?)
                    });
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
        assert_eq!(
            (left.width, left.height, left.bg),
            (right.width, right.height, right.bg)
        );
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
            ("draw:{A weight -1 =>};", "finite and nonnegative"),
            ("draw:{A weight 1+2 =>};", "numeric literal"),
            ("draw:{A specificity 1.5 =>};", "integer literal"),
            ("axiom: ${'F';", "Expected"),
            ("axiom: A", "Unclosed word"),
            ("name: 42;", "string literal"),
            ("meta:{author:42};", "string literal"),
            ("recommended_iterations:-1;", "nonnegative"),
            ("seed: random();", "integer literal"),
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
            "bg:-1;",
            "bg:#ffffffff;",
            "reclim:-1;",
            "width:true;",
        ] {
            assert!(load(text).is_err(), "{text}");
        }
        assert_eq!(load("reclim:0;")?.rec_lim, 0);
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
}
