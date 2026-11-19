//! Re-emitting one document of a write-aware YAML stream.

use super::{Document, FormatTag, Value, first_difference, with_line_ending_at};
use crate::Result;
use crate::error::Error;
use crate::format::Format as _;
use crate::parsers::yaml_spans::{document_extents, parse_yaml_stream_with_spans};

impl Document {
    /// Replace document `index` of a write-aware multi-document YAML
    /// stream with `value`, written through the YAML writer, and keep
    /// every other byte of the file.
    ///
    /// Only that document's body is rewritten: its `---` line (with any
    /// comment on it) and every other document — separators, comments,
    /// quoting and all — stay byte for byte. Comments inside the
    /// rewritten document are dropped, as any writer re-emit drops them.
    /// The rewritten body breaks lines the way the file does around it:
    /// `\r\n` in a CRLF file, `\n` otherwise.
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
        if self.format != FormatTag::Yaml || self.original_bytes.is_empty() {
            return Err(unavailable("not a write-aware YAML stream"));
        }
        let mut expected = self
            .values()
            .ok_or_else(|| unavailable("not a multi-document stream"))?
            .to_vec();
        let Some(slot) = expected.get_mut(index) else {
            return Err(unavailable(&format!("the stream has no document {index}")));
        };
        *slot = value;
        let extents = document_extents(&self.original_bytes)
            .filter(|extents| extents.len() == expected.len())
            .ok_or_else(|| unavailable("the stream's documents could not be located"))?;
        let extent = &extents[index];

        let mut emitted = Vec::new();
        crate::parsers::Yaml.write(
            &Document::value_only(expected[index].clone(), FormatTag::Yaml),
            &mut emitted,
        )?;
        if extent.needs_line_break {
            emitted.insert(0, b'\n');
        }
        // The writer breaks lines with `\n`; a CRLF file gets `\r\n`, the
        // same way a textual edit's inserted lines do.
        let emitted = with_line_ending_at(&self.original_bytes, extent.body.start, emitted);
        let mut bytes = Vec::with_capacity(self.original_bytes.len() + emitted.len());
        bytes.extend_from_slice(&self.original_bytes[..extent.body.start]);
        bytes.extend_from_slice(&emitted);
        bytes.extend_from_slice(&self.original_bytes[extent.body.end..]);

        let pointer = format!("/{index}");
        let rejected = |reason: String| Error::EditRejected {
            action: "set",
            pointer: pointer.clone(),
            reason,
        };
        let reparsed = parse_yaml_stream_with_spans(&bytes)
            .map_err(|err| rejected(format!("the rewritten file does not parse: {err}")))?;
        let documents = reparsed.values().unwrap_or_default();
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
        *self = reparsed;
        Ok(())
    }
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
