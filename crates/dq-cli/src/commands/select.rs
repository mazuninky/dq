//! `dq select FILE EXPR` — JSONPath query (RFC 9535 subset) over the document.
//!
//! Reaches for `jsonpath_rust = "1.0"` (RFC 9535 grammar) because that
//! crate's API is what the workspace pins. Notes on the API:
//! - `JsonPath::query(&value, &str)` parses and evaluates in one call and
//!   returns `Vec<&Value>`; a parse failure is the `Err`. Descendant
//!   segments (`$..*`) list a node's children before descending into them.
//! - Blanks are allowed between segments and inside a member-name shorthand,
//!   where they stay part of the name: `$.a b` looks up the key `"a b"`
//!   instead of failing to parse.
//! - An empty `Vec` means "no matches" — there is no sentinel value.
//!
//! Per spec, an empty match list is NOT an error: stdout becomes `[]` and the
//! exit code is 0.
//!
//! The query runs over the document's own values ([`PathValue`], a
//! `jsonpath_rust::Queryable` tree), so a filter sees a NaN or an infinity
//! as a number, not as the string `".nan"`. The crate does a filter's
//! arithmetic itself, in `f64`: `<` / `>` order an infinity past every
//! finite number and a NaN below and above nothing; `==` is a difference
//! under `f64::EPSILON`, so a NaN equals nothing (and `!=` holds), two
//! infinities of one sign are not equal, and a number past an `f64`'s
//! precision (`18446744073709551617`) compares as the `f64` nearest it. The
//! matched nodes are printed as the document's own values, so every output
//! format writes a NaN, an infinity or a big number as `get` does.

use std::io::Write;

use dq_core::Value;
use indexmap::IndexMap;
use jsonpath_rust::query::queryable::Queryable;

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, SelectArgs};
use crate::output::Reporter;

/// Run the `select` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `select` is a read subcommand.
/// - `dq_core::Error::Io` / `Parse` / `UnsupportedFormat` for the usual
///   I/O and parse failures.
/// - `anyhow::Error` (exit 1) when the JSONPath expression itself is
///   malformed.
pub fn run(
    cli: &Cli,
    args: &SelectArgs,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;
    let (_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    // Same view as `get`: document 0 of a stream unless `--doc` says
    // otherwise; `--doc all` queries the array of every document.
    let view = select_document(&doc, cli.doc, &args.file)?;
    let tree = PathValue::from(view.as_ref());

    let matched = jsonpath_rust::query::js_path_vals(args.jsonpath.as_str(), &tree)
        .map_err(|e| anyhow::anyhow!("invalid JSONPath expression: {e}"))?;
    let items = matched.into_iter().map(PathValue::to_value).collect();

    reporter.report_value(&Value::Array(items), out)?;
    Ok(())
}

/// A document value as `jsonpath_rust` queries it: the crate hands out
/// `&Vec<Self>` for an array, so the tree is its own, built from the
/// [`Value`] and turned back into one per match. A number stays the
/// document's [`Value`], exact.
#[derive(Debug, Clone, Default)]
pub(crate) enum PathValue {
    #[default]
    Null,
    Bool(bool),
    /// An int, a float (a NaN or an infinity included) or a big number.
    Number(Value),
    String(String),
    Array(Vec<PathValue>),
    Object(IndexMap<String, PathValue>),
}

impl PathValue {
    /// The document value this is.
    fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(*b),
            Self::Number(number) => number.clone(),
            Self::String(s) => Value::String(s.clone()),
            Self::Array(items) => Value::Array(items.iter().map(Self::to_value).collect()),
            Self::Object(map) => Value::Map(
                map.iter()
                    .map(|(key, member)| (key.clone(), member.to_value()))
                    .collect(),
            ),
        }
    }
}

impl From<&Value> for PathValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::BigFloat(_) => {
                Self::Number(value.clone())
            }
            Value::String(s) => Self::String(s.clone()),
            Value::Array(items) => Self::Array(items.iter().map(Self::from).collect()),
            Value::Map(map) => Self::Object(
                map.iter()
                    .map(|(key, member)| (key.clone(), Self::from(member)))
                    .collect(),
            ),
        }
    }
}

impl From<&str> for PathValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

impl From<String> for PathValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<bool> for PathValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i64> for PathValue {
    fn from(n: i64) -> Self {
        Self::Number(Value::Int(n))
    }
}

impl From<f64> for PathValue {
    fn from(f: f64) -> Self {
        Self::Number(Value::Float(f))
    }
}

impl From<Vec<PathValue>> for PathValue {
    fn from(items: Vec<PathValue>) -> Self {
        Self::Array(items)
    }
}

/// The crate compares two numbers through [`Queryable::as_f64`]; this is
/// what it falls back on for everything else — two arrays, two objects, a
/// string and a number — and there a number is equal only to the same
/// number ([`Value::same_number`]: never a NaN).
impl PartialEq for PathValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a.same_number(b),
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Object(a), Self::Object(b)) => {
                a.len() == b.len() && a.iter().all(|(key, x)| b.get(key) == Some(x))
            }
            _ => false,
        }
    }
}

