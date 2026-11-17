//! Helm / Go-template / GitHub Actions expression guard.
//!
//! YAML and JSON parsers cannot parse files that contain raw template
//! expressions like `{{ .Values.image.tag }}` or `${{ secrets.GITHUB_TOKEN }}`
//! — the parsers will report cryptic syntax errors. This module provides two
//! complementary tools (`add-safe-writes` D6):
//!
//! 1. [`detect_templates`] — a fast regex check that returns the first marker
//!    seen, used by the CLI's `set` / `del` handlers to short-circuit with a
//!    helpful "this looks like a Helm chart, did you mean `--allow-templates`
//!    or `--raw-template-strings`?" error.
//! 2. [`substitute_placeholders`] / [`restore_placeholders`] — a substitute /
//!    restore pair that swaps every template block for an opaque placeholder
//!    (`__DQ_TPL_<N>__`) so the remaining bytes are valid YAML/JSON for the
//!    parser, then puts the originals back after the textual edit. Quoting
//!    around the block is preserved (the placeholder takes the place of just
//!    the `{{...}}` substring, not any quotes wrapping it).
//!
//! The implementation is deliberately simple: a single regex covers Go
//! template (`{{ ... }}`, `{{- ... }}`) and GitHub Actions (`${{ ... }}`)
//! syntax; the substitute pass is a left-to-right scan that records every
//! match in insertion order. The substitute / restore pair is byte-equal on
//! round-trip — a property exercised by the unit test below.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::bytes::Regex;

/// A single template marker found in source bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateMarker {
    /// 1-based line number where the marker appears.
    pub line: u32,
    /// Up to ~80 characters of the matching line, with surrounding whitespace
    /// trimmed. Useful for error messages without leaking too much context.
    pub snippet: String,
}

/// The detection regex.
///
/// Two alternatives:
/// - `\{\{[-\s]?[\.\w]` matches typical Go-template / Helm syntax. The
///   `[-\s]?` allows the leading whitespace-trim marker `{{-` and an optional
///   whitespace; the trailing `[\.\w]` requires a `.` or word character so we
///   don't false-positive on stray `{{` in prose.
/// - `\$\{\{` matches GitHub Actions expression syntax `${{ secrets.X }}`.
fn detection_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\$\{\{)|(\{\{[-\s]?[\.\w])").expect("detection regex must compile")
    })
}

/// The substitution regex — broader than the detection regex because we want
/// to capture *every* `{{...}}` / `${{...}}` block, including ones whose body
/// starts with a non-word character (e.g. `{{ /* comment */ }}`). The match is
/// non-greedy on the body so adjacent blocks on the same line round-trip
/// correctly.
fn substitution_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$?\{\{.*?\}\}").expect("substitution regex must compile"))
}

