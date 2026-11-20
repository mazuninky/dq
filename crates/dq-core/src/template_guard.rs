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
//!    (`DQtplxq<N>qx`, with more `x` when the file holds that text) so the
//!    remaining bytes are valid YAML/JSON for the
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
/// placeholder of the form `DQtplxq<N>qx` (N is a 0-based, stable counter;
/// more `x` where `bytes` hold that text already, see [`Marks`]): letters
/// and digits only, so that every format reads it as plain text —
/// Markdown read the underscores of the former `__DQ_TPL_<N>__` as strong
/// emphasis.
///
/// Returns the substituted bytes and a map from placeholder → original
/// substring. Use [`restore_placeholders`] to reverse the substitution.
///
/// Quotes around a block are NOT included in the placeholder — `"{{ .x }}"`
/// becomes `"DQtplxq0qx"`, preserving the YAML/JSON-level quoting so the
/// downstream parser sees a syntactically valid string scalar.
#[must_use]
pub fn substitute_placeholders(bytes: &[u8]) -> (Vec<u8>, HashMap<String, String>) {
    substitute_with(bytes, &Marks::for_bytes(bytes))
}

/// [`substitute_placeholders`] with the placeholders of `marks`.
fn substitute_with(bytes: &[u8], marks: &Marks) -> (Vec<u8>, HashMap<String, String>) {
    let re = substitution_regex();
    let mut out = Vec::with_capacity(bytes.len());
    let mut map = HashMap::new();
    let mut last_end = 0usize;

    for (counter, m) in re.find_iter(bytes).enumerate() {
        out.extend_from_slice(&bytes[last_end..m.start()]);
        let placeholder = marks.name(counter);
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
/// placeholders are constructed with a unique counter between an
/// unambiguous prefix and suffix (`DQtplxq<N>qx`), no overlap or collision
/// is possible across the map.
#[must_use]
pub fn restore_placeholders(bytes: &[u8], map: &HashMap<String, String>) -> Vec<u8> {
    match Restorer::new(map) {
        Some(restorer) => restorer.restore(bytes).into_owned(),
        None => bytes.to_vec(),
    }
}

/// Puts the template text back in place of the placeholders of one map, in
/// one scan of the text: every match of the placeholder pattern looked up
/// by its text, rather than every placeholder looked for in turn — a file
/// of thousands of templates would cost thousands of scans per string.
struct Restorer<'m> {
    /// A placeholder of the map's prefix (all of a map's share it).
    re: Regex,
    map: &'m HashMap<String, String>,
}

impl<'m> Restorer<'m> {
    /// `None` for an empty map, which has nothing to put back.
    fn new(map: &'m HashMap<String, String>) -> Option<Self> {
        let any = map.keys().next()?;
        let prefix = any
            .strip_suffix(MARK_SUFFIX)
            .unwrap_or(any)
            .trim_end_matches(|c: char| c.is_ascii_digit());
        let re = Regex::new(&format!("{}[0-9]+{MARK_SUFFIX}", regex::escape(prefix)))
            .expect("placeholder regex must compile");
        Some(Self { re, map })
    }

    /// Whether `text` holds a placeholder of the map.
    fn holds(&self, text: &str) -> bool {
        self.re
            .find_iter(text.as_bytes())
            .any(|m| self.map.contains_key(lossy(m.as_bytes()).as_ref()))
    }

    /// `text` with every placeholder of the map its template text.
    fn restore<'t>(&self, text: &'t [u8]) -> std::borrow::Cow<'t, [u8]> {
        self.re
            .replace_all(text, |caps: &regex::bytes::Captures<'_>| {
                let found = &caps[0];
                self.map
                    .get(lossy(found).as_ref())
                    .map_or_else(|| found.to_vec(), |original| original.as_bytes().to_vec())
            })
    }
}

