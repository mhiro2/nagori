//! Dependency-free, best-effort language detection for code-kind clips.
//!
//! The result is a canonical lowercase identifier (`json`, `sql`, `rust`, …)
//! stored on [`CodeContent::language_hint`](super::content::CodeContent) and
//! mirrored into [`SearchDocument::language`](super::search::SearchDocument).
//! Three surfaces consume it from there:
//!
//! * the desktop preview pane picks a syntax-highlight profile,
//! * the result-row language badge shows the same canonical label, and
//! * the ranker can reason about code without re-sniffing the body.
//!
//! Detection is deliberately conservative — an unrecognised snippet returns
//! `None` and the preview falls back to the neutral highlight profile rather
//! than mislabelling the body. It is only meaningful for bodies that already
//! passed [`CodeContent::looks_like_code`](super::content::CodeContent::looks_like_code),
//! so the heuristics here only have to *disambiguate among languages*, not
//! decide "code vs. prose".

/// Cap on the prefix we inspect. Language cues live near the top of a snippet
/// (shebang, imports, opening braces), so scanning the whole of a multi-MB
/// paste would burn cycles for no extra signal.
const SCAN_CHARS: usize = 2_048;

/// Above this length the JSON check skips the full parse. A genuinely large
/// JSON paste is still caught as code by the brace+newline heuristic in
/// `looks_like_code`; this only bounds the cost of *probing* a multi-MB body
/// that merely opens and closes with a brace (a templated doc, a code block).
const MAX_JSON_PROBE_BYTES: usize = 256 * 1024;

/// Detect the language of an already-classified code body.
///
/// Returns a canonical lowercase id understood by the desktop tokenizer's
/// `normalizeLanguage`, or `None` when no profile is confident enough.
#[must_use]
pub fn detect(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    // Shebang is the strongest single signal — trust the named interpreter.
    if trimmed.starts_with("#!") {
        return Some(shebang_language(trimmed));
    }
    let full = text.trim();
    if looks_like_json(full) {
        return Some("json".to_owned());
    }
    if let Some(markup) = markup_language(full) {
        return Some(markup.to_owned());
    }
    // Lower-cased, length-bounded window reused by the remaining checks so we
    // only allocate one scratch string regardless of how many profiles run.
    let head: String = trimmed.chars().take(SCAN_CHARS).collect();
    let lower = head.to_ascii_lowercase();
    if looks_like_sql(&lower) {
        return Some("sql".to_owned());
    }
    if looks_like_rust(&head) {
        return Some("rust".to_owned());
    }
    if looks_like_go(&head) {
        return Some("go".to_owned());
    }
    if looks_like_python(&head) {
        return Some("python".to_owned());
    }
    if looks_like_typescript(&head) {
        return Some("typescript".to_owned());
    }
    if looks_like_shell(&head) {
        return Some("shell".to_owned());
    }
    if looks_like_yaml(full) {
        return Some("yaml".to_owned());
    }
    None
}

/// Map a `#!` interpreter line onto a canonical id. Falls back to `shell`
/// for an unrecognised interpreter because a shebang almost always fronts a
/// shell script when it isn't one of the named runtimes.
fn shebang_language(trimmed: &str) -> String {
    let first = trimmed
        .lines()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if first.contains("python") {
        "python".to_owned()
    } else if first.contains("node") || first.contains("deno") || first.contains("bun") {
        "typescript".to_owned()
    } else {
        // bash / zsh / sh / dash / ksh / env-with-unknown all read as shell.
        "shell".to_owned()
    }
}

/// A body that actually parses as a JSON object or array.
///
/// A cheap structural gate (must open and close with matching brackets, must
/// be within the probe size cap) runs first so arbitrary prose never reaches
/// the parser; only then does `serde_json` confirm it is well-formed. Parsing
/// — rather than a brace/quote substring heuristic — is what keeps broken or
/// prose-in-braces inputs (`[foo, bar]`, `{"a":}`, `{see note}`) out of the
/// `Code(json)` bucket.
///
/// Shared with [`CodeContent::looks_like_code`](super::content::CodeContent::looks_like_code)
/// so a minified single-line JSON body is classified as code by the same rule
/// that labels it `json` here.
pub(crate) fn looks_like_json(s: &str) -> bool {
    let bytes = s.as_bytes();
    let bracketed = matches!(
        (bytes.first().copied(), bytes.last().copied()),
        (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']'))
    );
    if !bracketed || s.len() > MAX_JSON_PROBE_BYTES {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(s).is_ok()
}

/// Angle-bracket markup. `<?xml` is unambiguous; everything else that opens
/// with a tag is treated as HTML for highlighting purposes.
fn markup_language(s: &str) -> Option<&'static str> {
    if !s.starts_with('<') {
        return None;
    }
    let head = s.chars().take(64).collect::<String>().to_ascii_lowercase();
    if head.starts_with("<?xml") {
        return Some("xml");
    }
    if head.starts_with("<!doctype html") || head.contains("<html") {
        return Some("html");
    }
    // Generic open-tag with a matching close somewhere — still HTML-ish.
    if s.contains("</") || s.contains("/>") {
        return Some("html");
    }
    None
}

/// SQL DML/DDL. Operates on a lower-cased window; pairs (`select … from`)
/// keep single-keyword prose ("update available") from matching.
fn looks_like_sql(lower: &str) -> bool {
    (lower.contains("select ") && lower.contains(" from "))
        || lower.contains("insert into ")
        || lower.contains("create table ")
        || lower.contains("delete from ")
        || lower.contains("alter table ")
        || (lower.contains("update ") && lower.contains(" set "))
}

