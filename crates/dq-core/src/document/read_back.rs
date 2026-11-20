//! What a tree written whole through a format's writer reads back as, when
//! the writer loses nothing: the read-back check of a rewrite edit
//! (`Document::write_pending`) compares the tree written with the tree read
//! back, both in this form, and refuses the edit where they differ.

use super::{FormatTag, Layout, Value};

/// `value` as the file `format`'s writer writes it (with `layout`) reads
/// back where nothing is lost — the same form for a tree the reader built:
///
/// - INI, dotenv, CSV and TSV hold strings: a number, a boolean or a big
///   number reads back as its text, a null as the empty string. A CSV row
///   lacking a column of another, or of the header the file was read with,
///   reads back with it empty; an INI section of no name and no keys reads
///   back as no section.
/// - XML: see [`crate::parsers::xml::read_back_form`].
/// - HCL: a key read as blocks reads back as their bodies, a list of one as
///   the body alone, none as no key
///   ([`crate::parsers::hcl::read_back_form`]).
/// - Any other format: `value` itself (JSON Lines reads back its records
///   exactly).
pub(super) fn read_back_form(format: FormatTag, layout: &Layout, value: &Value) -> Value {
    match (format, value) {
        (FormatTag::Ini, Value::Map(sections)) => Value::Map(
            sections
                .iter()
                .filter(|(name, section)| {
                    !(name.is_empty() && matches!(section, Value::Map(keys) if keys.is_empty()))
                })
                .map(|(name, section)| (name.clone(), texts(section)))
                .collect(),
        ),
        (FormatTag::DotEnv, Value::Map(_)) => texts(value),
        (FormatTag::Csv | FormatTag::Tsv, Value::Array(rows)) => csv_rows(layout, rows),
        (FormatTag::Xml, _) => crate::parsers::xml::read_back_form(value),
        (FormatTag::Hcl, Value::Map(body)) => match layout {
            Layout::Hcl(layout) => crate::parsers::hcl::read_back_form(body, layout),
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

/// The text the INI, dotenv and CSV writers write a scalar as: a string as
/// it is, a number or a boolean as it prints (a float in its
/// [shortest spelling](super::shortest_float)), a big number as its literal,
/// a null as nothing; `None` for a container.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) | Value::BigInt(s) | Value::BigFloat(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Int(n) => Some(n.to_string()),
        Value::Float(f) => Some(super::shortest_float(*f)),
        Value::Null => Some(String::new()),
        Value::Array(_) | Value::Map(_) => None,
    }
}

/// `map` with every scalar member as its [`text`].
fn texts(map: &Value) -> Value {
    match map {
        Value::Map(members) => Value::Map(
            members
                .iter()
                .map(|(key, member)| {
                    let member = text(member).map_or_else(|| member.clone(), Value::String);
                    (key.clone(), member)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// CSV rows as they read back: every row with every column the writer
/// writes ([`crate::parsers::csv::columns`]), a missing one empty, each
/// cell its [`text`].
fn csv_rows(layout: &Layout, rows: &[Value]) -> Value {
    let columns = crate::parsers::csv::columns(layout, rows);
    Value::Array(
        rows.iter()
            .map(|row| match row {
                Value::Map(cells) => Value::Map(
                    columns
                        .iter()
                        .map(|column| {
                            let cell = cells.get(*column).map_or_else(
                                || Value::String(String::new()),
                                |cell| text(cell).map_or_else(|| cell.clone(), Value::String),
                            );
                            ((*column).to_owned(), cell)
                        })
                        .collect(),
                ),
                other => other.clone(),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(json: serde_json::Value) -> Value {
        Value::from_serde_json(&json)
    }

    #[test]
    fn string_formats_read_every_scalar_back_as_its_text() {
        let ini = tree(serde_json::json!({"": {}, "s": {"n": 1.5, "b": true, "z": null}}));
        assert_eq!(
            read_back_form(FormatTag::Ini, &Layout::None, &ini),
            tree(serde_json::json!({"s": {"n": "1.5", "b": "true", "z": ""}}))
        );
        let csv = tree(serde_json::json!([{"a": 1}, {"b": "x"}]));
        assert_eq!(
            read_back_form(FormatTag::Csv, &Layout::None, &csv),
            tree(serde_json::json!([{"a": "1", "b": ""}, {"a": "", "b": "x"}]))
        );
        let jsonl = tree(serde_json::json!([{"a": 1}]));
        assert_eq!(
            read_back_form(FormatTag::Jsonl, &Layout::None, &jsonl),
            jsonl
        );
    }

    #[test]
    fn xml_and_hcl_read_back_in_their_writers_shapes() {
        let xml = tree(serde_json::json!({
            "#xml": {"standalone": true, "bom": "UTF-8"},
            "#comments": "top",
            "r": [{"@attrs": {"k": 1, "z": null}, "#text": "", "#cdata": ["a]]>b"],
                   "a": ["x", null, {"#text": 2}], "b": []}]
        }));
        assert_eq!(
            read_back_form(FormatTag::Xml, &Layout::None, &xml),
            tree(serde_json::json!({
                "#xml": {"version": "1.0", "standalone": "yes"},
                "#comments": ["top"],
                "r": [{"@attrs": {"k": "1", "z": ""}, "#cdata": ["a]]", ">b"],
                       "a": [{"#text": "x"}, {}, {"#text": "2"}]}]
            }))
        );
        let hcl = crate::format::Format::parse(
            &crate::parsers::Hcl,
            b"b {\n  p = 1\n}\nb {\n  p = 2\n}\nl \"x\" {}\n",
        )
        .unwrap();
        let Value::Map(mut edited) = hcl.value().clone() else {
            panic!("a map")
        };
        edited.insert("b".to_owned(), tree(serde_json::json!([{"p": 1}])));
        edited.insert("l".to_owned(), tree(serde_json::json!({"x": []})));
        assert_eq!(
            read_back_form(FormatTag::Hcl, hcl.layout(), &Value::Map(edited)),
            tree(serde_json::json!({"b": {"p": 1}}))
        );
    }
}
