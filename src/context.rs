//! Branch-aware neighborhoods and pattern matching for rule predecessors.
//! Trees retain indexes into the input generation; drawing instructions are
//! skipped, while square brackets determine which symbols share a branch.

use crate::err::{Result, SyntaxError, ensure};
use crate::syntax::*;
use formulas::EvalValue as V;
use std::collections::BTreeSet;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
struct Node {
    // Structural nodes have no token: they represent roots, forks, or empty paths.
    token: Option<usize>,
    children: Vec<usize>,
}
/// An arena of neighborhood nodes; child indexes remain stable as it grows.
pub struct Tree {
    nodes: Vec<Node>,
}
impl Tree {
    pub fn build(seq: &[Token], at: usize, left: bool) -> Self {
        let mut tree = Self {
            nodes: vec![Node::default()],
        };
        let mut cur = 0;
        if left {
            // Walk back along the ancestry, skipping completed side branches.
            // Reverse the collected indexes to restore source order for matching.
            let mut stack = 0;
            let mut chain = Vec::new();
            for i in (0..at).rev() {
                match seq[i].ch.as_ref() {
                    "]" => stack += 1,
                    "[" => stack = (stack - 1).max(0),
                    c if stack == 0 && !DRAW.contains(c) => chain.push(i),
                    _ => (),
                }
            }
            for i in chain.into_iter().rev() {
                cur = tree.add(cur, Some(i));
            }
        } else {
            // Right context fans out into branches. An unmatched closing bracket
            // ends this neighborhood instead of continuing into the parent's tail.
            let mut stack = Vec::new();
            for (i, t) in seq.iter().enumerate().skip(at + 1) {
                match t.ch.as_ref() {
                    "[" => {
                        stack.push(cur);
                        cur = tree.add(cur, None)
                    }
                    "]" => {
                        // Preserve an empty continuation when a segment ends at a fork.
                        if !tree.nodes[cur].children.is_empty() {
                            tree.add(cur, None);
                        }
                        if let Some(p) = stack.pop() {
                            cur = p
                        } else {
                            return tree;
                        }
                    }
                    c if !DRAW.contains(c) => cur = tree.add(cur, Some(i)),
                    _ => (),
                }
            }
            if !tree.nodes[cur].children.is_empty() {
                tree.add(cur, None);
            }
        }
        tree
    }
    fn add(&mut self, p: usize, token: Option<usize>) -> usize {
        let i = self.nodes.len();
        self.nodes.push(Node {
            token,
            children: Vec::new(),
        });
        self.nodes[p].children.push(i);
        i
    }
    fn visit_paths(&self, mut visit: impl FnMut(&[usize]) -> Result<bool>) -> Result<bool> {
        // Each pending node remembers its prefix length, letting one path buffer
        // serve the depth-first traversal. A successful visitor stops the search.
        let mut path = Vec::new();
        let mut pending = vec![(0, 0)];
        while let Some((i, n)) = pending.pop() {
            path.truncate(n);
            let node = &self.nodes[i];
            if let Some(t) = node.token {
                path.push(t)
            }
            if node.children.is_empty() {
                if visit(&path)? {
                    return Ok(true);
                }
            } else {
                pending.extend(node.children.iter().rev().map(|&c| (c, path.len())))
            }
        }
        Ok(false)
    }
    /// Returns the nested neighborhood, or tokens at a requested path depth.
    /// Nonnegative depths count from the root; negative depths count from each leaf.
    pub fn value(&self, seq: &[Token], depth: Option<i64>) -> V {
        let mut out = Vec::new();
        if let Some(d) = depth {
            if d < 0 {
                self.visit_paths(|path| {
                    if let Some(i) = path.len().checked_sub(d.unsigned_abs() as usize) {
                        out.push(seq[path[i]].value())
                    }
                    Ok(false)
                })
                .unwrap();
            } else {
                let mut pending = vec![(0, d)];
                while let Some((i, n)) = pending.pop() {
                    let node = &self.nodes[i];
                    let mut rem = n;
                    if let Some(t) = node.token {
                        if n == 0 {
                            out.push(seq[t].value());
                            continue;
                        }
                        rem -= 1
                    }
                    pending.extend(node.children.iter().rev().map(|&c| (c, rem)));
                }
            }
        } else {
            // Build each linear segment once, recursing only at a fork.
            fn segment(tree: &Tree, seq: &[Token], mut i: usize) -> V {
                let mut values = Vec::new();
                loop {
                    let n = &tree.nodes[i];
                    if let Some(t) = n.token {
                        values.push(seq[t].value())
                    }
                    match n.children.as_slice() {
                        [] => break,
                        [child] => i = *child,
                        children => {
                            values.push(V::Array(
                                children
                                    .iter()
                                    .map(|&c| segment(tree, seq, c))
                                    .collect::<Vec<_>>()
                                    .into(),
                            ));
                            break;
                        }
                    }
                }
                V::Array(values.into())
            }
            return segment(self, seq, 0);
        }
        V::Array(out.into())
    }
}