/// Detect the first template marker in `bytes`.
///
/// Returns `None` if no marker is found. The match is line-based: the
/// returned [`TemplateMarker::line`] is 1-indexed and the `snippet` is the
/// matching line trimmed and truncated to ~80 characters.
#[must_use]
pub fn detect_templates(bytes: &[u8]) -> Option<TemplateMarker> {
    let re = detection_regex();
    let m = re.find(bytes)?;
    let match_start = m.start();

    // Compute the 1-based line number by counting `\n` in the prefix.
    let line_number = bytes[..match_start].iter().filter(|&&b| b == b'\n').count() as u32 + 1;

    // Find the line bounds.
    let line_start = bytes[..match_start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let line_end = bytes[match_start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| match_start + p);

    let raw_line = &bytes[line_start..line_end];
    let snippet = String::from_utf8_lossy(raw_line)
        .trim()
        .chars()
        .take(80)
        .collect::<String>();

    Some(TemplateMarker {
        line: line_number,
        snippet,
    })
}

/// Replace every `{{...}}` / `${{...}}` block in `bytes` with a unique
/// placeholder of the form `__DQ_TPL_<N>__` (N is a 0-based, stable counter).
///
/// Returns the substituted bytes and a map from placeholder → original
/// substring. Use [`restore_placeholders`] to reverse the substitution.
///
/// Quotes around a block are NOT included in the placeholder — `"{{ .x }}"`
/// becomes `"__DQ_TPL_0__"`, preserving the YAML/JSON-level quoting so the
/// downstream parser sees a syntactically valid string scalar.
#[must_use]
pub fn substitute_placeholders(bytes: &[u8]) -> (Vec<u8>, HashMap<String, String>) {
    let re = substitution_regex();
    let mut out = Vec::with_capacity(bytes.len());
    let mut map = HashMap::new();
    let mut last_end = 0usize;

    for (counter, m) in re.find_iter(bytes).enumerate() {
        out.extend_from_slice(&bytes[last_end..m.start()]);
        let placeholder = format!("__DQ_TPL_{counter}__");
        out.extend_from_slice(placeholder.as_bytes());
        // The original block bytes are not guaranteed UTF-8 in pathological
        // inputs, but YAML/JSON template files in practice are; round-trip
        // via lossy decode is acceptable here because the placeholder map is
        // only used to reconstruct text we have already produced.
        let original = String::from_utf8_lossy(m.as_bytes()).into_owned();
        map.insert(placeholder, original);
        last_end = m.end();
    }
    out.extend_from_slice(&bytes[last_end..]);

    (out, map)
}

/// Restore every placeholder in `bytes` to its original template block.
///
/// Iterates the map and replaces all occurrences of each placeholder. Because
/// placeholders are constructed with a unique counter and an unambiguous
/// `__DQ_TPL_*__` prefix, no overlap or collision is possible across the map.
#[must_use]
pub fn restore_placeholders(bytes: &[u8], map: &HashMap<String, String>) -> Vec<u8> {
    // Decode lossily — placeholders are pure ASCII, so any non-UTF-8 bytes in
    // the surrounding text round-trip through the lossy path unchanged in the
    // common case (and template files are UTF-8 in practice).
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    for (placeholder, original) in map {
        text = text.replace(placeholder, original);
    }
    text.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_templates_finds_helm_style_marker() {
        let src = b"image:\n  tag: {{ .Values.image.tag }}\n";
        let marker = detect_templates(src).expect("marker present");
        assert_eq!(marker.line, 2);
        assert!(
            marker.snippet.contains("{{ .Values.image.tag }}"),
            "snippet missing template: {:?}",
            marker.snippet
        );
    }

    #[test]
    fn detect_templates_finds_github_actions_marker() {
        let src = b"jobs:\n  run:\n    secrets: ${{ secrets.GITHUB_TOKEN }}\n";
        let marker = detect_templates(src).expect("marker present");
        assert_eq!(marker.line, 3);
        assert!(marker.snippet.contains("${{ secrets.GITHUB_TOKEN }}"));
    }

    #[test]
    fn detect_templates_negative_on_plain_yaml() {
        let src = b"image:\n  tag: 1.2.3\n  pullPolicy: IfNotPresent\n";
        assert!(detect_templates(src).is_none());
    }

    #[test]
    fn detect_templates_ignores_lone_double_brace_in_prose() {
        // `{{` followed by whitespace-only or `}}` shouldn't false-positive.
        let src = b"description: |\n  Use {{}} as a placeholder syntax.\n";
        assert!(detect_templates(src).is_none());
    }

    #[test]
    fn substitute_then_restore_is_byte_equal() {
        let original: &[u8] = b"image:\n  tag: \"{{ .Values.tag }}\"\n  repo: {{- .Values.repo }}\n  token: ${{ secrets.X }}\n";
        let (substituted, map) = substitute_placeholders(original);
        assert_ne!(
            substituted, original,
            "expected substitution to alter bytes"
        );
        assert_eq!(map.len(), 3, "expected three template blocks");
        let restored = restore_placeholders(&substituted, &map);
        assert_eq!(
            restored,
            original,
            "round-trip must be byte-equal\nsubstituted: {}\nrestored: {}",
            String::from_utf8_lossy(&substituted),
            String::from_utf8_lossy(&restored),
        );
    }

    #[test]
    fn substitute_preserves_surrounding_quotes() {
        let original: &[u8] = b"tag: \"{{ .Values.tag }}\"\n";
        let (substituted, _map) = substitute_placeholders(original);
        let s = std::str::from_utf8(&substituted).expect("ascii output");
        assert!(
            s.contains("\"__DQ_TPL_0__\""),
            "quotes must remain around the placeholder, got: {s}"
        );
    }
}
