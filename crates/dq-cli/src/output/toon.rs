//! TOON reporter — emits Token-Oriented Object Notation via `toon-format`.

use std::io::Write;

use super::Reporter;

/// TOON reporter.
///
/// Delegates to [`toon_format::encode_default`] which accepts any
/// `serde::Serialize` value and returns the TOON string. We then write the
/// string and append a trailing newline so the output is friendly to shell
/// piping.
///
/// `toon_format` reads every number as an `i64`, a `u64` or an `f64`, so a
/// number no one of them holds — a 40-digit integer, a float literal with
/// more digits than an `f64`, `1e400` — would come out rounded or as
/// `null`. Such a value is refused instead ([`ensure_toon_numbers`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct ToonReporter;

impl Reporter for ToonReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        ensure_toon_numbers(value)?;
        let mut respelled = Vec::new();
        let marks = FloatMarks::for_document(value);
        let marked = marks.mark_tie_floats(value, &mut respelled);
        let mut encoded = toon_format::encode_default(marked.as_ref())
            .map_err(|e| anyhow::anyhow!("toon encode failed: {e}"))?;
        if !respelled.is_empty() {
            encoded = marks.unmark(&encoded, &respelled);
        }
        w.write_all(encoded.as_bytes())?;
        if !encoded.ends_with('\n') {
            w.write_all(b"\n")?;
        }
        Ok(())
    }

    /// As [`ToonReporter::report`], refusing a number no `i64`, `u64` or
    /// `f64` holds by the literal dq holds — the one the TOML and HCL
    /// writers name too.
    fn report_value(&self, value: &dq_core::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        ensure_toon_value(value)?;
        self.report(&value.to_serde_json(), w)
    }
}

/// Opens a float marker: U+E000, a private-use character TOON writes bare.
const MARK_OPEN: char = '\u{E000}';
/// Closes a float marker: U+E001.
const MARK_CLOSE: char = '\u{E001}';

/// The placeholders that stand for floats in the text `toon_format`
/// writes, until [`FloatMarks::unmark`] puts each float's spelling there.
///
/// A marker is a run of [`MARK_OPEN`] one longer than the longest run in
/// any string or key of the document, the float's index, and
/// [`MARK_CLOSE`]. No string or key holds such a run, and none of the
/// marker's characters is one TOON writes between values (`,`, `:`, a
/// space, a quote, a line break), so the run is found in the written text
/// only where a marker stands — whatever the strings hold.
struct FloatMarks {
    run: String,
}

impl FloatMarks {
    fn for_document(value: &serde_json::Value) -> Self {
        fn longest_run(s: &str) -> usize {
            s.split(|c| c != MARK_OPEN)
                .map(|run| run.chars().count())
                .max()
                .unwrap_or(0)
        }
        fn longest(value: &serde_json::Value) -> usize {
            match value {
                serde_json::Value::String(s) => longest_run(s),
                serde_json::Value::Array(items) => items.iter().map(longest).max().unwrap_or(0),
                serde_json::Value::Object(map) => map
                    .iter()
                    .map(|(k, v)| longest_run(k).max(longest(v)))
                    .max()
                    .unwrap_or(0),
                _ => 0,
            }
        }
        Self {
            run: MARK_OPEN.to_string().repeat(longest(value) + 1),
        }
    }

    /// The marker for the `at`-th float respelled.
    fn marker(&self, at: usize) -> String {
        format!("{}{at}{MARK_CLOSE}", self.run)
    }