#[derive(Clone)]
/// A small regular-expression grammar over symbols and argument constraints.
pub enum Pattern {
    Atom(Template),
    Anchor(Arc<str>),
    Seq(Vec<Self>),
    Alt(Vec<Self>),
    Repeat(Box<Self>, usize, Option<usize>),
}
// A pattern may end at several offsets. Keeping all of them avoids greedy choices
// that would prevent a later sequence component from matching.
type Positions = BTreeSet<usize>;
impl Pattern {
    pub fn parse(s: &str) -> Result<Self> {
        let mut p = Parser(s);
        let ast = p.alt(0)?;
        ensure!(
            p.peek().is_none(),
            SyntaxError::UnexpectedContextSuffix { suffix: p.0.into() }
        );
        Ok(ast)
    }
    pub fn matches(
        &self,
        tree: &Tree,
        seq: &[Token],
        left: bool,
        mut check: impl FnMut(&Template, &Token) -> Result<bool>,
    ) -> Result<bool> {
        let mut cache = std::collections::HashMap::new();
        tree.visit_paths(|path| {
            // Left context must end next to the predecessor; right context must
            // start there. The opposite endpoint is free unless explicitly anchored.
            let start = if left {
                (0..=path.len()).collect()
            } else {
                Positions::from([0])
            };
            let ends = self.advance(path.len(), left, start, &mut |atom, pos| {
                let idx = path[pos];
                let key = (atom as *const Template as usize, idx);
                // Shared path prefixes must not re-evaluate the same predicate:
                // formulas can consume random values or record marks.
                if let Some(&v) = cache.get(&key) {
                    return Ok(v);
                }
                let t = &seq[idx];
                let v = (atom.ch.as_ref() == "." || t.ch == atom.ch) && check(atom, t)?;
                cache.insert(key, v);
                Ok(v)
            })?;
            Ok(if left {
                ends.contains(&path.len())
            } else {
                !ends.is_empty()
            })
        })
    }
    fn advance(
        &self,
        n: usize,
        left: bool,
        start: Positions,
        test: &mut impl FnMut(&Template, usize) -> Result<bool>,
    ) -> Result<Positions> {
        let mut result = Positions::new();
        match self {
            Self::Atom(t) => {
                for pos in start {
                    if pos < n && test(t, pos)? {
                        result.insert(pos + 1);
                    }
                }
            }
            Self::Anchor(c) => {
                for p in start {
                    if (left && c.as_ref() == "^" && p == 0) || (!left && c.as_ref() == "$" && p == n) {
                        result.insert(p);
                    }
                }
            }
            Self::Seq(parts) => {
                result = start;
                for p in parts {
                    if result.is_empty() {
                        break;
                    }
                    result = p.advance(n, left, result, test)?
                }
            }
            Self::Alt(parts) => {
                for p in parts {
                    result.extend(p.advance(n, left, start.clone(), test)?)
                }
            }
            Self::Repeat(p, min, max) => {
                let mut front = start;
                for _ in 0..*min {
                    front = p.advance(n, left, front, test)?;
                    if front.is_empty() {
                        return Ok(front);
                    }
                }
                result = front.clone();
                // Stop once repetitions reach no new offsets, including when the
                // repeated pattern can match without consuming a token.
                for _ in 0..max.map_or(n.saturating_add(1), |m| m - min) {
                    front = p
                        .advance(n, left, front, test)?
                        .difference(&result)
                        .copied()
                        .collect();
                    if front.is_empty() {
                        break;
                    }
                    result.extend(front.iter().copied());
                }
            }
        }
        Ok(result)
    }
}
struct Parser<'a>(&'a str);
impl Parser<'_> {
    fn peek(&mut self) -> Option<&str> {
        self.0 = self.0.trim_start();
        self.0.graphemes(true).next()
    }
    fn pop(&mut self) -> Option<Arc<str>> {
        let c: Arc<str> = self.peek()?.into();
        self.0 = &self.0[c.len()..];
        Some(c)
    }
    fn int(&mut self) -> Result<usize> {
        self.peek();
        let n = self.0.bytes().take_while(u8::is_ascii_digit).count();
        let v = self.0[..n]
            .parse()
            .map_err(|source| SyntaxError::InvalidRepetitionCount {
                input: self.0[..n].into(),
                source,
            })?;
        self.0 = &self.0[n..];
        Ok(v)
    }
    fn alt(&mut self, depth: usize) -> Result<Pattern> {
        ensure!(
            depth <= 100,
            SyntaxError::ContextNestingLimit { limit: 100 }
        );
        let mut branches = Vec::new();
        loop {
            let mut parts = Vec::new();
            while let Some(c) = self.peek() {
                if c == "|" || c == ")" {
                    break;
                }
                let c: Arc<str> = Arc::from(c);
                let mut p = match c.as_ref() {
                    "(" => {
                        self.pop();
                        let p = self.alt(depth + 1)?;
                        ensure!(
                            self.pop() == Some(")".into()),
                            SyntaxError::MissingClosingParenthesis
                        );
                        p
                    }
                    "^" | "$" => {
                        self.pop();
                        Pattern::Anchor(c.into())
                    }
                    _ => {
                        ensure!(
                            c.as_ref() == "."
                                || (!META.contains(c.as_ref())
                                    && !DRAW.contains(c.as_ref())
                                    && !"[]{}:,\"'".contains(c.as_ref())),
                            SyntaxError::InvalidContextAtom { symbol: c.into() }
                        );
                        let (t, rest) = atom(self.0)?;
                        self.0 = rest;
                        Pattern::Atom(t)
                    }
                };
                let peek = self.peek().map(|s| Arc::<str>::from(s));
                if let Some(q @ ("*" | "+" | "?" | "[")) = peek.as_deref() {
                    ensure!(
                        !matches!(p, Pattern::Anchor(_)),
                        SyntaxError::RepeatedAnchor
                    );
                    self.pop();
                    let (lo, hi) = match q {
                        "*" => (0, None),
                        "+" => (1, None),
                        "?" => (0, Some(1)),
                        _ => {
                            let lo = self.int()?;
                            let hi = if self.peek() == Some(",") {
                                self.pop();
                                self.int()?
                            } else {
                                lo
                            };
                            ensure!(
                                lo <= hi && hi <= 10000 && self.pop() == Some("]".into()),
                                SyntaxError::InvalidRepetitionBounds
                            );
                            (lo, Some(hi))
                        }
                    };
                    p = Pattern::Repeat(Box::new(p), lo, hi);
                }
                parts.push(p)
            }
            branches.push(Pattern::Seq(parts));
            if self.peek() != Some("|") {
                break;
            }
            self.pop();
        }
        Ok(Pattern::Alt(branches))
    }
}
/// Splits `left < symbol > right`, ignoring comparison operators inside arguments.
pub fn predecessor(s: &str) -> Result<(Option<&str>, Template, Option<&str>)> {
    let mut rest = s;
    let mut start = 0;
    let mut end = s.len();
    let mut left = None;
    let mut right = None;
    while let Some(c) = rest.graphemes(true).next() {
        let i = s.len() - rest.len();
        match c {
            "{" => {
                rest = &rest[block(rest)?..];
                continue;
            }
            "<" if left.is_none() && right.is_none() => {
                left = Some(&s[..i]);
                start = i + 1
            }
            ">" if right.is_none() => {
                end = i;
                right = Some(&s[i + 1..])
            }
            "<" | ">" => return Err(SyntaxError::InvalidPredecessor { input: s.into() }.into()),
            _ => (),
        }
        rest = &rest[c.len()..];
    }
    Ok((
        left,
        single(
            s.get(start..end)
                .ok_or_else(|| SyntaxError::InvalidPredecessor { input: s.into() })?,
        )?,
        right,
    ))
}
