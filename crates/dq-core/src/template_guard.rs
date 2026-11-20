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

    let snippet = snippet_of(&bytes[line_start..line_end]);

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
            let held = Held::new(bytes);
            let placeheld = parse(&held.text).map_err(|_| refuse())?;
            let positions = as_is.format() == crate::FormatTag::Markdown;
            if as_is.is_multi() != placeheld.is_multi() {
                return Err(refuse());
            }
            if same_reading(as_is.value(), placeheld.value(), positions) {
                return Ok(as_is);
            }
            // Cite a template that changes the reading: the first whose
            // placeholder sits where the two readings first differ.
            let culprit = differing_blocks(as_is.value(), placeheld.value(), positions)
                .and_then(|found| found.into_iter().min())
                .and_then(|n| held.blocks.get(n))
                .and_then(|block| marker_on_line(bytes, block.line));
            Err(culprit.map_or_else(refuse, crate::Error::templated_file))
        }
    }
}

/// What the guard's placeholders start with: letters only, so that no
/// format reads one as anything but plain text (Markdown reads the
/// underscores of `__DQ_TPL_0__` as strong emphasis), and a placeholder
/// is a word in YAML, a bare key in TOML.
const HELD_PREFIX: &str = "DQtplxq";
/// What the guard's placeholders end with.
const HELD_SUFFIX: &str = "qx";

/// The text a reading is compared in, with every template block of the
/// file swapped for a placeholder `DQtplxq<N>qx` — the guard's own; the
/// `--raw-template-strings` ones are [`substitute_placeholders`].
struct Held {
    /// The text with placeholders.
    text: Vec<u8>,
    /// Every block in order, `N` of placeholder `N`.
    blocks: Vec<HeldBlock>,
}

/// A template block of the text [`Held`] holds.
struct HeldBlock {
    /// The block as written.
    text: String,
    /// Its 1-based line.
    line: u32,
    /// Where its placeholder starts in [`Held::text`].
    at: usize,
}

impl Held {
    fn new(bytes: &[u8]) -> Self {
        let mut text = Vec::with_capacity(bytes.len());
        let mut blocks = Vec::new();
        let mut last_end = 0usize;
        let mut line = 1u32;
        for (n, m) in substitution_regex().find_iter(bytes).enumerate() {
            let before = &bytes[last_end..m.start()];
            line +=
                u32::try_from(before.iter().filter(|&&b| b == b'\n').count()).unwrap_or(u32::MAX);
            text.extend_from_slice(before);
            blocks.push(HeldBlock {
                text: String::from_utf8_lossy(m.as_bytes()).into_owned(),
                line,
                at: text.len(),
            });
            text.extend_from_slice(format!("{HELD_PREFIX}{n}{HELD_SUFFIX}").as_bytes());
            last_end = m.end();
        }
        text.extend_from_slice(&bytes[last_end..]);
        Self { text, blocks }
    }
}

/// The guard's placeholders in a string.
fn held_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!("{HELD_PREFIX}([0-9]+){HELD_SUFFIX}"))
            .expect("placeholder regex must compile")
    })
}

/// A template block in a string as read — the parser has already taken
/// the quoting's escapes out, and an escaped `\n` is a line break.
fn read_block_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)\$?\{\{.*?\}\}").expect("block regex must compile"))
}

/// The numbers of the guard's placeholders in `s`, in order.
fn held_in(s: &str) -> Vec<usize> {
    held_regex()
        .captures_iter(s.as_bytes())
        .filter_map(|c| std::str::from_utf8(&c[1]).ok()?.parse().ok())
        .collect()
}

/// Mark that stands for one template block in [`same_text`].
const BLOCK_MARK: &str = "\u{0}";

