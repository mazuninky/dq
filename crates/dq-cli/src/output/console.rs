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

/// Escape every control byte (`< 0x20`, `0x7f`, including `\n`/`\t`/`\r`/`\x1b`)
/// in `s` using a Rust-style `\xNN` form.
///
/// Returns the borrowed input when no escaping is needed (no control bytes).
/// Otherwise allocates a new `String`. Non-ASCII bytes are passed through
/// unchanged — they participate in valid UTF-8 multi-byte sequences and the
/// shell renders them safely.
fn escape_control(s: &str) -> Cow<'_, str> {
    // Escape every C0 control byte (`< 0x20`, includes `\x1b` ESC, `\n`, `\t`,
    // `\r`) plus DEL (`0x7f`). Non-ASCII bytes (`>= 0x80`) pass through —
    // they participate in valid UTF-8 multi-byte sequences.
    let needs_escape = s.bytes().any(|b| b < 0x20 || b == 0x7f);
    if !needs_escape {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b < 0x20 || b == 0x7f {
            out.push_str(&format!("\\x{b:02x}"));
        } else {
            out.push(b as char);
        }
    }
    Cow::Owned(out)
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
        serde_json::Value::Number(n) => write!(w, "{n}"),
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
