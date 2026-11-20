//! Human-readable console reporter.
//!
//! Scalars are written as their textual form. Arrays render one element per
//! line. Objects render `key: value` per line. When `use_color` is `true`,
//! object keys are coloured cyan and the type tag (`null`, `bool`, ...) green;
//! when `use_color` is `false`, no ANSI escapes are emitted under any
//! conditions, regardless of the surrounding TTY state.

use std::borrow::Cow;
use std::io::Write;

use super::Reporter;

/// Console reporter.
///
/// The colour decision is captured at construction (see
/// [`crate::output::resolve_color`]). Use the `with_color` constructor when
/// you want a coloured reporter even in tests; the default constructor is the
/// uncoloured form, useful as a snapshot baseline.
#[derive(Debug, Clone, Copy)]
pub struct ConsoleReporter {
    use_color: bool,
}

impl ConsoleReporter {
    /// Build a reporter, choosing whether it emits ANSI escapes.
    #[must_use]
    pub fn new(use_color: bool) -> Self {
        Self { use_color }
    }
}

impl Reporter for ConsoleReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        write_top(value, w, self.use_color)?;
        Ok(())
    }
}

/// Escape every control character in `s` — C0 (`< 0x20`, including
/// `\n`/`\t`/`\r`/`\x1b`), DEL (`0x7f`) and C1 (U+0080–U+009F, including
/// U+009B CSI and U+0085 NEL) — as `\xNN`.
///
/// `NN` is the character's code point in two lowercase hex digits, the way
/// YAML and Python read `\xNN` and the way dq's YAML writer spells these
/// characters: U+009B prints as `\x9b`, although its UTF-8 bytes are
/// `c2 9b`. A C1 control is escaped for the same reason as ESC: U+009B
/// starts a terminal control sequence on its own.
///
/// The invisible direction controls (U+200E LRM, U+200F RLM, U+202A–U+202E
/// embeddings and overrides, U+2066–U+2069 isolates — the characters of
/// Trojan Source) and the separators U+2028 / U+2029 print as `\uNNNN`, four
/// lowercase hex digits, as a YAML double-quoted string reads them
/// (`\u202e`): they reorder or break the line a terminal shows while being
/// invisible themselves.
///
/// Returns the borrowed input when no escaping is needed. Otherwise
/// allocates a new `String`. Every other character, non-ASCII included
/// (é, NBSP, emoji), is copied whole: the walk is over chars, not bytes,
/// so a multi-byte character next to an escaped control stays intact.
pub(crate) fn escape_control(s: &str) -> Cow<'_, str> {
    if !s
        .chars()
        .any(|c| is_escaped_control(c) || is_invisible_layout(c))
    {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_escaped_control(c) {
            out.push_str(&format!("\\x{:02x}", u32::from(c)));
        } else if is_invisible_layout(c) {
            out.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// A C0 control, DEL or a C1 control: Unicode's `Cc` category, the set
/// `char::is_control` tests, spelled out so the ranges are visible.
fn is_escaped_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}

/// A bidi control that changes the order a terminal shows text in (LRM,
/// RLM, the embeddings, overrides and isolates) or a line / paragraph
/// separator.
fn is_invisible_layout(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}'
    )
}

/// A number as the console prints it: an integer as its literal, a float
/// as dq's YAML writer spells it (`3.0`, `1.0e+300`, `1.0e+400`), so `get`
/// and `-F yaml` agree, as they do on `.nan` — not serde_json's `1e+300`.
fn number_text(n: &serde_json::Number) -> String {
    let literal = n.to_string();
    dq_core::Value::from_number_literal(&literal)
        .as_ref()
        .and_then(dq_core::parsers::yaml_float_text)
        .unwrap_or(literal)
}