impl Queryable for PathValue {
    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(map) => map.get(key),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&Vec<Self>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    fn as_object(&self) -> Option<Vec<(&str, &Self)>> {
        match self {
            Self::Object(map) => Some(
                map.iter()
                    .map(|(key, member)| (key.as_str(), member))
                    .collect(),
            ),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(Value::Int(n)) => Some(*n),
            _ => None,
        }
    }

    /// Every number but an `i64` ([`Self::as_i64`]): a NaN and an
    /// infinity as themselves, a big number as the `f64` nearest it (an
    /// infinity past the `f64` range).
    fn as_f64(&self) -> Option<f64> {
        let Self::Number(number) = self else {
            return None;
        };
        if let Some(f) = number.non_finite_float() {
            return Some(f);
        }
        match number {
            Value::Float(f) => Some(*f),
            Value::BigInt(s) | Value::BigFloat(s) => s.parse().ok(),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn null() -> Self {
        Self::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
    use crate::output::JsonReporter;
    use clap::Parser;
    use tempfile::NamedTempFile;

    // Returns a `TempPath` (not a `NamedTempFile`) so the underlying `File`
    // handle is released after writing. Required for Windows: production
    // atomic-write uses `MoveFileEx` which fails with `Access is denied` if
    // the target is still held open elsewhere in the same process.
    fn write_yaml(content: &str) -> tempfile::TempPath {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.into_temp_path()
    }

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "select", file, "$"]).expect("clap parse")
    }

    #[test]
    fn select_returns_array_of_one_for_single_match() {
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.spec.replicas".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!([3]));
    }

    #[test]
    fn select_returns_empty_array_for_no_match() {
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.does.not.exist".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        // Per spec: empty match list is NOT an error.
        run(&cli, &args, None, &reporter, &mut out).expect("empty match should not error");
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!([]));
    }

    #[test]
    fn select_returns_multi_match_in_order() {
        let tmp =
            write_yaml("spec:\n  containers:\n    - image: a\n    - image: b\n    - image: c\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.spec.containers[*].image".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!(["a", "b", "c"]));
    }

    #[test]
    fn select_rejects_malformed_expression() {
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "this is not jsonpath".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out).unwrap_err();
        assert!(err.to_string().contains("invalid JSONPath"));
    }

    #[test]
    fn select_evaluates_rfc_9535_filters_and_negative_indices() {
        let tmp = write_yaml("items:\n  - {name: x, n: 1}\n  - {name: y, n: 2}\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        for expr in ["$.items[?@.n > 1].name", "$.items[-1].name"] {
            let args = SelectArgs {
                file: path.clone(),
                jsonpath: expr.to_owned(),
            };
            let mut out: Vec<u8> = Vec::new();
            run(&cli, &args, None, &JsonReporter, &mut out).unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(parsed, serde_json::json!(["y"]), "{expr}");
        }
    }

    #[test]
    fn select_filters_compare_non_finite_and_big_numbers_as_numbers() {
        let tmp = write_yaml(
            "items:\n  - {name: nan, n: .nan}\n  - {name: inf, n: .inf}\n  \
             - {name: ninf, n: -.inf}\n  - {name: one, n: 1}\n  \
             - {name: big, n: 18446744073709551616}\n  - {name: str, n: '.nan'}\n",
        );
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let cases = [
            ("$.items[?@.n > 1].name", serde_json::json!(["inf", "big"])),
            ("$.items[?@.n < 1].name", serde_json::json!(["ninf"])),
            ("$.items[?@.n == 1].name", serde_json::json!(["one"])),
            ("$.items[?@.n == '.nan'].name", serde_json::json!(["str"])),
            (
                "$.items[?@.n != 1].name",
                serde_json::json!(["nan", "inf", "ninf", "big", "str"]),
            ),
        ];
        for (expr, expected) in cases {
            let args = SelectArgs {
                file: path.clone(),
                jsonpath: expr.to_owned(),
            };
            let mut out: Vec<u8> = Vec::new();
            run(&cli, &args, None, &JsonReporter, &mut out).unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(parsed, expected, "{expr}");
        }
    }

    #[test]
    fn path_value_round_trips_the_document_value() {
        let value = Value::Map(
            [
                (
                    "big".to_owned(),
                    Value::BigInt("18446744073709551617".into()),
                ),
                ("nan".to_owned(), Value::Float(f64::NAN)),
                (
                    "list".to_owned(),
                    Value::Array(vec![Value::Null, Value::Bool(true)]),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let tree = PathValue::from(&value);
        assert!(matches!(tree.to_value(), Value::Map(ref m) if m.len() == 3));
        assert_eq!(
            PathValue::from(&value).get("big").map(PathValue::to_value),
            Some(Value::BigInt("18446744073709551617".into()))
        );
        assert_eq!(
            tree.get("nan").and_then(Queryable::as_f64).map(f64::is_nan),
            Some(true)
        );
        assert_ne!(tree.get("nan"), tree.get("nan"));
    }

    #[test]
    fn select_rejects_non_rfc_9535_length_method() {
        let tmp = write_yaml("items: [1, 2]\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.items.length()".to_owned(),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &JsonReporter, &mut out).unwrap_err();
        assert!(err.to_string().contains("invalid JSONPath"), "{err}");
    }

    #[test]
    fn select_rejects_in_place_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "-i", "select", "/nope.yaml", "$"]).unwrap();
        let args = SelectArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            jsonpath: "$".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out).unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
