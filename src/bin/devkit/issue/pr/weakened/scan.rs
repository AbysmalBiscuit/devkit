//! What one test file holds, read from its text: test declarations,
//! assertions, skip markers, focus markers and tautological assertions.
//! Comments and string contents are masked out first, so a commented-out
//! test does not count and a test's fixture text is not read as code.

use std::collections::BTreeMap;

/// The languages whose test files the check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Rust,
    Js,
    Python,
}

impl Lang {
    /// The language of `path`, by extension.
    pub(crate) fn of(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.')?.1;
        match ext {
            "rs" => Some(Self::Rust),
            "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => Some(Self::Js),
            "py" => Some(Self::Python),
            _ => None,
        }
    }
}

/// The tallies the rules compare between the merge base and the head.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub tests: usize,
    pub assertions: usize,
    pub skips: usize,
    pub focused: usize,
    /// Each tautological assertion, whitespace removed, with how often it
    /// appears.
    pub tautologies: BTreeMap<String, usize>,
}

pub(crate) fn counts(lang: Lang, text: &str) -> Counts {
    let masked = mask(lang, text);
    let src = Source {
        text,
        masked: &masked,
    };
    let mut c = Counts::default();
    match lang {
        Lang::Rust => rust(&src, &mut c),
        Lang::Js => js(&src, &mut c),
        Lang::Python => python(&src, &mut c),
    }
    c
}

/// The original text beside its masked copy. Both have the same length and
/// the same byte offsets, so a span found in one reads the same span of the
/// other.
struct Source<'a> {
    text: &'a str,
    masked: &'a str,
}

impl Source<'_> {
    /// Every identifier in the masked text, with its byte offset.
    fn idents(&self) -> impl Iterator<Item = (usize, &str)> {
        let m = self.masked;
        let bytes = m.as_bytes();
        let mut i = 0;
        std::iter::from_fn(move || {
            while i < bytes.len() {
                if is_ident(bytes[i]) && (i == 0 || !is_ident(bytes[i - 1])) {
                    let start = i;
                    while i < bytes.len() && is_ident(bytes[i]) {
                        i += 1;
                    }
                    return Some((start, &m[start..i]));
                }
                i += 1;
            }
            None
        })
    }

    /// The offset of the next non-whitespace byte at or after `i`.
    fn skip_ws(&self, mut i: usize) -> usize {
        let b = self.masked.as_bytes();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    }

    fn byte(&self, i: usize) -> Option<u8> {
        self.masked.as_bytes().get(i).copied()
    }

    /// The byte before `i` that is not whitespace.
    fn prev(&self, i: usize) -> Option<u8> {
        self.masked.as_bytes()[..i]
            .iter()
            .rev()
            .find(|b| !b.is_ascii_whitespace())
            .copied()
    }

    /// The arguments of the call whose `(` (or `[`, `{`) is at `open`, as
    /// whitespace-free original text, and the offset just past the closing
    /// bracket.
    fn args(&self, open: usize) -> Option<(Vec<String>, usize)> {
        let b = self.masked.as_bytes();
        let mut depth = 0usize;
        let mut start = open + 1;
        let mut args = Vec::new();
        for (i, &c) in b.iter().enumerate().skip(open) {
            match c {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let last = squash(&self.text[start..i]);
                        if !last.is_empty() || !args.is_empty() {
                            args.push(last);
                        }
                        return Some((args, i + 1));
                    }
                }
                b',' if depth == 1 => {
                    args.push(squash(&self.text[start..i]));
                    start = i + 1;
                }
                _ => {}
            }
        }
        None
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect()
}

/// `a == b` with both sides the same, read from whitespace-free text. Only a
/// single top-level `==` counts.
fn same_sides(expr: &str) -> bool {
    let mut parts = expr.split("==");
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => !a.is_empty() && a == b,
        _ => false,
    }
}

fn record(c: &mut Counts, tautology: String) {
    *c.tautologies.entry(tautology).or_default() += 1;
}

