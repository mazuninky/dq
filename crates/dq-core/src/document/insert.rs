//! A new item of an array, written as a textual edit: RFC 6902 `add`
//! into an array, which inserts where `set` replaces.

use std::ops::Range;

use super::{
    Document, FormatTag, JQ_REWRITE_HINT, LayoutCache, Value, first_difference, line_column,
    line_start, parse_again, with_line_ending_at,
};
use crate::Result;
use crate::error::{Error, PathErrorKind};
use crate::pointer::{Pointer, Segment};

impl Document {
    /// Insert `value` into the array that holds `pointer`, at the index
    /// its last segment names; the items from there on move up one. The
    /// array's length, or `-`, appends. This is RFC 6902 §4.1 `add` into an
    /// array — [`Document::set_at`] replaces the item at an index instead.
    ///
    /// In a write-aware YAML stream a pointer of one segment (`/N`) names
    /// a document: [`Document::insert_stream_document`] puts a new one in.
    ///
    /// The new item is written where the others are, as they are written:
    /// a `- ` line in a YAML block sequence, an entry with the separator the
    /// entries already have between them in a YAML flow sequence, a JSON
    /// array or a TOML inline array, a `[[header]]` section in a TOML array
    /// of tables. Every other byte stays, comments included. The edited
    /// bytes are parsed again and must read back as the array with `value`
    /// inserted; otherwise the document is left as it was.
    ///
    /// # Errors
    ///
    /// - [`Error::Path`] — the pointer is the root; its parent does not
    ///   resolve, or is no array (`TypeMismatch`); the index is past the
    ///   array's length (`OutOfBounds`) or no index.
    /// - [`Error::WriteUnavailable`] — the document was loaded read-only,
    ///   or its format has no textual edits.
    /// - [`Error::EditRejected`] — the item cannot be written here as a
    ///   textual edit, or the edit would not read back.
    pub fn insert_at(&mut self, pointer: &Pointer, value: Value) -> Result<()> {
        let canonical = pointer.as_canonical();
        let Some((last, parent)) = pointer.segments().split_last() else {
            return Err(Error::Path {
                pointer: canonical,
                matched_prefix: String::new(),
                kind: PathErrorKind::TypeMismatch {
                    expected: "an array item",
                    found: "root",
                },
                did_you_mean: Vec::new(),
            });
        };
        let parent = Pointer::new(parent.to_vec());
        let len = match parent.resolve(&self.value)? {
            Value::Array(items) => items.len(),
            other => {
                return Err(Error::Path {
                    pointer: canonical,
                    matched_prefix: parent.as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "array",
                        found: other.type_name(),
                    },
                    did_you_mean: Vec::new(),
                });
            }
        };
        let index = array_index(last, len, &canonical, &parent)?;
        if self.format == FormatTag::Yaml && self.values().is_some() && parent.is_root() {
            return self.insert_stream_document(index, value);
        }
        self.with_lf_line_breaks("insert", pointer, |doc| {
            let before = doc.clone();
            let result = doc.insert_unchecked(&parent, index, value);
            if result.is_err() {
                *doc = before;
            }
            result
        })
    }

    /// Insert `value` at `index` of the array at `parent`: first a
    /// placeholder item, checked to read back, then `value` set over it.
    fn insert_unchecked(&mut self, parent: &Pointer, index: usize, value: Value) -> Result<()> {
        if !self.write_aware {
            return Err(Error::WriteUnavailable {
                reason: format!(
                    "{} document was loaded read-only; reload via a write-aware parser to enable add",
                    self.format.name(),
                ),
            });
        }
        let mut item = parent.clone();
        item.push_segment(Segment::Index(index));
        let canonical = item.as_canonical();
        let cannot = || Error::EditRejected {
            action: "insert",
            pointer: canonical.clone(),
            reason: format!("dq cannot write a new item here as a textual edit; {JQ_REWRITE_HINT}"),
        };
        let parent_canonical = parent.as_canonical();
        let len = match parent.resolve(&self.value) {
            Ok(Value::Array(items)) => items.len(),
            _ => return Err(cannot()),
        };
        let (placeholder, splice) = match self.format {
            FormatTag::Yaml => (
                Value::Null,
                self.yaml_item_insertion(&parent_canonical, index, len),
            ),
            FormatTag::Json => (
                Value::Null,
                self.json_item_insertion(&parent_canonical, index, len),
            ),
            FormatTag::Toml => match self.toml_item_insertion(&parent_canonical, index, len) {
                // A table of an array of tables: its keys go in one by one.
                TomlInsertion::Table(splice) => {
                    let Value::Map(map) = value else {
                        return Err(Error::EditRejected {
                            action: "insert",
                            pointer: canonical,
                            reason: format!(
                                "an array of tables holds only tables; {JQ_REWRITE_HINT}"
                            ),
                        });
                    };
                    self.splice_placeholder(splice, parent, index, Value::Map(Default::default()))
                        .map_err(|()| cannot())?;
                    for (key, value) in map {
                        let mut at = item.clone();
                        at.push_segment(Segment::Key(key));
                        self.set_at(&at, value)?;
                    }
                    return Ok(());
                }
                TomlInsertion::Item(splice) => (Value::Int(0), splice),
                TomlInsertion::None => (Value::Int(0), None),
            },
            _ => {
                return Err(Error::WriteUnavailable {
                    reason: format!("{} has no textual edits", self.format.name()),
                });
            }
        };
        let splice = splice.map(|(at, text)| {
            let text = match placeholder {
                Value::Int(_) => text.replace(PLACEHOLDER, "0"),
                _ => text.replace(PLACEHOLDER, "null"),
            };
            (at, text.into_bytes())
        });
        if self
            .splice_placeholder(splice, parent, index, placeholder.clone())
            .is_err()
        {
            // An array under an alias is the anchored node's: its bytes,
            // and the place to edit, are there.
            return Err(self
                .through_alias(&item, &LayoutCache::new(), "insert")
                .unwrap_or_else(cannot));
        }
        if value.same_data(&placeholder) && !matches!(placeholder, Value::Int(_)) {
            return Ok(());
        }
        self.set_at(&item, value)
    }

    /// Splice `splice`'s text in and adopt the bytes when they read back
    /// as the tree with `placeholder` inserted at `index` of `parent`.
    fn splice_placeholder(
        &mut self,
        splice: Option<(usize, Vec<u8>)>,
        parent: &Pointer,
        index: usize,
        placeholder: Value,
    ) -> std::result::Result<(), ()> {
        let (at, text) = splice.ok_or(())?;
        let text = with_line_ending_at(&self.original_bytes, at, text);
        // The edited node is the array: an alias of the item at `index`
        // keeps naming that item, now one further on.
        let insert = |tree: &mut Value, array: &Pointer| -> Result<()> {
            match value_mut(tree, array) {
                Some(Value::Array(items)) if index <= items.len() => {
                    items.insert(index, placeholder.clone());
                    Ok(())
                }
                _ => Err(Error::WriteUnavailable {
                    reason: "no array to insert into".to_owned(),
                }),
            }
        };
        let mut bytes = self.original_bytes.clone();
        bytes.splice(at..at, text);
        let Some(Ok(fresh)) = parse_again(self.format, &bytes) else {
            return Err(());
        };
        // The edit alone first; in YAML with aliases, also under every
        // alias of an anchored node at or above the array, which shares
        // the new item — as `set_at` expects it.
        let reads_back = |aliases: Option<&LayoutCache>| {
            self.expected_after_edit(parent, aliases, &insert)
                .is_some_and(|tree| first_difference(&tree, &fresh.value, "").is_none())
        };
        let layout = LayoutCache::new();
        let fits = reads_back(None)
            || (self.format == FormatTag::Yaml
                && self.original_bytes.contains(&b'*')
                && reads_back(Some(&layout)));
        if !fits {
            return Err(());
        }
        self.original_bytes = bytes;
        self.value = fresh.value;
        self.spans = fresh.spans;
        self.provenance = fresh.provenance;
        Ok(())
    }

    /// Where a new item of the YAML array at `parent` goes, and its text.
    fn yaml_item_insertion(
        &self,
        parent: &str,
        index: usize,
        len: usize,
    ) -> Option<(usize, String)> {
        let cache = LayoutCache::new();
        let layout = self.edit_layout(&cache)?;
        let container = layout.containers.get(parent)?;
        let bytes = &self.original_bytes;
        if !container.block {
            let items = (0..len)
                .map(|k| layout.flow_entries.get(&format!("{parent}/{k}")).cloned())
                .collect::<Option<Vec<_>>>()?;
            return flow_item_insertion(bytes, container.brackets.clone()?, &items, index);
        }
        let entry = |k: usize| layout.entries.get(&format!("{parent}/{k}"));
        if index < len {
            let entry = entry(index)?;
            let start = line_start(bytes, entry.at);
            let column = entry.at - start;
            if !entry.owns_line || bytes[start..entry.at].iter().any(|&b| b != b' ') {
                // The first item of a compact sequence (`- - 1`) shares its
                // line with what holds it: the new item takes its place
                // there, and it moves to the next line, at its column.
                return Some((entry.at, format!("- {PLACEHOLDER}\n{}", " ".repeat(column))));
            }
            // The comment lines right above an item lead into it.
            let at = if index == 0 {
                start
            } else {
                before_comment_lines(bytes, start, column)
            };
            Some((at, format!("{}- {PLACEHOLDER}\n", " ".repeat(column))))
        } else {
            let column = line_column(bytes, entry(len.checked_sub(1)?)?.at);
            let at = container.end;
            let mut text = format!("{}- {PLACEHOLDER}\n", " ".repeat(column));
            if at > super::bom_end(bytes) && bytes[at - 1] != b'\n' {
                text.insert(0, '\n');
            }
            Some((at, text))
        }
    }

    /// Where a new item of the JSON array at `parent` goes, and its text.
    fn json_item_insertion(
        &self,
        parent: &str,
        index: usize,
        len: usize,
    ) -> Option<(usize, String)> {
        let bytes = &self.original_bytes;
        let containers = crate::parsers::json::json_container_ranges(bytes).ok()?;
        let brackets = match containers.get(parent) {
            Some(range) => range.clone(),
            None => self.spans.get(parent)?.value_range.clone(),
        };
        let items = (0..len)
            .map(|k| {
                let item = format!("{parent}/{k}");
                containers
                    .get(&item)
                    .cloned()
                    .or_else(|| Some(self.spans.get(&item)?.value_range.clone()))
            })
            .collect::<Option<Vec<_>>>()?;
        flow_item_insertion(bytes, brackets, &items, index)
    }

    /// Where a new item of the TOML array at `parent` goes, and its text.
    fn toml_item_insertion(&self, parent: &str, index: usize, len: usize) -> TomlInsertion {
        let bytes = &self.original_bytes;
        let Some(inline) = crate::parsers::toml::toml_inline_collections(bytes) else {
            return TomlInsertion::None;
        };
        let brackets = match inline.get(parent) {
            Some((range, _)) => Some(range.clone()),
            None => self
                .spans
                .get(parent)
                .filter(|_| len == 0)
                .map(|span| span.value_range.clone()),
        };
        if let Some(brackets) = brackets {
            let items = (0..len)
                .map(|k| {
                    let item = format!("{parent}/{k}");
                    inline
                        .get(&item)
                        .map(|(range, _)| range.clone())
                        .or_else(|| Some(self.spans.get(&item)?.value_range.clone()))
                })
                .collect::<Option<Vec<_>>>();
            return TomlInsertion::Item(
                items.and_then(|items| flow_item_insertion(bytes, brackets, &items, index)),
            );
        }
        let Some(tables) = crate::parsers::toml::toml_tables(bytes) else {
            return TomlInsertion::None;
        };
        let Some(neighbour) = (if index < len {
            Some(index)
        } else {
            len.checked_sub(1)
        }) else {
            return TomlInsertion::None;
        };
        let neighbour = format!("{parent}/{neighbour}");
        let ranges = tables.ranges_of(&neighbour);
        let header = tables
            .header_line_ends
            .get(&neighbour)
            .and_then(|&end| {
                let line = &bytes[line_start(bytes, end.saturating_sub(1))..end];
                let close = line.windows(2).position(|pair| pair == b"]]")?;
                Some(
                    String::from_utf8_lossy(&line[..close + 2])
                        .trim()
                        .to_owned(),
                )
            })
            .filter(|header| header.starts_with("[["));
        let (Some(header), Some(first), Some(last)) = (header, ranges.first(), ranges.last())
        else {
            return TomlInsertion::None;
        };
        let at = if index < len { first.start } else { last.end };
        let mut text = format!("{header}\n").into_bytes();
        if at > super::bom_end(bytes) && bytes[at - 1] != b'\n' {
            text.insert(0, b'\n');
        }
        // A blank line ends the section before: it ends the new one too.
        if bytes[..at].ends_with(b"\n\n") || bytes[..at].ends_with(b"\n\r\n") {
            text.push(b'\n');
        }
        TomlInsertion::Table(Some((at, text)))
    }
}