/// `bytes` as text: placeholders are ASCII.
fn lossy(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
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
            let (mut doc, map) = parse_placeheld(bytes, parse)?;
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
            let marks = &held.marks;
            if same_reading(marks, as_is.value(), placeheld.value(), positions) {
                return Ok(as_is);
            }
            // Cite a template that changes the reading: the first whose
            // placeholder sits where the two readings first differ.
            let culprit = differing_blocks(marks, as_is.value(), placeheld.value(), positions)
                .and_then(|found| found.into_iter().min())
                .and_then(|n| held.blocks.get(n))
                .and_then(|block| marker_on_line(bytes, block.line));
            Err(culprit.map_or_else(refuse, crate::Error::templated_file))
        }
    }
}

/// The placeholders of one file, the guard's and those of
/// `--raw-template-strings` ([`substitute_placeholders`]): `DQtplxq<N>qx`,
/// letters and digits only, so that no format reads one as anything but
/// plain text (Markdown reads the underscores of `__DQ_TPL_0__` as strong
/// emphasis), and a placeholder is a word in YAML, a bare key in TOML.
/// Where the file holds `DQtplxq` already, one more `x` (`DQtplxxq…`) — as
/// many as it takes to name a prefix the file does not hold — so text in
/// the file is never taken for a placeholder, nor put back as a template.
/// Text that spells one only once its escapes are read is caught by
/// reading the file twice ([`parse_placeheld`]).
struct Marks {
    /// What every placeholder of the file starts with.
    prefix: String,
    /// How many fill letters (`x`) the prefix has.
    fill: usize,
    /// A placeholder, its number captured.
    re: Regex,
}

/// What every placeholder ends with.
const MARK_SUFFIX: &str = "qx";

impl Marks {
    /// The placeholders for `bytes`: the first of `DQtplxq`, `DQtplxxq`, …
    /// they do not hold.
    fn for_bytes(bytes: &[u8]) -> Self {
        let fill = (1..)
            .find(|&n| !holds(bytes, &Self::prefix_of('x', n)))
            .expect("a long enough prefix is not in the bytes");
        Self::new('x', fill)
    }

    /// Placeholders that start with `DQtpl`, `fill` times `letter`, `q`.
    fn new(letter: char, fill: usize) -> Self {
        let prefix = Self::prefix_of(letter, fill);
        let re = Regex::new(&format!("{prefix}([0-9]+){MARK_SUFFIX}"))
            .expect("placeholder regex must compile");
        Self { prefix, fill, re }
    }

    fn prefix_of(letter: char, fill: usize) -> String {
        format!("DQtpl{}q", letter.to_string().repeat(fill))
    }

    /// Two sets of placeholders of one length, `DQtplx…q` and `DQtply…q`,
    /// neither of which `bytes` hold, with at least `min_fill` fill
    /// letters: [`parse_placeheld`] reads the file with each.
    fn pair(bytes: &[u8], min_fill: usize) -> (Self, Self) {
        let fill = (min_fill..)
            .find(|&n| {
                !holds(bytes, &Self::prefix_of('x', n)) && !holds(bytes, &Self::prefix_of('y', n))
            })
            .expect("a long enough prefix is not in the bytes");
        (Self::new('x', fill), Self::new('y', fill))
    }

    /// Placeholder `n`.
    fn name(&self, n: usize) -> String {
        format!("{}{n}{MARK_SUFFIX}", self.prefix)
    }

    /// The numbers of the placeholders in `s`, in order.
    fn numbers_in(&self, s: &str) -> Vec<usize> {
        self.re
            .captures_iter(s.as_bytes())
            .filter_map(|c| std::str::from_utf8(&c[1]).ok()?.parse().ok())
            .collect()
    }
}

/// Whether `bytes` hold `text`.
fn holds(bytes: &[u8], text: &str) -> bool {
    bytes.windows(text.len()).any(|w| w == text.as_bytes())
}

/// How many times [`parse_placeheld`] tries a longer prefix before it
/// takes the reading it has.
const MAX_FILL: usize = 8;