fn rust(src: &Source<'_>, c: &mut Counts) {
    let m = src.masked;
    let mut from = 0;
    while let Some(at) = m[from..].find("#[") {
        let start = src.skip_ws(from + at + 2);
        let end = m[start..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == ':'))
            .map_or(m.len(), |e| start + e);
        let path = &m[start..end];
        if path == "test" || path.ends_with("::test") || path == "rstest" || path == "test_case" {
            c.tests += 1;
        }
        if path == "ignore" {
            c.skips += 1;
        }
        from = end.max(from + at + 2);
    }
    for (at, ident) in src.idents() {
        let bang = at + ident.len();
        if !ident.contains("assert") || src.byte(bang) != Some(b'!') {
            continue;
        }
        c.assertions += 1;
        let open = src.skip_ws(bang + 1);
        if !matches!(src.byte(open), Some(b'(' | b'[' | b'{')) {
            continue;
        }
        let Some((args, _)) = src.args(open) else {
            continue;
        };
        let tautological = match ident {
            "assert" | "debug_assert" => args.first().is_some_and(|a| a == "true" || same_sides(a)),
            "assert_eq" | "debug_assert_eq" => args.len() >= 2 && args[0] == args[1],
            _ => false,
        };
        if tautological {
            record(c, format!("{ident}!({})", args.join(", ")));
        }
    }
}

fn js(src: &Source<'_>, c: &mut Counts) {
    for (at, ident) in src.idents() {
        let after = src.skip_ws(at + ident.len());
        let next = src.byte(after);
        let dotted = src.prev(at) == Some(b'.');
        let called = next == Some(b'(');
        match ident {
            "it" | "test" | "xit" | "xtest" | "fit" if !dotted => {
                if called || (next == Some(b'.') && chain_is_called(src, after)) {
                    c.tests += 1;
                }
                if called && matches!(ident, "xit" | "xtest") {
                    c.skips += 1;
                }
                if called && ident == "fit" {
                    c.focused += 1;
                }
            }
            "xdescribe" if !dotted && called => c.skips += 1,
            "fdescribe" if !dotted && called => c.focused += 1,
            "skip" | "skipIf" | "todo" if dotted && (called || next == Some(b'.')) => {
                c.skips += 1;
            }
            "only" if dotted && (called || next == Some(b'.')) => c.focused += 1,
            "expect" | "assert" if !dotted && (called || next == Some(b'.')) => {
                c.assertions += 1;
                if let Some(t) = js_tautology(src, ident, after) {
                    record(c, t);
                }
            }
            _ => {}
        }
    }
}

/// Whether the `.a.b` chain starting at `dot` ends in a call, as
/// `it.skip(` and `test.each(` do.
fn chain_is_called(src: &Source<'_>, mut dot: usize) -> bool {
    let m = src.masked.as_bytes();
    while src.byte(dot) == Some(b'.') {
        let mut i = src.skip_ws(dot + 1);
        let start = i;
        while i < m.len() && is_ident(m[i]) {
            i += 1;
        }
        if i == start {
            return false;
        }
        dot = src.skip_ws(i);
    }
    src.byte(dot) == Some(b'(')
}

fn js_tautology(src: &Source<'_>, ident: &str, after: usize) -> Option<String> {
    let m = src.masked;
    if ident == "assert" {
        let (method, open) = if src.byte(after) == Some(b'.') {
            let start = src.skip_ws(after + 1);
            let end = m[start..]
                .find(|ch: char| !ch.is_ascii_alphanumeric())
                .map_or(m.len(), |e| start + e);
            (&m[start..end], src.skip_ws(end))
        } else {
            ("", after)
        };
        if src.byte(open) != Some(b'(') {
            return None;
        }
        let (args, _) = src.args(open)?;
        let hit = match method {
            "" | "ok" => args.first().is_some_and(|a| a == "true"),
            "equal" | "strictEqual" | "deepEqual" | "deepStrictEqual" => {
                args.len() >= 2 && args[0] == args[1]
            }
            _ => false,
        };
        let name = if method.is_empty() {
            "assert".to_string()
        } else {
            format!("assert.{method}")
        };
        return hit.then(|| format!("{name}({})", args.join(", ")));
    }
    if src.byte(after) != Some(b'(') {
        return None;
    }
    let (actual, close) = src.args(after)?;
    let dot = src.skip_ws(close);
    if src.byte(dot) != Some(b'.') {
        return None;
    }
    let start = src.skip_ws(dot + 1);
    let end = m[start..]
        .find(|ch: char| !ch.is_ascii_alphanumeric())
        .map_or(m.len(), |e| start + e);
    let matcher = &m[start..end];
    let open = src.skip_ws(end);
    if src.byte(open) != Some(b'(') {
        return None;
    }
    let (expected, _) = src.args(open)?;
    let actual = actual.join(",");
    let hit = match matcher {
        "toBe" | "toEqual" | "toStrictEqual" => {
            !actual.is_empty() && expected.len() == 1 && expected[0] == actual
        }
        "toBeTruthy" => actual == "true",
        "toBeFalsy" => actual == "false",
        _ => false,
    };
    hit.then(|| format!("expect({actual}).{matcher}({})", expected.join(", ")))
}

