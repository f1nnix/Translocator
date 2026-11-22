//! Syntax colouring for the native shell's text views (the viewer, the
//! editor and quick look), with the tree-sitter grammars the GPUI shell
//! colours with.
//!
//! [`highlight`] parses a text in one of the languages
//! `tl_app::editor::language_for` names and runs the grammar's own
//! highlight query over it, the way `tree-sitter highlight` does: an inner
//! node's colour wins over its parent's, and of two patterns on the same
//! node the later one wins. What comes back is a small fixed set of
//! [`HighlightKind`]s over UTF-16 ranges, for the shell to colour from its
//! own palette. Every call stands alone: the shell runs it off the main
//! thread and again (debounced) after edits.
//!
//! Large files stay quick: only the first [`HIGHLIGHT_LIMIT`] bytes (up to
//! a line end) are coloured, and parsing and querying stop after
//! [`HIGHLIGHT_BUDGET`], keeping what was found by then.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tree_sitter::{Language, ParseOptions, ParseState, Parser, Query, QueryCursor, StreamingIterator as _};

/// How much of a text is coloured; the rest is drawn plain.
pub const HIGHLIGHT_LIMIT: usize = 2 * 1024 * 1024;
/// How long one call may parse and query before it settles for what it has.
pub const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(250);

/// What a piece of text is, for its colour.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Keyword,
    String,
    /// An escape inside a string (`\n`).
    Escape,
    Number,
    /// Booleans, built-in constants, `CONSTANT_CASE` names, labels.
    Constant,
    Comment,
    Function,
    /// Types, constructors, modules.
    Type,
    /// Fields, properties, keys (YAML, TOML, JSON).
    Property,
    Punctuation,
    Operator,
    /// Markup tags (HTML, JSX).
    Tag,
    /// Attributes, decorators, CSS selectors' attributes, diff locations.
    Attribute,
    /// Markdown headings, the header lines of a diff.
    Heading,
    /// Markdown link targets and references.
    Link,
    /// Markdown code blocks.
    Literal,
    /// Diff lines added.
    Inserted,
    /// Diff lines removed.
    Deleted,
}

impl HighlightKind {
    const ALL: [HighlightKind; 18] = [
        HighlightKind::Keyword,
        HighlightKind::String,
        HighlightKind::Escape,
        HighlightKind::Number,
        HighlightKind::Constant,
        HighlightKind::Comment,
        HighlightKind::Function,
        HighlightKind::Type,
        HighlightKind::Property,
        HighlightKind::Punctuation,
        HighlightKind::Operator,
        HighlightKind::Tag,
        HighlightKind::Attribute,
        HighlightKind::Heading,
        HighlightKind::Link,
        HighlightKind::Literal,
        HighlightKind::Inserted,
        HighlightKind::Deleted,
    ];

    /// Its code in a paint buffer, where 0 is plain text.
    fn code(self) -> u8 {
        Self::ALL.iter().position(|k| *k == self).map_or(0, |ix| ix as u8 + 1)
    }

    fn from_code(code: u8) -> Option<HighlightKind> {
        code.checked_sub(1).and_then(|ix| Self::ALL.get(ix as usize).copied())
    }
}

/// A coloured range of a text, in UTF-16 code units (`NSString` offsets).
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HighlightSpan {
    pub start_utf16: u32,
    pub len_utf16: u32,
    pub kind: HighlightKind,
}

/// The coloured ranges of `text` in `language` (as `language_for` names
/// it; "text" and unknown names give none), in order, not overlapping.
#[uniffi::export]
pub fn highlight(text: String, language: String) -> Vec<HighlightSpan> {
    spans(&text, &language, HIGHLIGHT_LIMIT, HIGHLIGHT_BUDGET)
}

/// What is parsed again when the whole limit does not parse in time: the
/// start of the file, which is what is on screen first.
const FALLBACK_LIMIT: usize = 128 * 1024;

