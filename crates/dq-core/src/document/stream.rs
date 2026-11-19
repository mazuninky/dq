//! Re-emitting one document of a write-aware YAML stream.

use super::{Document, FormatTag, Value, first_difference, with_line_ending_at};
use crate::Result;
use crate::error::Error;
use crate::format::Format as _;
use crate::parsers::yaml_spans::{DocumentExtent, document_extents, parse_yaml_stream_with_spans};

impl Document {
    /// Replace document `index` of a write-aware multi-document YAML
    /// stream with `value`, written through the YAML writer, and keep
    /// every other byte of the file.
    ///
    /// Only that document's content is rewritten: its `---` line (with any
    /// comment on it), the comment and blank lines between that line and
    /// the content and after its last content line, and every other document — separators, comments, quoting and
    /// all — stay byte for byte. Comments among the rewritten document's
    /// content are dropped, as any writer re-emit drops them. Node
    /// properties on the marker line (`--- !!map`, `--- &anchor`) are kept
    /// when the tag still fits `value` (see `tags_fit`) and the rewritten
    /// document reads back as `value` under them, and dropped otherwise.
    /// The rewritten content breaks lines the way
    /// the file does around it: `\r\n` in a CRLF file, `\n` otherwise.
    /// The result is parsed again and must read back as the requested
    /// stream, so the document stays write-aware: later textual edits
    /// (`set_at`, `del_at`, a `fix.ops` script) apply to it as before.
    ///
    /// # Errors
    ///
    /// - [`Error::WriteUnavailable`] — the document is not a write-aware
    ///   YAML stream, or has no document `index`.
    /// - [`Error::EditRejected`] — the rewritten file would not read back
    ///   as the stream with document `index` replaced; `self` is
    ///   unchanged.
    /// - The YAML writer's error when `value` cannot be written as YAML.
    pub fn reemit_stream_document(&mut self, index: usize, value: Value) -> Result<()> {
        let unavailable = |reason: &str| Error::WriteUnavailable {
            reason: reason.to_owned(),
        };
        if self.format != FormatTag::Yaml || !self.write_aware {
            return Err(unavailable("not a write-aware YAML stream"));
        }
        let expected = self
            .values()
            .ok_or_else(|| unavailable("not a multi-document stream"))?
            .to_vec();
        if index >= expected.len() {
            return Err(unavailable(&format!("the stream has no document {index}")));
        }
        self.reemit_yaml_document(Some(index), expected, value)
    }

    /// Replace the value of a write-aware YAML file that is one document
    /// (not a stream) with `value`, the way [`Document::reemit_stream_document`]
    /// replaces one document of a stream: the comment and blank lines before
    /// the content, the `---` line with any directives above it, and the
    /// comment lines after the last content line stay byte for byte. A file
    /// with no content (only comments) gets the value after them.
    ///
    /// # Errors
    ///
    /// As [`Document::reemit_stream_document`], for a file that is not one
    /// write-aware YAML document.
    pub fn reemit_yaml_root(&mut self, value: Value) -> Result<()> {
        if self.format != FormatTag::Yaml || !self.write_aware || self.multi_doc {
            return Err(Error::WriteUnavailable {
                reason: "not a write-aware YAML document".to_owned(),
            });
        }
        let expected = vec![self.value.clone()];
        self.reemit_yaml_document(None, expected, value)
    }