/// `bytes` parsed with every template block swapped for a placeholder,
/// and the map from placeholder to block — what `--raw-template-strings`
/// reads. A prefix the bytes do not hold may still be in the data once
/// the parser reads an escape (`DQtpl\x78q0qx` in YAML, `\u0078` in JSON),
/// where it would pass for a placeholder: the bytes are read a second time
/// with placeholders of the same length spelled with `y`, and the two
/// readings must say the same once the placeholders are set aside. Where
/// they do not, the data holds a prefix's text, and both are tried again
/// one fill letter longer — up to [`MAX_FILL`] letters, past which the
/// file is refused rather than read wrongly. Bytes with no escape character
/// (`\\`, or `&` for an XML or Markdown character reference) and no `<` (the
/// comment, processing instruction or element that XML, Markdown or HTML read
/// between two runs of text as one text node) spell no prefix the bytes do
/// not hold, and are read once.
///
/// # Errors
///
/// The error of `parse`; [`crate::Error::TemplatedFile`] for bytes that
/// spell every prefix tried.
fn parse_placeheld(
    bytes: &[u8],
    parse: impl Fn(&[u8]) -> crate::Result<crate::Document>,
) -> crate::Result<(crate::Document, HashMap<String, String>)> {
    let spells_apart = bytes.iter().any(|&b| matches!(b, b'\\' | b'&' | b'<'));
    let mut min_fill = 1;
    loop {
        let (marks, twin) = Marks::pair(bytes, min_fill);
        let (text, map) = substitute_with(bytes, &marks);
        let doc = parse(&text)?;
        if map.is_empty() || !spells_apart {
            return Ok((doc, map));
        }
        let (twin_text, _) = substitute_with(bytes, &twin);
        let alike = parse(&twin_text)
            .is_ok_and(|other| alike_but_marks(&marks, doc.value(), &twin, other.value()));
        if alike {
            return Ok((doc, map));
        }
        if marks.fill >= MAX_FILL {
            return Err(spells_every_placeholder(bytes));
        }
        min_fill = marks.fill + 1;
    }
}

/// The refusal of `bytes` that spell, with escapes, the placeholder text
/// of every prefix [`parse_placeheld`] tries: the first template's line.
fn spells_every_placeholder(bytes: &[u8]) -> crate::Error {
    let at = substitution_regex().find(bytes).map_or(0, |m| m.start());
    let line = 1 + bytes[..at].iter().filter(|&&b| b == b'\n').count();
    let marker = u32::try_from(line)
        .ok()
        .and_then(|line| marker_on_line(bytes, line))
        .unwrap_or(TemplateMarker {
            line: 1,
            snippet: String::new(),
        });
    crate::Error::TemplatedFile {
        line: marker.line,
        snippet: marker.snippet,
        hint: format!(
            "the file spells dq's placeholder text (`DQtplxq0qx`, with up to {MAX_FILL} `x`) \
             with escapes, so --raw-template-strings cannot tell its templates apart from it; \
             use --allow-templates to parse anyway"
        ),
    }
}

/// Whether `a`, read with the placeholders of `ma`, and `b`, read with
/// those of `mb`, hold the same data once every placeholder is taken out
/// of their strings and keys: a placeholder's text the data held besides
/// stays in one and not in the other.
fn alike_but_marks(ma: &Marks, a: &crate::Value, mb: &Marks, b: &crate::Value) -> bool {
    use crate::Value;
    let text = |m: &Marks, s: &str| m.re.replace_all(s.as_bytes(), &b""[..]).into_owned();
    match (a, b) {
        (Value::String(a), Value::String(b)) => text(ma, a) == text(mb, b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| alike_but_marks(ma, a, mb, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|((ak, av), (bk, bv))| {
                    text(ma, ak) == text(mb, bk) && alike_but_marks(ma, av, mb, bv)
                })
        }
        (a, b) => a.same_data(b),
    }
}

/// The text a reading is compared in, with every template block of the
/// file swapped for a placeholder ([`Marks`]) — the form
/// [`substitute_placeholders`] uses for `--raw-template-strings` too.
struct Held {
    /// The text with placeholders.
    text: Vec<u8>,
    /// The placeholders of the text.
    marks: Marks,
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
        let marks = Marks::for_bytes(bytes);
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
            text.extend_from_slice(marks.name(n).as_bytes());
            last_end = m.end();
        }
        text.extend_from_slice(&bytes[last_end..]);
        Self {
            text,
            marks,
            blocks,
        }
    }
}

