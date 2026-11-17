//! Markdown frontmatter wrapper: parses YAML / TOML / JSON frontmatter and
//! carries the body as opaque bytes.
//!
//! The parser detects three frontmatter delimiters at the start of the file:
//!
//! - `---\n` … `---\n` → header parsed via the YAML format.
//! - `+++\n` … `+++\n` → header parsed via the TOML format.
//! - `{` … `}\n` followed by a blank line → header parsed via the JSON format.
//!
//! If no opening delimiter is recognised within the first byte OR no matching
//! closing delimiter is found within the first 64 KiB of the file, the
//! parser returns `Document::frontmatter(empty_map, whole_file_bytes,
//! FrontmatterKind::Yaml)` — the value is an empty map and the body equals
//! the entire input.
//!
//! Round-trip contract (`Format::write`):
//! 1. Re-serialize the value through the inner format (YAML / TOML / JSON).
//! 2. Emit the opening delimiter, header, closing delimiter.
//! 3. Concatenate the stored body bytes verbatim.
//!
//! Empty-map fallback (no frontmatter): the writer emits just the body bytes
//! — no synthetic `---\n---\n` markers — so a file with no frontmatter
//! round-trips verbatim.

use std::io::Write;

use camino::Utf8PathBuf;
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, FrontmatterKind, Value};
use crate::error::Error;
use crate::format::Format;
use crate::parsers::{Json, Toml, Yaml};

/// Maximum byte distance scanned for a closing frontmatter delimiter. Files
/// whose closing marker is past this offset fall back to the "no frontmatter"
/// branch (per spec).
///
/// Visible to the M9 [`crate::parsers::markdown`] parser so the two share the
/// same scan-limit constant.
pub(crate) const FRONTMATTER_SCAN_LIMIT: usize = 64 * 1024;

/// Markdown frontmatter format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Frontmatter;

impl Format for Frontmatter {
    fn name(&self) -> &'static str {
        "frontmatter"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["md", "markdown"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        // Detect leading delimiter. Each branch returns either a parsed
        // (header_value, body_bytes, kind) triple or `None` — in which case
        // we fall through to the no-frontmatter empty-map default.
        if let Some((header_bytes, body_bytes)) = detect_yaml_frontmatter(bytes) {
            let header_doc = Yaml.parse(header_bytes)?;
            return Ok(Document::frontmatter(
                header_doc.value().clone(),
                body_bytes.to_vec(),
                FrontmatterKind::Yaml,
            ));
        }
        if let Some((header_bytes, body_bytes)) = detect_toml_frontmatter(bytes) {
            let header_doc = Toml.parse(header_bytes)?;
            return Ok(Document::frontmatter(
                header_doc.value().clone(),
                body_bytes.to_vec(),
                FrontmatterKind::Toml,
            ));
        }
        if let Some((header_bytes, body_bytes)) = detect_json_frontmatter(bytes) {
            let header_doc = Json.parse(header_bytes)?;
            return Ok(Document::frontmatter(
                header_doc.value().clone(),
                body_bytes.to_vec(),
                FrontmatterKind::Json,
            ));
        }
        // No recognised frontmatter — preserve the whole file as the body
        // and pin the value to an empty map. Kind is `Yaml` as a benign
        // placeholder; the writer's empty-map shortcut emits only the body
        // so the placeholder is never observable on round-trip.
        Ok(Document::frontmatter(
            Value::Map(IndexMap::new()),
            bytes.to_vec(),
            FrontmatterKind::Yaml,
        ))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        let payload = doc.frontmatter_payload().ok_or_else(|| Error::Format {
            format: "frontmatter",
            message: "document is not a frontmatter document (no payload)".to_owned(),
        })?;

        // Empty-map shortcut: a frontmatter doc whose value is an empty map
        // came from the no-frontmatter fallback — emit just the body so the
        // round-trip is byte-identical.
        if let Value::Map(m) = doc.value()
            && m.is_empty()
        {
            return write_io(w, &payload.body);
        }

        match payload.kind {
            FrontmatterKind::Yaml => {
                write_io(w, b"---\n")?;
                let inner = Document::value_only(doc.value().clone(), FormatTag::Yaml);
                Yaml.write(&inner, w)?;
                write_io(w, b"---\n")?;
                write_io(w, &payload.body)?;
            }
            FrontmatterKind::Toml => {
                write_io(w, b"+++\n")?;
                let inner = Document::value_only(doc.value().clone(), FormatTag::Toml);
                Toml.write(&inner, w)?;
                write_io(w, b"+++\n")?;
                write_io(w, &payload.body)?;
            }
            FrontmatterKind::Json => {
                let inner = Document::value_only(doc.value().clone(), FormatTag::Json);
                Json.write(&inner, w)?;
                write_io(w, b"\n\n")?;
                write_io(w, &payload.body)?;
            }
        }
        Ok(())
    }
}