/// Whether `as_is`, a string read from the file as it is, and `held`, the
/// same string read with placeholders, say the same once each template
/// block is set aside — every placeholder of `held` standing where `as_is`
/// holds one block. The block's text is not compared: the parser reads
/// the escapes of a quoted string (`''`, `\"`) out of the block in
/// `as_is`, and the placeholder stood for the text before that.
fn same_text(as_is: &str, held: &str) -> bool {
    if as_is == held {
        return true;
    }
    let as_is = read_block_regex().replace_all(as_is.as_bytes(), BLOCK_MARK.as_bytes());
    let held = held_regex().replace_all(held.as_bytes(), BLOCK_MARK.as_bytes());
    let held = read_block_regex().replace_all(&held, BLOCK_MARK.as_bytes());
    as_is == held
}

/// Whether `as_is` and `held` (read with placeholders) hold the same data,
/// strings and keys compared by [`same_text`], map entries in order. With
/// `positions` the `position` member of a map — where the Markdown tree
/// keeps a node's line and column, which a placeholder longer or shorter
/// than its block moves — is not compared.
fn same_reading(as_is: &crate::Value, held: &crate::Value, positions: bool) -> bool {
    use crate::Value;
    match (as_is, held) {
        (Value::String(a), Value::String(h)) => same_text(a, h),
        (Value::Array(a), Value::Array(h)) => {
            a.len() == h.len() && a.iter().zip(h).all(|(a, h)| same_reading(a, h, positions))
        }
        (Value::Map(a), Value::Map(h)) => {
            let kept = |(key, _): &(&String, &Value)| !(positions && key.as_str() == "position");
            let a: Vec<_> = a.iter().filter(kept).collect();
            let h: Vec<_> = h.iter().filter(kept).collect();
            a.len() == h.len()
                && a.iter().zip(&h).all(|((ak, av), (hk, hv))| {
                    same_text(ak, hk) && same_reading(av, hv, positions)
                })
        }
        (a, h) => a.same_data(h),
    }
}

/// The guard's placeholders (their numbers) in the first part of `held`
/// that reads differently from `as_is`, walking both in document order;
/// `None` when they read the same. A string, or a key, that differs gives
/// its own placeholders; a collection whose length or kind differs gives
/// all of those inside it.
fn differing_blocks(
    as_is: &crate::Value,
    held: &crate::Value,
    positions: bool,
) -> Option<Vec<usize>> {
    use crate::Value;
    if same_reading(as_is, held, positions) {
        return None;
    }
    match (as_is, held) {
        (Value::String(_), Value::String(h)) => Some(held_in(h)),
        (Value::Array(a), Value::Array(h)) if a.len() == h.len() => a
            .iter()
            .zip(h)
            .find_map(|(a, h)| differing_blocks(a, h, positions)),
        (Value::Map(a), Value::Map(h)) => {
            let kept = |(key, _): &(&String, &Value)| !(positions && key.as_str() == "position");
            let a: Vec<_> = a.iter().filter(kept).collect();
            let h: Vec<_> = h.iter().filter(kept).collect();
            if a.len() != h.len() {
                return Some(blocks_within(held));
            }
            a.iter().zip(&h).find_map(|((ak, av), (hk, hv))| {
                if same_text(ak, hk) {
                    differing_blocks(av, hv, positions)
                } else {
                    let mut found = held_in(hk);
                    found.extend(blocks_within(hv));
                    Some(found)
                }
            })
        }
        _ => Some(blocks_within(held)),
    }
}

/// The guard's placeholders in every string and key of `v`.
fn blocks_within(v: &crate::Value) -> Vec<usize> {
    use crate::Value;
    match v {
        Value::String(s) => held_in(s),
        Value::Array(items) => items.iter().flat_map(blocks_within).collect(),
        Value::Map(map) => map
            .iter()
            .flat_map(|(k, v)| held_in(k).into_iter().chain(blocks_within(v)))
            .collect(),
        _ => Vec::new(),
    }
}