/// A template block in a string as read — the parser has already taken
/// the quoting's escapes out, and an escaped `\n` is a line break.
fn read_block_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)\$?\{\{.*?\}\}").expect("block regex must compile"))
}

/// Mark that stands for one template block in [`same_text`].
const BLOCK_MARK: &str = "\u{0}";

/// Whether `as_is`, a string read from the file as it is, and `held`, the
/// same string read with placeholders, say the same once each template
/// block is set aside — every placeholder of `held` standing where `as_is`
/// holds one block. The block's text is not compared: the parser reads
/// the escapes of a quoted string (`''`, `\"`) out of the block in
/// `as_is`, and the placeholder stood for the text before that.
fn same_text(marks: &Marks, as_is: &str, held: &str) -> bool {
    if as_is == held {
        return true;
    }
    let as_is = read_block_regex().replace_all(as_is.as_bytes(), BLOCK_MARK.as_bytes());
    let held = marks.re.replace_all(held.as_bytes(), BLOCK_MARK.as_bytes());
    let held = read_block_regex().replace_all(&held, BLOCK_MARK.as_bytes());
    as_is == held
}

/// Whether `as_is` and `held` (read with placeholders) hold the same data,
/// strings and keys compared by [`same_text`], map entries in order. With
/// `positions` the `position` member of a map — where the Markdown tree
/// keeps a node's line and column, which a placeholder longer or shorter
/// than its block moves — is not compared.
fn same_reading(marks: &Marks, as_is: &crate::Value, held: &crate::Value, positions: bool) -> bool {
    use crate::Value;
    match (as_is, held) {
        (Value::String(a), Value::String(h)) => same_text(marks, a, h),
        (Value::Array(a), Value::Array(h)) => {
            a.len() == h.len()
                && a.iter()
                    .zip(h)
                    .all(|(a, h)| same_reading(marks, a, h, positions))
        }
        (Value::Map(a), Value::Map(h)) => {
            let kept = |(key, _): &(&String, &Value)| !(positions && key.as_str() == "position");
            let a: Vec<_> = a.iter().filter(kept).collect();
            let h: Vec<_> = h.iter().filter(kept).collect();
            a.len() == h.len()
                && a.iter().zip(&h).all(|((ak, av), (hk, hv))| {
                    same_text(marks, ak, hk) && same_reading(marks, av, hv, positions)
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
    marks: &Marks,
    as_is: &crate::Value,
    held: &crate::Value,
    positions: bool,
) -> Option<Vec<usize>> {
    use crate::Value;
    if same_reading(marks, as_is, held, positions) {
        return None;
    }
    match (as_is, held) {
        (Value::String(_), Value::String(h)) => Some(marks.numbers_in(h)),
        (Value::Array(a), Value::Array(h)) if a.len() == h.len() => a
            .iter()
            .zip(h)
            .find_map(|(a, h)| differing_blocks(marks, a, h, positions)),
        (Value::Map(a), Value::Map(h)) => {
            let kept = |(key, _): &(&String, &Value)| !(positions && key.as_str() == "position");
            let a: Vec<_> = a.iter().filter(kept).collect();
            let h: Vec<_> = h.iter().filter(kept).collect();
            if a.len() != h.len() {
                return Some(blocks_within(marks, held));
            }
            a.iter().zip(&h).find_map(|((ak, av), (hk, hv))| {
                if same_text(marks, ak, hk) {
                    differing_blocks(marks, av, hv, positions)
                } else {
                    let mut found = marks.numbers_in(hk);
                    found.extend(blocks_within(marks, hv));
                    Some(found)
                }
            })
        }
        _ => Some(blocks_within(marks, held)),
    }
}

/// The guard's placeholders in every string and key of `v`.
fn blocks_within(marks: &Marks, v: &crate::Value) -> Vec<usize> {
    use crate::Value;
    match v {
        Value::String(s) => marks.numbers_in(s),
        Value::Array(items) => items
            .iter()
            .flat_map(|item| blocks_within(marks, item))
            .collect(),
        Value::Map(map) => map
            .iter()
            .flat_map(|(k, v)| {
                marks
                    .numbers_in(k)
                    .into_iter()
                    .chain(blocks_within(marks, v))
            })
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
    if let Some(restorer) = Restorer::new(map) {
        restore_value(value, &restorer);
    }
}

/// [`restore_in_value`] with the map's [`Restorer`].
fn restore_value(value: &mut crate::Value, restorer: &Restorer<'_>) {
    use crate::Value;
    match value {
        Value::String(s) => {
            if let Some(restored) = restore_in_str(s, restorer) {
                *s = restored;
            }
        }
        Value::Array(items) => {
            for item in items {
                restore_value(item, restorer);
            }
        }
        Value::Map(entries) => {
            let keyed = entries.keys().any(|k| restorer.holds(k));
            if keyed {
                let old = std::mem::take(entries);
                for (key, mut item) in old {
                    restore_value(&mut item, restorer);
                    let key = restore_in_str(&key, restorer).unwrap_or(key);
                    entries.insert(key, item);
                }
            } else {
                for item in entries.values_mut() {
                    restore_value(item, restorer);
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

/// `s` with each placeholder replaced by its template text; `None` when `s`
/// holds no placeholder.
///
/// A string that is a placeholder in `${…}` alone is the template alone:
/// HCL reads a bare placeholder (`a = {{ .x }}`) as an expression and
/// spells it `${placeholder}`.
fn restore_in_str(s: &str, restorer: &Restorer<'_>) -> Option<String> {
    if !restorer.holds(s) {
        return None;
    }
    if let Some(original) = s
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .and_then(|placeholder| restorer.map.get(placeholder))
    {
        return Some(original.clone());
    }
    Some(String::from_utf8_lossy(&restorer.restore(s.as_bytes())).into_owned())
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
            let (doc, map) = parse_placeheld(original, parse)?;
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
        &held.marks,
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
        &held.marks,
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
    marks: &Marks,
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
            keep(marks, found, edited, s, h, &at.as_is);
        }
        Value::Array(items) => {
            let held = match held {
                Some(Value::Array(h)) if h.len() == items.len() => Some(h),
                _ => None,
            };
            for (i, item) in items.iter().enumerate() {
                let index = i.to_string();
                let lens = at.push(&index, &index);
                collect(marks, item, held.map(|h| &h[i]), at, edited, found);
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
                    marks,
                    found,
                    edited,
                    key,
                    held_entry.map(|(k, _)| k.as_str()),
                    &pointer,
                );
                collect(marks, item, held_entry.map(|(_, v)| v), at, edited, found);
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
fn keep(
    marks: &Marks,
    found: &mut Found,
    edited: &[String],
    text: &str,
    held: Option<&str>,
    pointer: &str,
) {
    if read_block_regex().is_match(text.as_bytes()) {
        found.strings.push(TemplateString {
            text: text.to_owned(),
            edited: edited.iter().any(|e| under(pointer, e)),
            held: held
                .filter(|h| same_text(marks, text, h))
                .map(|h| marks.numbers_in(h))
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
        let marks = Marks::for_bytes(b"");
        assert!(!same_text(&marks, "{{ a }}", "DQtplxq0qxDQtplxq1qx"));
        assert!(same_text(
            &marks,
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
                .finish(b"port: DQtplxq0qx\n".to_vec(), &[], true, yaml)
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

    /// Every string and key of `v`.
    fn strings_in(v: &crate::Value) -> Vec<String> {
        use crate::Value;
        match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(items) => items.iter().flat_map(strings_in).collect(),
            Value::Map(map) => map
                .iter()
                .flat_map(|(k, v)| std::iter::once(k.clone()).chain(strings_in(v)))
                .collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn raw_template_strings_read_a_template_as_text_in_every_format() {
        // The placeholder must read as plain text wherever the template
        // stands: Markdown took `__DQ_TPL_0__` for strong emphasis, HCL a
        // bare one for an expression.
        for (format, text, template) in [
            ("yaml", "a: {{ .x }}\n", "{{ .x }}"),
            ("json", "{\"a\": \"{{ .x }}\"}\n", "{{ .x }}"),
            ("toml", "a = \"{{ .x }}\"\n", "{{ .x }}"),
            ("hcl", "a = {{ .x }}\n", "{{ .x }}"),
            ("hcl", "a = \"{{ .x }}\"\n", "{{ .x }}"),
            ("ini", "[s]\na = {{ .x }}\n", "{{ .x }}"),
            ("dotenv", "A={{ .x }}\n", "{{ .x }}"),
            ("csv", "a\n{{ .x }}\n", "{{ .x }}"),
            ("xml", "<a>{{ .x }}</a>\n", "{{ .x }}"),
            ("dockerfile", "FROM {{ .x }}\n", "{{ .x }}"),
            ("markdown", "# T\n\n{{ .x }}\n", "{{ .x }}"),
            ("markdown", "Hello {{ .x }}!\n", "Hello {{ .x }}!"),
            (
                "markdown",
                "- {{ .x }} and {{ .y }}\n",
                "{{ .x }} and {{ .y }}",
            ),
        ] {
            let parser = crate::by_name(format).expect("format");
            let doc = guarded_parse(text.as_bytes(), TemplateMode::RawStrings, |b| {
                parser.parse(b)
            })
            .unwrap_or_else(|e| panic!("{format} {text:?}: {e}"));
            let strings = strings_in(doc.value());
            assert!(
                strings.iter().any(|s| s == template),
                "{format} {text:?}: {strings:?}"
            );
            assert!(
                !strings.iter().any(|s| s == "strong"),
                "{format} {text:?}: {strings:?}"
            );
        }
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
    fn restoring_many_templates_is_linear() {
        // Each string used to be checked against every placeholder: 20 000
        // templates took tens of seconds in a debug build. One scan per
        // string takes milliseconds; the bound is generous.
        let n = 20_000;
        let text: String = (0..n)
            .map(|i| format!("k{i}: \"a {{{{ .v{i} }}}} b\"\n"))
            .collect();
        let (substituted, map) = substitute_placeholders(text.as_bytes());
        let mut value = crate::Value::Array(
            std::str::from_utf8(&substituted)
                .expect("ascii")
                .lines()
                .map(|line| crate::Value::String(line.to_owned()))
                .collect(),
        );
        let started = std::time::Instant::now();
        restore_in_value(&mut value, &map);
        let restored = restore_placeholders(&substituted, &map);
        let took = started.elapsed();
        assert_eq!(restored, text.as_bytes());
        let crate::Value::Array(items) = &value else {
            panic!("an array")
        };
        assert_eq!(
            items[7],
            crate::Value::String("k7: \"a {{ .v7 }} b\"".into())
        );
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn marks_pick_a_prefix_the_file_does_not_hold() {
        assert_eq!(Marks::for_bytes(b"a: {{ .x }}").name(0), "DQtplxq0qx");
        assert_eq!(Marks::for_bytes(b"d: DQtplxq0qx").name(3), "DQtplxxq3qx");
        let marks = Marks::for_bytes(b"DQtplxq DQtplxxq");
        assert_eq!(marks.name(0), "DQtplxxxq0qx");
        assert_eq!(marks.numbers_in("DQtplxq1qx DQtplxxxq2qx"), vec![2]);
        let (text, map) = substitute_placeholders(b"a: \"{{ .x }}\"\nd: DQtplxq0qx\n");
        assert_eq!(text, b"a: \"DQtplxxq0qx\"\nd: DQtplxq0qx\n");
        assert_eq!(map.get("DQtplxxq0qx").map(String::as_str), Some("{{ .x }}"));
    }

    #[test]
    fn substitute_preserves_surrounding_quotes() {
        let original: &[u8] = b"tag: \"{{ .Values.tag }}\"\n";
        let (substituted, _map) = substitute_placeholders(original);
        let s = std::str::from_utf8(&substituted).expect("ascii output");
        assert!(
            s.contains("\"DQtplxq0qx\""),
            "quotes must remain around the placeholder, got: {s}"
        );
    }
}