/// Detect a `---\n...---\n` (or `---\r\n`-prefixed) YAML frontmatter block.
/// Returns `(header_bytes, body_bytes)` on a successful match.
///
/// Visible to the M9 markdown parser ([`crate::parsers::markdown`]) so both
/// formats share the same delimiter-scanner.
pub(crate) fn detect_yaml_frontmatter(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    detect_delimited_frontmatter(bytes, b"---")
}

/// Detect a `+++\n...+++\n` TOML frontmatter block.
///
/// Visible to the M9 markdown parser ([`crate::parsers::markdown`]).
pub(crate) fn detect_toml_frontmatter(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    detect_delimited_frontmatter(bytes, b"+++")
}

/// Common scanner for `MARKER\n...MARKER\n` style frontmatter (YAML / TOML).
fn detect_delimited_frontmatter<'a>(
    bytes: &'a [u8],
    marker: &[u8],
) -> Option<(&'a [u8], &'a [u8])> {
    let after_open = strip_open_delim(bytes, marker)?;
    let header_start = bytes.len() - after_open.len();
    // Search for a line equal to `marker` (followed by newline or EOF) within
    // the scan limit measured from the start of the file.
    let scan_end = bytes.len().min(FRONTMATTER_SCAN_LIMIT);
    let mut cursor = header_start;
    while cursor < scan_end {
        // Match if at line start: cursor == 0 OR previous byte is `\n`. Inside
        // the loop we only ever check line-start positions because we advance
        // via newline scanning.
        if line_starts_with(bytes, cursor, marker) && line_ends_at(bytes, cursor + marker.len()) {
            // Header bytes are everything between header_start and the line
            // that holds the closing marker (exclusive of that line).
            let header_end = cursor;
            // Body starts past the closing marker line. Walk past `\r?\n`.
            let mut body_start = cursor + marker.len();
            if body_start < bytes.len() && bytes[body_start] == b'\r' {
                body_start += 1;
            }
            if body_start < bytes.len() && bytes[body_start] == b'\n' {
                body_start += 1;
            }
            return Some((&bytes[header_start..header_end], &bytes[body_start..]));
        }
        // Advance to next line.
        match memchr_newline(bytes, cursor) {
            Some(nl) => cursor = nl + 1,
            None => break,
        }
    }
    None
}

/// Strip the opening delimiter `marker` followed by `\n` (optionally `\r\n`).
/// Returns the slice starting at the first byte of the header on success.
fn strip_open_delim<'a>(bytes: &'a [u8], marker: &[u8]) -> Option<&'a [u8]> {
    let rest = bytes.strip_prefix(marker)?;
    if let Some(after) = rest.strip_prefix(b"\r\n") {
        return Some(after);
    }
    rest.strip_prefix(b"\n")
}