    /// Re-emit document `index` of a stream (`None`: the file's only
    /// document) whose documents are `expected` as `value`.
    fn reemit_yaml_document(
        &mut self,
        index: Option<usize>,
        mut expected: Vec<Value>,
        value: Value,
    ) -> Result<()> {
        let unavailable = |reason: &str| Error::WriteUnavailable {
            reason: reason.to_owned(),
        };
        let at = index.unwrap_or(0);
        let pointer = index.map_or_else(String::new, |index| format!("/{index}"));
        let previous = std::mem::replace(&mut expected[at], value);
        let mut extents = document_extents(&self.original_bytes).unwrap_or_default();
        if index.is_none() && extents.is_empty() {
            // Comments alone: no document to locate; the value goes after them.
            extents.push(end_of_file_extent(&self.original_bytes));
        }
        if extents.len() != expected.len() {
            return Err(unavailable("the stream's documents could not be located"));
        }
        let extent = &extents[at];

        let mut emitted = Vec::new();
        crate::parsers::Yaml.write(
            &Document::value_only(expected[at].clone(), FormatTag::Yaml),
            &mut emitted,
        )?;

        // The ranges to replace, most preserving first: keep the marker
        // line's node properties and the trailing comment lines; keep only
        // the comment lines; replace the whole body. The first that reads
        // back as `expected` wins.
        // The comment and blank lines between the marker line and the
        // content lead into the content and stay before the new one.
        let bytes = &self.original_bytes;
        let past_lead = |(start, needs_line_break): (usize, bool)| {
            if needs_line_break {
                (start, needs_line_break)
            } else {
                (past_lead_in(bytes, start, extent.content_end), false)
            }
        };
        let body = (extent.body.start, extent.needs_line_break);
        let mut attempts = Vec::with_capacity(4);
        if let Some(after_properties) = extent.after_properties.filter(|&(end, _)| {
            tags_fit(
                &self.original_bytes[extent.body.start..end],
                &previous,
                &expected[at],
                &emitted,
            )
        }) {
            attempts.push((past_lead(after_properties), extent.content_end));
        }
        attempts.push((past_lead(body), extent.content_end));
        attempts.push((past_lead(body), extent.body.end));
        attempts.push((body, extent.body.end));
        attempts.dedup();

        let mut rejected = None;
        for ((start, needs_line_break), end) in attempts {
            let bytes = self.splice(start..end.max(start), needs_line_break, &emitted);
            match read_back(&bytes, &expected, index.is_some(), &pointer, "set") {
                Ok(reparsed) => {
                    *self = reparsed;
                    return Ok(());
                }
                Err(err) => rejected = Some(err),
            }
        }
        Err(rejected.unwrap_or_else(|| unavailable("nothing to rewrite")))
    }

    /// Remove document `index` of a write-aware multi-document YAML
    /// stream, with its `---` line and the comment lines just above that
    /// line, which lead into it; every other byte stays. Its own trailing
    /// comment lines lead into the next document and stay. A stream left
    /// with one document is read as that document, no longer a stream.
    ///
    /// # Errors
    ///
    /// - [`Error::WriteUnavailable`] — the document is not a write-aware
    ///   YAML stream, or has no document `index`.
    /// - [`Error::EditRejected`] — the rest of the file would not read back
    ///   as the other documents (or an alias elsewhere names an anchor in
    ///   the removed one); `self` is unchanged.
    pub(crate) fn remove_stream_document(&mut self, index: usize) -> Result<()> {
        let unavailable = |reason: &str| Error::WriteUnavailable {
            reason: reason.to_owned(),
        };
        if self.format != FormatTag::Yaml || !self.write_aware {
            return Err(unavailable("not a write-aware YAML stream"));
        }
        let mut expected = self
            .values()
            .ok_or_else(|| unavailable("not a multi-document stream"))?
            .to_vec();
        if index >= expected.len() {
            return Err(unavailable(&format!("the stream has no document {index}")));
        }
        expected.remove(index);
        let extents = document_extents(&self.original_bytes)
            .filter(|extents| extents.len() == expected.len() + 1)
            .ok_or_else(|| unavailable("the stream's documents could not be located"))?;
        let extent = &extents[index];
        // The `...` line that ends a document, when one does.
        let end_marker = |at: usize| {
            let extent = &extents[at];
            let next = extents
                .get(at + 1)
                .map_or(self.original_bytes.len(), |next| next.line_start);
            (extent.body.end < next).then(|| {
                self.original_bytes[extent.body.end..next]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(next, |newline| extent.body.end + newline + 1)
            })
        };
        let start = match index.checked_sub(1) {
            // The lead-in after the document before: unless that one ends
            // with a `...` line, which stays to end it.
            Some(before) if end_marker(before).is_none() => extents[before].content_end,
            Some(_) => extent.line_start,
            None => extent.line_start.max(super::bom_end(&self.original_bytes)),
        };
        let mut end = extent.content_end.max(extent.body.start).max(start);
        // Its own `...` line goes too where nothing is left for it to end:
        // before the first document, or after another `...` line.
        if (index == 0 || start == extent.line_start)
            && let Some(marker_end) = end_marker(index)
        {
            end = marker_end;
        }
        let mut bytes = self.original_bytes[..start].to_vec();
        bytes.extend_from_slice(&self.original_bytes[end..]);
        // An alias cannot name an anchor of another document, so what
        // is left reads as the other documents or the read-back says why.
        let stream = expected.len() > 1;
        let reparsed = read_back(&bytes, &expected, stream, &format!("/{index}"), "delete")?;
        *self = reparsed;
        Ok(())
    }