/// The value at `pointer` in `root`, to change.
fn value_mut<'a>(root: &'a mut Value, pointer: &Pointer) -> Option<&'a mut Value> {
    pointer
        .segments()
        .iter()
        .try_fold(root, |current, segment| match (current, segment) {
            (Value::Map(map), Segment::Key(key)) => map.get_mut(key),
            (Value::Array(items), Segment::Index(index)) => items.get_mut(*index),
            (Value::Array(items), Segment::Key(key)) => items.get_mut(key.parse::<usize>().ok()?),
            _ => None,
        })
}

/// What a TOML array takes a new item as.
enum TomlInsertion {
    /// An entry of an inline array: where, and its text.
    Item(Option<(usize, String)>),
    /// A `[[header]]` section: where, and its header line.
    Table(Option<(usize, Vec<u8>)>),
    /// Neither could be located.
    None,
}

/// Stands for the placeholder item's text until the format is known.
const PLACEHOLDER: &str = "\u{0}";

/// The index the last segment of an `add` pointer names in an array of
/// `len` items: a number up to `len`, or `-` for `len`.
fn array_index(segment: &Segment, len: usize, canonical: &str, parent: &Pointer) -> Result<usize> {
    let index = match segment {
        Segment::Index(index) => Some(*index),
        Segment::Key(key) if key == "-" => Some(len),
        Segment::Key(key) => key.parse::<usize>().ok(),
    };
    let Some(index) = index else {
        return Err(Error::Path {
            pointer: canonical.to_owned(),
            matched_prefix: parent.as_canonical(),
            kind: PathErrorKind::TypeMismatch {
                expected: "array index",
                found: "non-numeric key",
            },
            did_you_mean: Vec::new(),
        });
    };
    if index > len {
        return Err(Error::Path {
            pointer: canonical.to_owned(),
            matched_prefix: parent.as_canonical(),
            kind: PathErrorKind::OutOfBounds,
            did_you_mean: Vec::new(),
        });
    }
    Ok(index)
}