fn write_top(value: &serde_json::Value, w: &mut dyn Write, use_color: bool) -> std::io::Result<()> {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                write_inline(item, w, use_color)?;
                w.write_all(b"\n")?;
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let safe_key = escape_control(k);
                if use_color {
                    write!(w, "\x1b[36m{safe_key}\x1b[0m: ")?;
                } else {
                    write!(w, "{safe_key}: ")?;
                }
                write_inline(v, w, use_color)?;
                w.write_all(b"\n")?;
            }
        }
        other => {
            write_inline(other, w, use_color)?;
            w.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn write_inline(
    value: &serde_json::Value,
    w: &mut dyn Write,
    use_color: bool,
) -> std::io::Result<()> {
    match value {
        serde_json::Value::Null => {
            if use_color {
                w.write_all(b"\x1b[32mnull\x1b[0m")
            } else {
                w.write_all(b"null")
            }
        }
        serde_json::Value::Bool(b) => write!(w, "{b}"),
        serde_json::Value::Number(n) => w.write_all(number_text(n).as_bytes()),
        serde_json::Value::String(s) => {
            let safe = escape_control(s);
            w.write_all(safe.as_bytes())
        }
        serde_json::Value::Array(items) => {
            // Inline arrays compactly so a `key: [...]` row stays on one line.
            w.write_all(b"[")?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    w.write_all(b", ")?;
                }
                write_inline(item, w, use_color)?;
            }
            w.write_all(b"]")
        }
        serde_json::Value::Object(map) => {
            w.write_all(b"{")?;
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    w.write_all(b", ")?;
                }
                let safe_key = escape_control(k);
                w.write_all(safe_key.as_bytes())?;
                w.write_all(b": ")?;
                write_inline(v, w, use_color)?;
            }
            w.write_all(b"}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_value_spells_non_finite_floats_the_yaml_way() {
        let mut map = indexmap::IndexMap::new();
        map.insert("t".to_owned(), dq_core::Value::BigFloat("+inf".to_owned()));
        map.insert("y".to_owned(), dq_core::Value::BigFloat(".NaN".to_owned()));
        let value = dq_core::Value::Array(vec![
            dq_core::Value::Float(f64::NAN),
            dq_core::Value::Float(f64::INFINITY),
            dq_core::Value::Array(vec![dq_core::Value::Float(f64::NEG_INFINITY)]),
            dq_core::Value::Map(map),
            dq_core::Value::Float(1.5),
        ]);
        let mut buf: Vec<u8> = Vec::new();
        ConsoleReporter::new(false)
            .report_value(&value, &mut buf)
            .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            ".nan\n.inf\n[-.inf]\n{t: .inf, y: .nan}\n1.5\n"
        );
    }

    #[test]
    fn report_value_spells_floats_as_the_yaml_writer_does() {
        let value = dq_core::Value::Array(vec![
            dq_core::Value::Float(1e300),
            dq_core::Value::Float(3.0),
            dq_core::Value::Float(1e-7),
            dq_core::Value::BigFloat("1e400".to_owned()),
            dq_core::Value::BigInt("18446744073709551617".to_owned()),
            dq_core::Value::Int(7),
        ]);
        let mut buf: Vec<u8> = Vec::new();
        ConsoleReporter::new(false)
            .report_value(&value, &mut buf)
            .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "1.0e+300\n3.0\n1.0e-7\n1.0e+400\n18446744073709551617\n7\n"
        );
    }

    fn render(value: &serde_json::Value, use_color: bool) -> String {
        let reporter = ConsoleReporter::new(use_color);
        let mut buf: Vec<u8> = Vec::new();
        reporter.report(value, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn renders_scalar_without_color() {
        let out = render(&serde_json::json!("hello"), false);
        assert_eq!(out, "hello\n");
        assert!(!out.contains("\x1b["), "no ANSI when use_color = false");
    }

    #[test]
    fn renders_object_one_per_line_no_color() {
        let out = render(&serde_json::json!({"a": 1, "b": "x"}), false);
        // serde_json::Value uses BTreeMap by default unless preserve_order is on;
        // dq-cli enables preserve_order, so "a" comes first.
        assert_eq!(out, "a: 1\nb: x\n");
        assert!(!out.contains("\x1b["));
    }

    #[test]
    fn renders_object_with_ansi_when_color_on() {
        let out = render(&serde_json::json!({"a": 1}), true);
        assert!(out.contains("\x1b[36m"), "expected cyan key, got: {out:?}");
    }

    #[test]
    fn renders_array_one_per_line() {
        let out = render(&serde_json::json!(["a", "b"]), false);
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn escape_control_passes_through_clean_strings() {
        let s = "hello world";
        match escape_control(s) {
            Cow::Borrowed(b) => assert_eq!(b, "hello world"),
            Cow::Owned(_) => panic!("clean string must be borrowed"),
        }
    }

    #[test]
    fn escape_control_escapes_ansi_csi() {
        // ESC ([1B) starts CSI sequences; this would otherwise colour the terminal.
        let s = "\x1b[31mfake error\x1b[0m";
        let escaped = escape_control(s).into_owned();
        assert!(
            !escaped.contains('\x1b'),
            "ESC must be escaped: {escaped:?}"
        );
        assert!(escaped.contains("\\x1b"));
    }

    #[test]
    fn escape_control_escapes_newlines_and_tabs_in_data() {
        let s = "a\nb\tc\rd";
        let escaped = escape_control(s).into_owned();
        assert_eq!(escaped, "a\\x0ab\\x09c\\x0dd");
    }

    #[test]
    fn escape_control_keeps_multi_byte_characters_whole() {
        assert_eq!(
            escape_control("é\u{1}ж\u{7f}🦀").into_owned(),
            "é\\x01ж\\x7f🦀"
        );
    }

    #[test]
    fn escape_control_escapes_c1_controls_by_code_point() {
        assert_eq!(
            escape_control("\u{80}a\u{85}\u{9b}31m\u{9f}").into_owned(),
            "\\x80a\\x85\\x9b31m\\x9f"
        );
    }

    #[test]
    fn escape_control_escapes_direction_controls_as_u_escapes() {
        assert_eq!(
            escape_control("a\u{202e}b\u{2066}c\u{2028}\u{1b}").into_owned(),
            "a\\u202eb\\u2066c\\u2028\\x1b"
        );
        let near = "\u{200d}\u{2065}\u{206a}\u{202f}";
        assert!(matches!(escape_control(near), Cow::Borrowed(b) if b == near));
    }

    #[test]
    fn escape_control_borrows_non_control_non_ascii() {
        // U+00A0 (NBSP) is the first character after the C1 block.
        let s = "é\u{a0}ж🦀";
        assert!(matches!(escape_control(s), Cow::Borrowed(b) if b == s));
    }

    #[test]
    fn is_escaped_control_matches_char_is_control() {
        for c in ('\u{0}'..='\u{2ff}').chain(['\u{feff}', '\u{2028}']) {
            assert_eq!(is_escaped_control(c), c.is_control(), "{c:?}");
        }
    }

    #[test]
    fn renders_value_with_ansi_escape_sanitizes_to_terminal() {
        let out = render(&serde_json::json!("\x1b[31mfake\x1b[0m"), false);
        // Output must NOT contain the raw ESC byte.
        assert!(!out.contains('\x1b'), "raw ESC leaked into output: {out:?}");
        assert!(out.contains("\\x1b"));
    }

    #[test]
    fn renders_object_key_with_control_byte_is_sanitized() {
        let mut obj = serde_json::Map::new();
        obj.insert("ev\x1bil".to_owned(), serde_json::json!("ok"));
        let out = render(&serde_json::Value::Object(obj), false);
        assert!(!out.contains('\x1b'), "raw ESC leaked into key: {out:?}");
        assert!(out.contains("ev\\x1bil"));
    }
}