    /// The original bytes with `range` replaced by `emitted` — the
    /// writer's `\n`-broken output, after a line break when
    /// `needs_line_break` — in the line ending of the file around it.
    fn splice(
        &self,
        range: std::ops::Range<usize>,
        needs_line_break: bool,
        emitted: &[u8],
    ) -> Vec<u8> {
        let mut inserted = Vec::with_capacity(emitted.len() + 1);
        if needs_line_break {
            inserted.push(b'\n');
        }
        inserted.extend_from_slice(emitted);
        // The writer breaks lines with `\n`; a CRLF file gets `\r\n`, the
        // same way a textual edit's inserted lines do.
        let inserted = with_line_ending_at(&self.original_bytes, range.start, inserted);
        let mut bytes = Vec::with_capacity(self.original_bytes.len() + inserted.len());
        bytes.extend_from_slice(&self.original_bytes[..range.start]);
        bytes.extend_from_slice(&inserted);
        bytes.extend_from_slice(&self.original_bytes[range.end..]);
        bytes
    }
}

/// Whether the tag among the node properties `properties` (the marker
/// line's `!tag` / `&anchor`, then any comment) still fits the document
/// once its value `previous` becomes `value`, written as `emitted`.
///
/// The read-back cannot tell: dq reads a document's value without its
/// tag, so `--- !!map` over a sequence reads back fine while other YAML
/// readers reject it. So:
///
/// - a YAML core-schema tag (`!!map`, `!<tag:yaml.org,2002:seq>`) fits a
///   value of the kind it names;
/// - the non-specific `!` fits only a string written as a plain scalar
///   (`! 5` is the string `"5"`, which dq would read as an int);
/// - any other tag — an application's `!foo` — fits only a value of the
///   same node kind as `previous`: mapping, sequence or scalar.
///
/// Without a tag the properties always fit.
fn tags_fit(properties: &[u8], previous: &Value, value: &Value, emitted: &[u8]) -> bool {
    let node_kind = |value: &Value| match value {
        Value::Map(_) => 0,
        Value::Array(_) => 1,
        _ => 2,
    };
    properties
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty())
        .take_while(|token| !token.starts_with(b"#"))
        .filter(|token| token.starts_with(b"!"))
        .all(|tag| {
            let core = tag.strip_prefix(b"!!").or_else(|| {
                tag.strip_prefix(b"!<tag:yaml.org,2002:")
                    .and_then(|rest| rest.strip_suffix(b">"))
            });
            if let Some(kind) = core {
                return matches!(
                    (kind, value),
                    (b"map", Value::Map(_))
                        | (b"seq", Value::Array(_))
                        | (b"str", Value::String(_))
                        | (b"int", Value::Int(_) | Value::BigInt(_))
                        | (b"float", Value::Float(_) | Value::BigFloat(_))
                        | (b"bool", Value::Bool(_))
                        | (b"null", Value::Null)
                );
            }
            if tag == b"!" {
                let plain = emitted
                    .first()
                    .is_some_and(|b| !matches!(b, b'\'' | b'"' | b'|' | b'>'));
                return matches!(value, Value::String(_)) && plain;
            }
            node_kind(previous) == node_kind(value)
        })
}

/// Past the comment and blank lines from the line start `start` on, up
/// to `content_end` at most: where a document's content begins.
fn past_lead_in(bytes: &[u8], start: usize, content_end: usize) -> usize {
    let mut at = start;
    while at < content_end {
        let line_end = bytes[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |newline| at + newline + 1);
        let line = bytes[at..line_end].trim_ascii();
        if !(line.is_empty() || line.starts_with(b"#")) {
            break;
        }
        at = line_end;
    }
    at.min(content_end)
}

/// The extent of a document after the last byte of `bytes`, for a file
/// that holds no document yet (only comments).
fn end_of_file_extent(bytes: &[u8]) -> DocumentExtent {
    let end = bytes.len();
    DocumentExtent {
        line_start: end,
        body: end..end,
        // A byte order mark alone is no line to end.
        needs_line_break: bytes.len() > super::bom_end(bytes) && !bytes.ends_with(b"\n"),
        content_end: end,
        after_properties: None,
    }
}

