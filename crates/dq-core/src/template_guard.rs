//! Helm / Go-template / GitHub Actions expression guard.
//!
//! YAML and JSON parsers cannot parse files that contain raw template
//! expressions like `{{ .Values.image.tag }}` or `${{ secrets.GITHUB_TOKEN }}`
//! — the parsers will report cryptic syntax errors. This module provides two
//! complementary tools (`add-safe-writes` D6):
//!
//! 1. [`detect_templates`] — a fast regex check that returns the first marker
//!    seen, used by the CLI's write handlers (`set`, `del`, `patch`,
//!    `merge`) to short-circuit with a helpful "this looks like a Helm
//!    chart, did you mean `--allow-templates` or `--raw-template-strings`?"
//!    error, and by [`guarded_parse`], the guard of every command that only
//!    reads a file.
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
/// - `\{\{-?[ \t]*[.\w$]` matches typical Go-template / Helm syntax. The
///   `-?` allows the whitespace-trim marker `{{-`, then any run of blanks
///   (`{{- if .x }}`, not only `{{-.x}}` or `{{ .x }}`); the `[.\w$]`
///   requires a `.`, a word character or a `$` variable so we don't
///   false-positive on stray `{{` in prose.
/// - `\$\{\{` matches GitHub Actions expression syntax `${{ secrets.X }}`.
fn detection_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\$\{\{)|(\{\{-?[ \t]*[.\w$])").expect("detection regex must compile")
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

/// How a command that reads a file treats the templates in it — the guard
/// by default, or one of the two escape hatches the global flags select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TemplateMode {
    /// Refuse a file whose templates change what the parser reads
    /// ([`guarded_parse`]).
    #[default]
    Guard,
    /// `--allow-templates`: parse the bytes as they are.
    Allow,
    /// `--raw-template-strings`: read every template block as the string
    /// it is written as.
    RawStrings,
}

/// Parse `bytes` with `parse`, treating the templates in them as `mode`
/// says — the guard every command that only reads a file runs.
///
/// - [`TemplateMode::Guard`]: bytes without a template marker are parsed
///   as they are. With one, the bytes are parsed twice — as they are, and
///   with every template block swapped for a placeholder. When both parse
///   and hold the same data once the template text is put back in place of
///   each placeholder, the templates do not change the reading (a GitHub
///   Actions `${{ … }}` inside a plain scalar, a `{{ … }}` inside a quoted
///   string) and the as-is parse is returned. Otherwise — a Helm
///   `{{ .x }}` YAML reads as a flow mapping, or a parse that fails — the
///   file is refused with [`crate::Error::TemplatedFile`].
/// - [`TemplateMode::Allow`]: parsed as they are, no check.
/// - [`TemplateMode::RawStrings`]: parsed with placeholders, and the
///   template text put back into every string and key of the result. The
///   document's bytes and spans stay those of the substituted text: lines
///   match the file (a block never spans a line break), columns after a
///   block on its line do not.
///
/// # Errors
///
/// [`crate::Error::TemplatedFile`] as above; any error `parse` returns for
/// the bytes it is handed in the `Allow` and `RawStrings` modes, and for
/// bytes without a template marker.
pub fn guarded_parse(
    bytes: &[u8],
    mode: TemplateMode,
    parse: impl Fn(&[u8]) -> crate::Result<crate::Document>,
) -> crate::Result<crate::Document> {
    match mode {
        TemplateMode::Allow => parse(bytes),
        TemplateMode::RawStrings => {
            let (substituted, map) = substitute_placeholders(bytes);
            let mut doc = parse(&substituted)?;
            if !map.is_empty() {
                restore_in_value(doc.value_mut(), &map);
            }
            Ok(doc)
        }
        TemplateMode::Guard => {
            let Some(marker) = detect_templates(bytes) else {
                return parse(bytes);
            };
            let refuse = || crate::Error::templated_file(marker.clone());
            let as_is = parse(bytes).map_err(|_| refuse())?;
            let (substituted, map) = substitute_placeholders(bytes);
            let mut placeheld = parse(&substituted).map_err(|_| refuse())?;
            restore_in_value(placeheld.value_mut(), &map);
            if as_is.is_multi() == placeheld.is_multi()
                && as_is.value().same_data(placeheld.value())
            {
                Ok(as_is)
            } else {
                Err(refuse())
            }
        }
    }
}

/// Put the template text back in place of every placeholder in the strings
/// and keys of `value` — the read-side counterpart of
/// [`restore_placeholders`], which works on bytes. Key order is kept.
pub fn restore_in_value(value: &mut crate::Value, map: &HashMap<String, String>) {
    use crate::Value;
    match value {
        Value::String(s) => {
            if let Some(restored) = restore_in_str(s, map) {
                *s = restored;
            }
        }
        Value::Array(items) => {
            for item in items {
                restore_in_value(item, map);
            }
        }
        Value::Map(entries) => {
            let keyed = entries.keys().any(|k| k.contains(PLACEHOLDER_PREFIX));
            if keyed {
                let old = std::mem::take(entries);
                for (key, mut item) in old {
                    restore_in_value(&mut item, map);
                    let key = restore_in_str(&key, map).unwrap_or(key);
                    entries.insert(key, item);
                }
            } else {
                for item in entries.values_mut() {
                    restore_in_value(item, map);
                }
            }
        }
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::BigInt(_)
        | Value::Float(_)
        | Value::BigFloat(_) => {}
    }
}

/// What every placeholder starts with.
const PLACEHOLDER_PREFIX: &str = "__DQ_TPL_";

/// `s` with each placeholder replaced by its template text; `None` when `s`
/// holds no placeholder.
fn restore_in_str(s: &str, map: &HashMap<String, String>) -> Option<String> {
    if !s.contains(PLACEHOLDER_PREFIX) {
        return None;
    }
    let mut text = s.to_owned();
    for (placeholder, original) in map {
        if text.contains(placeholder.as_str()) {
            text = text.replace(placeholder.as_str(), original);
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(bytes: &[u8]) -> crate::Result<crate::Document> {
        crate::by_name("yaml").expect("yaml format").parse(bytes)
    }

    #[test]
    fn guarded_parse_refuses_a_template_that_changes_the_reading() {
        let err = guarded_parse(b"a: 1\nport: {{ .p }}\n", TemplateMode::Guard, yaml)
            .expect_err("a flow mapping is not what the author wrote");
        assert!(
            matches!(err, crate::Error::TemplatedFile { line: 2, .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn guarded_parse_refuses_a_template_the_parser_fails_on() {
        let err = guarded_parse(
            b"{{- if .x }}\na: 1\n{{- end }}\n",
            TemplateMode::Guard,
            yaml,
        )
        .expect_err("refused");
        assert!(matches!(err, crate::Error::TemplatedFile { line: 1, .. }));
    }

    #[test]
    fn guarded_parse_passes_a_template_that_reads_the_same() {
        let doc = guarded_parse(
            b"run: echo ${{ github.ref }}\nq: \"{{ .x }}\"\n",
            TemplateMode::Guard,
            yaml,
        )
        .expect("passes");
        let crate::Value::Map(m) = doc.value() else {
            panic!("map")
        };
        assert_eq!(
            m.get("run"),
            Some(&crate::Value::String("echo ${{ github.ref }}".into()))
        );
    }

    #[test]
    fn guarded_parse_raw_strings_reads_templates_as_strings() {
        let doc = guarded_parse(
            b"port: {{ .p }}\n{{ .k }}: v\n",
            TemplateMode::RawStrings,
            yaml,
        )
        .expect("parses");
        let crate::Value::Map(m) = doc.value() else {
            panic!("map")
        };
        assert_eq!(
            m.keys().collect::<Vec<_>>(),
            ["port", "{{ .k }}"],
            "keys restored, order kept"
        );
        assert_eq!(
            m.get("port"),
            Some(&crate::Value::String("{{ .p }}".into()))
        );
    }

    #[test]
    fn guarded_parse_allow_reads_the_bytes_as_they_are() {
        let doc = guarded_parse(b"port: {{ .p }}\n", TemplateMode::Allow, yaml).expect("parses");
        let crate::Value::Map(m) = doc.value() else {
            panic!("map")
        };
        assert!(matches!(m.get("port"), Some(crate::Value::Map(_))));
    }

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
    fn detect_templates_finds_a_trim_marker_followed_by_a_blank() {
        // `{{- if … }}` — the trim marker, a blank, then the action.
        let marker = detect_templates(b"a: 1\n{{- if .Values.x }}\n").expect("marker present");
        assert_eq!(marker.line, 2);
        assert!(detect_templates(b"x: {{ $v := 1 }}\n").is_some());
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