/// [`highlight`] with its limits given. A text that does not parse within
/// the budget (a slow machine, a huge file) still gets its first
/// [`FALLBACK_LIMIT`] bytes coloured, with a budget of their own.
pub fn spans(text: &str, language: &str, limit: usize, budget: Duration) -> Vec<HighlightSpan> {
    match spans_within(text, language, limit, budget) {
        Some(spans) => spans,
        None if limit > FALLBACK_LIMIT && text.len() > FALLBACK_LIMIT => {
            spans_within(text, language, FALLBACK_LIMIT, budget).unwrap_or_default()
        }
        None => Vec::new(),
    }
}

/// The spans of the first `limit` bytes, or `None` when they did not parse
/// within `budget`.
fn spans_within(text: &str, language: &str, limit: usize, budget: Duration) -> Option<Vec<HighlightSpan>> {
    let Some(grammar) = grammar(language) else {
        return Some(Vec::new());
    };
    let started = Instant::now();
    let text = head(text, limit);
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Some(Vec::new());
    }

    let mut parser = Parser::new();
    if parser.set_language(&grammar.language).is_err() {
        return Some(Vec::new());
    }
    let mut out_of_time = |_: &ParseState| {
        if started.elapsed() > budget {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut out_of_time);
    // Out of time: no tree.
    let tree = parser.parse_with_options(&mut |at, _| &bytes[at.min(bytes.len())..], None, Some(options))?;

    // (start, end, pattern, paint code)
    let mut found: Vec<(usize, usize, usize, u8)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&grammar.query, tree.root_node(), bytes);
    let mut seen = 0usize;
    while let Some((m, ix)) = captures.next() {
        let capture = m.captures[*ix];
        if let Some(paint) = grammar.paints[capture.index as usize] {
            let node = capture.node;
            found.push((node.start_byte(), node.end_byte(), m.pattern_index, paint));
        }
        seen += 1;
        if seen.is_multiple_of(1024) && started.elapsed() > budget {
            break;
        }
    }

    // Outer ranges first so inner ones paint over them; on the same range
    // the later pattern paints last and wins.
    found.sort_unstable_by_key(|&(start, end, pattern, _)| (start, Reverse(end), pattern));
    let mut paint = vec![0u8; bytes.len()];
    for (start, end, _, code) in found {
        let end = end.min(bytes.len());
        if start < end {
            paint[start..end].fill(code);
        }
    }
    Some(runs(text, &paint))
}

/// The paint buffer as spans over UTF-16 offsets. A character takes the
/// paint of its first byte.
fn runs(text: &str, paint: &[u8]) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut at: u32 = 0;
    let mut run: (u32, u8) = (0, 0);
    let close = |spans: &mut Vec<HighlightSpan>, (start, code): (u32, u8), end: u32| {
        if let Some(kind) = HighlightKind::from_code(code)
            && end > start
        {
            spans.push(HighlightSpan {
                start_utf16: start,
                len_utf16: end - start,
                kind,
            });
        }
    };
    for (ix, ch) in text.char_indices() {
        let code = paint[ix];
        if code != run.1 {
            close(&mut spans, run, at);
            run = (at, code);
        }
        at += ch.len_utf16() as u32;
    }
    close(&mut spans, run, at);
    spans
}

/// The first `limit` bytes of `text`, cut after a line end (or at a
/// character boundary when the first line is longer than that).
fn head(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    match text[..cut].rfind('\n') {
        Some(nl) => &text[..=nl],
        None => &text[..cut],
    }
}

/// A language's grammar with its query compiled, and what each capture of
/// the query paints (`None`: nothing; `Some(0)`: plain, over its parent).
struct Grammar {
    language: Language,
    query: Query,
    paints: Vec<Option<u8>>,
}

/// Compiled once per language, on first use.
fn grammar(language: &str) -> Option<Arc<Grammar>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Grammar>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock();
    if let Some(grammar) = cache.get(language) {
        return grammar.clone();
    }
    let grammar = build(language).map(Arc::new);
    cache.insert(language.to_string(), grammar.clone());
    grammar
}