fn python(src: &Source<'_>, c: &mut Counts) {
    let mut offset = 0;
    for line in src.masked.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("def test") || t.starts_with("async def test") {
            c.tests += 1;
        }
        if let Some(rest) = t.strip_prefix("assert")
            && rest.starts_with([' ', '('])
        {
            c.assertions += 1;
            let lead = line.len() - line.trim_start().len();
            let start = offset + lead + "assert".len();
            let end = offset + line.trim_end().len();
            let expr = squash(&src.text[start..end]);
            let condition = top_level_split(&expr);
            if condition == "True" || same_sides(condition) {
                record(c, format!("assert {condition}"));
            }
        }
        for marker in [
            "@pytest.mark.skip",
            "@pytest.mark.xfail",
            "@unittest.skip",
            "pytest.skip(",
            ".skipTest(",
        ] {
            c.skips += t.matches(marker).count();
        }
        offset += line.len();
    }
    for (at, ident) in src.idents() {
        let after = src.skip_ws(at + ident.len());
        let dotted = src.prev(at) == Some(b'.');
        if dotted && ident.starts_with("assert") && src.byte(after) == Some(b'(') {
            c.assertions += 1;
            if let Some((args, _)) = src.args(after) {
                let hit = match ident {
                    "assertTrue" => args.first().is_some_and(|a| a == "True"),
                    "assertEqual" => args.len() >= 2 && args[0] == args[1],
                    _ => false,
                };
                if hit {
                    record(c, format!("{ident}({})", args.join(", ")));
                }
            }
        }
        if ident == "raises" && dotted && src.byte(after) == Some(b'(') {
            c.assertions += 1;
        }
    }
}

/// `expr` up to its first comma outside brackets: an `assert`'s condition
/// without its message.
fn top_level_split(expr: &str) -> &str {
    let mut depth = 0i32;
    for (i, ch) in expr.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => return &expr[..i],
            _ => {}
        }
    }
    expr
}

/// `text` with every comment blanked and every string's contents blanked,
/// its quotes kept. Blanked bytes become spaces and newlines stay, so
/// offsets and line numbers match the original.
fn mask(lang: Lang, text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        let mut to = to.min(b.len());
        while !text.is_char_boundary(to) {
            to += 1;
        }
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        match (lang, b[i]) {
            (Lang::Python, b'#') | (Lang::Rust | Lang::Js, b'/') if is_line_comment(lang, rest) => {
                let end = rest
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |e| i + e);
                blank(&mut out, i, end);
                i = end;
            }
            (Lang::Rust | Lang::Js, b'/') if rest.starts_with(b"/*") => {
                let end = block_comment_end(lang, b, i);
                blank(&mut out, i, end);
                i = end;
            }
            (Lang::Python, b'"' | b'\'') => {
                let quote = b[i];
                let triple = rest.len() >= 3 && rest[1] == quote && rest[2] == quote;
                let width = if triple { 3 } else { 1 };
                let end = string_end(b, i + width, quote, width);
                blank(
                    &mut out,
                    i + width,
                    end.saturating_sub(width).max(i + width),
                );
                i = end;
            }
            (Lang::Js, b'"' | b'\'' | b'`') | (Lang::Rust, b'"') => {
                let end = string_end(b, i + 1, b[i], 1);
                blank(&mut out, i + 1, end.saturating_sub(1).max(i + 1));
                i = end;
            }
            (Lang::Rust, b'r') if (i == 0 || !is_ident(b[i - 1])) && is_raw_string(rest) => {
                let hashes = rest[1..].iter().take_while(|&&c| c == b'#').count();
                let body = i + 2 + hashes;
                let mut close = vec![b'"'];
                close.extend(std::iter::repeat_n(b'#', hashes));
                let end = b[body..]
                    .windows(close.len())
                    .position(|w| w == close.as_slice())
                    .map_or(b.len(), |e| body + e);
                blank(&mut out, body, end);
                i = (end + close.len()).min(b.len());
            }
            (Lang::Rust, b'\'') => i += char_literal_len(rest),
            _ => i += 1,
        }
    }
    String::from_utf8(out).expect("masking replaces whole characters with spaces")
}

fn is_line_comment(lang: Lang, rest: &[u8]) -> bool {
    match lang {
        Lang::Python => true,
        Lang::Rust | Lang::Js => rest.starts_with(b"//"),
    }
}