fn looks_like_rust(head: &str) -> bool {
    head.contains("fn ")
        && (head.contains("->")
            || head.contains("let ")
            || head.contains("impl ")
            || head.contains("pub ")
            || head.contains("::")
            || head.contains('!')
            || head.contains("use std")
            || head.contains("#["))
}

fn looks_like_go(head: &str) -> bool {
    head.contains("func ")
        && (head.contains("package ")
            || head.contains(":=")
            || head.contains("import (")
            || head.contains("fmt.")
            || head.contains("interface{"))
}

fn looks_like_python(head: &str) -> bool {
    head.contains("def ") && head.contains(':')
        || head.contains("elif ")
        || head.contains("self.")
        || head.contains("__name__")
        || (head.contains("import ") && head.contains("print("))
}

fn looks_like_typescript(head: &str) -> bool {
    head.contains("=>")
        || head.contains("function ")
        || head.contains("interface ")
        || (head.contains("const ") && head.contains('='))
        || (head.contains("import ") && head.contains(" from "))
        || head.contains("export ")
        || head.contains(": string")
        || head.contains(": number")
}

fn looks_like_shell(head: &str) -> bool {
    head.contains("\nfi")
        || head.contains("\ndone")
        || head.contains("esac")
        || head.contains("then\n")
        || head.contains("$(")
        || head.contains("export ")
        || head.starts_with("$ ")
}

/// Multi-line `key: value` body that is not JSON. `---` document markers are
/// a strong signal; otherwise require at least two top-level key lines and no
/// JSON braces so a single `label: text` sentence is not mistaken for YAML.
fn looks_like_yaml(s: &str) -> bool {
    if s.starts_with("---") {
        return true;
    }
    if s.contains('{') || s.contains('}') {
        return false;
    }
    let key_lines = s
        .lines()
        .filter(|line| is_yaml_key_line(line))
        .take(2)
        .count();
    key_lines >= 2
}

/// `^\s*<key>:` followed by end-of-line or a space — the canonical YAML
/// mapping shape, excluding `key:value` with no space (which is more often
/// prose like a URL `http://`).
fn is_yaml_key_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(colon) = trimmed.find(':') else {
        return false;
    };
    if colon == 0 {
        return false;
    }
    let key = &trimmed[..colon];
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return false;
    }
    let rest = &trimmed[colon + 1..];
    rest.is_empty() || rest.starts_with(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_minified_and_pretty_json() {
        assert_eq!(detect("{\"a\":1}").as_deref(), Some("json"));
        assert_eq!(
            detect("{\n  \"name\": \"nagori\",\n  \"n\": 1\n}").as_deref(),
            Some("json")
        );
        assert_eq!(detect("[1, 2, 3]").as_deref(), Some("json"));
    }

    #[test]
    fn braced_prose_and_broken_json_are_not_json() {
        // These open/close with brackets but do not *parse* as JSON, so the
        // serde_json-backed check declines rather than mislabelling them.
        assert_eq!(detect("{see the note below}"), None);
        assert_eq!(detect("[foo, bar]"), None); // unquoted words
        assert_eq!(detect("{\"a\":}"), None); // missing value
        assert_eq!(detect("{\"a\": 1"), None); // unbalanced (also not bracketed-closed)
    }

    #[test]
    fn detects_sql_select_and_ddl() {
        assert_eq!(
            detect("SELECT id, name FROM users WHERE id = 1").as_deref(),
            Some("sql")
        );
        assert_eq!(
            detect("create table t (id int primary key)").as_deref(),
            Some("sql")
        );
        // A lone "update" word in prose must not match (needs the SET pair).
        assert_eq!(detect("update the docs\nplease review"), None);
    }

    #[test]
    fn detects_shell_via_shebang_and_keywords() {
        assert_eq!(detect("#!/bin/bash\necho hi").as_deref(), Some("shell"));
        assert_eq!(
            detect("#!/usr/bin/env python3\nprint(1)").as_deref(),
            Some("python")
        );
        assert_eq!(
            detect("#!/usr/bin/env node\nconsole.log(1)").as_deref(),
            Some("typescript")
        );
        assert_eq!(
            detect("for f in *; do\n  echo $f\ndone").as_deref(),
            Some("shell")
        );
    }

    #[test]
    fn detects_rust_typescript_python_go() {
        assert_eq!(
            detect("fn main() {\n    println!(\"hi\");\n}").as_deref(),
            Some("rust")
        );
        assert_eq!(
            detect("export const add = (a: number, b: number) => a + b;").as_deref(),
            Some("typescript")
        );
        assert_eq!(
            detect("def greet(name):\n    print(name)").as_deref(),
            Some("python")
        );
        assert_eq!(
            detect("package main\n\nfunc main() {\n    fmt.Println(\"hi\")\n}").as_deref(),
            Some("go")
        );
    }

    #[test]
    fn detects_markup() {
        assert_eq!(
            detect("<!DOCTYPE html>\n<html><body>hi</body></html>").as_deref(),
            Some("html")
        );
        assert_eq!(
            detect("<?xml version=\"1.0\"?>\n<root/>").as_deref(),
            Some("xml")
        );
    }

    #[test]
    fn detects_yaml_but_not_single_key_prose() {
        assert_eq!(
            detect("name: nagori\nversion: 1\nitems:\n  - a").as_deref(),
            Some("yaml")
        );
        assert_eq!(detect("note: see the readme"), None);
    }

    #[test]
    fn unknown_snippet_returns_none() {
        assert_eq!(detect("just some words\nwith two lines"), None);
        assert_eq!(detect(""), None);
        assert_eq!(detect("   \n  "), None);
    }
}