fn build(language: &str) -> Option<Grammar> {
    let (lang, queries) = sources(language)?;
    let query = match Query::new(&lang, &queries.concat()) {
        Ok(query) => query,
        Err(err) if queries.len() > 1 => {
            // A query borrowed from a base grammar does not fit: the
            // language's own (the last one) alone still colours most of it.
            log::warn!("highlight query for {language}: {err}; using its own query only");
            Query::new(&lang, queries[queries.len() - 1])
                .inspect_err(|err| log::warn!("highlight query for {language}: {err}"))
                .ok()?
        }
        Err(err) => {
            log::warn!("highlight query for {language}: {err}");
            return None;
        }
    };
    let paints = query
        .capture_names()
        .iter()
        .map(|name| paint(language, name).map(|kind| kind.map_or(0, HighlightKind::code)))
        .collect();
    Some(Grammar {
        language: lang,
        query,
        paints,
    })
}

/// The grammar and its highlight queries, in the order `tree-sitter.json`
/// lists them (a base grammar's first, so the language's own win).
fn sources(language: &str) -> Option<(Language, Vec<&'static str>)> {
    use tree_sitter_javascript as js;
    let found: (Language, Vec<&'static str>) = match language {
        "bash" => (tree_sitter_bash::LANGUAGE.into(), vec![tree_sitter_bash::HIGHLIGHT_QUERY]),
        "c" => (tree_sitter_c::LANGUAGE.into(), vec![tree_sitter_c::HIGHLIGHT_QUERY]),
        "cpp" => (
            tree_sitter_cpp::LANGUAGE.into(),
            vec![tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY],
        ),
        "css" => (tree_sitter_css::LANGUAGE.into(), vec![tree_sitter_css::HIGHLIGHTS_QUERY]),
        "diff" => (tree_sitter_diff::LANGUAGE.into(), vec![tree_sitter_diff::HIGHLIGHTS_QUERY]),
        "go" => (tree_sitter_go::LANGUAGE.into(), vec![tree_sitter_go::HIGHLIGHTS_QUERY]),
        "html" => (tree_sitter_html::LANGUAGE.into(), vec![tree_sitter_html::HIGHLIGHTS_QUERY]),
        "javascript" => (js::LANGUAGE.into(), vec![js::HIGHLIGHT_QUERY, js::JSX_HIGHLIGHT_QUERY]),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            vec![js::HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY],
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            vec![js::HIGHLIGHT_QUERY, js::JSX_HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY],
        ),
        "json" => (tree_sitter_json::LANGUAGE.into(), vec![tree_sitter_json::HIGHLIGHTS_QUERY]),
        "make" => (tree_sitter_make::LANGUAGE.into(), vec![tree_sitter_make::HIGHLIGHTS_QUERY]),
        "markdown" => (tree_sitter_md::LANGUAGE.into(), vec![tree_sitter_md::HIGHLIGHT_QUERY_BLOCK]),
        "python" => (tree_sitter_python::LANGUAGE.into(), vec![tree_sitter_python::HIGHLIGHTS_QUERY]),
        "rust" => (tree_sitter_rust::LANGUAGE.into(), vec![tree_sitter_rust::HIGHLIGHTS_QUERY]),
        "sql" => (tree_sitter_sequel::LANGUAGE.into(), vec![tree_sitter_sequel::HIGHLIGHTS_QUERY]),
        "toml" => (tree_sitter_toml_ng::LANGUAGE.into(), vec![tree_sitter_toml_ng::HIGHLIGHTS_QUERY]),
        "yaml" => (tree_sitter_yaml::LANGUAGE.into(), vec![tree_sitter_yaml::HIGHLIGHTS_QUERY]),
        _ => return None,
    };
    Some(found)
}