/// The offset just past the `*/` closing the block comment at `start`.
/// Rust's block comments nest; JS's do not.
fn block_comment_end(lang: Lang, b: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i + 1 < b.len() {
        if b[i] == b'/' && b[i + 1] == b'*' && (depth == 0 || lang == Lang::Rust) {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// The offset just past the string closing `width` copies of `quote`, its
/// body starting at `from`. A backslash escapes the next byte.
fn string_end(b: &[u8], from: usize, quote: u8, width: usize) -> usize {
    let mut i = from;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == quote && b[i..].iter().take(width).all(|&c| c == quote) && i + width <= b.len() {
            return i + width;
        }
        i += 1;
    }
    b.len()
}

fn is_raw_string(rest: &[u8]) -> bool {
    let hashes = rest[1..].iter().take_while(|&&c| c == b'#').count();
    rest.get(1 + hashes) == Some(&b'"')
}

/// How far a Rust `'` reaches: a whole char literal, or just the quote of a
/// lifetime.
fn char_literal_len(rest: &[u8]) -> usize {
    if rest.get(1) == Some(&b'\\') {
        return rest[2..]
            .iter()
            .position(|&c| c == b'\'')
            .map_or(1, |e| e + 3);
    }
    let width = rest.get(1).map_or(1, |&lead| match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    });
    if rest.get(1 + width) == Some(&b'\'') {
        width + 2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_counts_tests_assertions_and_ignores_outside_comments_and_strings() {
        let text = r##"
#[test]
fn a() { assert_eq!(f(), 2); assert!(g(), "msg with assert!(true)"); }
#[tokio::test]
#[ignore]
async fn b() { prop_assert!(x); }
// #[test]
/* #[test] fn c() { assert!(true); } */
fn d() { let s = r#"#[test] assert!(true)"#; let ch = '"'; let l: &'static str = "x"; }
"##;
        let c = counts(Lang::Rust, text);
        assert_eq!(c.tests, 2);
        assert_eq!(c.assertions, 3);
        assert_eq!(c.skips, 1);
        assert!(c.tautologies.is_empty(), "{:?}", c.tautologies);
    }

    #[test]
    fn rust_tautologies() {
        let text = "assert!(true);\nassert_eq!(x.len(), x.len());\nassert!(a == a);\nassert_eq!(\"a\", \"b\");\n";
        let c = counts(Lang::Rust, text);
        assert_eq!(c.tautologies.keys().collect::<Vec<_>>(), [
            "assert!(a==a)",
            "assert!(true)",
            "assert_eq!(x.len(), x.len())"
        ]);
    }

    #[test]
    fn js_counts_every_marker() {
        let text = r#"
describe("suite", () => {
  it("one", () => { expect(a).toBe(1); });
  it.skip("two", () => {});
  it.skipIf(process.env.CI)("three", () => {});
  test.todo("four");
  test.only("five", () => { assert.equal(a, 1); });
  xit("six", () => {});
  test.each([1, 2])("seven %i", (n) => { expect(n).toBeGreaterThan(0); });
  // it("commented", () => {});
  const glob = "src/**/*.ts";
  it("eight", () => {});
});
"#;
        let c = counts(Lang::Js, text);
        assert_eq!(c.tests, 8);
        assert_eq!(c.skips, 4);
        assert_eq!(c.focused, 1);
        assert_eq!(c.assertions, 3);
    }

    #[test]
    fn js_tautologies() {
        let text = "expect(true).toBe(true);\nexpect(x).toEqual(x);\nexpect(a).toBe(b);\nassert(true);\nassert.ok(true);\nexpect(true).toBeTruthy();\n";
        let c = counts(Lang::Js, text);
        assert_eq!(c.tautologies.len(), 5, "{:?}", c.tautologies);
    }

    #[test]
    fn python_counts_and_tautologies() {
        let text = r#"
import pytest

def test_one():
    assert f() == 2

@pytest.mark.skipif(True, reason="x")
def test_two():
    assert True
    self.assertEqual(a, a)
    with pytest.raises(ValueError):
        g()

# def test_three():
def helper():
    """def test_four(): assert True"""
"#;
        let c = counts(Lang::Python, text);
        assert_eq!(c.tests, 2);
        assert_eq!(c.skips, 1);
        assert_eq!(c.assertions, 4);
        assert_eq!(c.tautologies.keys().collect::<Vec<_>>(), [
            "assert True",
            "assertEqual(a, a)"
        ]);
    }
}