    /// `value` with each float that `toon_format` would spell otherwise
    /// than [`dq_core::plain_float`] replaced by a marker string, its
    /// spelling pushed to `respelled`. `toon_format` writes a non-whole
    /// float with Rust's `Display`, which breaks a tie in the 17th digit to
    /// the odd digit (`1658206780088562.3`), while every other dq writer
    /// picks the even one (`…562.2`). Borrowed when there is none.
    fn mark_tie_floats<'v>(
        &self,
        value: &'v serde_json::Value,
        respelled: &mut Vec<String>,
    ) -> std::borrow::Cow<'v, serde_json::Value> {
        fn respelling(n: &serde_json::Number) -> Option<String> {
            if n.as_i64().is_some() || n.as_u64().is_some() {
                return None;
            }
            let f = n.as_f64().filter(|f| f.is_finite())?;
            // `toon_format` writes a whole float in `i64` range, [-2^63, 2^63),
            // as that integer.
            const TWO_TO_63: f64 = 9_223_372_036_854_775_808.0;
            let in_i64 = f.fract() == 0.0 && (-TWO_TO_63..TWO_TO_63).contains(&f);
            let plain = dq_core::plain_float(f);
            (!in_i64 && plain != f.to_string()).then_some(plain)
        }
        fn has_tie(value: &serde_json::Value) -> bool {
            match value {
                serde_json::Value::Number(n) => respelling(n).is_some(),
                serde_json::Value::Array(items) => items.iter().any(has_tie),
                serde_json::Value::Object(map) => map.values().any(has_tie),
                _ => false,
            }
        }
        fn mark(marks: &FloatMarks, value: &mut serde_json::Value, respelled: &mut Vec<String>) {
            match value {
                serde_json::Value::Number(n) => {
                    if let Some(plain) = respelling(n) {
                        *value = serde_json::Value::String(marks.marker(respelled.len()));
                        respelled.push(plain);
                    }
                }
                serde_json::Value::Array(items) => {
                    items.iter_mut().for_each(|v| mark(marks, v, respelled));
                }
                serde_json::Value::Object(map) => {
                    map.values_mut().for_each(|v| mark(marks, v, respelled));
                }
                _ => {}
            }
        }
        if !has_tie(value) {
            return std::borrow::Cow::Borrowed(value);
        }
        let mut owned = value.clone();
        mark(self, &mut owned, respelled);
        std::borrow::Cow::Owned(owned)
    }

    /// `encoded` with every marker replaced by the spelling `respelled`
    /// holds at its index, in one pass.
    fn unmark(&self, encoded: &str, respelled: &[String]) -> String {
        let mut out = String::with_capacity(encoded.len());
        let mut rest = encoded;
        while let Some(start) = rest.find(&self.run) {
            out.push_str(&rest[..start]);
            let after = &rest[start + self.run.len()..];
            let spelled = after.split_once(MARK_CLOSE).and_then(|(at, tail)| {
                let spelling = respelled.get(at.parse::<usize>().ok()?)?;
                Some((spelling, tail))
            });
            match spelled {
                Some((spelling, tail)) => {
                    out.push_str(spelling);
                    rest = tail;
                }
                // Not a marker after all: no string holds the run, so this
                // does not happen; keep the text as it is.
                None => {
                    out.push_str(&self.run);
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// Fail on the first number of `value` that `toon_format` would not write
/// as the number it is, naming it as `value` spells it.
///
/// # Errors
///
/// [`dq_core::Error::Format`] naming the number.
fn ensure_toon_value(value: &dq_core::Value) -> Result<(), dq_core::Error> {
    use dq_core::Value;
    match value {
        Value::BigInt(s) if s.parse::<u64>().is_ok() => Ok(()),
        Value::BigFloat(_) if value.non_finite_float().is_some() => Ok(()),
        Value::BigInt(s) | Value::BigFloat(s) => Err(toon_refuses(s)),
        Value::Array(items) => items.iter().try_for_each(ensure_toon_value),
        Value::Map(map) => map.values().try_for_each(ensure_toon_value),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::String(_) => Ok(()),
    }
}

fn toon_refuses(literal: &str) -> dq_core::Error {
    dq_core::Error::Format {
        format: "toon",
        message: format!("TOON cannot represent arbitrary-precision number {literal}"),
    }
}

/// Fail on the first number of `value` that `toon_format` would not write
/// as the number it is.
///
/// # Errors
///
/// [`dq_core::Error::Format`] naming the number.
fn ensure_toon_numbers(value: &serde_json::Value) -> Result<(), dq_core::Error> {
    match value {
        serde_json::Value::Number(n) => {
            let literal = n.to_string();
            let exact = n.as_u64().is_some()
                || matches!(
                    dq_core::Value::from_number_literal(&literal),
                    Some(dq_core::Value::Int(_) | dq_core::Value::Float(_))
                );
            if exact {
                Ok(())
            } else {
                Err(toon_refuses(&literal))
            }
        }
        serde_json::Value::Array(items) => items.iter().try_for_each(ensure_toon_numbers),
        serde_json::Value::Object(map) => map.values().try_for_each(ensure_toon_numbers),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_some_toon_for_an_object() {
        let mut buf: Vec<u8> = Vec::new();
        ToonReporter
            .report(&serde_json::json!({"name": "alice"}), &mut buf)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("name"), "expected key in toon output: {out:?}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn refuses_a_number_toon_would_round() {
        for literal in [
            "1e400",
            "0.1000000000000000000000000001",
            "-18446744073709551617",
        ] {
            let value: serde_json::Value =
                serde_json::from_str(&format!("{{\"a\": [{literal}]}}")).unwrap();
            let err = ToonReporter
                .report(&value, &mut Vec::new())
                .expect_err(literal);
            // serde_json spells `1e400` as `1e+400`.
            let message = err.to_string();
            assert!(
                message.starts_with(
                    "toon format error: TOON cannot represent arbitrary-precision number "
                ),
                "{message}"
            );
        }
        let exact = serde_json::json!({"u": 18446744073709551615_u64, "f": 1.5, "i": -3});
        assert!(ToonReporter.report(&exact, &mut Vec::new()).is_ok());
    }

    #[test]
    fn report_value_names_the_number_as_dq_holds_it() {
        for literal in ["1e400", "1E+400", "0.1000000000000000000000000001"] {
            let value = dq_core::Value::Array(vec![dq_core::Value::BigFloat(literal.into())]);
            let err = ToonReporter
                .report_value(&value, &mut Vec::new())
                .expect_err(literal);
            assert_eq!(
                err.to_string(),
                format!(
                    "toon format error: TOON cannot represent arbitrary-precision number {literal}"
                )
            );
        }
        let exact = dq_core::Value::BigInt("18446744073709551615".into());
        assert!(ToonReporter.report_value(&exact, &mut Vec::new()).is_ok());
    }

    fn toon(value: &serde_json::Value) -> String {
        let mut buf: Vec<u8> = Vec::new();
        ToonReporter.report(value, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    /// A string or a key holding the private-use characters the tie
    /// respelling marks floats with — one of them, a whole marker, or a
    /// run of them — is written as it is, and the float on a 17th-digit tie
    /// next to it is still spelled as every dq writer spells it, `…562.2`,
    /// not `toon_format`'s `…562.3`. Before, any such character turned the
    /// respelling off for the whole document.
    #[test]
    fn a_tie_float_is_spelled_the_same_whatever_the_strings_hold() {
        let tie = 1_658_206_780_088_562.2_f64;
        let marker = "\u{E000}0\u{E001}";
        let run = "\u{E000}\u{E000}\u{E000}1\u{E001}";
        assert_eq!(
            toon(&serde_json::json!({"s": "a\u{E000}b\u{E001}c", "t": [1.5, marker]})),
            format!("s: a\u{E000}b\u{E001}c\nt[2]: 1.5,{marker}\n")
        );
        for (value, first) in [
            (
                serde_json::json!({"s": "\u{E000}", "v": tie}),
                "s: \u{E000}".to_owned(),
            ),
            (
                serde_json::json!({"s": marker, "v": tie}),
                format!("s: {marker}"),
            ),
            (serde_json::json!({"s": run, "v": tie}), format!("s: {run}")),
            (
                serde_json::json!({marker: "k", "v": tie}),
                format!("\"{marker}\": k"),
            ),
        ] {
            assert_eq!(
                toon(&value),
                format!("{first}\nv: 1658206780088562.2\n"),
                "{value}"
            );
        }
        // Several ties among marker-shaped strings, each in its place.
        assert_eq!(
            toon(&serde_json::json!({"a": [tie, run, -tie, marker, tie]})),
            format!(
                "a[5]: 1658206780088562.2,{run},-1658206780088562.2,{marker},1658206780088562.2\n"
            )
        );
    }

    #[test]
    fn a_marker_run_is_one_longer_than_any_in_the_document() {
        let marks =
            FloatMarks::for_document(&serde_json::json!({"k\u{E000}\u{E000}": ["\u{E000}"]}));
        assert_eq!(marks.marker(7), "\u{E000}\u{E000}\u{E000}7\u{E001}");
        let marks = FloatMarks::for_document(&serde_json::json!([1, true]));
        assert_eq!(marks.marker(0), "\u{E000}0\u{E001}");
        // A run that is no marker of this document is kept.
        assert_eq!(
            marks.unmark("x\u{E000}9\u{E001} \u{E000}0\u{E001}", &["2.5".to_owned()]),
            "x\u{E000}9\u{E001} 2.5"
        );
    }

    #[test]
    fn encodes_toon_spec_v4_empty_arrays_and_quoting() {
        let mut buf: Vec<u8> = Vec::new();
        ToonReporter
            .report(&serde_json::json!({"roles": [], "tag": "#bob"}), &mut buf)
            .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "roles: []\ntag: \"#bob\"\n"
        );
    }
}