/// What a capture name paints: `None` leaves it alone (`@variable`),
/// `Some(None)` makes it plain over its parent (`@none`).
fn paint(language: &str, name: &str) -> Option<Option<HighlightKind>> {
    use HighlightKind as K;
    if language == "diff" {
        // The diff query borrows names for colours: added lines are
        // strings, removed ones keywords, `diff --git` a builtin.
        match name {
            "string" => return Some(Some(K::Inserted)),
            "keyword" => return Some(Some(K::Deleted)),
            "variable.builtin" => return Some(Some(K::Heading)),
            _ => {}
        }
    }
    let kind = match name {
        "none" => return Some(None),
        "string.escape" | "escape" => K::Escape,
        "string.special.key" => K::Property,
        "variable.builtin" | "type.qualifier" => K::Keyword,
        "text.title" => K::Heading,
        "text.uri" | "text.reference" => K::Link,
        "text.literal" => K::Literal,
        _ => match name.split('.').next().unwrap_or(name) {
            "comment" => K::Comment,
            "string" | "character" => K::String,
            "number" | "float" => K::Number,
            "boolean" | "constant" | "label" => K::Constant,
            "keyword" | "conditional" | "repeat" | "include" | "import" | "exception" | "storageclass"
            | "charset" | "media" | "keyframes" | "supports" => K::Keyword,
            "function" | "method" => K::Function,
            "constructor" | "type" | "namespace" | "module" => K::Type,
            "property" | "field" => K::Property,
            "punctuation" | "delimiter" => K::Punctuation,
            "operator" => K::Operator,
            "tag" => K::Tag,
            "attribute" => K::Attribute,
            _ => return None,
        },
    };
    Some(Some(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kind painted over the first occurrence of `needle`, if one span
    /// covers all of it.
    fn kind_of(text: &str, language: &str, needle: &str) -> Option<HighlightKind> {
        let spans = highlight(text.into(), language.into());
        let byte = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {text:?}"));
        let start = text[..byte].encode_utf16().count() as u32;
        let end = start + needle.encode_utf16().count() as u32;
        spans
            .iter()
            .find(|s| s.start_utf16 <= start && end <= s.start_utf16 + s.len_utf16)
            .map(|s| s.kind)
    }

    #[test]
    fn rust_is_coloured() {
        let text = "// hello\nfn main() {\n    let x: u32 = 42;\n    println!(\"hi\\n\");\n}\n";
        assert_eq!(kind_of(text, "rust", "// hello"), Some(HighlightKind::Comment));
        assert_eq!(kind_of(text, "rust", "fn"), Some(HighlightKind::Keyword));
        assert_eq!(kind_of(text, "rust", "main"), Some(HighlightKind::Function));
        assert_eq!(kind_of(text, "rust", "u32"), Some(HighlightKind::Type));
        assert!(matches!(kind_of(text, "rust", "42"), Some(HighlightKind::Constant | HighlightKind::Number)));
        assert_eq!(kind_of(text, "rust", "\"hi"), Some(HighlightKind::String));
        // The escape is inside the string and wins over it.
        assert_eq!(kind_of(text, "rust", "\\n"), Some(HighlightKind::Escape));
        assert_eq!(kind_of(text, "rust", "x"), None);
    }

    #[test]
    fn every_language_the_core_names_is_coloured() {
        let samples: &[(&str, &str, &str, HighlightKind)] = &[
            ("make", "all: build\n\t@echo \"done\"\n", "all", HighlightKind::Constant),
            ("bash", "if [ -n \"$x\" ]; then echo hi; fi # c\n", "# c", HighlightKind::Comment),
            ("rust", "fn f() {}\n", "fn", HighlightKind::Keyword),
            ("toml", "[package]\nname = \"x\" # c\n", "# c", HighlightKind::Comment),
            ("json", "{\"a\": 1, \"b\": true}\n", "true", HighlightKind::Constant),
            ("markdown", "# Title\n\ntext\n", "Title", HighlightKind::Heading),
            ("python", "def f():\n    return 'x'\n", "def", HighlightKind::Keyword),
            ("javascript", "function f() { return \"x\"; }\n", "function", HighlightKind::Keyword),
            ("typescript", "let x: number = 1;\n", "number", HighlightKind::Type),
            ("tsx", "const a = <div className=\"x\" />;\n", "div", HighlightKind::Tag),
            ("yaml", "key: value # c\n", "# c", HighlightKind::Comment),
            ("go", "package main\nfunc main() {}\n", "func", HighlightKind::Keyword),
            ("c", "int main(void) { return 0; } /* c */\n", "/* c */", HighlightKind::Comment),
            ("cpp", "class A { public: int x; };\n", "class", HighlightKind::Keyword),
            ("html", "<p class=\"x\">hi</p>\n", "p", HighlightKind::Tag),
            ("css", "a { color: red; } /* c */\n", "/* c */", HighlightKind::Comment),
            ("sql", "SELECT a FROM t;\n", "SELECT", HighlightKind::Keyword),
            ("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n", "+new", HighlightKind::Inserted),
        ];
        let mut wrong = Vec::new();
        for (language, text, needle, kind) in samples {
            assert!(grammar(language).is_some(), "{language} has no grammar");
            let got = kind_of(text, language, needle);
            if got != Some(*kind) {
                wrong.push(format!("{language}: {needle:?} is {got:?}, not {kind:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert_eq!(kind_of("-old\n", "diff", "-old"), Some(HighlightKind::Deleted));
        assert!(highlight("fn main() {}".into(), "text".into()).is_empty());
        assert!(highlight("fn main() {}".into(), "cobol".into()).is_empty());
        assert!(highlight(String::new(), "rust".into()).is_empty());
    }

    #[test]
    fn offsets_are_utf16() {
        // "é" is one UTF-16 unit and two bytes, the emoji two and four.
        let text = "let s = \"é😀\"; // ok\n";
        let spans = highlight(text.into(), "rust".into());
        let string = spans.iter().find(|s| s.kind == HighlightKind::String).unwrap();
        assert_eq!((string.start_utf16, string.len_utf16), (8, 5));
        let comment = spans.iter().find(|s| s.kind == HighlightKind::Comment).unwrap();
        assert_eq!(comment.start_utf16, 15);
        assert_eq!(comment.len_utf16, 5);
        // In order and apart.
        for pair in spans.windows(2) {
            assert!(pair[0].start_utf16 + pair[0].len_utf16 <= pair[1].start_utf16, "{pair:?}");
        }
    }

    #[test]
    fn only_the_head_of_a_long_text_is_coloured() {
        assert_eq!(head("ab\ncd\nef", 7), "ab\ncd\n");
        assert_eq!(head("abcdef", 3), "abc");
        assert_eq!(head("é", 1), "");
        assert_eq!(head("short", 100), "short");

        let line = "let x = \"a string\"; // and a comment\n";
        let text = line.repeat(1000);
        let limit = line.len() * 10 + 5;
        let spans = spans(&text, "rust", limit, Duration::from_secs(10));
        let end = spans.iter().map(|s| s.start_utf16 + s.len_utf16).max().unwrap();
        assert!(end as usize <= line.len() * 10, "{end}");
        assert_eq!(spans.iter().filter(|s| s.kind == HighlightKind::Comment).count(), 10);
    }

    #[test]
    fn a_file_that_does_not_parse_in_time_keeps_its_start_coloured() {
        let text = "fn f(a: u32) -> u32 { a + 1 } // comment\n".repeat(60_000);
        assert!(text.len() > 2 * FALLBACK_LIMIT);
        // No time at all for the whole text: only the fallback is coloured.
        let spans = spans(&text, "rust", text.len(), Duration::ZERO);
        let fallback = spans_within(&text, "rust", FALLBACK_LIMIT, Duration::from_secs(10)).unwrap();
        assert!(spans.is_empty() || spans == fallback);
        let spans = spans_within(&text, "rust", FALLBACK_LIMIT, Duration::from_secs(10)).unwrap();
        assert!(!spans.is_empty());
        let end = spans.iter().map(|s| s.start_utf16 + s.len_utf16).max().unwrap();
        assert!(end as usize <= FALLBACK_LIMIT, "{end}");
    }

    #[test]
    fn a_large_file_stays_quick() {
        let text = "fn f(a: u32) -> u32 { a + 1 } // eight megabytes of this\n".repeat(150_000);
        assert!(text.len() > 8_000_000);
        let started = Instant::now();
        let spans = highlight(text, "rust".into());
        let took = started.elapsed();
        assert!(!spans.is_empty());
        // The budget plus converting what was found; generous for debug
        // builds on a busy machine.
        assert!(took < Duration::from_secs(3), "{took:?}");
    }
}