/// A [`TemplateMarker`] for 1-based `line` of `bytes`, as
/// [`detect_templates`] makes one; `None` past the last line.
fn marker_on_line(bytes: &[u8], line: u32) -> Option<TemplateMarker> {
    let raw = bytes
        .split(|&b| b == b'\n')
        .nth(usize::try_from(line).ok()?.checked_sub(1)?)?;
    Some(TemplateMarker {
        line,
        snippet: snippet_of(raw),
    })
}

/// A line as a [`TemplateMarker::snippet`]: trimmed, at most 80 characters.
fn snippet_of(raw_line: &[u8]) -> String {
    String::from_utf8_lossy(raw_line)
        .trim()
        .chars()
        .take(80)
        .collect()
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

/// The template guard of one write: what [`parse_for_write`] decided, and
/// what [`WriteGuard::finish`] checks or restores once the edit is made.
#[derive(Debug, Default)]
pub struct WriteGuard {
    /// `--raw-template-strings`: the placeholders to put back in the text.
    placeholders: Option<HashMap<String, String>>,
    /// The guard let templates through: the data and the bytes as read, to
    /// check the written text against.
    kept: Option<(crate::Value, Vec<u8>)>,
}

/// Parse `original` for a write command, running the template guard as
/// `mode` says.
///
/// - [`TemplateMode::Guard`]: [`guarded_parse`] — a file whose templates
///   change what the parser reads is refused; one whose templates do not
///   is parsed as it is, and [`WriteGuard::finish`] then checks that the
///   edit kept them.
/// - [`TemplateMode::Allow`]: parsed as it is, no check.
/// - [`TemplateMode::RawStrings`]: parsed with placeholders, which
///   [`WriteGuard::finish`] puts back as the template text.
///
/// # Errors
///
/// [`crate::Error::TemplatedFile`] from the guard; any error of `parse`.
pub fn parse_for_write(
    original: &[u8],
    mode: TemplateMode,
    parse: impl Fn(&[u8]) -> crate::Result<crate::Document>,
) -> crate::Result<(crate::Document, WriteGuard)> {
    match mode {
        TemplateMode::RawStrings => {
            let (substituted, map) = substitute_placeholders(original);
            let doc = parse(&substituted)?;
            Ok((
                doc,
                WriteGuard {
                    placeholders: Some(map),
                    kept: None,
                },
            ))
        }
        TemplateMode::Allow => Ok((parse(original)?, WriteGuard::default())),
        TemplateMode::Guard => {
            let doc = guarded_parse(original, TemplateMode::Guard, &parse)?;
            let kept = detect_templates(original)
                .is_some()
                .then(|| (doc.value().clone(), original.to_vec()));
            Ok((
                doc,
                WriteGuard {
                    placeholders: None,
                    kept,
                },
            ))
        }
    }
}

impl WriteGuard {
    /// The bytes to write for the edited `written` text.
    ///
    /// Under `--raw-template-strings` the placeholders are put back. When
    /// the guard let templates through, [`check_templates_kept`] runs:
    /// `edited` are the pointers the edit set, removed or replaced (their
    /// templates may go), `textual` says the edit spliced the text rather
    /// than rewriting it through the format's writer, and `parse` reads the
    /// written bytes back.
    ///
    /// # Errors
    ///
    /// [`crate::Error::TemplatedFile`] when the edit would not keep a
    /// template as it was.
    pub fn finish(
        &self,
        written: Vec<u8>,
        edited: &[crate::Pointer],
        textual: bool,
        parse: impl Fn(&[u8]) -> crate::Result<crate::Document>,
    ) -> crate::Result<Vec<u8>> {
        if let Some(map) = &self.placeholders {
            return Ok(restore_placeholders(&written, map));
        }
        if let Some((before, original)) = &self.kept {
            check_templates_kept(before, edited, original, textual, &written, parse)?;
        }
        Ok(written)
    }
}

/// Check that a write keeps the templates of the data it edited.
///
/// `before` is the data the templates were read in, `edited` the pointers
/// whose subtrees the edit changed on purpose (their templates may go or
/// change), `original` the text `before` was read from, `written` the
/// bytes the edit produced, `parse` how to read them.
///
/// 1. The written text must still read the same with its templates as
///    placeholders ([`guarded_parse`]): no edit may turn a template that
///    lived inside a value into one that changes the reading.
/// 2. Every string (value or key) of `before` outside `edited` that holds a
///    template must read back from `written` as the very same string — a
///    writer that re-quotes or escapes a template must leave its data alone.
///    Strings are counted, not located, so items that move up an array
///    after a removal still match.
/// 3. With `textual` — the edit spliced `original` rather than rewriting
///    it — every template block of `original` must stand in `written` byte
///    for byte, but for the blocks in the nodes under `edited` and in the
///    comments on the lines those nodes take up. A block is told apart by
///    where it stands in `original`, not by its text: a quoted block reads
///    as other text than it is written as (`'${{ f(''x'') }}'`).
///
/// # Errors
///
/// [`crate::Error::TemplatedFile`] naming the line in `original` of the
/// first template not kept.
pub fn check_templates_kept(
    before: &crate::Value,
    edited: &[crate::Pointer],
    original: &[u8],
    textual: bool,
    written: &[u8],
    parse: impl Fn(&[u8]) -> crate::Result<crate::Document>,
) -> crate::Result<()> {
    let edited: Vec<String> = edited.iter().map(crate::Pointer::as_canonical).collect();
    let held = Held::new(original);
    let held_doc = parse(&held.text).ok();
    let mut found = Found::default();
    collect(
        before,
        held_doc.as_ref().map(crate::Document::value),
        &mut Pointers::default(),
        &edited,
        &mut found,
    );

    let after = guarded_parse(written, TemplateMode::Guard, &parse).map_err(|err| match err {
        crate::Error::TemplatedFile { line, snippet, .. } => not_kept(line, snippet),
        other => other,
    })?;
    let mut read_back = Found::default();
    collect(
        after.value(),
        None,
        &mut Pointers::default(),
        &[],
        &mut read_back,
    );
    let mut read_back = counts(read_back.strings.into_iter().map(|s| s.text).collect());
    for s in found.strings.iter().filter(|s| !s.edited) {
        if !take(&mut read_back, &s.text) {
            let line = s
                .held
                .first()
                .and_then(|&n| held.blocks.get(n))
                .map(|b| b.line);
            return Err(not_kept_in(written, original, line, &s.text));
        }
    }

    if textual {
        check_blocks_kept(&held, held_doc.as_ref(), &found, original, written)?;
    }
    Ok(())
}

/// Step 3 of [`check_templates_kept`]: every block of `original` (`held`,
/// read as `held_doc`) the edit did not target stands in `written`.
fn check_blocks_kept(
    held: &Held,
    held_doc: Option<&crate::Document>,
    found: &Found,
    original: &[u8],
    written: &[u8],
) -> crate::Result<()> {
    let original_lines: Vec<&[u8]> = original.split(|&b| b == b'\n').collect();
    let written_lines: std::collections::HashSet<&[u8]> = written.split(|&b| b == b'\n').collect();
    // Whether the line `block` is on in `original` is gone from `written`.
    let line_gone = |block: &HeldBlock| {
        original_lines
            .get(block.line as usize - 1)
            .is_some_and(|line| !written_lines.contains(line))
    };
    let mut exempt = vec![false; held.blocks.len()];
    if let Some(doc) = held_doc {
        let mut in_data = vec![false; held.blocks.len()];
        for s in &found.strings {
            for &n in &s.held {
                if let Some(slot) = in_data.get_mut(n) {
                    *slot = true;
                    exempt[n] |= s.edited;
                }
            }
        }
        // A block in a comment goes with the node whose lines it is on —
        // and, when the edit removed its line, with the node's key line and
        // the comments above it: the lines between the content before the
        // node and the node.
        let mut regions = Vec::new();
        for edited in &found.edited_nodes {
            let (inside, outside): (Vec<_>, Vec<_>) = doc
                .spans()
                .iter()
                .partition(|(pointer, _)| under(pointer, edited));
            let Some(start) = inside.iter().map(|(_, s)| s.line_range.start).min() else {
                continue;
            };
            let end = inside
                .iter()
                .map(|(_, s)| s.line_range.end)
                .max()
                .unwrap_or(start);
            let before = outside
                .iter()
                .map(|(_, s)| s.line_range.end)
                .filter(|&e| e <= start)
                .max()
                .unwrap_or(0);
            regions.push((before, start, end));
        }
        for (n, block) in held.blocks.iter().enumerate() {
            if in_data[n] {
                continue;
            }
            for &(before, start, end) in &regions {
                if (start..end).contains(&block.at)
                    || ((before..start).contains(&block.at) && line_gone(block))
                {
                    exempt[n] = true;
                }
            }
        }
    } else {
        // No reading with placeholders to tell the blocks apart: exempt
        // one block of each text an edited string holds.
        for s in found.strings.iter().filter(|s| s.edited) {
            for text in blocks(s.text.as_bytes()) {
                if let Some(n) =
                    (0..held.blocks.len()).find(|&n| !exempt[n] && held.blocks[n].text == text)
                {
                    exempt[n] = true;
                }
            }
        }
    }

    let mut left = counts(blocks(written));
    let mut lost = Vec::new();
    for (n, block) in held.blocks.iter().enumerate() {
        if !exempt[n] && !take(&mut left, &block.text) {
            lost.push(block.text.as_str());
        }
    }
    let Some(&first) = lost.first() else {
        return Ok(());
    };
    // Counted, a lost block is one of those with its text: name the first
    // one whose line is gone from `written`.
    let candidates = held
        .blocks
        .iter()
        .enumerate()
        .filter(|(n, b)| !exempt[*n] && lost.contains(&b.text.as_str()));
    let gone = candidates.clone().find(|(_, b)| line_gone(b));
    let line = gone
        .or_else(|| candidates.clone().next())
        .map(|(_, b)| b.line);
    Err(not_kept_in(written, original, line, first))
}

/// Whether `pointer` is `edited` or under it.
fn under(pointer: &str, edited: &str) -> bool {
    edited.is_empty()
        || pointer == edited
        || pointer
            .strip_prefix(edited)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Every template block (`{{ … }}`, `${{ … }}`) of `bytes`, in order.
fn blocks(bytes: &[u8]) -> Vec<String> {
    substitution_regex()
        .find_iter(bytes)
        .map(|m| String::from_utf8_lossy(m.as_bytes()).into_owned())
        .collect()
}

/// A string (value or key) that holds a template.
struct TemplateString {
    /// The string as read.
    text: String,
    /// Under an edited pointer.
    edited: bool,
    /// The guard's placeholders the string holds when read with them.
    held: Vec<usize>,
}

/// What [`collect`] gathers.
#[derive(Default)]
struct Found {
    strings: Vec<TemplateString>,
    /// The pointers, in the reading with placeholders, of the nodes an
    /// edited pointer names.
    edited_nodes: Vec<String>,
}

/// The pointer of a node as read, and of the same node read with
/// placeholders — where a key holds a template the two differ.
#[derive(Default)]
struct Pointers {
    as_is: String,
    held: String,
}

impl Pointers {
    fn push(&mut self, as_is: &str, held: &str) -> (usize, usize) {
        let lens = (self.as_is.len(), self.held.len());
        for (pointer, segment) in [(&mut self.as_is, as_is), (&mut self.held, held)] {
            pointer.push('/');
            pointer.push_str(&segment.replace('~', "~0").replace('/', "~1"));
        }
        lens
    }

    fn truncate(&mut self, (as_is, held): (usize, usize)) {
        self.as_is.truncate(as_is);
        self.held.truncate(held);
    }
}

/// Gather into `found` the strings and keys of `value` that hold a template
/// block, each with the placeholders of its counterpart in `held` (the same
/// data read with placeholders, walked alongside while its shape matches),
/// and the nodes `edited` names.
fn collect(
    value: &crate::Value,
    held: Option<&crate::Value>,
    at: &mut Pointers,
    edited: &[String],
    found: &mut Found,
) {
    use crate::Value;
    if held.is_some() && edited.contains(&at.as_is) {
        found.edited_nodes.push(at.held.clone());
    }
    match value {
        Value::String(s) => {
            let h = match held {
                Some(Value::String(h)) => Some(h.as_str()),
                _ => None,
            };
            keep(found, edited, s, h, &at.as_is);
        }
        Value::Array(items) => {
            let held = match held {
                Some(Value::Array(h)) if h.len() == items.len() => Some(h),
                _ => None,
            };
            for (i, item) in items.iter().enumerate() {
                let index = i.to_string();
                let lens = at.push(&index, &index);
                collect(item, held.map(|h| &h[i]), at, edited, found);
                at.truncate(lens);
            }
        }
        Value::Map(entries) => {
            let held = match held {
                Some(Value::Map(h)) if h.len() == entries.len() => Some(h),
                _ => None,
            };
            for (i, (key, item)) in entries.iter().enumerate() {
                let held_entry = held.and_then(|h| h.get_index(i));
                let lens = at.push(key, held_entry.map_or(key.as_str(), |(k, _)| k.as_str()));
                let pointer = at.as_is.clone();
                keep(
                    found,
                    edited,
                    key,
                    held_entry.map(|(k, _)| k.as_str()),
                    &pointer,
                );
                collect(item, held_entry.map(|(_, v)| v), at, edited, found);
                at.truncate(lens);
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

/// Add `text`, read with placeholders as `held`, at `pointer` to `found`
/// when it holds a template block.
fn keep(found: &mut Found, edited: &[String], text: &str, held: Option<&str>, pointer: &str) {
    if read_block_regex().is_match(text.as_bytes()) {
        found.strings.push(TemplateString {
            text: text.to_owned(),
            edited: edited.iter().any(|e| under(pointer, e)),
            held: held
                .filter(|h| same_text(text, h))
                .map(held_in)
                .unwrap_or_default(),
        });
    }
}

/// `items` as a count per distinct item.
fn counts(items: Vec<String>) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for item in items {
        *map.entry(item).or_insert(0) += 1;
    }
    map
}

/// Take one `item` out of `counts`; `false` when there is none left.
fn take(counts: &mut HashMap<String, usize>, item: &str) -> bool {
    match counts.get_mut(item) {
        Some(n) if *n > 0 => {
            *n -= 1;
            true
        }
        _ => false,
    }
}

/// [`crate::Error::TemplatedFile`] for a template `s` the edit would not
/// keep, at `line` of `original` when it is known; else at the line of its
/// first block in `original` (or in `written`, or line 1).
fn not_kept_in(written: &[u8], original: &[u8], line: Option<u32>, s: &str) -> crate::Error {
    let line_of = |text: &[u8], line: usize| {
        String::from_utf8_lossy(text)
            .lines()
            .nth(line.checked_sub(1)?)
            .map(|l| l.trim().chars().take(80).collect::<String>())
    };
    if let Some(line) = line
        && let Some(snippet) = line_of(original, line as usize)
    {
        return not_kept(line, snippet);
    }
    let block = blocks(s.as_bytes())
        .into_iter()
        .next()
        .unwrap_or_else(|| s.to_owned());
    let locate = |text: &[u8]| {
        let at = String::from_utf8_lossy(text).find(&block)?;
        let line = String::from_utf8_lossy(&text[..at]).matches('\n').count() + 1;
        Some((
            u32::try_from(line).unwrap_or(u32::MAX),
            line_of(text, line)?,
        ))
    };
    let (line, snippet) = locate(original)
        .or_else(|| locate(written))
        .unwrap_or((1, block.chars().take(80).collect()));
    not_kept(line, snippet)
}

/// The refusal of an edit that would not keep a template as it was.
fn not_kept(line: u32, snippet: String) -> crate::Error {
    crate::Error::TemplatedFile {
        line,
        snippet,
        hint: "the edit would not keep this template as written; edit the file by hand, \
               or use --raw-template-strings to treat template values as opaque strings"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(bytes: &[u8]) -> crate::Result<crate::Document> {
        crate::by_name("yaml").expect("yaml format").parse(bytes)
    }

    fn yaml_stream(bytes: &[u8]) -> crate::Result<crate::Document> {
        crate::parse_yaml_stream_with_spans(bytes)
    }

    fn before(text: &str) -> crate::Value {
        yaml(text.as_bytes()).expect("parses").value().clone()
    }

    fn ptr(s: &str) -> crate::Pointer {
        crate::Pointer::parse(s).expect("pointer")
    }

    #[test]
    fn kept_templates_pass_and_edited_ones_may_go() {
        let text = "a: \"${{ x }}\"\nl:\n  - run ${{ y }}\n  - plain\n  - run ${{ z }}\nb: 1\n";
        let data = before(text);
        // `b` changed, every template kept byte for byte.
        let written = text.replace("b: 1", "b: 2");
        check_templates_kept(
            &data,
            &[ptr("/b")],
            text.as_bytes(),
            true,
            written.as_bytes(),
            yaml_stream,
        )
        .expect("kept");
        // `/l/0` removed: its template goes, `/l/2` moves up to `/l/1`.
        let written = text.replace("  - run ${{ y }}\n", "");
        check_templates_kept(
            &data,
            &[ptr("/l/0")],
            text.as_bytes(),
            true,
            written.as_bytes(),
            yaml_stream,
        )
        .expect("removed on purpose");
    }

    #[test]
    fn a_template_whose_data_changes_is_refused() {
        let text = "a: \"${{ x }}\"\nb: 1\n";
        let err = check_templates_kept(
            &before(text),
            &[ptr("/b")],
            text.as_bytes(),
            false,
            b"a: \"${{ x  }}\"\nb: 2\n",
            yaml_stream,
        )
        .expect_err("refused");
        assert!(
            matches!(err, crate::Error::TemplatedFile { line: 1, .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("would not keep"), "{err}");
    }

    #[test]
    fn a_template_written_where_it_changes_the_reading_is_refused() {
        let text = "a: \"{{ .x }}\"\n";
        let err = check_templates_kept(
            &before(text),
            &[],
            text.as_bytes(),
            false,
            b"a: {{ .x }}\n",
            yaml_stream,
        )
        .expect_err("refused");
        assert!(matches!(err, crate::Error::TemplatedFile { .. }), "{err:?}");
    }

    #[test]
    fn a_textual_edit_must_keep_template_text_byte_for_byte() {
        // The same data, written with other quoting: fine as data, not as
        // text.
        let text = "a: '${{ f(''x'') }}'\nb: 1\n";
        let written = b"a: \"${{ f('x') }}\"\nb: 2\n";
        check_templates_kept(
            &before(text),
            &[ptr("/b")],
            text.as_bytes(),
            false,
            written,
            yaml_stream,
        )
        .expect("same data");
        let err = check_templates_kept(
            &before(text),
            &[ptr("/b")],
            text.as_bytes(),
            true,
            written,
            yaml_stream,
        )
        .expect_err("text changed");
        assert!(
            matches!(err, crate::Error::TemplatedFile { line: 1, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn guarded_parse_passes_templates_whose_quoting_has_escapes() {
        let doc = guarded_parse(
            b"a: '${{ f(''x'') }}'\nb: \"${{ g(\\\"y\\\") }} and ${{ h }}\"\n",
            TemplateMode::Guard,
            yaml,
        )
        .expect("passes");
        let crate::Value::Map(m) = doc.value() else {
            panic!("map")
        };
        assert_eq!(
            m.get("a"),
            Some(&crate::Value::String("${{ f('x') }}".into()))
        );
        let json = |b: &[u8]| crate::by_name("json").expect("json").parse(b);
        guarded_parse(br#"{"a": "${{ g(\"y\") }}"}"#, TemplateMode::Guard, json).expect("passes");
        // Two blocks read as one string where the file holds two: refused.
        assert!(!same_text("{{ a }}", "DQtplxq0qxDQtplxq1qx"));
        assert!(same_text(
            "x {{ a }} y {{ b }}",
            "x DQtplxq0qx y DQtplxq1qx"
        ));
    }

    #[test]
    fn guarded_parse_leaves_markdown_positions_out() {
        let md = |b: &[u8]| crate::by_name("markdown").expect("markdown").parse(b);
        guarded_parse(
            b"# T {{ .x }}\n\nSee `{{ .Values.long.path }}` and ${{ github.ref }}.\n",
            TemplateMode::Guard,
            md,
        )
        .expect("templates in text");
        let err = guarded_parse(b"---\nname: {{ .n }}\n---\n# T\n", TemplateMode::Guard, md)
            .expect_err("a template that changes the frontmatter");
        assert!(
            matches!(err, crate::Error::TemplatedFile { line: 2, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_refusal_names_the_line_of_the_template_lost() {
        // The same block on lines 1 and 3; the one on line 3 is lost.
        let text = "a: ${{ m }}\nb: 1\nc: ${{ m }} build\n";
        let written = b"a: ${{ m }}\nb: 2\nc: x build\n";
        for textual in [false, true] {
            let err = check_templates_kept(
                &before(text),
                &[ptr("/b")],
                text.as_bytes(),
                textual,
                written,
                yaml_stream,
            )
            .expect_err("lost");
            assert!(
                matches!(&err, crate::Error::TemplatedFile { line: 3, snippet, .. }
                    if snippet == "c: ${{ m }} build"),
                "{err:?}"
            );
        }
    }

    #[test]
    fn parse_for_write_modes() {
        let helm = b"port: {{ .p }}\n";
        assert!(matches!(
            parse_for_write(helm, TemplateMode::Guard, yaml),
            Err(crate::Error::TemplatedFile { .. })
        ));
        let (_, guard) = parse_for_write(helm, TemplateMode::RawStrings, yaml).expect("parses");
        assert_eq!(
            guard
                .finish(b"port: __DQ_TPL_0__\n".to_vec(), &[], true, yaml)
                .expect("restored"),
            helm
        );
        let (_, guard) = parse_for_write(helm, TemplateMode::Allow, yaml).expect("parses");
        assert_eq!(
            guard.finish(b"x".to_vec(), &[], true, yaml).expect("as is"),
            b"x"
        );
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
    fn a_refusal_cites_the_template_that_changes_the_reading() {
        // The first template reads the same either way; the refusal names
        // the one whose value differs, not the first.
        for (text, line) in [
            (
                "run: echo ${{ github.ref }}\nq: \"{{ .x }}\"\nport: {{ .p }}\n",
                3,
            ),
            ("a: \"{{ .x }}\"\nb:\n  - x\n  - {{ .y }}\n", 4),
            ("a: \"{{ .x }}\"\n{{ .k }}: v\n", 2),
            ("a: \"{{ .x }}\"\nb: [1, {{ .y }}]\n", 2),
        ] {
            let err =
                guarded_parse(text.as_bytes(), TemplateMode::Guard, yaml).expect_err("refused");
            assert!(
                matches!(&err, crate::Error::TemplatedFile { line: l, .. } if *l == line),
                "{text:?}: {err:?}"
            );
        }
        // When the file does not parse as it is, nothing tells which
        // template is to blame: the first one is cited.
        let err = guarded_parse(
            b"a: \"{{ .x }}\"\n{{- if .y }}\nb: 1\n",
            TemplateMode::Guard,
            yaml,
        )
        .expect_err("refused");
        assert!(
            matches!(err, crate::Error::TemplatedFile { line: 1, .. }),
            "{err:?}"
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