/// Where a new entry of a flow array goes, and its text: before entry
/// `index`, or after the last one, with the separator the entries have
/// between them; between the brackets of an empty array.
///
/// Without a separator free of comments (one entry, or a comment after
/// every comma), an entry that starts its line gets the new one on a line
/// of its own, at its indent: before it, or after the last one — past the
/// comma and the comment that follow it, when they do. Otherwise `, `.
fn flow_item_insertion(
    bytes: &[u8],
    brackets: Range<usize>,
    items: &[Range<usize>],
    index: usize,
) -> Option<(usize, String)> {
    let Some(last) = items.last() else {
        return (bytes.get(brackets.start) == Some(&b'['))
            .then(|| (brackets.start + 1, PLACEHOLDER.to_owned()));
    };
    let separator = items
        .windows(2)
        .map(|pair| &bytes[pair[0].end..pair[1].start])
        .find(|between| is_plain_separator(between))
        .map(|between| String::from_utf8_lossy(between).into_owned());
    let neighbour = &items[index.min(items.len() - 1)];
    let start = line_start(bytes, neighbour.start);
    let own_line = separator.is_none()
        && start > brackets.start
        && bytes[start..neighbour.start]
            .iter()
            .all(|&b| b == b' ' || b == b'\t');
    if own_line {
        let indent = String::from_utf8_lossy(&bytes[start..neighbour.start]).into_owned();
        if index < items.len() {
            return Some((neighbour.start, format!("{PLACEHOLDER},\n{indent}")));
        }
        let blank = |b: &&u8| matches!(b, b' ' | b'\t');
        let after = last.end + bytes[last.end..].iter().take_while(blank).count();
        if bytes.get(after) == Some(&b',') {
            // Past the comma and what follows it on the line.
            let line_end = bytes[after..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |at| after + at);
            let line_end = if line_end > after && bytes[line_end - 1] == b'\r' {
                line_end - 1
            } else {
                line_end
            };
            return Some((line_end, format!("\n{indent}{PLACEHOLDER},")));
        }
        if matches!(bytes.get(after), Some(b'\n' | b'\r')) {
            return Some((last.end, format!(",\n{indent}{PLACEHOLDER}")));
        }
    }
    let separator = separator.unwrap_or_else(|| ", ".to_owned());
    if index < items.len() {
        Some((items[index].start, format!("{PLACEHOLDER}{separator}")))
    } else {
        Some((last.end, format!("{separator}{PLACEHOLDER}")))
    }
}