/// True when `bytes[at..at+marker.len()] == marker` AND `at` is at the start
/// of a line (`at == 0` or `bytes[at-1] == b'\n'`).
fn line_starts_with(bytes: &[u8], at: usize, marker: &[u8]) -> bool {
    if at != 0 && bytes.get(at - 1) != Some(&b'\n') {
        return false;
    }
    bytes
        .get(at..at + marker.len())
        .is_some_and(|slice| slice == marker)
}

/// True when position `at` is end-of-line (`\n`, `\r\n`, or EOF).
fn line_ends_at(bytes: &[u8], at: usize) -> bool {
    match bytes.get(at) {
        None => true,
        Some(&b'\n') => true,
        Some(&b'\r') => bytes.get(at + 1).is_none_or(|b| *b == b'\n'),
        _ => false,
    }
}

/// Find the first `\n` at or after `from`, returning its byte offset. None
/// when no newline is found in `bytes[from..]`.
fn memchr_newline(bytes: &[u8], from: usize) -> Option<usize> {
    bytes
        .iter()
        .skip(from)
        .position(|b| *b == b'\n')
        .map(|off| from + off)
}

/// Detect a JSON frontmatter block: file starts with `{`, header runs to
/// the matching closing `}`, followed by `\n\n` (a blank line) or EOF.
///
/// Returns `(header_bytes, body_bytes)` on a successful match. JSON nesting
/// is tracked through string awareness — `{` inside a string does not bump
/// the depth.
///
/// Visible to the M9 markdown parser ([`crate::parsers::markdown`]).
pub(crate) fn detect_json_frontmatter(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let scan_end = bytes.len().min(FRONTMATTER_SCAN_LIMIT);
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut closing: Option<usize> = None;
    for (i, &b) in bytes[..scan_end].iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    closing = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close_idx = closing?;
    // Require a `}\n` then a blank line (or EOF) to disambiguate JSON
    // frontmatter from a Markdown body that happens to start with `{...}`.
    let after_close = close_idx + 1;
    let mut cursor = after_close;
    if cursor < bytes.len() && bytes[cursor] == b'\r' {
        cursor += 1;
    }
    if cursor < bytes.len() && bytes[cursor] == b'\n' {
        cursor += 1;
    } else if cursor != bytes.len() {
        return None;
    }
    // Optional second newline (the blank line). EOF here is also accepted.
    let body_start = if cursor < bytes.len() && bytes[cursor] == b'\r' {
        cursor + 1
    } else {
        cursor
    };
    let body_start = if body_start < bytes.len() && bytes[body_start] == b'\n' {
        body_start + 1
    } else if body_start == bytes.len() {
        body_start
    } else {
        // No blank line follows — refuse to treat as JSON frontmatter so
        // ordinary `{ ... }` Markdown openings don't get mis-parsed.
        return None;
    };
    Some((&bytes[..=close_idx], &bytes[body_start..]))
}

fn write_io(w: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    w.write_all(bytes).map_err(|source| Error::Io {
        path: Utf8PathBuf::from("<frontmatter-writer>"),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_yaml_frontmatter_extracts_title_and_body() {
        let doc = Frontmatter
            .parse(b"---\ntitle: x\n---\n# body\n")
            .expect("parse");
        let Value::Map(m) = doc.value() else {
            panic!("expected map")
        };
        assert_eq!(m.get("title"), Some(&Value::String("x".into())));
        let payload = doc
            .frontmatter_payload()
            .expect("frontmatter payload present");
        assert_eq!(payload.kind, FrontmatterKind::Yaml);
        assert_eq!(payload.body, b"# body\n");
    }

    #[test]
    fn parse_no_frontmatter_falls_back_to_empty_map_and_full_body() {
        let input = b"# Just markdown\n\nNo frontmatter here.\n";
        let doc = Frontmatter.parse(input).expect("parse");
        let Value::Map(m) = doc.value() else {
            panic!("expected empty map")
        };
        assert!(m.is_empty(), "fallback value must be empty map");
        let payload = doc
            .frontmatter_payload()
            .expect("frontmatter payload present");
        assert_eq!(payload.body, input);
    }
}