/// `bytes` parsed as a stream, when it reads back as the documents
/// `expected` — as one document, not a stream, unless `stream` — and
/// [`Error::EditRejected`] for `pointer` otherwise.
fn read_back(
    bytes: &[u8],
    expected: &[Value],
    stream: bool,
    pointer: &str,
    action: &'static str,
) -> Result<Document> {
    let rejected = |reason: String| Error::EditRejected {
        action,
        pointer: pointer.to_owned(),
        reason,
    };
    let reparsed = parse_yaml_stream_with_spans(bytes)
        .map_err(|err| rejected(format!("the rewritten file does not parse: {err}")))?;
    let single;
    let documents = match (stream, reparsed.values()) {
        (true, Some(documents)) => documents,
        (false, None) => {
            single = [reparsed.value().clone()];
            &single[..]
        }
        _ => &[],
    };
    if documents.len() != expected.len() {
        return Err(rejected(format!(
            "the rewritten file holds {} document(s) instead of {}",
            documents.len(),
            expected.len()
        )));
    }
    for (at, (want, got)) in expected.iter().zip(documents).enumerate() {
        if let Some(path) = first_difference(want, got, &format!("/{at}")) {
            return Err(rejected(format!(
                "the rewritten file reads back differently at {path}"
            )));
        }
    }
    Ok(reparsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(bytes: &str) -> Document {
        let doc = parse_yaml_stream_with_spans(bytes.as_bytes()).expect("parse");
        assert!(doc.is_multi(), "fixture must be a stream");
        doc
    }

    fn text(doc: &Document) -> &str {
        std::str::from_utf8(doc.original_bytes()).expect("UTF-8")
    }

    fn map(key: &str, value: &str) -> Value {
        let mut map = indexmap::IndexMap::new();
        map.insert(key.to_owned(), Value::String(value.to_owned()));
        Value::Map(map)
    }

    #[test]
    fn rewrites_one_document_and_keeps_every_other_byte() {
        let mut doc =
            stream("# head\nname: a # keep\n---  # sep\nx: 1 # c\n---\n- 1\n---\nname: z\n");
        doc.reemit_stream_document(1, map("name", "fixed"))
            .expect("re-emit");
        assert_eq!(
            text(&doc),
            "# head\nname: a # keep\n---  # sep\nname: fixed\n---\n- 1\n---\nname: z\n"
        );
        assert!(doc.is_multi());
        // Still write-aware: a textual edit in another document applies.
        doc.set_at(
            &crate::Pointer::parse("/3/name").unwrap(),
            Value::String("q".to_owned()),
        )
        .expect("set_at after re-emit");
        assert!(text(&doc).ends_with("name: q\n"), "{}", text(&doc));
        assert!(text(&doc).starts_with("# head\nname: a # keep\n"));
    }

    #[test]
    fn rewrites_the_first_and_the_last_document() {
        let mut doc = stream("name: a # c\n---\nname: b # d\n");
        doc.reemit_stream_document(0, map("name", "x"))
            .expect("first");
        assert_eq!(text(&doc), "name: x\n---\nname: b # d\n");
        doc.reemit_stream_document(1, map("name", "w"))
            .expect("last");
        assert_eq!(text(&doc), "name: x\n---\nname: w\n");
    }

    #[test]
    fn rewrites_content_on_the_marker_line_and_an_empty_document() {
        let mut doc = stream("--- {a: 1}\n--- |\n  x\n");
        doc.reemit_stream_document(0, map("a", "2")).expect("flow");
        assert_eq!(text(&doc), "---\na: '2'\n--- |\n  x\n");

        let mut doc = stream("name: a\n---");
        doc.reemit_stream_document(1, map("name", "b"))
            .expect("empty");
        assert_eq!(text(&doc), "name: a\n---\nname: b\n");
    }

    #[test]
    fn keeps_crlf_line_ends_in_the_rewritten_document_and_its_block_scalar() {
        let mut doc = stream("x: 1\r\n---\r\nname: b # k\r\n");
        doc.reemit_stream_document(1, map("text", "one\ntwo\n"))
            .expect("re-emit");
        assert_eq!(text(&doc), "x: 1\r\n---\r\ntext: |\r\n  one\r\n  two\r\n");
        assert_eq!(doc.values().unwrap()[1], map("text", "one\ntwo\n"));

        let mut doc = stream("--- {a: 1}\r\n---\r\nb: 2\r\n");
        doc.reemit_stream_document(0, map("a", "2"))
            .expect("marker line");
        assert_eq!(text(&doc), "---\r\na: '2'\r\n---\r\nb: 2\r\n");
    }

    #[test]
    fn keeps_the_comment_lines_between_the_marker_and_the_content() {
        let mut doc = stream("a: 1\n--- # m\n# lead\n\n  # two\nb: 2\n---\n# c\n---\nc: 3\n");
        doc.reemit_stream_document(1, map("b", "x"))
            .expect("lead-in");
        doc.reemit_stream_document(2, map("e", "y")).expect("empty");
        assert_eq!(
            text(&doc),
            "a: 1\n--- # m\n# lead\n\n  # two\nb: x\n---\ne: 'y'\n# c\n---\nc: 3\n"
        );
        assert_eq!(past_lead_in(b"# a\n\nb: 1\n", 0, 9), 5);
        assert_eq!(past_lead_in(b"# a\n", 0, 0), 0);
    }

    #[test]
    fn keeps_the_comment_lines_after_the_last_content_line() {
        let mut doc = stream("a: 1\n---\nfixme: 1 # k\n\n# next\n---\nb: 2\n# tail\n");
        doc.reemit_stream_document(1, map("name", "fixed"))
            .expect("middle");
        doc.reemit_stream_document(2, map("b", "3")).expect("last");
        assert_eq!(
            text(&doc),
            "a: 1\n---\nname: fixed\n\n# next\n---\nb: '3'\n# tail\n"
        );

        let mut doc = stream("a: 1\r\n# c\r\n---\r\nb: 2\r\n");
        doc.reemit_stream_document(0, map("a", "2")).expect("crlf");
        assert_eq!(text(&doc), "a: '2'\r\n# c\r\n---\r\nb: 2\r\n");
    }

    #[test]
    fn keeps_node_properties_on_the_marker_line_when_they_still_fit() {
        let mut doc = stream("--- !!map\nfixme: 1\n--- &anc # c\nb: 2\n");
        doc.reemit_stream_document(0, map("name", "fixed"))
            .expect("tag");
        doc.reemit_stream_document(1, map("b", "3"))
            .expect("anchor");
        assert_eq!(text(&doc), "--- !!map\nname: fixed\n--- &anc # c\nb: '3'\n");

        // A `!!map` tag does not fit a sequence: it is dropped.
        let mut doc = stream("--- !!map\nfixme: 1\n---\nb: 2\n");
        doc.reemit_stream_document(0, Value::Array(vec![Value::Bool(true)]))
            .expect("sequence");
        assert_eq!(text(&doc), "---\n- true\n---\nb: 2\n");
    }

    #[test]
    fn keeps_an_application_tag_only_over_the_same_node_kind() {
        let src = "a: 1\n--- !foo\ntoscalar: 1\n---\nb: 2\n";
        let mut doc = stream(src);
        doc.reemit_stream_document(1, Value::Int(5)).expect("int");
        assert_eq!(text(&doc), "a: 1\n---\n5\n---\nb: 2\n");
        let mut doc = stream(src);
        doc.reemit_stream_document(1, map("name", "fixed"))
            .expect("map");
        assert_eq!(text(&doc), "a: 1\n--- !foo\nname: fixed\n---\nb: 2\n");
        let mut doc = stream("a: 1\n--- !foo 3\n---\nb: 2\n");
        doc.reemit_stream_document(1, Value::Int(5))
            .expect("scalar");
        assert_eq!(text(&doc), "a: 1\n--- !foo\n5\n---\nb: 2\n");
    }

    #[test]
    fn keeps_the_non_specific_tag_only_over_a_plain_string() {
        let src = "a: 1\n--- !\ntoscalar: 1\n---\nb: 2\n";
        let mut doc = stream(src);
        doc.reemit_stream_document(1, Value::Int(5)).expect("int");
        assert_eq!(text(&doc), "a: 1\n---\n5\n---\nb: 2\n");
        let mut doc = stream(src);
        doc.reemit_stream_document(1, Value::String("5".to_owned()))
            .expect("quoted string");
        assert_eq!(text(&doc), "a: 1\n---\n'5'\n---\nb: 2\n");
        let mut doc = stream(src);
        doc.reemit_stream_document(1, Value::String("hi".to_owned()))
            .expect("plain string");
        assert_eq!(text(&doc), "a: 1\n--- !\nhi\n---\nb: 2\n");
    }

    #[test]
    fn refuses_what_is_not_a_write_aware_stream() {
        let mut single =
            crate::parsers::yaml_spans::parse_yaml_stream_with_spans(b"a: 1\n").unwrap();
        assert!(matches!(
            single.reemit_stream_document(0, Value::Null),
            Err(Error::WriteUnavailable { .. })
        ));
        let mut doc = stream("a: 1\n---\nb: 2\n");
        assert!(matches!(
            doc.reemit_stream_document(2, Value::Null),
            Err(Error::WriteUnavailable { .. })
        ));
    }
}