/// Whether `between` two flow entries is a comma with blanks around it
/// and at most one line break after it.
fn is_plain_separator(between: &[u8]) -> bool {
    let blank = |b: &u8| matches!(b, b' ' | b'\t');
    let rest = &between[between.iter().take_while(|b| blank(b)).count()..];
    let Some(rest) = rest.strip_prefix(b",") else {
        return false;
    };
    let rest = &rest[rest.iter().take_while(|b| blank(b)).count()..];
    let rest = rest
        .strip_prefix(b"\r\n")
        .or_else(|| rest.strip_prefix(b"\n"))
        .unwrap_or(rest);
    rest.iter().all(blank)
}

/// `start`, the start of a line, moved back over the comment-only lines
/// right above it with their `#` at `column`.
fn before_comment_lines(bytes: &[u8], mut start: usize, column: usize) -> usize {
    while start > 0 {
        let above = line_start(bytes, start - 1);
        let line = &bytes[above..start];
        let indent = line.iter().take_while(|&&b| b == b' ').count();
        if indent != column || line.get(indent) != Some(&b'#') {
            break;
        }
        start = above;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inserted(format: &str, src: &str, pointer: &str, value: Value) -> Result<String> {
        let mut doc = match format {
            "yaml" => crate::parsers::parse_yaml_stream_with_spans(src.as_bytes())?,
            "json" => crate::parsers::parse_json_with_spans(src.as_bytes())?,
            _ => crate::format::Format::parse(&crate::parsers::Toml, src.as_bytes())?,
        };
        doc.insert_at(&Pointer::parse(pointer)?, value)?;
        Ok(String::from_utf8(doc.original_bytes().to_vec()).unwrap())
    }

    #[test]
    fn inserts_into_each_format_where_the_items_are() {
        let nine = || Value::Int(9);
        for (format, src, pointer, want) in [
            ("yaml", "- 1\n- 2\n", "/0", "- 9\n- 1\n- 2\n"),
            ("yaml", "- 1\n- 2\n", "/-", "- 1\n- 2\n- 9\n"),
            (
                "yaml",
                "k:\n  - 1\n  # two\n  - 2\nz: 0\n",
                "/k/1",
                "k:\n  - 1\n  - 9\n  # two\n  - 2\nz: 0\n",
            ),
            (
                "yaml",
                "k:\n  - 1\nz: 0\n",
                "/k/1",
                "k:\n  - 1\n  - 9\nz: 0\n",
            ),
            ("yaml", "a: [1, 2]\n", "/a/1", "a: [1, 9, 2]\n"),
            ("yaml", "a: []\n", "/a/0", "a: [9]\n"),
            ("json", "[1,2]", "/2", "[1,2,9]"),
            (
                "json",
                "{\"a\": [\n  1,\n  2\n]}",
                "/a/0",
                "{\"a\": [\n  9,\n  1,\n  2\n]}",
            ),
            ("json", "{\"a\": []}", "/a/-", "{\"a\": [9]}"),
            ("toml", "a = [1, 2] # c\n", "/a/1", "a = [1, 9, 2] # c\n"),
            ("toml", "a = []\n", "/a/0", "a = [9]\n"),
        ] {
            assert_eq!(
                inserted(format, src, pointer, nine()).unwrap(),
                want,
                "{format} {src:?} {pointer}"
            );
        }
    }

    #[test]
    fn inserts_a_table_into_a_toml_array_of_tables() {
        let mut table = indexmap::IndexMap::new();
        table.insert("y".to_owned(), Value::Int(2));
        let got = inserted("toml", "[[t]]\nx = 1\n", "/t/0", Value::Map(table)).unwrap();
        assert_eq!(got, "[[t]]\ny = 2\n[[t]]\nx = 1\n");
    }

    #[test]
    fn inserts_a_document_into_a_stream() {
        let got = inserted("yaml", "a: 1\n---\nb: 2\n", "/1", Value::Int(9)).unwrap();
        assert_eq!(got, "a: 1\n---\n9\n---\nb: 2\n");
        // Before a document's directives, ended by `...`.
        let got = inserted(
            "yaml",
            "%YAML 1.2\n---\na: 1\n---\nb\n",
            "/0",
            Value::Int(9),
        )
        .unwrap();
        assert_eq!(got, "---\n9\n...\n%YAML 1.2\n---\na: 1\n---\nb\n");
    }

    #[test]
    fn inserts_into_an_anchored_array_and_before_a_compact_first_item() {
        let got = inserted("yaml", "a: &q [1]\nb: *q\n", "/a/0", Value::Int(9)).unwrap();
        assert_eq!(got, "a: &q [9, 1]\nb: *q\n");
        let got = inserted("yaml", "- - 1\n  - 2\n", "/0/0", Value::Int(9)).unwrap();
        assert_eq!(got, "- - 9\n  - 1\n  - 2\n");
    }

    #[test]
    fn refuses_under_an_alias_naming_it() {
        let err = inserted("yaml", "a: &q [1]\nb: *q\n", "/b/0", Value::Int(9)).unwrap_err();
        assert!(
            matches!(&err, Error::EditRejected { reason, .. } if reason.contains("alias on line 2")),
            "{err:?}"
        );
    }

    #[test]
    fn refuses_past_the_end_and_under_no_array() {
        let err = inserted("yaml", "- 1\n", "/2", Value::Int(9)).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Path {
                    kind: PathErrorKind::OutOfBounds,
                    ..
                }
            ),
            "{err:?}"
        );
        let err = inserted("yaml", "a: 1\n", "/a/0", Value::Int(9)).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Path {
                    kind: PathErrorKind::TypeMismatch { .. },
                    ..
                }
            ),
            "{err:?}"
        );
    }
}
