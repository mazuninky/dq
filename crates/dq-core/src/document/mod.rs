//! Document model: the in-memory shape parsers produce and writers consume.
//!
//! In M1 the model carried only a `Value` tree (key order via `IndexMap`,
//! big-int / big-float as textual literals). M2 extends it with the metadata
//! that the textual-edit write path needs:
//!
//! - `original_bytes` — the unchanged source bytes parsers receive. Write
//!   operations splice into this buffer; read operations never touch it.
//! - `spans` — a [`SpanMap`] mapping canonical RFC 6901 pointers to the
//!   byte ranges and surrounding context required to render a replacement.
//! - `format` — a [`FormatTag`] so the renderer factory can pick the right
//!   per-format `ScalarRenderer` / `InsertionRenderer` without holding a
//!   `&dyn Format`.
//!
//! Read-path callers continue to work through [`Document::value`] /
//! [`Document::values`]; the metadata is transparent to them. Write-path
//! callers use [`Document::set_at`] / [`Document::del_at`].
//!
//! The `Document::Single` / `Document::Multi` enum from M1 is preserved as
//! a tagged-union behaviour via the public constructors [`Document::single`]
//! and [`Document::multi`], and the read-side accessors [`Document::value`]
//! (single only) and [`Document::values`] (multi only).

mod insert;
mod number;
pub mod spans;
mod stream;

use std::fmt;
use std::str::FromStr;

use indexmap::IndexMap;
use serde::ser::{SerializeMap, SerializeSeq, Serializer};

use crate::Result;
use crate::error::{Error, PathErrorKind};
use crate::ir::{Ir, Provenance, ProvenanceMap};
use crate::pointer::{Pointer, Segment};
use crate::textual_edit::{insertion_renderer_for_format, renderer_for_format};

pub(crate) use number::{DecimalParts, float_literal_value, may_exceed_f64};
pub use number::{plain_float, shortest_float};
pub use spans::{SpanContext, SpanMap, SpanRecomputeDelta, ValueSpan, apply_delta};

/// A single value node within a parsed document.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// JSON `null`, YAML `~`/`null`, TOML has no equivalent.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed 64-bit integer — anything that fits in `i64`.
    Int(i64),
    /// Integer that does not fit in `i64`, stored as the original textual literal.
    BigInt(String),
    /// 64-bit float.
    Float(f64),
    /// Float whose `f64` round-trip would be lossy, stored as the original textual literal.
    BigFloat(String),
    /// String.
    String(String),
    /// Sequence of values.
    Array(Vec<Value>),
    /// Mapping from string key to value, preserving insertion order.
    Map(IndexMap<String, Value>),
}

/// Stable format tag carried by every [`Document`].
///
/// The renderer factory dispatches on this to pick the format-specific
/// `ScalarRenderer` / `InsertionRenderer` for a write. Section 3 of M2 will
/// register concrete implementations; until then the factory in
/// [`crate::textual_edit`] returns `None` for every tag, and `set_at` / `del_at`
/// fall through to a [`Error::WriteUnavailable`] error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatTag {
    /// YAML 1.2.
    Yaml,
    /// JSON (RFC 8259).
    Json,
    /// TOML 1.0.
    Toml,
    /// Newline-delimited JSON (JSONL / NDJSON).
    Jsonl,
    /// HCL (HashiCorp Configuration Language) — Terraform et al.
    Hcl,
    /// INI / Java `.properties` files.
    Ini,
    /// `.env` files (KEY=VALUE).
    DotEnv,
    /// CSV (comma-separated values).
    Csv,
    /// TSV (tab-separated values).
    Tsv,
    /// Dockerfile / Containerfile (read-only).
    Dockerfile,
    /// `.gitignore` / `.dockerignore` style ignore lists (read-only).
    IgnoreList,
    /// Markdown with YAML/TOML/JSON frontmatter.
    Frontmatter,
    /// Markdown (CommonMark + GFM, M9). Body parsed into a typed AST tree
    /// with frontmatter folded in as a top-level node field.
    Markdown,
    /// XML 1.0 (M11). Mapped onto the `Value` enum via conventional keys
    /// (`@attrs`, `#text`, `#comments`, `#cdata`, `#pi`, `#xml`); see
    /// `parsers/xml.rs` for the full conventional-key contract.
    Xml,
}

impl FormatTag {
    /// Map a [`crate::Format::name`] string back into a [`FormatTag`].
    ///
    /// Returns `None` for unknown names so callers can pass the result through
    /// `ok_or_else` rather than panicking on a typo. Every registered format
    /// is accepted lower-case.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "yaml" => Some(Self::Yaml),
            "json" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "jsonl" => Some(Self::Jsonl),
            "hcl" => Some(Self::Hcl),
            "ini" => Some(Self::Ini),
            "dotenv" => Some(Self::DotEnv),
            "csv" => Some(Self::Csv),
            "tsv" => Some(Self::Tsv),
            "dockerfile" => Some(Self::Dockerfile),
            "ignore-list" => Some(Self::IgnoreList),
            "frontmatter" => Some(Self::Frontmatter),
            "markdown" => Some(Self::Markdown),
            "xml" => Some(Self::Xml),
            _ => None,
        }
    }

    /// Stable lower-case name of this format tag.
    ///
    /// Inverse of [`FormatTag::from_name`]; matches the
    /// [`crate::Format::name`] string of the registered parser. Used in
    /// diagnostic messages so the user sees the same identifier they pass via
    /// `-F` on the command line (e.g. "dockerfile", "ignore-list").
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Yaml => "yaml",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Jsonl => "jsonl",
            Self::Hcl => "hcl",
            Self::Ini => "ini",
            Self::DotEnv => "dotenv",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Dockerfile => "dockerfile",
            Self::IgnoreList => "ignore-list",
            Self::Frontmatter => "frontmatter",
            Self::Markdown => "markdown",
            Self::Xml => "xml",
        }
    }
}

/// Payload accompanying a parsed Markdown frontmatter document: the inner
/// format that produced the header, and the unaltered body bytes that
/// follow the closing delimiter.
///
/// Carried on [`Document`] through a private `Option<FrontmatterPayload>`
/// field; access goes through [`Document::frontmatter_payload`]. Stage 2 of
/// M5 wires this into the `Frontmatter` parser/writer pair so the body
/// round-trips byte-identical.
#[derive(Debug, Clone, PartialEq)]
pub struct FrontmatterPayload {
    /// Which inner format produced the header.
    pub kind: FrontmatterKind,
    /// Body bytes following the closing delimiter, preserved verbatim.
    pub body: Vec<u8>,
}

/// Inner format used to parse a frontmatter header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmatterKind {
    /// `---` … `---` block parsed as YAML.
    Yaml,
    /// `+++` … `+++` block parsed as TOML.
    Toml,
    /// `{` … `}` block parsed as JSON.
    Json,
}

/// A parsed document: a single top-level value or a multi-document stream.
///
/// The struct keeps the M1 single/multi distinction via the `multi_doc` flag
/// — `value` is the single value when `multi_doc == false`, and a
/// `Value::Array` carrying each document when `multi_doc == true`. Use
/// [`Document::single`] / [`Document::multi`] / [`Document::value_only`] to
/// build instances; pattern-matching on internals is intentionally not part
/// of the public API.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    value: Value,
    original_bytes: Vec<u8>,
    spans: SpanMap,
    format: FormatTag,
    multi_doc: bool,
    /// Body bytes carried alongside a parsed frontmatter header (Markdown).
    ///
    /// `Some(payload)` only when `format == FormatTag::Frontmatter`. The
    /// payload remembers which inner format produced the parsed header so
    /// `Format::write` can re-serialise through the right writer and then
    /// concatenate the body bytes verbatim.
    frontmatter_payload: Option<FrontmatterPayload>,
    /// Provenance side-channel paired with `spans`.
    ///
    /// One [`Provenance::Original`] entry per pointer in `spans`, sharing
    /// the same canonical RFC 6901 keys. Empty for read-only formats whose
    /// parsers do not record byte ranges (jsonl, hcl, ini, dotenv, csv,
    /// tsv, ignore-list, markdown body) — see [`Document::as_ir`] for how
    /// this surfaces to callers. The read-only Markdown and Dockerfile
    /// parsers supply theirs through
    /// [`Document::with_value_bytes_and_provenance`] with no `spans`.
    ///
    /// Eagerly built in the [`Document::with_spans`] constructor (Phase 1
    /// `add-ir-foundation`): the cost is one ordered iteration over an
    /// already-allocated [`SpanMap`], paid at parse time, so the public
    /// [`Document::as_ir`] view is strictly zero-copy at call time. The
    /// alternative — lazy [`std::sync::OnceLock`] materialisation — would
    /// have required hand-rolled `Clone` / `PartialEq` impls for
    /// `Document` to ignore the cache; the eager path keeps the existing
    /// derives intact for a fixed parse-time hashmap iteration.
    provenance: ProvenanceMap,
    /// Whether a write-aware parser built this document, so the textual
    /// edits apply to it. Not the same as having bytes: an empty YAML file
    /// parsed write-aware has neither bytes nor spans.
    write_aware: bool,
}

/// The not-a-number or infinite float a number literal spells in YAML
/// (`.nan`, `-.inf`, `.Inf`) or TOML (`nan`, `+inf`) — how a
/// [`Value::BigFloat`] carries one; `None` for any other literal.
pub(crate) fn non_finite_literal(literal: &str) -> Option<f64> {
    let (negative, rest) = match literal.as_bytes().first() {
        Some(b'-') => (true, &literal[1..]),
        Some(b'+') => (false, &literal[1..]),
        _ => (false, literal),
    };
    let rest = rest.strip_prefix('.').unwrap_or(rest);
    match rest {
        "nan" | "NaN" | "NAN" => Some(f64::NAN),
        "inf" | "Inf" | "INF" => Some(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }),
        _ => None,
    }
}

/// `.nan`, `.inf` or `-.inf`: a non-finite float spelled the YAML way.
fn non_finite_spelling(f: f64) -> &'static str {
    if f.is_nan() {
        ".nan"
    } else if f > 0.0 {
        ".inf"
    } else {
        "-.inf"
    }
}

impl Value {
    /// How dq writes the NaN or infinity this scalar is where a format
    /// has no number for it (console output, JSON handed to a plugin,
    /// messages): `.nan`, `.inf`, `-.inf`, the YAML way, whatever format
    /// it was read from. `None` for anything else.
    #[must_use]
    pub fn non_finite_spelling(&self) -> Option<&'static str> {
        self.non_finite_float().map(non_finite_spelling)
    }

    /// The NaN or infinite float this scalar is — a [`Value::Float`]
    /// holding one, or a [`Value::BigFloat`] spelling one (YAML `.nan` /
    /// `-.inf`, TOML `nan` / `+inf`); `None` for anything else.
    #[must_use]
    pub fn non_finite_float(&self) -> Option<f64> {
        match self {
            Self::Float(f) if !f.is_finite() => Some(*f),
            Self::BigFloat(s) => non_finite_literal(s),
            _ => None,
        }
    }

    /// Whether this value is or holds a NaN or infinite float, which no
    /// JSON number can spell.
    #[must_use]
    pub fn holds_non_finite(&self) -> bool {
        match self {
            Self::Array(items) => items.iter().any(Self::holds_non_finite),
            Self::Map(map) => map.values().any(Self::holds_non_finite),
            scalar => scalar.non_finite_float().is_some(),
        }
    }

    /// Whether this is a number whose exact value is a whole number:
    /// any [`Value::Int`] / [`Value::BigInt`], a finite [`Value::Float`]
    /// without a fraction, a [`Value::BigFloat`] literal such as `1e400`
    /// or `1.50e1`. `false` for a NaN, an infinity, `1e-400` and anything
    /// that is not a number — the JSON Schema `integer` type.
    #[must_use]
    pub fn is_whole_number(&self) -> bool {
        match self {
            Self::Int(_) | Self::BigInt(_) => true,
            Self::Float(f) => f.is_finite() && f.fract() == 0.0,
            Self::BigFloat(s) => number::decimal_is_whole(s),
            _ => false,
        }
    }

    /// Whether `self` and `other` are numbers of the same value, exactly
    /// and whatever their variant: `Int(1)`, `Float(1.0)` and
    /// `BigFloat("1e0")` are one number, `-0.0` is `0`, `1e400` is
    /// `1.0e+400`. An infinity equals the infinity of its sign; a NaN
    /// equals no number. `false` when either is not a number.
    #[must_use]
    pub fn same_number(&self, other: &Self) -> bool {
        if let (Self::Int(a), Self::Int(b)) = (self, other) {
            return a == b;
        }
        if let (Some(a), Some(b)) = (self.exact_f64(), other.exact_f64()) {
            return a == b;
        }
        match (self.non_finite_float(), other.non_finite_float()) {
            (Some(a), Some(b)) => a == b,
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => match (self.finite_literal(), other.finite_literal()) {
                (Some(a), Some(b)) => number::same_decimal(&a, &b),
                _ => false,
            },
        }
    }

    /// How `self` and `other` order as numbers, by their exact value and
    /// whatever their variant: `0.1` is below `0.10000000000000000001`,
    /// `1e400` above every `f64`, an infinity past every finite number on
    /// its side. `None` when either is a NaN — unordered, as every IEEE
    /// comparison with one is false — or is not a number.
    #[must_use]
    pub fn compare_number(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if let (Self::Int(a), Self::Int(b)) = (self, other) {
            return Some(a.cmp(b));
        }
        if let (Some(a), Some(b)) = (self.exact_f64(), other.exact_f64()) {
            return a.partial_cmp(&b);
        }
        match (self.non_finite_float(), other.non_finite_float()) {
            (Some(a), _) if a.is_nan() => None,
            (_, Some(b)) if b.is_nan() => None,
            (Some(a), Some(b)) => a.partial_cmp(&b),
            // An infinity against a finite number: the sign decides.
            (Some(a), None) => other.finite_literal().map(|_| a.total_cmp(&0.0)),
            (None, Some(b)) => self.finite_literal().map(|_| 0.0_f64.total_cmp(&b)),
            (None, None) => {
                number::compare_decimal(&self.finite_literal()?, &other.finite_literal()?)
            }
        }
    }

    /// Whether `self` is a whole multiple of `divisor`, as numbers and by
    /// their exact value: `self / divisor` is an integer. `0.3` is a
    /// multiple of `0.1`, `4` is not one of `1e400`, `0` is one of every
    /// divisor. `None` when either is not a finite number or `divisor` is
    /// zero.
    #[must_use]
    pub fn is_multiple_of(&self, divisor: &Self) -> Option<bool> {
        if let (Some(x), Some(d)) = (self.decimal_parts(), divisor.decimal_parts()) {
            return Some(number::parts_are_multiple(x, d));
        }
        number::decimal_is_multiple(&self.finite_literal()?, &divisor.finite_literal()?)
    }

    /// The `f64` that is this number's value exactly, for the fast paths
    /// of the comparisons above: a [`Value::Float`], or a [`Value::Int`]
    /// below 2^53 in magnitude. `None` for anything else.
    ///
    /// Comparing two such `f64`s is comparing the values the slow path
    /// reads — an `Int`'s digits, a float's [`shortest_float`] literal —
    /// because a float's shortest literal lies in the interval of reals
    /// that round to it; those intervals do not overlap and order as their
    /// floats do; and an `Int` below 2^53, an `f64` itself, is the shortest
    /// literal of its own interval.
    fn exact_f64(&self) -> Option<f64> {
        const TWO_TO_53: u64 = 1 << 53;
        match self {
            Self::Float(f) => Some(*f),
            Self::Int(i) if i.unsigned_abs() < TWO_TO_53 => Some(*i as f64),
            _ => None,
        }
    }

    /// This nonzero number as `m · 10^e`, `m` whole, for the fast path of
    /// [`Value::is_multiple_of`]: an [`Value::Int`], or a [`Value::Float`]
    /// by its [`shortest_float`] literal's digits
    /// ([`number::float_decimal`]). `None` for zero and anything else.
    fn decimal_parts(&self) -> Option<(u64, i32)> {
        match self {
            Self::Int(i) if *i != 0 => Some((i.unsigned_abs(), 0)),
            Self::Float(f) => number::float_decimal(*f),
            _ => None,
        }
    }

    /// A finite number as a decimal literal of its exact value: an `f64`
    /// in its shortest round-trip form (`1.5e-1`); `None` for anything
    /// else.
    fn finite_literal(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Self::Int(i) => Some(i.to_string().into()),
            // `shortest_float`, not `{f:e}`: on a tie in the 17th digit Rust
            // picks the odd digit, serde_json and every dq writer the even one.
            Self::Float(f) if f.is_finite() => Some(number::shortest_float(*f).into()),
            Self::BigInt(s) | Self::BigFloat(s) if non_finite_literal(s).is_none() => {
                Some(s.as_str().into())
            }
            _ => None,
        }
    }

    /// The first number literal this value is or holds that no `f64` or
    /// `i64` holds ([`Value::BigInt`], a finite [`Value::BigFloat`]), as
    /// dq holds it; `None` when there is none.
    fn big_number(&self) -> Option<&str> {
        match self {
            Self::Array(items) => items.iter().find_map(Self::big_number),
            Self::Map(map) => map.values().find_map(Self::big_number),
            Self::BigFloat(s) if non_finite_literal(s).is_some() => None,
            Self::BigInt(s) | Self::BigFloat(s) => Some(s),
            _ => None,
        }
    }

    /// Whether `self` and `other` hold the same data.
    ///
    /// Unlike `==`, a NaN equals a NaN, a non-finite float equals the
    /// literal spelling it (`Float(NaN)` and `BigFloat(".nan")`), and
    /// `-0.0` differs from `0.0` (finite floats compare by bits), and a
    /// big-number literal equals another spelling of the same decimal
    /// (`BigFloat("1e400")` and `BigFloat("1.0e+400")`, exponents of any
    /// size) and a float the literal is one of
    /// ([`Value::from_number_literal`]: `BigFloat("0.10")` and
    /// `Float(0.1)`) — never a number of another value, so
    /// `0.1000000000000000000000000001` differs from `Float(0.1)`; map key
    /// order is ignored, as `==` ignores it.
    #[must_use]
    pub fn same_data(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Map(a), Self::Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(key, a)| b.get(key).is_some_and(|b| a.same_data(b)))
            }
            (Self::Array(a), Self::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same_data(b))
            }
            (a, b) => {
                match (a.non_finite_float(), b.non_finite_float()) {
                    (Some(a), Some(b)) => a == b || (a.is_nan() && b.is_nan()),
                    (Some(_), None) | (None, Some(_)) => false,
                    // By bits: `-0.0 == 0.0`, but a fix from one to the other
                    // is a change.
                    (None, None) => match (a, b) {
                        (Self::Float(a), Self::Float(b)) => a.to_bits() == b.to_bits(),
                        // A literal by its decimal value: `1e400` is `1.0e+400`;
                        // a literal is a float when the float holds it
                        // ([`Value::from_number_literal`]: `0.10` is `0.1`).
                        (Self::BigInt(a), Self::BigInt(b))
                        | (Self::BigFloat(a), Self::BigFloat(b)) => {
                            a == b || number::same_decimal(a, b)
                        }
                        (Self::Float(f), Self::BigFloat(s))
                        | (Self::BigFloat(s), Self::Float(f)) => matches!(
                            number::float_literal_value(s),
                            Some(Self::Float(g)) if g.to_bits() == f.to_bits()
                        ),
                        _ => a == b,
                    },
                }
            }
        }
    }

    /// Whether `self` and `other` are the same number, one of them a big
    /// number literal ([`Value::BigInt`] / [`Value::BigFloat`]) — `1e400`
    /// and `1e+400`, which `==` tells apart by their spelling. `false` for
    /// any pair of other values, collections included.
    #[must_use]
    pub fn same_big_number(&self, other: &Self) -> bool {
        let big = |v: &Self| matches!(v, Self::BigInt(_) | Self::BigFloat(_));
        (big(self) || big(other)) && self.same_data(other)
    }

    /// The number a decimal literal spells, with its exact value — the one
    /// rule every reader and the jq bridge use:
    ///
    /// - an integer that fits `i64` is an [`Value::Int`], any other a
    ///   [`Value::BigInt`];
    /// - a literal with a fraction or an exponent is a [`Value::Float`] when
    ///   it parses to a finite `f64`, has at most 17 significant digits, and
    ///   is that `f64` correctly rounded to its own number of digits
    ///   (`format!("{:.*e}", n - 1, f)` spells the same decimal): `1.10`,
    ///   `1e3`, `0.10000000000000001` (`%.17g` output), `4.9e-324`. Any
    ///   other is a [`Value::BigFloat`]: more digits than an `f64` holds
    ///   (`0.1000000000000000000000000001`), a 17-digit decimal another
    ///   one rounds the `f64` to (`0.10000000000000004`), or a value past
    ///   its range (`1e400`, `1e-400`).
    ///
    /// A big literal is kept as JSON spells it: without a leading `+` or
    /// leading zeros, with a digit on both sides of a `.` (`+.5e400` is
    /// `0.5e400`), the exponent as written, so every writer can copy it.
    /// `None` for anything not shaped `[-+]digits[.digits][(e|E)[-+]digits]`
    /// (a `.` may lead or end the mantissa, not both): hex, `.inf`,
    /// `1_000`, words.
    #[must_use]
    pub fn from_number_literal(literal: &str) -> Option<Self> {
        number::number_value(literal)
    }

    /// The kind of value this is, as a message names it in a sentence:
    /// `a map`, `an array`, `a string`, `an integer`, `a float`,
    /// `a boolean`, `null`.
    #[must_use]
    pub(crate) fn kind_with_article(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "a boolean",
            Self::Int(_) | Self::BigInt(_) => "an integer",
            Self::Float(_) | Self::BigFloat(_) => "a float",
            Self::String(_) => "a string",
            Self::Array(_) => "an array",
            Self::Map(_) => "a map",
        }
    }

    /// Stable type name used by the `type` command and error messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Int(_) | Self::BigInt(_) => "int",
            Self::Float(_) | Self::BigFloat(_) => "float",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Map(_) => "object",
        }
    }

    /// Convert into a [`serde_json::Value`].
    ///
    /// `BigInt` / `BigFloat` are routed through
    /// [`serde_json::Number::from_str`], which honours the workspace's
    /// `serde_json/arbitrary_precision` feature and preserves the original
    /// textual literal across the round-trip. When the literal cannot be
    /// parsed as a number (malformed input from a transformation), the
    /// value falls back to a `String` — losing numeric typing but never
    /// panicking.
    ///
    /// Object key insertion order is preserved via the workspace-level
    /// `serde_json/preserve_order` feature.
    ///
    /// Non-finite floats (`NaN`, `±Infinity`) — which are not
    /// representable in JSON — fall back to a [`serde_json::Value::String`]
    /// spelling them the YAML way, `".nan"`, `".inf"`, `"-.inf"`, whatever
    /// format they were read from (a TOML `nan`, a YAML `.NaN`) or jq made
    /// them in, so the value survives reporting instead of silently
    /// becoming `Null`. Callers that must tell such a value from a string
    /// check [`Value::holds_non_finite`] first.
    #[must_use]
    pub fn to_serde_json(&self) -> serde_json::Value {
        if let Some(f) = self.non_finite_float() {
            return serde_json::Value::String(non_finite_spelling(f).to_owned());
        }
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::Int(n) => serde_json::Value::Number((*n).into()),
            Self::Float(f) => serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            // A float spelled as an integer (`!!float 18446744073709551617`)
            // gets a `.0`, so JSON reads it back as a float.
            Self::BigFloat(s) if DecimalParts::parse(s).is_some_and(|p| p.is_integer()) => {
                let float = format!("{s}.0");
                serde_json::Number::from_str(&float).map_or_else(
                    |_| serde_json::Value::String(s.clone()),
                    serde_json::Value::Number,
                )
            }
            Self::BigInt(s) | Self::BigFloat(s) => match serde_json::Number::from_str(s) {
                Ok(n) => serde_json::Value::Number(n),
                Err(_) => serde_json::Value::String(s.clone()),
            },
            Self::String(s) => serde_json::Value::String(s.clone()),
            Self::Array(items) => {
                serde_json::Value::Array(items.iter().map(Self::to_serde_json).collect())
            }
            Self::Map(map) => {
                let mut out = serde_json::Map::new();
                for (k, child) in map {
                    out.insert(k.clone(), child.to_serde_json());
                }
                serde_json::Value::Object(out)
            }
        }
    }

    /// Convert from a [`serde_json::Value`].
    ///
    /// Inverse of [`Value::to_serde_json`]. Numbers go through
    /// [`serde_json::Number`]'s textual literal (the workspace's
    /// `arbitrary_precision` feature keeps the original literal verbatim) so
    /// 22-digit integers survive as [`Value::BigInt`] and round-trip
    /// byte-for-byte. Object key insertion order is preserved via
    /// [`indexmap::IndexMap`].
    #[must_use]
    pub fn from_serde_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => serde_json_number_to_value(n),
            serde_json::Value::String(s) => Self::String(s.clone()),
            serde_json::Value::Array(items) => {
                Self::Array(items.iter().map(Self::from_serde_json).collect())
            }
            serde_json::Value::Object(map) => {
                let mut out = IndexMap::with_capacity(map.len());
                for (k, child) in map {
                    out.insert(k.clone(), Self::from_serde_json(child));
                }
                Self::Map(out)
            }
        }
    }
}

/// Convert a [`serde_json::Number`] into a [`Value`] with its exact value.
///
/// With the workspace's `arbitrary_precision` feature on `serde_json`,
/// `Number::to_string()` returns the original textual literal verbatim —
/// `as_i64()` / `as_f64()` lossily collapse a 22-digit integer to a
/// `Float(4.7e21)`; [`Value::from_number_literal`] reads the literal.
fn serde_json_number_to_value(n: &serde_json::Number) -> Value {
    let literal = n.to_string();
    Value::from_number_literal(&literal).unwrap_or(Value::BigFloat(literal))
}

impl Document {
    /// Build a single-document, read-only `Document` carrying just a [`Value`].
    ///
    /// `original_bytes` is empty and `spans` is empty — write operations
    /// will return [`Error::WriteUnavailable`] until the document is
    /// re-parsed via a write-aware parser. This constructor is the M1
    /// shape: read-pat parsers (`Json`, `Yaml`, `Toml`, `Jsonl`) all use it.
    #[must_use]
    pub fn value_only(value: Value, format: FormatTag) -> Self {
        Self {
            value,
            original_bytes: Vec::new(),
            spans: SpanMap::new(),
            format,
            multi_doc: false,
            frontmatter_payload: None,
            provenance: ProvenanceMap::new(),
            write_aware: false,
        }
    }

    /// Build a multi-document, read-only `Document` carrying a sequence of
    /// top-level [`Value`]s (e.g. a YAML stream separated by `---`).
    ///
    /// Internally stored as a `Value::Array(values)` with the `multi_doc`
    /// flag set so [`Document::values`] returns the per-document slice and
    /// [`Document::value`] errors. Like [`Document::value_only`], this
    /// shape carries no spans — writes are unavailable until a write-aware
    /// parser populates them.
    #[must_use]
    pub fn multi_value_only(values: Vec<Value>, format: FormatTag) -> Self {
        Self {
            value: Value::Array(values),
            original_bytes: Vec::new(),
            spans: SpanMap::new(),
            format,
            multi_doc: true,
            frontmatter_payload: None,
            provenance: ProvenanceMap::new(),
            write_aware: false,
        }
    }

    /// Build a write-aware `Document` carrying spans and the original source.
    ///
    /// Used by Section 3+ parsers once the saphyr-parser-based YAML span
    /// builder, the JSON span builder, and the TOML `toml_edit` adapter
    /// land. `original_bytes` is the byte slice the parser received, copied
    /// in. `spans` maps canonical pointer strings to [`ValueSpan`]s.
    #[must_use]
    pub fn with_spans(
        value: Value,
        original_bytes: Vec<u8>,
        spans: SpanMap,
        format: FormatTag,
    ) -> Self {
        let provenance = provenance_from_spans(&spans);
        Self {
            value,
            original_bytes,
            spans,
            format,
            multi_doc: false,
            frontmatter_payload: None,
            provenance,
            write_aware: true,
        }
    }

    /// Build a `Document` from an explicit provenance map alongside the value
    /// tree, source bytes, and spans.
    ///
    /// This is the constructor parsers use when they need to populate
    /// fields on `Provenance::Original` that the default
    /// [`Document::with_spans`] path does not surface — most importantly
    /// `inline_offset` for YAML block scalars and markdown fenced code
    /// blocks (Phase 2 of `add-validation-and-extended-formats`). Callers
    /// are expected to derive `provenance` from `spans` (one entry per
    /// canonical pointer) and then enrich the entries that need
    /// inline-offset metadata.
    ///
    /// `spans` and `provenance` SHOULD share the same key set — the IR
    /// `as_ir()` path looks them up by the same canonical string and a
    /// divergence would surface as inconsistent `span_for` /
    /// `inline_offset_for` results. Tests in `tests/ir_yaml_provenance.rs`
    /// pin the cross-channel agreement.
    #[must_use]
    pub fn with_spans_and_provenance(
        value: Value,
        original_bytes: Vec<u8>,
        spans: SpanMap,
        format: FormatTag,
        provenance: ProvenanceMap,
    ) -> Self {
        Self {
            value,
            original_bytes,
            spans,
            format,
            multi_doc: false,
            frontmatter_payload: None,
            provenance,
            write_aware: true,
        }
    }

    /// Build a read-only `Document` whose provenance map is supplied
    /// externally, without a span map.
    ///
    /// Used by parsers that record `Provenance::Original` entries (with
    /// `span: None` and an `inline_offset`) but cannot produce a write-pat
    /// `SpanMap` for their format — the markdown parser (Phase 2 of
    /// `add-validation-and-extended-formats`) is the first such caller.
    /// The resulting document still has `original_bytes` populated for
    /// round-trip detection in `Format::write`.
    ///
    /// The Dockerfile parser records `span: Some(_)` entries here: the
    /// spans let [`crate::ir::Ir::line_col_for`] place a diagnostic, and with no
    /// `SpanMap` and `write_aware` false the document stays read-only.
    #[must_use]
    pub fn with_value_bytes_and_provenance(
        value: Value,
        original_bytes: Vec<u8>,
        format: FormatTag,
        provenance: ProvenanceMap,
    ) -> Self {
        Self {
            value,
            original_bytes,
            spans: SpanMap::new(),
            format,
            multi_doc: false,
            frontmatter_payload: None,
            provenance,
            write_aware: false,
        }
    }

    /// Build a frontmatter `Document` carrying a parsed header value and an
    /// opaque body. The body is preserved verbatim through round-trip; the
    /// header is re-emitted via the inner format's writer.
    #[must_use]
    pub fn frontmatter(value: Value, body: Vec<u8>, kind: FrontmatterKind) -> Self {
        Self {
            value,
            original_bytes: Vec::new(),
            spans: SpanMap::new(),
            format: FormatTag::Frontmatter,
            multi_doc: false,
            frontmatter_payload: Some(FrontmatterPayload { kind, body }),
            provenance: ProvenanceMap::new(),
            write_aware: false,
        }
    }

    // -------------------------------------------------------------------
    // Backward-compat M1 constructors. The CLI commands and parsers built
    // before M2 used `Document::Single(v)` / `Document::Multi(vs)` enum
    // variants. They have been migrated to call these functions, which
    // produce the equivalent struct instance with empty span metadata.
    // -------------------------------------------------------------------

    /// M1-compatible single-document constructor. Equivalent to
    /// [`Document::value_only`] with `format = FormatTag::Yaml` (a benign
    /// default for read-only call sites that did not previously carry a
    /// format tag — every read-path consumer goes through `value()`,
    /// `values()`, or `enumerate_pointers`, none of which inspect the
    /// format tag).
    #[must_use]
    pub fn single(value: Value) -> Self {
        Self::value_only(value, FormatTag::Yaml)
    }

    /// M1-compatible multi-document constructor. See [`Document::single`]
    /// for the rationale on the `Yaml` placeholder.
    #[must_use]
    pub fn multi(values: Vec<Value>) -> Self {
        Self::multi_value_only(values, FormatTag::Yaml)
    }

    /// Borrow the underlying single-document value.
    ///
    /// Returns the inner [`Value`] regardless of `multi_doc`; for multi-doc
    /// streams this is the synthetic `Value::Array(documents)` form, which
    /// is what every M1 `Document::Single(v) => v` call site previously
    /// extracted. Callers who care about the multi-doc structure use
    /// [`Document::values`] or [`Document::is_multi`].
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Borrow the underlying value mutably. Used by the write path when
    /// applying `set_at` / `del_at` to keep the in-memory tree consistent
    /// with `original_bytes`.
    pub fn value_mut(&mut self) -> &mut Value {
        &mut self.value
    }

    /// Returns true when this document represents a multi-document stream.
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.multi_doc
    }

    /// Borrow the per-document slice of a multi-document stream.
    ///
    /// Returns `Some(&[Value])` when `is_multi()`, else `None`. This is the
    /// shape M1 callers consumed via `Document::Multi(vs) => vs`.
    #[must_use]
    pub fn values(&self) -> Option<&[Value]> {
        if !self.multi_doc {
            return None;
        }
        match &self.value {
            Value::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    /// Each document of a multi-document stream as a document of its own,
    /// with its provenance rebased onto the document's root — see
    /// [`crate::StreamDocument`]. `None` when this is not a stream
    /// (`!is_multi()`), so a single document that holds a sequence is never
    /// split into its items.
    #[must_use]
    pub fn stream_documents(&self) -> Option<Vec<crate::StreamDocument<'_>>> {
        let documents = self.values()?;
        // Where each document sits in the bytes, so a document's fallback
        // position stays inside it — an empty document has no span of its
        // own inside it.
        let bounds: Option<Vec<std::ops::Range<usize>>> = (self.format == FormatTag::Yaml
            && !self.original_bytes.is_empty())
        .then(|| crate::parsers::yaml_spans::document_extents(&self.original_bytes))
        .flatten()
        .filter(|extents| extents.len() == documents.len())
        .map(|extents| {
            extents
                .into_iter()
                .map(|extent| extent.line_start..extent.body.end)
                .collect()
        });
        Some(crate::ir::split_stream(
            documents,
            &self.provenance,
            self.format,
            &self.original_bytes,
            bounds.as_deref(),
        ))
    }

    /// Mark a write-aware parse of a multi-document stream as the stream
    /// it is. The span parsers return a stream as one document holding a
    /// `Value::Array` of the documents, because that is the tree the
    /// `/{doc_idx}/...` spans describe; this flag is what tells it apart
    /// from a single document holding a sequence. The textual edits read
    /// neither, so they behave the same either way.
    pub(crate) fn mark_stream(&mut self) {
        self.multi_doc = matches!(self.value, Value::Array(_));
    }

    /// Whether a write-aware parser built this document, so
    /// [`Document::set_at`] and [`Document::del_at`] can edit its text. An
    /// empty file parsed write-aware is write-aware with no bytes.
    #[must_use]
    pub fn is_write_aware(&self) -> bool {
        self.write_aware
    }

    /// Borrow the original source bytes, if this document was parsed by a
    /// write-aware parser.
    #[must_use]
    pub fn original_bytes(&self) -> &[u8] {
        &self.original_bytes
    }

    /// Borrow the span map (test/inspection helper — production callers go
    /// through `set_at` / `del_at`).
    #[must_use]
    pub fn spans(&self) -> &SpanMap {
        &self.spans
    }

    /// Look up the span for `pointer`'s canonical RFC 6901 form.
    ///
    /// Returns `None` for read-only documents and for pointers whose target
    /// the parser did not record. Callers must not assume every existing
    /// node has a span — only the leaf scalars produced by Section 3
    /// parsers do, and only on documents built via `with_spans`.
    #[must_use]
    pub fn span_at(&self, pointer: &Pointer) -> Option<&ValueSpan> {
        self.spans.get(&pointer.as_canonical())
    }

    /// Format tag used to dispatch the write-path renderer factory.
    #[must_use]
    pub fn format(&self) -> FormatTag {
        self.format
    }

    /// Borrow this `Document` as an [`Ir<'_>`] — the read-only IR view used
    /// by the lint pipeline (Phase 2+ of `add-ir-foundation`) to resolve
    /// per-pointer provenance and source spans.
    ///
    /// # Zero-copy contract
    ///
    /// This call neither clones the [`Value`] tree nor the underlying
    /// [`SpanMap`]. The returned [`Ir<'_>`] borrows three already-allocated
    /// fields:
    ///
    /// - `&self.value` — the parsed value tree.
    /// - `&self.provenance` — the [`ProvenanceMap`] eagerly derived in
    ///   [`Document::with_spans`] from the parser-supplied span map.
    ///   Mutated alongside `spans` inside [`Document::set_at`] /
    ///   [`Document::del_at`] so the IR view stays consistent across
    ///   writes.
    /// - `self.format` — a `Copy` [`FormatTag`].
    ///
    /// # Empty provenance
    ///
    /// Documents produced by read-only parsers (jsonl, hcl, ini, dotenv,
    /// csv, tsv, ignore-list, frontmatter body, markdown body)
    /// carry an empty [`ProvenanceMap`] — the parsers route through
    /// [`Document::value_only`] / [`Document::multi_value_only`] /
    /// [`Document::frontmatter`], none of which emit provenance entries.
    /// `as_ir().provenance_for(p)` returns `None` for every pointer in
    /// such documents, while `as_ir().format()` still surfaces the
    /// concrete tag — callers can therefore distinguish "no provenance
    /// available for this format" from "this format does not exist".
    #[must_use]
    pub fn as_ir(&self) -> Ir<'_> {
        Ir::with_bytes(
            &self.value,
            &self.provenance,
            self.format,
            &self.original_bytes,
        )
    }

    /// Access the frontmatter payload, if this `Document` was produced by the
    /// frontmatter parser. Returns `None` for every other format.
    #[must_use]
    pub fn frontmatter_payload(&self) -> Option<&FrontmatterPayload> {
        self.frontmatter_payload.as_ref()
    }

    /// Returns the type name of the addressed leaf when this is a single
    /// document, or `"array"` for a multi-document stream (the multi-doc
    /// stream is addressed as if it were a top-level array of documents).
    #[must_use]
    pub fn leaf_type_name(&self) -> &'static str {
        if self.multi_doc {
            "array"
        } else {
            self.value.type_name()
        }
    }

    /// Replace the value at `pointer` with `value`, splicing the rendered
    /// bytes into `original_bytes` and shifting affected spans.
    ///
    /// # mkdir-p semantics
    ///
    /// When `pointer`'s leaf segment does not yet exist but its parent
    /// container does, `set_at` inserts the missing key via the format's
    /// [`InsertionRenderer`]. Two single-level cases are covered:
    ///
    /// 1. **Sibling-anchored** — parent has at least one existing direct
    ///    child whose scalar span was recorded by the parser. The new key
    ///    is spliced next to that sibling.
    /// 2. **Empty-parent** — parent is a recorded empty container
    ///    (`{}` / `[]`) with no children. The new key is spliced inside
    ///    the parent's `{}` / `[]` body. Required by RFC 7396 §2 (merging
    ///    `{}` with `{"a":1}` must yield `{"a":1}`); the parsers now
    ///    record an empty-container span at the parent's pointer
    ///    specifically so this path can fire.
    ///
    /// A chain of missing keys (`/a/missing1/missing2/key`) is created as
    /// mappings, the first missing key holding the rest (see
    /// `Document::try_mkdir_p`); the node it hangs from must be a mapping,
    /// else [`Error::Path`] with `kind = MissingKey`.
    ///
    /// Formats whose [`Format::insertion_renderer`] returns `None` (XML,
    /// CSV, INI, .env, Dockerfile, ignore-list, markdown body) remain
    /// strictly read-only for mkdir-p and continue to return
    /// [`Error::Path`].
    ///
    /// # Read-back check
    ///
    /// The edited bytes are parsed again and must read back as the tree
    /// before the edit with `value` at `pointer` — and, in YAML, at every
    /// alias of an anchored node at or above `pointer`, which shares the
    /// edit. Otherwise the document is left as it was and the edit fails.
    /// On success the value tree, spans and provenance are the fresh
    /// parse's.
    ///
    /// # Errors
    ///
    /// - [`Error::WriteUnavailable`] when the document was loaded
    ///   read-only (no spans or no renderer registered).
    /// - [`Error::Path`] when the pointer addresses a non-existent path
    ///   that cannot be created via the mkdir-p path.
    /// - [`Error::EditRejected`] when the edited bytes fail the read-back
    ///   check.
    pub fn set_at(&mut self, pointer: &Pointer, value: Value) -> Result<()> {
        self.with_lf_line_breaks("set", pointer, |doc| {
            if doc.format == FormatTag::Yaml && doc.write_aware {
                // A whole document: one of a stream whose content is a
                // collection, or a file's only document, which has no
                // bytes of its own to replace; the YAML writer writes it
                // and the comments around it stay.
                // An empty document (null) has no text to replace either.
                if let Some(index) = doc.stream_document_index(pointer)
                    && doc
                        .values()
                        .is_some_and(|documents| match &documents[index] {
                            Value::Map(map) => !map.is_empty(),
                            Value::Array(items) => !items.is_empty(),
                            value => *value == Value::Null,
                        })
                {
                    return doc.reemit_stream_document(index, value);
                }
                if pointer.is_root() && (!doc.spans.contains_key("") || doc.value == Value::Null) {
                    if doc.multi_doc {
                        return Err(Error::Path {
                            pointer: String::new(),
                            matched_prefix: String::new(),
                            kind: PathErrorKind::StreamRoot,
                            did_you_mean: Vec::new(),
                        });
                    }
                    return doc.reemit_yaml_root(value);
                }
                // A key of an empty document — an empty file, comments
                // alone, a bare `---`: the document becomes a mapping
                // holding it, written whole.
                if let Some((index, rest)) = doc.empty_document_path(pointer) {
                    let mut fresh = Value::Map(IndexMap::new());
                    if set_value_at(&mut fresh, &rest, value.clone()).is_ok() {
                        return match index {
                            Some(index) => doc.reemit_stream_document(index, fresh),
                            None => doc.reemit_yaml_root(fresh),
                        };
                    }
                }
            }
            let before = doc.clone();
            let layout = LayoutCache::new();
            let result = doc.set_at_unchecked(pointer, value.clone(), &layout);
            doc.check_edit(before, result, "set", pointer, &layout, |tree, at| {
                set_value_at(tree, at, value.clone())
            })
        })
    }

    /// Run `edit` on a YAML or JSON file whose lines end in a lone `\r`
    /// as on the same file with `\n` line breaks — each `\r` is a line
    /// break to both, and the textual edits look for `\n` — then turn every
    /// `\n` back into `\r`, so the lines the edit writes end the way the
    /// file's do. The bytes as written are parsed again and must read as
    /// the `\n` form did; otherwise, as on any error, `self` is left as it
    /// was. Other files go to `edit` as they are.
    pub(crate) fn with_lf_line_breaks(
        &mut self,
        action: &'static str,
        pointer: &Pointer,
        edit: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        if !matches!(self.format, FormatTag::Yaml | FormatTag::Json)
            || !LineBreak::cr_only(&self.original_bytes)
        {
            return edit(self);
        }
        let before = self.clone();
        let swap = |bytes: &mut Vec<u8>, from: u8, to: u8| {
            bytes
                .iter_mut()
                .filter(|b| **b == from)
                .for_each(|b| *b = to);
        };
        swap(&mut self.original_bytes, b'\r', b'\n');
        // The spans of the `\n` form: a line range ends past a `\n`. The
        // YAML reader builds a lone-`\r` file's spans so already.
        if self.format == FormatTag::Json
            && let Some(Ok(fresh)) = parse_again(self.format, &self.original_bytes)
        {
            self.spans = fresh.spans;
            self.provenance = fresh.provenance;
        }
        if let Err(err) = edit(self) {
            *self = before;
            return Err(err);
        }
        swap(&mut self.original_bytes, b'\n', b'\r');
        match parse_again(self.format, &self.original_bytes) {
            Some(Ok(fresh)) if first_difference(&self.value, &fresh.value, "").is_none() => {
                self.value = fresh.value;
                self.spans = fresh.spans;
                self.provenance = fresh.provenance;
                Ok(())
            }
            _ => {
                *self = before;
                Err(Error::EditRejected {
                    action,
                    pointer: pointer.as_canonical(),
                    reason: format!("the edited document would not parse; {JQ_REWRITE_HINT}"),
                })
            }
        }
    }

    /// The refusal of a textual edit — `what` it would do, as "set a value
    /// in it" — in a format dq writes no textual edits for (Markdown, INI,
    /// CSV, …: the ones with no scalar renderer).
    pub(crate) fn no_textual_edits(&self, what: &str) -> Error {
        let name = self.format.name();
        Error::WriteUnavailable {
            reason: format!(
                "{name} has no textual edits: dq can read a {name} file but cannot {what}"
            ),
        }
    }

    /// The index of the document of a write-aware YAML stream `pointer`
    /// names (`/1`), `None` for any other pointer or document.
    fn stream_document_index(&self, pointer: &Pointer) -> Option<usize> {
        match pointer.segments() {
            [segment] => self.stream_document_at(segment),
            _ => None,
        }
    }

    /// Where `pointer` names a key below a write-aware YAML document with
    /// no content (not even `~`): the document — `None`
    /// for a file's only document, `Some(index)` for one of a stream — and
    /// the pointer within it. `None` for any other pointer or document.
    fn empty_document_path(&self, pointer: &Pointer) -> Option<(Option<usize>, Pointer)> {
        if self.format != FormatTag::Yaml || !self.write_aware {
            return None;
        }
        let (index, rest) = if self.multi_doc {
            let (first, rest) = pointer.segments().split_first()?;
            let index = self.stream_document_at(first)?;
            (Some(index), rest)
        } else {
            (None, pointer.segments())
        };
        let document = match index {
            Some(index) => self.values()?.get(index)?,
            None => &self.value,
        };
        if !matches!(rest.first(), Some(Segment::Key(_))) || *document != Value::Null {
            return None;
        }
        // No content and no node properties (`--- !!str`); the spans
        // cannot tell, since an empty node is spanned at the next token.
        let extents = crate::parsers::yaml_spans::document_extents(&self.original_bytes)?;
        let empty = match extents.get(index.unwrap_or(0)) {
            Some(extent) => {
                extent.content_end == extent.body.start && extent.after_properties.is_none()
            }
            None => extents.is_empty(),
        };
        empty.then(|| (index, Pointer::new(rest.to_vec())))
    }

    /// The index of the document of a write-aware YAML stream `segment`
    /// names, `None` when it names none.
    fn stream_document_at(&self, segment: &Segment) -> Option<usize> {
        if self.format != FormatTag::Yaml || !self.write_aware {
            return None;
        }
        let documents = self.values()?;
        let index = match segment {
            Segment::Index(index) => *index,
            Segment::Key(key) => key
                .parse()
                .ok()
                .filter(|index: &usize| index.to_string() == *key)?,
        };
        (index < documents.len()).then_some(index)
    }

    /// The YAML edit layout of the bytes as they were before the edit
    /// under way, worked out once per edit through `cache`; `None` for
    /// other formats or bytes saphyr rejects. Call it only before the
    /// edit splices.
    fn edit_layout<'a>(
        &self,
        cache: &'a LayoutCache,
    ) -> Option<&'a crate::parsers::yaml_spans::EditLayout> {
        cache
            .get_or_init(|| {
                (self.format == FormatTag::Yaml)
                    .then(|| {
                        crate::parsers::yaml_spans::edit_layout(&self.original_bytes)
                            .inspect_err(|err| tracing::debug!(%err, "YAML edit layout failed"))
                            .ok()
                    })
                    .flatten()
            })
            .as_ref()
    }

    /// Write `value` over the whole JSON or TOML document: JSON as the
    /// textual-edit renderer writes a value, in place of the content between
    /// the file's leading and trailing blanks; TOML through its writer, after
    /// the block of comment and blank lines the file starts with. The caller's
    /// read-back checks the result.
    ///
    /// # Errors
    ///
    /// [`Error::EditRejected`] for a TOML value that is no table — a TOML
    /// document is one — and the TOML writer's error for a table it cannot
    /// write.
    fn replace_root_text(
        &mut self,
        value: Value,
        renderer: &dyn crate::textual_edit::ScalarRenderer,
    ) -> Result<()> {
        let bytes = &self.original_bytes;
        let content_start = bom_end(bytes);
        let (range, inserted) = if self.format == FormatTag::Toml {
            if let Some(reason) = toml_root_refusal(&value) {
                return Err(Error::EditRejected {
                    action: "set",
                    pointer: String::new(),
                    reason,
                });
            }
            let mut emitted = Vec::new();
            crate::format::Format::write(
                &crate::parsers::Toml,
                &Document::value_only(value, FormatTag::Toml),
                &mut emitted,
            )?;
            // Past the comment and blank lines that open the file.
            let mut keep = content_start;
            while keep < bytes.len() {
                let line_end = bytes[keep..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(bytes.len(), |at| keep + at + 1);
                let line = bytes[keep..line_end].trim_ascii();
                if !(line.is_empty() || line.starts_with(b"#")) {
                    break;
                }
                keep = line_end;
            }
            let mut inserted = Vec::with_capacity(emitted.len() + 1);
            if keep > content_start && bytes[keep - 1] != b'\n' && !emitted.is_empty() {
                inserted.push(b'\n');
            }
            inserted.extend_from_slice(&emitted);
            (
                keep..bytes.len(),
                with_line_ending_at(bytes, keep, inserted),
            )
        } else {
            let start = bytes[content_start..]
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .map_or(bytes.len(), |at| content_start + at);
            let end = bytes
                .iter()
                .rposition(|b| !b.is_ascii_whitespace())
                .map_or(start, |at| (at + 1).max(start));
            let rendered = renderer.render_replacement(&value, SpanContext::BlockMapValue, &[]);
            (start..end, rendered)
        };
        self.original_bytes.splice(range, inserted);
        Ok(())
    }

    /// [`Document::set_at`] without the read-back check.
    fn set_at_unchecked(
        &mut self,
        pointer: &Pointer,
        value: Value,
        layout: &LayoutCache,
    ) -> Result<()> {
        let Some(renderer) = renderer_for_format(self.format) else {
            return Err(self.no_textual_edits("set a value in it"));
        };
        // Read-only parsers go through `value_only` / `multi_value_only`;
        // neither bytes nor spans tell, since an empty YAML file parsed
        // write-aware has none of either.
        if !self.write_aware {
            return Err(Error::WriteUnavailable {
                reason: format!(
                    "{} document was loaded read-only; reload via a write-aware parser to enable set",
                    self.format.name(),
                ),
            });
        }

        let canonical = pointer.as_canonical();
        if self.format == FormatTag::Json && value.holds_non_finite() {
            return Err(Error::EditRejected {
                action: "set",
                pointer: canonical,
                reason: JSON_NON_FINITE.to_owned(),
            });
        }
        if self.format == FormatTag::Toml
            && let Some(number) = value.big_number()
        {
            return Err(Error::EditRejected {
                action: "set",
                pointer: canonical,
                reason: format!("{TOML_BIG_NUMBER} {number}"),
            });
        }
        if pointer.is_root()
            && (self.format == FormatTag::Toml
                || (self.format == FormatTag::Json && !self.spans.contains_key("")))
        {
            return self.replace_root_text(value, renderer);
        }
        if self.format == FormatTag::Yaml
            && match pointer.resolve(&self.value) {
                // `key: !!str` reads as "".
                Ok(Value::Null) => true,
                Ok(Value::String(s)) => s.is_empty(),
                _ => false,
            }
            && let Some(span) = self.spans.get(&canonical).cloned()
            && let Some(at) = self
                .edit_layout(layout)
                .and_then(|layout| layout.empty_values.get(&canonical).cloned())
        {
            // An empty node (`key:`, `- &a`) has no text to replace; its
            // recorded span sits on the token after it.
            let tags = CoreTags::at(&self.original_bytes, at.start);
            let rendered = renderer.render_replacement(&value, span.context, &[]);
            let flow_prefix = self
                .edit_layout(layout)
                .and_then(|layout| layout.flow_value_prefixes.get(&canonical).copied());
            let (mut replacement, mut value_offset) = if let Some(prefix) = flow_prefix {
                // `{n}` / `{n: }`: the value, with the `:` or blank it needs.
                let mut out = prefix.to_vec();
                out.extend_from_slice(&rendered);
                (out, prefix.len())
            } else {
                empty_node_replacement(&self.original_bytes[at.clone()], &rendered, &tags)
            };
            // `? key` with no `:` line: the value needs its indicator, on
            // a line of its own at the `?` column.
            let explicit_key = self
                .edit_layout(layout)
                .and_then(|layout| layout.entries.get(&canonical))
                .map(|entry| entry.at)
                .filter(|&entry| self.original_bytes[entry] == b'?');
            let indicator = skip_blanks_back(&self.original_bytes, at.start);
            if let Some(question) = explicit_key
                && span.context == SpanContext::BlockMapValue
                && at.is_empty()
                && self.original_bytes[..indicator].last() != Some(&b':')
            {
                let mut line = b"\n".to_vec();
                line.extend(std::iter::repeat_n(
                    b' ',
                    line_column(&self.original_bytes, question),
                ));
                line.extend_from_slice(b":");
                let line = with_line_ending_at(&self.original_bytes, at.start, line);
                value_offset += line.len();
                line.into_iter()
                    .rev()
                    .for_each(|b| replacement.insert(0, b));
            }
            self.original_bytes
                .splice(at.clone(), replacement.iter().copied());
            apply_delta(
                &mut self.spans,
                SpanRecomputeDelta {
                    at: at.start,
                    old_len: at.len(),
                    new_len: replacement.len(),
                },
            );
            set_value_at(&mut self.value, pointer, value)?;
            let start = at.start + value_offset;
            self.spans.insert(
                canonical,
                ValueSpan {
                    value_range: start..start + rendered.len(),
                    ..span
                },
            );
            self.provenance = provenance_from_spans(&self.spans);
            return Ok(());
        }
        if let Some(span) = self.spans.get(&canonical).cloned() {
            // Replace-in-span path: render the new value using the same
            // syntactic context as the original, splice into the source
            // buffer, then shift every span to the right of the edit.
            //
            // A `|` / `>` block scalar is rewritten whole as an inline
            // scalar, header included: a block scalar can only hold a
            // string, and its value depends on its header's chomping.
            let block_scalar = (self.format == FormatTag::Yaml
                && matches!(pointer.resolve(&self.value), Ok(Value::String(_))))
            .then(|| crate::parsers::yaml_spans::block_scalar_extent(&self.original_bytes, &span))
            .flatten();
            let (mut replaced, mut rendered, value_len) = match block_scalar {
                Some((extent, keep)) => {
                    let mut rendered = renderer.render_replacement(&value, span.context, &[]);
                    let value_len = rendered.len();
                    rendered.extend_from_slice(&self.original_bytes[keep]);
                    (extent, rendered, value_len)
                }
                None => {
                    let original_slice = &self.original_bytes[span.value_range.clone()];
                    let rendered =
                        renderer.render_replacement(&value, span.context, original_slice);
                    let value_len = rendered.len();
                    (span.value_range.clone(), rendered, value_len)
                }
            };
            // A core-schema tag fixes the type of what follows it
            // (`!!int`, `!!binary`): the new value is written untagged,
            // typed by what it is. dq's tree carries no tags; local tags
            // (`!Ref`) and anchors stay.
            let mut value_start = replaced.start;
            if self.format == FormatTag::Yaml {
                let tags = CoreTags::at(&self.original_bytes, replaced.start);
                let props_start = yaml_properties_start(&self.original_bytes, replaced.start);
                let props = &self.original_bytes[props_start..replaced.start];
                if tags.any_in(props) {
                    let mut kept = kept_properties(props, &tags);
                    value_start = props_start + kept.len();
                    kept.append(&mut rendered);
                    rendered = kept;
                    replaced.start = props_start;
                }
            }
            let delta = SpanRecomputeDelta {
                at: replaced.start,
                old_len: replaced.end - replaced.start,
                new_len: rendered.len(),
            };
            // `splice` requires an owned iterator; cloning into a Vec keeps
            // the operation straightforward and the cost is negligible for
            // human-scale documents.
            self.original_bytes
                .splice(replaced.clone(), rendered.iter().copied());
            apply_delta(&mut self.spans, delta);
            // Update the also-stored `Value` tree so subsequent reads via
            // `value()` reflect the write. Failures here are propagated as
            // the original `Error::Path` — the textual buffer was already
            // patched, but the in-memory tree was not, which is a bug
            // worth surfacing rather than silently masking.
            set_value_at(&mut self.value, pointer, value)?;
            // Patch span metadata in place: keep `at` and `line_range.start`
            // anchored to the original byte position, but extend the end to
            // the new length.
            let new_end = value_start + value_len;
            if let Some(updated) = self.spans.get_mut(&canonical) {
                updated.value_range = value_start..new_end;
                // For single-line scalars `line_range` typically equals
                // `value_range` plus the trailing newline. Conservatively
                // extend by the same delta so the line range stays anchored.
                let line_shift = (rendered.len() as isize) - (delta.old_len as isize);
                let line_end = signed_extend(span.line_range.end, line_shift);
                updated.line_range = span.line_range.start..line_end;
            }
            // Re-sync the provenance side-channel with the post-edit spans
            // so `Document::as_ir().span_for(p)` keeps matching
            // `Document::span_at(p)` after the write. Cost is one ordered
            // iteration over the updated `SpanMap`, dwarfed by the byte
            // splice and span-recompute that already ran above.
            self.provenance = provenance_from_spans(&self.spans);
            Ok(())
        } else if !pointer.is_root()
            && matches!(
                pointer.resolve(&self.value),
                Ok(Value::Map(_) | Value::Array(_))
            )
        {
            // A non-empty collection records no span of its own.
            self.replace_collection(pointer, &canonical, value, layout, renderer)
        } else {
            // mkdir-p path: insert the missing key, with the mappings
            // missing above it, before surfacing `Error::Path { MissingKey }`.
            self.try_mkdir_p(pointer, &canonical, value, layout)
        }
    }

    /// Insert `value` at the missing `pointer` together with the mappings
    /// missing on the way to it (`/b/c` in `a: 1`): the first missing key
    /// goes in through [`Document::try_single_level_mkdir_p`] holding the
    /// rest of the chain, as `set_value_at` builds it — every missing
    /// segment is a mapping key, numeric ones included (`/b/0` makes
    /// `{"b": {"0": …}}`); an array is never made. In TOML the chain is a
    /// table: a `[header]` of its own in a file that writes its tables
    /// under headers, when the table it hangs from is the root or has a
    /// header itself; a dotted key (`b.c = 1`) otherwise, and a nested
    /// inline table inside an inline one.
    ///
    /// # Errors
    ///
    /// As [`Document::try_single_level_mkdir_p`]; [`Error::Path`] for the
    /// requested pointer when the node the chain hangs from is no mapping,
    /// or a missing segment is the array-append marker `-`.
    fn try_mkdir_p(
        &mut self,
        pointer: &Pointer,
        canonical: &str,
        value: Value,
        layout: &LayoutCache,
    ) -> Result<()> {
        let segs = pointer.segments();
        let existing = existing_prefix_len(&self.value, pointer);
        if existing + 1 >= segs.len() {
            return self.try_single_level_mkdir_p(pointer, canonical, value, layout, None);
        }
        let missing_key = |doc: &Self| Error::Path {
            pointer: canonical.to_owned(),
            matched_prefix: longest_existing_value_prefix(&doc.value, pointer),
            kind: PathErrorKind::MissingKey,
            did_you_mean: Vec::new(),
        };
        let parent = Pointer::new(segs[..existing].to_vec());
        let keys: Option<Vec<String>> = segs[existing..]
            .iter()
            .map(|segment| match segment {
                Segment::Key(key) if key != "-" => Some(key.clone()),
                _ => None,
            })
            .collect();
        let (Some(keys), Ok(Value::Map(_))) = (keys, parent.resolve(&self.value)) else {
            return Err(missing_key(self));
        };
        let mut nested = Value::Map(IndexMap::new());
        set_value_at(
            &mut nested,
            &Pointer::new(segs[existing + 1..].to_vec()),
            value.clone(),
        )?;
        let first = Pointer::new(segs[..=existing].to_vec());
        let result = if self.format == FormatTag::Toml {
            match self.toml_header_mkdir_p(&parent, &keys, &value) {
                Some(inserted) => inserted.and_then(|()| self.adopt_reparse("set", canonical)),
                None => {
                    let (leaf, chain) = keys.split_last().expect("two keys or more");
                    let dotted = TomlDottedRenderer {
                        chain,
                        leaf,
                        value: &value,
                    };
                    self.try_single_level_mkdir_p(
                        &first,
                        &first.as_canonical(),
                        nested,
                        layout,
                        Some(&dotted),
                    )
                }
            }
        } else {
            self.try_single_level_mkdir_p(&first, &first.as_canonical(), nested, layout, None)
        };
        // Errors name the pointer asked for, not its first missing key.
        result.map_err(|err| match err {
            Error::Path { .. } => missing_key(self),
            Error::EditRejected { action, reason, .. } => Error::EditRejected {
                action,
                pointer: canonical.to_owned(),
                reason,
            },
            err => err,
        })
    }

    /// The `[header]` insertion of the TOML table chain `keys` (its last
    /// key is the leaf `value`'s) under the existing table `parent`, when
    /// the file writes its tables under headers and `parent` is the root
    /// or a table with a header of its own (no array item on the way):
    /// after the last section under `parent` — the end of the file for the
    /// root. `None` when a dotted key fits the file better; `Some(Err)`
    /// when the file does not parse.
    fn toml_header_mkdir_p(
        &mut self,
        parent: &Pointer,
        keys: &[String],
        value: &Value,
    ) -> Option<Result<()>> {
        let bytes = &self.original_bytes;
        let tables = crate::parsers::toml::toml_tables(bytes)?;
        let canonical = parent.as_canonical();
        let headers = tables
            .header_line_ends
            .keys()
            .any(|pointer| !pointer.rsplit('/').next().is_some_and(is_index));
        let named = parent
            .segments()
            .iter()
            .all(|segment| matches!(segment, Segment::Key(key) if !is_index(key)));
        if !headers
            || !named
            || !(parent.is_root() || tables.header_line_ends.contains_key(&canonical))
        {
            return None;
        }
        let prefix = format!("{canonical}/");
        let at = tables
            .sections
            .iter()
            .filter(|(pointer, _)| {
                parent.is_root() || *pointer == canonical || pointer.starts_with(&prefix)
            })
            .map(|(_, section)| section.end)
            .max()
            .unwrap_or(bytes.len());
        let (leaf, chain) = keys.split_last()?;
        let path: Vec<String> = parent
            .segments()
            .iter()
            .filter_map(|segment| match segment {
                Segment::Key(key) => Some(key.as_str()),
                Segment::Index(_) => None,
            })
            .chain(chain.iter().map(String::as_str))
            .map(crate::parsers::toml::toml_key_token)
            .collect();
        let mut out = Vec::new();
        if at > bom_end(bytes) && bytes[at - 1] != b'\n' {
            out.push(b'\n');
        }
        // A blank line before the header when the file puts one there.
        let blank_lines = blank_line_before_a_header(bytes);
        let after_blank = bytes[..at].ends_with(b"\n\n") || bytes[..at].ends_with(b"\n\r\n");
        if blank_lines && at > bom_end(bytes) && !after_blank {
            out.push(b'\n');
        }
        out.extend(format!("[{}]\n", path.join(".")).into_bytes());
        out.extend(crate::textual_edit::InsertionRenderer::render_insertion(
            &crate::parsers::toml::TomlInsertionRenderer,
            leaf,
            value,
            1,
            SpanContext::BlockMapValue,
        ));
        if blank_lines && at < bytes.len() {
            out.push(b'\n');
        }
        let out = with_line_ending_at(bytes, at, out);
        self.original_bytes.splice(at..at, out);
        Some(Ok(()))
    }

    /// Try to insert `value` at `pointer` when the pointer's parent exists
    /// and is either:
    ///
    /// 1. A non-empty map with at least one recorded sibling span (the
    ///    sibling-anchored branch); or
    /// 2. An empty map whose `{}` body has a recorded container span (the
    ///    empty-parent branch, added to close the RFC 7396 §2 conformance
    ///    gap around merging `{}` with `{"a":1}`).
    ///
    /// Falls back to `Error::Path { MissingKey }` on any unsupported shape.
    ///
    /// # Strategy
    ///
    /// 1. The parent path (pointer with its last segment dropped) must
    ///    resolve to a `Value::Map` in the tree — that's the only shape
    ///    this single-level baseline handles. Array mkdir-p (`/arr/3`
    ///    where `arr.len() == 3` is the boundary case) is intentionally
    ///    out of scope.
    /// 2. **Sibling-anchored**: locate the rightmost sibling span — the
    ///    existing scalar under the same parent whose `value_range.end` is
    ///    largest. That's our splice anchor. The new key is rendered via
    ///    the format's [`InsertionRenderer`] and spliced after the sibling
    ///    using [`format_specific_splice`].
    /// 3. **Empty-parent**: when no siblings exist but the parent's `{}` /
    ///    `[]` body has a recorded span (parsers record one for every
    ///    empty container), the new key is rendered inline and spliced
    ///    between the parent's `{` and `}` (or `[` and `]`) via
    ///    [`format_specific_empty_parent_splice`].
    /// 4. Refresh `original_bytes`, shift spans through `apply_delta`,
    ///    record a best-effort span for the new leaf, drop the
    ///    now-non-empty parent's container span, and rebuild the
    ///    provenance side-channel.
    ///
    /// # Errors
    ///
    /// `renderer` stands in for the format's [`InsertionRenderer`] (the
    /// TOML dotted chain of [`Document::try_mkdir_p`]).
    ///
    /// Returns `Error::Path { MissingKey }` whenever the single-level
    /// path cannot be taken (missing parent, array parent,
    /// format without an `InsertionRenderer`, or empty parent whose
    /// container span was not recorded by the parser). The pointer and
    /// `matched_prefix` mirror the original baseline so callers see the
    /// same structured diagnostic as before.
    fn try_single_level_mkdir_p(
        &mut self,
        pointer: &Pointer,
        canonical: &str,
        value: Value,
        layout: &LayoutCache,
        renderer: Option<&dyn crate::textual_edit::InsertionRenderer>,
    ) -> Result<()> {
        // Bail-out helper — every unsupported shape funnels through here so
        // callers continue to see the structured Path diagnostic they
        // already handle.
        let missing_key = || Error::Path {
            pointer: canonical.to_owned(),
            matched_prefix: longest_existing_value_prefix(&self.value, pointer),
            kind: PathErrorKind::MissingKey,
            did_you_mean: Vec::new(),
        };

        let Some(insertion_renderer) =
            renderer.or_else(|| insertion_renderer_for_format(self.format))
        else {
            // Read-only formats keep the legacy `MissingKey` diagnostic so
            // callers can still distinguish "format does not support
            // mkdir-p" from "wrong path" via the `Format::insertion_renderer`
            // capability check. XML, CSV, INI, .env, Dockerfile,
            // ignore-list and markdown body all land here.
            return Err(missing_key());
        };

        // Single level: the parent must resolve in the value tree and the
        // missing segment must be the last one; `try_mkdir_p` hands a
        // longer chain here as its first missing key.
        let segs = pointer.segments();
        if segs.is_empty() {
            return Err(missing_key());
        }
        let (last_seg, parent_segs) = segs.split_last().expect("segs non-empty");
        let parent_ptr = Pointer::new(parent_segs.to_vec());
        let parent_value = match parent_ptr.resolve(&self.value) {
            Ok(v) => v,
            Err(_) => return Err(missing_key()),
        };

        // The parent must be a Map — array mkdir-p needs different splice
        // arithmetic (think `arr/3` on an existing 3-element array) which
        // we defer to a follow-up.
        let Value::Map(parent_map) = parent_value else {
            return Err(missing_key());
        };

        // The leaf segment must be a map key (or an array-append marker on
        // a map context, which is meaningless). Reject anything else.
        let new_key = match last_seg {
            Segment::Key(k) if k != "-" => k.clone(),
            _ => return Err(missing_key()),
        };

        // Guard against duplicate-key edge case: if the value tree already
        // contains the key, the SpanMap should too — we shouldn't have
        // reached the mkdir-p branch. Returning MissingKey here is
        // conservative; in practice this branch is unreachable.
        if parent_map.contains_key(&new_key) {
            return Err(missing_key());
        }

        // Pick a splice anchor. Two acceptable shapes:
        //
        // 1. Sibling-anchored: parent has at least one direct child whose
        //    scalar span was recorded by the parser. Splice next to it.
        // 2. Empty-parent: parent is an empty map whose `{}` body the
        //    parser recorded as a container span. Splice between the
        //    `{` and `}`. This branch was added to close RFC 7396 §2 —
        //    merging `{}` with `{"a":1}` must yield `{"a":1}`.
        //
        // The two branches diverge enough that we route through different
        // helpers, but they share the renderer dispatch and the post-splice
        // span recompute so the contract is identical from the caller's
        // perspective.
        let parent_canonical = parent_ptr.as_canonical();
        let sibling_prefix = if parent_canonical.is_empty() {
            "/".to_owned()
        } else {
            format!("{parent_canonical}/")
        };
        let sibling = self.last_direct_child(&sibling_prefix, self.edit_layout(layout));
        let Some((sibling_key, mut sibling_span)) = sibling else {
            // No sibling — try the empty-parent path. The parent's `{}` /
            // `[]` body must have a recorded container span for this branch
            // to succeed; if it doesn't, fall through to MissingKey.
            //
            // Pre-materialize the structured error so the empty-parent
            // helper can take a borrow-free fallback (the closure
            // `missing_key` captures `self.value` via
            // `longest_existing_value_prefix` and so cannot coexist with
            // the `&mut self` we need for the splice).
            if self.format == FormatTag::Toml {
                // The root, or a `[table]` holding only tables: the new key
                // goes before the first header, or right under its own.
                let at = if parent_canonical.is_empty() {
                    crate::parsers::toml::toml_root_insertion_point(&self.original_bytes)
                } else {
                    crate::parsers::toml::toml_tables(&self.original_bytes)
                        .and_then(|tables| tables.header_line_ends.get(&parent_canonical).copied())
                };
                if let Some(at) = at {
                    return self.toml_table_insertion(
                        pointer,
                        &new_key,
                        value,
                        insertion_renderer,
                        at,
                    );
                }
                if parent_canonical.is_empty() {
                    return Err(Error::EditRejected {
                        action: "set",
                        pointer: pointer.as_canonical(),
                        reason: "the document does not parse".to_owned(),
                    });
                }
                // A table with no header: made by dotted keys or by the
                // headers of the tables under it.
                if let Some((at, inserted)) = self.toml_implicit_table_insertion(
                    &parent_ptr,
                    &new_key,
                    &value,
                    insertion_renderer,
                ) {
                    self.original_bytes.splice(at..at, inserted);
                    return self.adopt_reparse("set", canonical);
                }
            }
            let parent_is_empty_map = parent_map.is_empty();
            let fallback_err = missing_key();
            return self.try_empty_parent_mkdir_p(
                pointer,
                canonical,
                &parent_canonical,
                parent_is_empty_map,
                new_key,
                value,
                insertion_renderer,
                fallback_err,
            );
        };

        // Render the new key/value pair via the format's InsertionRenderer.
        // The renderers take `parent_indent` as the 1-indexed *key column*
        // of the parent's children (YAML's convention), or as the leading
        // indent count for JSON. The sibling's `indent` is the column of
        // the sibling's *value*, not its key, so we re-derive the key
        // column by scanning back through the sibling's line. A YAML value
        // can start lines below its key (a `|` / `>` block scalar's
        // content, a value on the next line); its `line_range` starts on
        // the key's line, so that is the line to scan.
        let key_line = if self.format == FormatTag::Yaml {
            sibling_span.line_range.start
        } else {
            sibling_span.value_range.start
        };
        let parent_children_indent = sibling_key_column(&self.original_bytes, key_line);
        if self.format == FormatTag::Yaml {
            if self.spans.contains_key(&sibling_key) {
                self.strip_yaml_chomping_before_eof_append(&sibling_key, &mut sibling_span);
            } else if sibling_span.line_range.end == self.original_bytes.len() {
                // A collection ending the document: its last scalar may be
                // a block scalar without a final line break.
                let prefix = format!("{sibling_key}/");
                if let Some((last_key, mut last_span)) = self
                    .spans
                    .iter()
                    .filter(|(key, _)| key.starts_with(&prefix))
                    .max_by_key(|(_, span)| span.value_range.end)
                    .map(|(key, span)| (key.clone(), span.clone()))
                {
                    self.strip_yaml_chomping_before_eof_append(&last_key, &mut last_span);
                    sibling_span.line_range.end = self.original_bytes.len();
                }
            }
        }
        let flow_layout = self.flow_layout(&sibling_span);
        // A YAML entry in a flow mapping is written on one line, one in a
        // block mapping as a block entry, whatever the sibling's own
        // context (an empty `[]` carries a flow one).
        let context = match (self.format, flow_layout) {
            (FormatTag::Yaml, Some(_)) => SpanContext::FlowMapValue,
            (FormatTag::Yaml, None) => SpanContext::BlockMapValue,
            _ => sibling_span.context,
        };
        let mut fragment =
            insertion_renderer.render_insertion(&new_key, &value, parent_children_indent, context);
        if self.format == FormatTag::Toml && flow_layout.is_none() {
            // A sibling written under a dotted key (`r0.k0 = 1`) has its
            // parent in the key, not in a header: the new key needs it too.
            let line_start = line_start(&self.original_bytes, sibling_span.value_range.start);
            if let Some(prefix) = toml_dotted_key_parent(&self.original_bytes[line_start..]) {
                let prefix = prefix.trim_ascii_start();
                let at = fragment.len() - fragment.trim_ascii_start().len();
                fragment.splice(at..at, prefix.iter().copied().chain(*b"."));
            }
        }

        // Compute the actual splice point + final bytes, format-aware so
        // we don't smear `\n`s or forget a `,` separator.
        let (splice_at, splice_bytes) = match flow_layout {
            Some(flow) => {
                let bare_tag = self
                    .edit_layout(layout)
                    .is_some_and(|layout| layout.bare_flow_tags.contains(&sibling_key));
                flow_sibling_splice(&fragment, &sibling_span, flow, bare_tag)
            }
            None => {
                format_specific_splice(self.format, &fragment, &sibling_span, &self.original_bytes)
            }
        };
        let splice_bytes = with_line_ending_at(&self.original_bytes, splice_at, splice_bytes);

        // Render the value through the scalar renderer so we can locate
        // its exact byte range inside the spliced bytes. Splicing the
        // whole fragment as the new pointer's `value_range` would break
        // idempotency: a follow-up `set_at(pointer, new_value)` would route
        // through the in-span replace path and the scalar renderer would
        // overwrite the syntax (commas, key, `:` separator) instead of just
        // the value bytes. The scalar renderer here is the SAME renderer
        // already wired for in-span replacements, so it produces the exact
        // byte sequence that ends the spliced region.
        let Some(scalar_renderer) = renderer_for_format(self.format) else {
            return Err(Error::WriteUnavailable {
                reason: format!(
                    "scalar renderer missing for format {}; insertion renderer is registered but scalar renderer is not",
                    self.format.name()
                ),
            });
        };
        let scalar_bytes =
            scalar_renderer.render_replacement(&value, sibling_span.context, &[] as &[u8]);
        let value_offset_in_splice = locate_value_in_fragment(&splice_bytes, &scalar_bytes);

        // Splice into original_bytes, shift downstream spans, record a
        // span for the new leaf, and refresh provenance.
        let inserted_len = splice_bytes.len();
        let delta = SpanRecomputeDelta {
            at: splice_at,
            old_len: 0,
            new_len: inserted_len,
        };
        self.original_bytes
            .splice(splice_at..splice_at, splice_bytes.iter().copied());
        apply_delta(&mut self.spans, delta);

        // Record the new leaf's span. Use the precise scalar byte range
        // (so a follow-up `set_at` routes through the in-span replace path
        // and only touches those bytes). `line_range` is the whole spliced
        // region, matching the parser convention that `line_range` covers
        // the full physical line — and the splice IS the new leaf's full
        // line (or sub-line for JSON appended to an existing line).
        let value_abs_start = splice_at + value_offset_in_splice;
        let value_abs_end = value_abs_start + scalar_bytes.len();
        let new_span = ValueSpan {
            value_range: value_abs_start..value_abs_end,
            line_range: splice_at..(splice_at + inserted_len),
            indent: parent_children_indent,
            context: sibling_span.context,
        };
        self.spans.insert(canonical.to_owned(), new_span);

        // Mutate the in-memory value tree so subsequent reads via `value()`
        // reflect the write (mirrors the in-span replace path).
        set_value_at(&mut self.value, pointer, value)?;

        // Re-sync the provenance side-channel.
        self.provenance = provenance_from_spans(&self.spans);
        Ok(())
    }

    /// Read the bytes as they are now — after a first step of a two-step
    /// edit — so the next step sees their spans and layout.
    ///
    /// # Errors
    ///
    /// [`Error::EditRejected`] when they do not parse.
    fn adopt_reparse(&mut self, action: &'static str, canonical: &str) -> Result<()> {
        match parse_again(self.format, &self.original_bytes) {
            Some(Ok(fresh)) => {
                self.value = fresh.value;
                self.spans = fresh.spans;
                self.provenance = fresh.provenance;
                Ok(())
            }
            Some(Err(err)) => {
                tracing::debug!(pointer = %canonical, action, %err, "edited bytes do not parse");
                Err(Error::EditRejected {
                    action,
                    pointer: canonical.to_owned(),
                    reason: "the edited document would not parse".to_owned(),
                })
            }
            None => Ok(()),
        }
    }

    /// `set` of `value` where a non-empty collection is, which the span
    /// map records no span for: a flow collection — a JSON value, a TOML
    /// inline table or array, a YAML `{…}` / `[…]` — is replaced between
    /// its brackets; a YAML block collection loses its children, leaving
    /// an empty node that the value then goes into; a TOML `[table]` or
    /// dotted table is deleted and the key written anew.
    ///
    /// The spans are left as they were: the read-back check that follows
    /// parses the bytes again.
    ///
    /// # Errors
    ///
    /// [`Error::EditRejected`] when the collection's bytes are not found.
    fn replace_collection(
        &mut self,
        pointer: &Pointer,
        canonical: &str,
        value: Value,
        layout: &LayoutCache,
        renderer: &dyn crate::textual_edit::ScalarRenderer,
    ) -> Result<()> {
        let rejected = || Error::EditRejected {
            action: "set",
            pointer: canonical.to_owned(),
            reason: "dq cannot find the bytes of the collection there".to_owned(),
        };
        let segments = pointer.segments();
        let parent = Pointer::new(segments[..segments.len().saturating_sub(1)].to_vec());
        let in_map = matches!(parent.resolve(&self.value), Ok(Value::Map(_)));
        let brackets = match self.format {
            FormatTag::Json => crate::parsers::json::json_container_ranges(&self.original_bytes)
                .ok()
                .and_then(|mut ranges| ranges.remove(canonical))
                .map(|range| (range, SpanContext::BlockMapValue, Vec::new())),
            FormatTag::Toml => crate::parsers::toml::toml_inline_collections(&self.original_bytes)
                .and_then(|mut ranges| ranges.remove(canonical))
                .map(|(range, in_flow)| {
                    let context = match (in_flow, in_map) {
                        (true, true) => SpanContext::FlowMapValue,
                        (true, false) => SpanContext::FlowSeqItem,
                        (false, _) => SpanContext::BlockMapValue,
                    };
                    (range, context, Vec::new())
                }),
            FormatTag::Yaml => {
                let collection = self
                    .edit_layout(layout)
                    .and_then(|layout| layout.containers.get(canonical))
                    .ok_or_else(rejected)?;
                if collection.top_level {
                    return Err(rejected());
                }
                if let Some(brackets) = collection.brackets.clone() {
                    // In a block parent the range runs on to the next
                    // token, past a comment.
                    let brackets = if collection.flow_value.is_some() {
                        brackets
                    } else {
                        brackets.start
                            ..yaml_flow_end(&self.original_bytes, brackets.clone())
                                .ok_or_else(rejected)?
                    };
                    let context = match (collection.flow_value.is_some(), in_map) {
                        (true, true) => SpanContext::FlowMapValue,
                        (true, false) => SpanContext::FlowSeqItem,
                        (false, true) => SpanContext::BlockMapValue,
                        (false, false) => SpanContext::BlockSeqItem,
                    };
                    if let Some(err) =
                        self.anchor_removal_refused(layout, &brackets, "set", canonical)
                    {
                        return Err(err);
                    }
                    // A `!!map` / `!!seq` tag before the brackets goes with
                    // them: it would retype the new value.
                    let tags = CoreTags::at(&self.original_bytes, brackets.start);
                    match core_tag_removal(&self.original_bytes, brackets.start, &tags) {
                        Some((props, kept)) => Some((
                            props.start..brackets.end,
                            context,
                            kept_properties(kept, &tags),
                        )),
                        None => Some((brackets, context, Vec::new())),
                    }
                } else {
                    // Its children go, with the comment lines among them
                    // and those after the last one indented as deep; the
                    // key or `-`, its properties and any comment on that
                    // line stay as an empty node.
                    let marker = collection.empty_marker_at.ok_or_else(rejected)?;
                    let first = collection.first_child_line.ok_or_else(rejected)?;
                    let end = self.edit_layout(layout).map_or(collection.end, |layout| {
                        self.yaml_trailing_comments_end(layout, canonical, collection.end)
                    });
                    let children = if first >= marker {
                        // From the line after the one the empty node is on:
                        // a comment line before the first child goes too.
                        past_line_break(&self.original_bytes, marker).min(first)..end
                    } else {
                        // `- key: …` / `- - …`: the first child shares the
                        // line with the `-`.
                        let mut end = end;
                        while end > marker && self.original_bytes[end - 1].is_ascii_whitespace() {
                            end -= 1;
                        }
                        marker..end
                    };
                    if let Some(err) =
                        self.anchor_removal_refused(layout, &children, "set", canonical)
                    {
                        return Err(err);
                    }
                    self.original_bytes.drain(children);
                    // A `!!map` / `!!seq` tag goes with the collection: on
                    // the empty node it would retype the new value. Before
                    // the children, so their removal leaves it in place.
                    let tags = CoreTags::at(&self.original_bytes, marker);
                    if let Some((props, kept)) =
                        core_tag_removal(&self.original_bytes, marker, &tags)
                    {
                        let kept = kept_properties(kept, &tags);
                        let kept = kept.trim_ascii_end();
                        let (mut from, mut to) = (props.start, props.end);
                        if kept.is_empty() {
                            while from > 0 && matches!(self.original_bytes[from - 1], b' ' | b'\t')
                            {
                                from -= 1;
                            }
                            // Properties on a line of their own: the line goes.
                            let line_end = past_line_break(&self.original_bytes, to);
                            if from == line_start(&self.original_bytes, from)
                                && self.original_bytes[to..line_end]
                                    .iter()
                                    .all(u8::is_ascii_whitespace)
                            {
                                to = line_end;
                            }
                        }
                        self.original_bytes.splice(from..to, kept.iter().copied());
                    }
                    self.adopt_reparse("set", canonical)?;
                    if !matches!(pointer.resolve(&self.value), Ok(Value::Null)) {
                        return Err(rejected());
                    }
                    return self.set_at_unchecked(pointer, value, &LayoutCache::new());
                }
            }
            _ => None,
        };
        if let Some((range, context, mut replacement)) = brackets {
            replacement.extend(renderer.render_replacement(&value, context, &[]));
            self.original_bytes.splice(range, replacement);
            return set_value_at(&mut self.value, pointer, value);
        }
        if self.format == FormatTag::Toml {
            let tables =
                crate::parsers::toml::toml_tables(&self.original_bytes).unwrap_or_default();
            let ranges = tables.ranges_of(canonical);
            let Some(first) = ranges.first().map(|range| range.start) else {
                return Err(rejected());
            };
            // A table written under a dotted key (`a.b.c = 1`): the value
            // goes where its first line was, under the key cut down to it.
            let dotted = tables
                .dotted_lines
                .iter()
                .find(|(_, line)| line.start == first)
                .and_then(|(at, line)| {
                    toml_dotted_key_prefix(&self.original_bytes[line.clone()], at, pointer)
                });
            // The line break of the file, which the removal may take all of.
            let line_break = LineBreak::at(&self.original_bytes, first);
            for range in ranges.into_iter().rev() {
                self.original_bytes.drain(range);
            }
            let emptied = !self.original_bytes.contains(&b'\n');
            self.adopt_reparse("set", canonical)?;
            if pointer.resolve(&self.value).is_ok() {
                return Err(rejected());
            }
            if let Some(mut line) = dotted {
                line.extend_from_slice(b" = ");
                line.extend(renderer.render_replacement(&value, SpanContext::BlockMapValue, &[]));
                line.push(b'\n');
                let line = with_line_break(line_break, line);
                self.original_bytes.splice(first..first, line);
                return self.adopt_reparse("set", canonical);
            }
            // A parent only the removed sections made (`[a.b]` alone makes
            // `a`) is gone with them: write the value at the first missing
            // ancestor, wrapped in the tables down to the key.
            let mut at = parent;
            let mut value = value;
            let mut key = segments.last().cloned();
            while !at.is_root() && at.resolve(&self.value).is_err() {
                let Some(Segment::Key(name)) = key.take() else {
                    return Err(rejected());
                };
                value = Value::Map(IndexMap::from([(name, value)]));
                key = at.pop_segment();
            }
            let mut target = at;
            target.push_segment(key.ok_or_else(rejected)?);
            let written = self
                .set_at_unchecked(&target, value, &LayoutCache::new())
                .map_err(|err| match err {
                    // `a` still stands, made by another `[a.…]` section:
                    // dq has no header to write the new key under.
                    Error::Path { .. } => Error::EditRejected {
                        action: "set",
                        pointer: canonical.to_owned(),
                        reason: "dq cannot write the key anew in a table with no header of its own"
                            .to_owned(),
                    },
                    err => err,
                });
            if written.is_ok() && emptied {
                // Every line break there is now is one the value brought.
                let bytes = std::mem::take(&mut self.original_bytes);
                self.original_bytes = with_line_break(line_break, bytes);
            }
            return written;
        }
        Err(rejected())
    }

    /// Insert `key = value` at `at` into a TOML table that has no key of
    /// its own to follow — the root, before its first table header or at
    /// the end of a file without one; a `[table]` right under its header.
    /// The read-back check settles the spans.
    fn toml_table_insertion(
        &mut self,
        pointer: &Pointer,
        new_key: &str,
        value: Value,
        insertion_renderer: &dyn crate::textual_edit::InsertionRenderer,
        at: usize,
    ) -> Result<()> {
        let mut inserted = Vec::new();
        if at > bom_end(&self.original_bytes) && self.original_bytes[at - 1] != b'\n' {
            inserted.push(b'\n');
        }
        inserted.extend(insertion_renderer.render_insertion(
            new_key,
            &value,
            1,
            SpanContext::BlockMapValue,
        ));
        let inserted = with_line_ending_at(&self.original_bytes, at, inserted);
        let delta = SpanRecomputeDelta {
            at,
            old_len: 0,
            new_len: inserted.len(),
        };
        self.original_bytes.splice(at..at, inserted);
        apply_delta(&mut self.spans, delta);
        self.spans.shift_remove("");
        set_value_at(&mut self.value, pointer, value)
    }

    /// Empty-parent splice: insert `value` at `pointer` when the parent
    /// container is empty (`{}` / `[]`) and has a recorded container span
    /// in `self.spans`.
    ///
    /// The parsers (JSON, YAML, TOML) record a [`ValueSpan`] at the empty
    /// container's own pointer specifically so this path can fire. The
    /// recorded span's `value_range` covers the literal `{}` / `[]`
    /// (or in YAML's block-empty form `parent:` followed by no children),
    /// and we splice the new key-value pair inside the delimiters via
    /// [`format_specific_empty_parent_splice`].
    ///
    /// `parent_is_empty_map` is the in-memory tree's view (`parent_map.is_empty()`)
    /// passed in by the caller — we trust that over the SpanMap for the
    /// emptiness decision because the SpanMap could in theory still carry
    /// a stale empty-container entry after an out-of-tree mutation.
    ///
    /// Returns `Err(missing_key())` when:
    /// - the parent is not actually empty in the in-memory tree, OR
    /// - the parent's container span is missing from `self.spans`
    ///   (read-only format, or parser that doesn't record empty
    ///   containers).
    #[allow(clippy::too_many_arguments)]
    fn try_empty_parent_mkdir_p(
        &mut self,
        pointer: &Pointer,
        canonical: &str,
        parent_canonical: &str,
        parent_is_empty_map: bool,
        new_key: String,
        value: Value,
        insertion_renderer: &dyn crate::textual_edit::InsertionRenderer,
        fallback_err: Error,
    ) -> Result<()> {
        if !parent_is_empty_map {
            return Err(fallback_err);
        }
        // The parser must have recorded a container span for this empty
        // parent. If it didn't (read-only format, or a multi-level missing
        // chain where the parent itself doesn't exist), fall through.
        let Some(parent_span) = self.spans.get(parent_canonical).cloned() else {
            return Err(fallback_err);
        };

        // Render the new key/value pair. The insertion-renderer trait
        // interprets `parent_indent` differently per format (see each
        // impl's docs), so we precompute the right column-vs-space
        // representation here:
        //
        // - JSON: parent's leading-space count. `parent_span.indent` is
        //   the 1-indexed column of `{`, so we subtract 1.
        // - YAML: 1-indexed column of children's keys. The flow-form
        //   splicer ignores leading whitespace anyway, so `indent + 2`
        //   is a safe default; the block-form path inherits it as
        //   children indent.
        // - TOML: ignored by [`TomlInsertionRenderer`].
        let json_indent = u32::try_from(json_line_indent(
            &self.original_bytes,
            parent_span.value_range.start,
        ))
        .unwrap_or(u32::MAX);
        let renderer_indent = match self.format {
            FormatTag::Json => json_indent,
            FormatTag::Yaml => parent_span.indent.saturating_add(2),
            _ => parent_span.indent,
        };
        let children_indent = parent_span.indent.saturating_add(2);
        // A YAML entry inside `{}` is written on one line.
        let context = match self.format {
            FormatTag::Yaml
                if self.original_bytes.get(parent_span.value_range.start) == Some(&b'{') =>
            {
                SpanContext::FlowMapValue
            }
            FormatTag::Yaml => SpanContext::BlockMapValue,
            _ => parent_span.context,
        };
        let fragment =
            insertion_renderer.render_insertion(&new_key, &value, renderer_indent, context);

        // In a JSON container written on one line the new entry is written
        // on that line too; the root `{}` is that container itself.
        let one_line = self.format == FormatTag::Json
            && match parent_canonical.rfind('/') {
                Some(cut) => crate::parsers::json::json_container_ranges(&self.original_bytes)
                    .ok()
                    .and_then(|ranges| ranges.get(&parent_canonical[..cut]).cloned()),
                None => Some(parent_span.value_range.clone()),
            }
            .and_then(|range| self.original_bytes.get(range))
            .is_some_and(|holder| !holder.iter().any(|&b| matches!(b, b'\n' | b'\r')));
        let (splice_at, splice_bytes) = format_specific_empty_parent_splice(
            self.format,
            &fragment,
            &parent_span,
            &self.original_bytes,
            one_line,
        );
        let splice_bytes = with_line_ending_at(&self.original_bytes, splice_at, splice_bytes);

        // Resolve the scalar renderer so we can locate the value bytes
        // inside the spliced fragment — mirrors the sibling-anchored path
        // so a follow-up `set_at(pointer, ...)` routes through the
        // in-span replace path.
        let Some(scalar_renderer) = renderer_for_format(self.format) else {
            return Err(Error::WriteUnavailable {
                reason: format!(
                    "scalar renderer missing for format {}; insertion renderer is registered but scalar renderer is not",
                    self.format.name()
                ),
            });
        };
        let scalar_bytes =
            scalar_renderer.render_replacement(&value, parent_span.context, &[] as &[u8]);
        let value_offset_in_splice = locate_value_in_fragment(&splice_bytes, &scalar_bytes);

        let inserted_len = splice_bytes.len();
        let delta = SpanRecomputeDelta {
            at: splice_at,
            old_len: 0,
            new_len: inserted_len,
        };
        self.original_bytes
            .splice(splice_at..splice_at, splice_bytes.iter().copied());
        apply_delta(&mut self.spans, delta);

        // The parent's container span is now stale (it covered the
        // `{}` body; after the splice the body is non-empty and the
        // recorded range no longer carries the `..key: value..` content).
        // Drop it so subsequent reads through `span_at(parent_ptr)` don't
        // hand out a span that points at bytes that have shifted under
        // them. The in-memory tree update below restores the parent's
        // entry as a Map.
        self.spans.shift_remove(parent_canonical);

        // Record the new leaf's span — same shape as the sibling-anchored
        // path so idempotency holds.
        let value_abs_start = splice_at + value_offset_in_splice;
        let value_abs_end = value_abs_start + scalar_bytes.len();
        let new_span = ValueSpan {
            value_range: value_abs_start..value_abs_end,
            line_range: splice_at..(splice_at + inserted_len),
            indent: children_indent,
            context: parent_span.context,
        };
        self.spans.insert(canonical.to_owned(), new_span);

        set_value_at(&mut self.value, pointer, value)?;
        self.provenance = provenance_from_spans(&self.spans);
        Ok(())
    }

    /// Find the rightmost direct-child span under `sibling_prefix`
    /// (e.g. `"/spec/"` to look at `/spec`'s direct children), ranked by
    /// descending `value_range.end`. Returns `None` when no direct child
    /// has a recorded span.
    ///
    /// Direct children only: keys like `/spec/nested/deep` are skipped so
    /// the splice anchors against an actual sibling at the parent's depth
    /// rather than a deeply nested grand-child whose `value_range` could
    /// be inside an entirely different container.
    /// How a new entry goes next to `sibling` when they share a flow
    /// collection — or a JSON value, which is all flow — rather than a
    /// block, where it takes a line of its own. `None` for a block sibling.
    fn flow_layout(&self, sibling: &ValueSpan) -> Option<FlowLayout> {
        match self.format {
            FormatTag::Json => {
                let bytes = &self.original_bytes;
                let start = &sibling.value_range.start;
                // The entry's start: its key when it has one.
                let sep = skip_blanks_back(bytes, *start);
                let entry = if sep > 0 && bytes[sep - 1] == b':' {
                    flow_key_start(bytes, skip_blanks_back(bytes, sep - 1), self.format)?
                } else {
                    *start
                };
                let mut before = entry;
                while before > 0 && matches!(bytes[before - 1], b' ' | b'\t') {
                    before -= 1;
                }
                let shares_line = before > 0 && matches!(bytes[before - 1], b',' | b'{' | b'[');
                Some(if shares_line {
                    FlowLayout::SameLine
                } else {
                    FlowLayout::OwnLine {
                        column: entry - before,
                        crlf: before >= 2 && bytes[before - 2] == b'\r',
                    }
                })
            }
            // An empty `{}` / `[]` carries a flow context of its own even
            // as a block entry: the token after the sibling tells.
            FormatTag::Yaml | FormatTag::Toml => (matches!(
                sibling.context,
                SpanContext::FlowMapValue | SpanContext::FlowSeqItem
            ) && matches!(
                self.original_bytes.get(skip_blanks_and_comments(
                    &self.original_bytes,
                    sibling.value_range.end
                )),
                Some(b',' | b'}')
            ))
            .then_some(FlowLayout::SameLine),
            _ => None,
        }
    }

    /// The last direct child of the map whose children's pointers start
    /// with `sibling_prefix` — the one a new key goes after — and its span.
    /// A non-empty collection has no recorded span: its span is made up of
    /// its entry's bytes (YAML) or its brackets (JSON), with `value_range`
    /// empty at its end for a YAML block entry.
    fn last_direct_child(
        &self,
        sibling_prefix: &str,
        layout: Option<&crate::parsers::yaml_spans::EditLayout>,
    ) -> Option<(String, ValueSpan)> {
        let direct = |pointer: &str| {
            pointer
                .strip_prefix(sibling_prefix)
                .is_some_and(|tail| !tail.contains('/'))
        };
        let scalar = || {
            self.rightmost_direct_sibling(sibling_prefix)
                .map(|(key, span)| (key.to_owned(), span.clone()))
        };
        match self.format {
            FormatTag::Yaml => {
                let Some(layout) = layout else {
                    return scalar();
                };
                let last_entry = layout
                    .entries
                    .iter()
                    .filter(|(pointer, _)| direct(pointer))
                    .max_by_key(|(_, entry)| entry.at);
                if let Some((pointer, entry)) = last_entry {
                    if let Some(span) = self.spans.get(pointer) {
                        return Some((pointer.clone(), span.clone()));
                    }
                    let collection = layout.containers.get(pointer)?;
                    let line_start = line_start(&self.original_bytes, entry.at);
                    let end = collection.end;
                    return Some((
                        pointer.clone(),
                        ValueSpan {
                            value_range: end..end,
                            line_range: line_start..end,
                            indent: 1,
                            context: SpanContext::BlockMapValue,
                        },
                    ));
                }
                // A flow mapping: the child that starts last.
                let flow = layout
                    .containers
                    .iter()
                    .filter(|(pointer, _)| direct(pointer))
                    .filter_map(|(pointer, collection)| {
                        Some((pointer.clone(), collection.flow_value.clone()?))
                    })
                    .max_by_key(|(_, value)| value.start);
                last_by_start(scalar(), flow, SpanContext::FlowMapValue)
            }
            FormatTag::Json => {
                let ranges = crate::parsers::json::json_container_ranges(&self.original_bytes)
                    .unwrap_or_default();
                let collection = ranges
                    .into_iter()
                    .filter(|(pointer, _)| direct(pointer))
                    .max_by_key(|(_, value)| value.start);
                last_by_start(scalar(), collection, SpanContext::BlockMapValue)
            }
            FormatTag::Toml => {
                let collection =
                    crate::parsers::toml::toml_inline_collections(&self.original_bytes)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|(pointer, _)| direct(pointer))
                        .max_by_key(|(_, (value, _))| value.start);
                // One inside an inline table is a flow entry.
                let context = match collection {
                    Some((_, (_, true))) => SpanContext::FlowMapValue,
                    _ => SpanContext::BlockMapValue,
                };
                let collection = collection.map(|(pointer, (value, _))| (pointer, value));
                last_by_start(scalar(), collection, context)
            }
            _ => scalar(),
        }
    }

    fn rightmost_direct_sibling(&self, sibling_prefix: &str) -> Option<(&str, &ValueSpan)> {
        self.spans
            .iter()
            .filter(|(k, _)| k.starts_with(sibling_prefix))
            .filter(|(k, _)| {
                // Exclude grand-children: a direct sibling's pointer string
                // has no further `/` after stripping the parent prefix.
                // An empty tail is the empty key `""`.
                let tail = &k.as_str()[sibling_prefix.len()..];
                !tail.contains('/')
            })
            .max_by_key(|(_, span)| span.value_range.end)
            .map(|(k, span)| (k.as_str(), span))
    }

    /// Before a YAML insertion after `sibling` at the end of a document that
    /// lacks a final line break: when the sibling is a `|` / `>` block
    /// scalar running to the end, switch its header to strip chomping so the
    /// line break the insertion adds does not become part of its value.
    /// Updates `sibling` (and its entry in `self.spans`) to the shifted
    /// bytes.
    fn strip_yaml_chomping_before_eof_append(
        &mut self,
        sibling_key: &str,
        sibling: &mut ValueSpan,
    ) {
        let Some((at, old_len, replacement)) =
            crate::parsers::yaml_spans::strip_chomping_before_eof_append(
                &self.original_bytes,
                sibling,
            )
        else {
            return;
        };
        self.original_bytes
            .splice(at..at + old_len, replacement.iter().copied());
        apply_delta(
            &mut self.spans,
            SpanRecomputeDelta {
                at,
                old_len,
                new_len: replacement.len(),
            },
        );
        // `apply_delta` moved the whole span, but its entry still starts on
        // the key's line, before the header.
        if let Some(span) = self.spans.get_mut(sibling_key) {
            span.line_range.start = sibling.line_range.start;
            *sibling = span.clone();
        }
    }

    /// Remove the value at `pointer`, splicing out its physical line(s) and
    /// dropping every span beneath it.
    ///
    /// YAML adds three cases the span map alone cannot serve, all from one
    /// re-parse of the current bytes ([`crate::parsers::yaml_spans::edit_layout`]):
    ///
    /// - a collection value outside flow collections is removed with its
    ///   whole entry — the key (or `-`) line down to the collection's last
    ///   line;
    /// - a block collection left without children gets an explicit `{}` /
    ///   `[]` (`key: {}`), so it stays a collection instead of reading back
    ///   as `null`;
    /// - removing bytes that define an anchor an alias outside them refers
    ///   to is refused: the alias would dangle, or resolve to an earlier
    ///   definition of the same name.
    ///
    /// # Errors
    ///
    /// - [`Error::WriteUnavailable`] when the document was loaded
    ///   read-only.
    /// - [`Error::Path`] with `kind = TypeMismatch` when `pointer` is
    ///   the root (the root is never deletable — it would empty the file).
    /// - [`Error::Path`] with `kind = MissingKey` when no span exists for
    ///   the pointer (and, for YAML, it is no removable collection).
    /// - [`Error::EditRejected`] when the YAML bytes to remove define an
    ///   anchor that an alias outside them refers to, or when the edited
    ///   bytes fail the read-back check [`Document::set_at`] describes.
    pub fn del_at(&mut self, pointer: &Pointer) -> Result<()> {
        self.with_lf_line_breaks("delete", pointer, |doc| {
            // A document of a stream goes with its `---` line.
            if let Some(index) = doc.stream_document_index(pointer) {
                return doc.remove_stream_document(index);
            }
            let before = doc.clone();
            let layout = LayoutCache::new();
            let result = doc.del_at_unchecked(pointer, &layout);
            doc.check_edit(before, result, "delete", pointer, &layout, delete_value_at)
        })
    }

    /// Finish an edit of `pointer` whose unchecked step returned `result`:
    /// on success, parse the edited bytes again and compare them with
    /// `before`'s tree after `apply` at `pointer` and, in YAML, at every
    /// alias sharing the edited node. Any failure restores `before`.
    fn check_edit(
        &mut self,
        before: Self,
        result: Result<()>,
        action: &'static str,
        pointer: &Pointer,
        layout: &LayoutCache,
        apply: impl Fn(&mut Value, &Pointer) -> Result<()>,
    ) -> Result<()> {
        if let Err(err) = result {
            *self = before;
            let err = match err {
                // A path `get` finds that runs through an alias: the alias
                // node has no bytes of its own below it to edit.
                Error::Path { .. } => self.through_alias(pointer, layout, action).unwrap_or(err),
                err => err,
            };
            return Err(match err {
                // Every refusal names the way around it.
                Error::EditRejected {
                    action,
                    pointer,
                    reason,
                } if !reason.contains(JQ_REWRITE_HINT)
                    && reason != JSON_NON_FINITE
                    && !reason.starts_with(TOML_BIG_NUMBER)
                    && !reason.starts_with(TOML_ROOT_TABLE) =>
                {
                    Error::EditRejected {
                        action,
                        pointer,
                        reason: format!("{reason}; {JQ_REWRITE_HINT}"),
                    }
                }
                err => err,
            });
        }
        let canonical = pointer.as_canonical();
        // No textual-edit renderer: the unchecked step refused already.
        let Some(reparsed) = parse_again(self.format, &self.original_bytes) else {
            return Ok(());
        };
        let reason = match reparsed {
            Ok(fresh) => {
                // The edit alone first: only when that does not read back
                // does the YAML alias layout, a parse of its own, get
                // worked out to expect the edit under the aliases too.
                let compare = |aliases: Option<&LayoutCache>| {
                    before
                        .expected_after_edit(pointer, aliases, &apply)
                        .map(|tree| first_difference(&tree, &fresh.value, ""))
                };
                let mut difference = compare(None);
                if difference != Some(None)
                    && before.format == FormatTag::Yaml
                    && before.original_bytes.contains(&b'*')
                {
                    difference = compare(Some(layout));
                }
                match difference {
                    Some(None) => {
                        self.value = fresh.value;
                        self.spans = fresh.spans;
                        self.provenance = fresh.provenance;
                        return Ok(());
                    }
                    Some(Some(at)) => {
                        tracing::debug!(
                            pointer = %canonical,
                            action,
                            difference = %at,
                            "edited bytes read back as a different tree",
                        );
                        // An array that lost or gained an item reports
                        // itself, the edited value's parent.
                        let parent = canonical.rsplit_once('/').map_or("", |(parent, _)| parent);
                        let at_the_edit = at == canonical
                            || at.starts_with(&format!("{canonical}/"))
                            || at == parent;
                        if at_the_edit && action == "set" {
                            "dq cannot write this value here as a textual edit"
                        } else if at_the_edit {
                            "dq cannot remove it as a textual edit"
                        } else {
                            "the edit would change other parts of the document"
                        }
                    }
                    None => {
                        tracing::debug!(
                            pointer = %canonical,
                            action,
                            "no expected tree: the edit does not apply to every alias",
                        );
                        "the edit would change other parts of the document"
                    }
                }
            }
            Err(err) => {
                tracing::debug!(pointer = %canonical, action, %err, "edited bytes do not parse");
                "the edited document would not parse"
            }
        };
        *self = before;
        Err(Error::EditRejected {
            action,
            pointer: canonical,
            reason: format!("{reason}; {JQ_REWRITE_HINT}"),
        })
    }

    /// The refusal of an edit that removes the YAML bytes `removed` when
    /// they define an anchor an alias outside them refers to: the alias
    /// would dangle, or resolve to an earlier definition of the name.
    fn anchor_removal_refused(
        &self,
        cache: &LayoutCache,
        removed: &std::ops::Range<usize>,
        action: &'static str,
        canonical: &str,
    ) -> Option<Error> {
        if !self.original_bytes.contains(&b'*') {
            return None;
        }
        let alias = self.edit_layout(cache)?.aliases.iter().find(|alias| {
            removed.contains(&alias.anchor_at) && !removed.contains(&alias.alias_at)
        })?;
        let line = 1 + self.original_bytes[..alias.alias_at]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        Some(Error::EditRejected {
            action,
            pointer: canonical.to_owned(),
            reason: format!("it holds an anchor that the alias on line {line} refers to"),
        })
    }

    /// The refusal of an edit at `pointer`, below a YAML alias, whose
    /// target (or, for `set`, its parent) the value tree resolves — the
    /// bytes there are the anchored node's.
    fn through_alias(
        &self,
        pointer: &Pointer,
        layout: &LayoutCache,
        action: &'static str,
    ) -> Option<Error> {
        if self.format != FormatTag::Yaml || !self.original_bytes.contains(&b'*') {
            return None;
        }
        let canonical = pointer.as_canonical();
        // `set` may add a key; `del` needs one there.
        let mut found = pointer.clone();
        if action != "delete" {
            found.pop_segment()?;
        }
        found.resolve(&self.value).ok()?;
        let alias = self.edit_layout(layout)?.aliases.iter().find(|alias| {
            alias.alias_pointer.as_deref().is_some_and(|at| {
                canonical
                    .strip_prefix(at)
                    .is_some_and(|rest| rest.starts_with('/'))
            })
        })?;
        let line = 1 + self.original_bytes[..alias.alias_at]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        let copied = match &alias.anchor_pointer {
            Some(at) if at.is_empty() => " of the root".to_owned(),
            Some(at) => format!(" of {at}"),
            None => String::new(),
        };
        Some(Error::EditRejected {
            action,
            pointer: canonical,
            reason: format!(
                "it lies under the alias on line {line}, a copy{copied}; \
                 an edit at the anchored node changes every copy"
            ),
        })
    }

    /// The value tree after applying an edit (`apply`) at `pointer` and,
    /// with the YAML alias `layout`, at the copy of it under every alias
    /// of an anchored node at or above it — nested aliases included.
    /// `None` if `apply` fails at one of them.
    fn expected_after_edit(
        &self,
        pointer: &Pointer,
        layout: Option<&LayoutCache>,
        apply: &impl Fn(&mut Value, &Pointer) -> Result<()>,
    ) -> Option<Value> {
        let mut targets = vec![pointer.as_canonical()];
        if let Some(layout) = layout {
            let aliases = self
                .edit_layout(layout)
                .map(|layout| layout.aliases.as_slice())
                .unwrap_or_default();
            // Alias pointers by the pointer of the anchored node they copy.
            let mut copies: std::collections::HashMap<&str, Vec<&str>> =
                std::collections::HashMap::new();
            for alias in aliases {
                if let (Some(alias_at), Some(anchor_at)) =
                    (&alias.alias_pointer, &alias.anchor_pointer)
                {
                    copies
                        .entry(anchor_at.as_str())
                        .or_default()
                        .push(alias_at.as_str());
                }
            }
            let mut seen: std::collections::HashSet<String> = targets.iter().cloned().collect();
            let mut i = 0;
            while i < targets.len() {
                let target = targets[i].clone();
                // Every node at or above the target: cut at each `/`.
                let cuts = target.match_indices('/').map(|(at, _)| at);
                for cut in cuts.chain(std::iter::once(target.len())) {
                    let (anchor_at, rest) = target.split_at(cut);
                    for alias_at in copies.get(anchor_at).into_iter().flatten() {
                        let copy = format!("{alias_at}{rest}");
                        if seen.insert(copy.clone()) {
                            targets.push(copy);
                        }
                    }
                }
                i += 1;
            }
        }
        let mut tree = self.value.clone();
        for target in &targets {
            let target = Pointer::parse(target).ok()?;
            apply(&mut tree, &target).ok()?;
        }
        Some(tree)
    }

    /// [`Document::del_at`] without the read-back check.
    fn del_at_unchecked(&mut self, pointer: &Pointer, cache: &LayoutCache) -> Result<()> {
        if renderer_for_format(self.format).is_none() {
            return Err(self.no_textual_edits("delete a value from it"));
        }
        // Read-only parsers go through `value_only` / `multi_value_only`.
        if !self.write_aware {
            return Err(Error::WriteUnavailable {
                reason: format!(
                    "{} document was loaded read-only; reload via a write-aware parser to enable del",
                    self.format.name(),
                ),
            });
        }
        if pointer.is_root() {
            return Err(Error::Path {
                pointer: pointer.as_canonical(),
                matched_prefix: String::new(),
                kind: PathErrorKind::DeleteRoot,
                did_you_mean: Vec::new(),
            });
        }
        let canonical = pointer.as_canonical();
        let parent_ptr = Pointer::new(pointer.segments()[..pointer.segments().len() - 1].to_vec());
        let target_is_collection = matches!(
            pointer.resolve(&self.value),
            Ok(Value::Map(_) | Value::Array(_))
        );
        let parent_loses_last_child = match parent_ptr.resolve(&self.value) {
            Ok(Value::Map(map)) => map.len() == 1,
            Ok(Value::Array(items)) => items.len() == 1,
            _ => false,
        };
        // The YAML layout is a parse of its own: work it out only for a
        // collection, an entry that may share its line with a `-` (or a
        // `?` / `:` indicator), and an emptied parent. A line opening with
        // a key cannot hold a compact entry.
        let layout_needed = self.format == FormatTag::Yaml
            && (target_is_collection
                || parent_loses_last_child
                || self.spans.get(&canonical).is_none_or(|span| {
                    matches!(
                        span.context,
                        SpanContext::FlowMapValue | SpanContext::FlowSeqItem
                    ) || self.original_bytes[span.line_range.start..]
                        .iter()
                        .find(|&&b| b != b' ' && b != b'\t')
                        .is_none_or(|b| matches!(b, b'-' | b'?' | b':'))
                }));
        let layout = if layout_needed {
            self.edit_layout(cache)
        } else {
            None
        };
        let in_map = matches!(parent_ptr.resolve(&self.value), Ok(Value::Map(_)));
        // The one key of a mapping written in a flow sequence without
        // braces (`[a: 1]`, `[? a]`): the mapping is left as `{}`.
        let parent_canonical = parent_ptr.as_canonical();
        if let Some(layout) = layout.as_ref()
            && layout
                .containers
                .get(&parent_canonical)
                .is_some_and(|parent| parent.implicit_pair)
            && let Some(mut pair) = layout.flow_entries.get(&parent_canonical).cloned()
        {
            // The blanks of an empty value go with it (`[a: ]`).
            let blanks = self.original_bytes[pair.end..]
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count();
            if self.original_bytes[..pair.end].ends_with(b":")
                && matches!(
                    self.original_bytes.get(pair.end + blanks),
                    Some(b',' | b']')
                )
            {
                pair.end += blanks;
            }
            if let Some(err) = self.anchor_removal_refused(cache, &pair, "delete", &canonical) {
                return Err(err);
            }
            self.original_bytes.splice(pair, b"{}".iter().copied());
            delete_value_at(&mut self.value, pointer)?;
            // The read-back check takes the spans of the edited bytes.
            return Ok(());
        }
        // What takes the place of the removed bytes: `{}` / `[]` for the
        // only entry of a compact collection.
        let mut inserted: Vec<u8> = Vec::new();
        let compact_entry = match layout.as_ref() {
            Some(layout) => self.compact_entry_removal(
                layout,
                &canonical,
                &parent_ptr,
                target_is_collection,
                parent_loses_last_child,
            )?,
            None => None,
        };
        if compact_entry.is_some() && parent_loses_last_child {
            inserted = if in_map { b"{}" } else { b"[]" }.to_vec();
        }
        // An entry of a YAML flow collection, by where the parse put it and
        // its siblings.
        let yaml_flow_entry = layout.as_ref().and_then(|layout| {
            let children = match parent_ptr.resolve(&self.value) {
                Ok(Value::Map(map)) => map.len(),
                Ok(Value::Array(items)) => items.len(),
                _ => return None,
            };
            yaml_flow_entry_removal(
                &self.original_bytes,
                layout,
                &canonical,
                &parent_ptr.as_canonical(),
                children,
            )
        });
        let collection = layout
            .as_ref()
            .filter(|_| target_is_collection)
            .and_then(|layout| layout.containers.get(&canonical));
        let collection_entry = collection.and_then(|collection| {
            let entry = collection.entry.clone().map(|entry| match layout.as_ref() {
                // With the comment lines inside it after its last entry.
                Some(layout) if collection.block => {
                    entry.start..self.yaml_trailing_comments_end(layout, &canonical, entry.end)
                }
                _ => entry,
            });
            entry.or_else(|| {
                let value = collection.flow_value.as_ref()?;
                match layout.as_ref()?.flow_entries.get(&canonical) {
                    Some(entry) => {
                        flow_entry_removal_from(&self.original_bytes, entry.start, value)
                    }
                    None => flow_entry_removal(&self.original_bytes, value, in_map, self.format),
                }
            })
        });
        // JSON records no span for a non-empty collection.
        let json_collection_entry = (self.format == FormatTag::Json && target_is_collection)
            .then(|| crate::parsers::json::json_container_ranges(&self.original_bytes).ok())
            .flatten()
            .and_then(|mut ranges| ranges.remove(&canonical))
            .and_then(|value| {
                flow_entry_removal(&self.original_bytes, &value, in_map, self.format)
            });
        // Nor does TOML for a non-empty inline table or array: inside
        // another one it is a flow entry, otherwise its `key = …` lines.
        let toml_collection_entry = (self.format == FormatTag::Toml && target_is_collection)
            .then(|| crate::parsers::toml::toml_inline_collections(&self.original_bytes))
            .flatten()
            .and_then(|mut ranges| ranges.remove(&canonical))
            .and_then(|(value, in_flow)| {
                if in_flow {
                    flow_entry_removal(&self.original_bytes, &value, in_map, self.format)
                } else {
                    let line_start = line_start(&self.original_bytes, value.start);
                    Some(line_start..past_line_break(&self.original_bytes, value.end))
                }
            });
        // A TOML `[table]`, `[[array]]` item or dotted table: its sections
        // and lines, wherever they are.
        if self.format == FormatTag::Toml
            && target_is_collection
            && toml_collection_entry.is_none()
            && let Some(tables) = crate::parsers::toml::toml_tables(&self.original_bytes)
            && let ranges = tables.ranges_of(&canonical)
            && let Some(first) = ranges.first().map(|range| range.start)
        {
            // How a parent made only by the removed bytes is written again
            // where they began, in the line ending of the file as it is
            // before they go.
            let respelled = (parent_loses_last_child && in_map)
                .then(|| self.toml_emptied_table(&tables, first, &parent_ptr))
                .flatten()
                .map(|line| with_line_ending_at(&self.original_bytes, first, line));
            let line_break = LineBreak::at(&self.original_bytes, first);
            for range in ranges.into_iter().rev() {
                self.original_bytes.drain(range);
            }
            self.adopt_reparse("delete", &canonical)?;
            // The last `[[array]]` item, or the last table under a table
            // with no header of its own: the parent goes with it unless
            // written as `key = []` / `key = {}`.
            if parent_loses_last_child && parent_ptr.resolve(&self.value).is_err() {
                if let Some(line) = respelled {
                    self.original_bytes.splice(first..first, line);
                    return self.adopt_reparse("delete", &canonical);
                }
                let empty = if in_map {
                    Value::Map(IndexMap::new())
                } else {
                    Value::Array(Vec::new())
                };
                // A file left with no line break cannot tell the new line
                // how to end: it ends as the file's did.
                let no_line_break_left = !self.original_bytes.contains(&b'\n');
                self.set_at_unchecked(&parent_ptr, empty, &LayoutCache::new())
                    .map_err(|_| Error::EditRejected {
                        action: "delete",
                        pointer: canonical.clone(),
                        reason: "dq cannot keep the table it empties".to_owned(),
                    })?;
                if no_line_break_left && line_break != LineBreak::Lf {
                    let bytes = std::mem::take(&mut self.original_bytes);
                    self.original_bytes = with_line_break(line_break, bytes);
                    return self.adopt_reparse("delete", &canonical);
                }
                return Ok(());
            }
            return Ok(());
        }
        // A dotted key inside an inline table (`{x."y z".w = 1}`, `/x`):
        // the entry of its one value.
        let toml_dotted_entry = (self.format == FormatTag::Toml
            && target_is_collection
            && toml_collection_entry.is_none())
        .then(|| {
            let prefix = format!("{canonical}/");
            let mut leaves = self.spans.iter().filter(|(key, span)| {
                key.starts_with(&prefix) && span.context == SpanContext::FlowMapValue
            });
            match (leaves.next(), leaves.next()) {
                (Some((_, leaf)), None) => {
                    flow_entry_removal(&self.original_bytes, &leaf.value_range, true, self.format)
                }
                _ => None,
            }
        })
        .flatten();
        let dotted_target = toml_dotted_entry.is_some();
        // The value of a TOML entry inside an inline table.
        let toml_flow_value = (self.format == FormatTag::Toml && !dotted_target)
            .then(|| match self.spans.get(&canonical) {
                Some(span) => {
                    (span.context == SpanContext::FlowMapValue).then(|| span.value_range.clone())
                }
                None => crate::parsers::toml::toml_inline_collections(&self.original_bytes)
                    .and_then(|mut ranges| ranges.remove(&canonical))
                    .and_then(|(value, in_flow)| in_flow.then_some(value)),
            })
            .flatten();
        let collection_entry = compact_entry
            .or(yaml_flow_entry.clone().filter(|_| target_is_collection))
            .or(collection_entry)
            .or(json_collection_entry)
            .or(toml_collection_entry)
            .or(toml_dotted_entry);
        if collection_entry.is_none()
            && target_is_collection
            && !self.spans.contains_key(&canonical)
            && self.format != FormatTag::Yaml
        {
            // A JSON or TOML collection whose bytes were not found, such as
            // a dotted key spread over several entries of an inline table.
            return Err(Error::EditRejected {
                action: "delete",
                pointer: canonical,
                reason: "dq cannot find the bytes of the collection there".to_owned(),
            });
        }
        if collection.is_some_and(|collection| !collection.top_level) && collection_entry.is_none()
        {
            // `- key: {…}`, or a flow entry with a comment in between: the
            // entry's bytes cannot be told from those around it.
            return Err(Error::EditRejected {
                action: "delete",
                pointer: canonical,
                reason: "it shares its line with the entry around it".to_owned(),
            });
        }
        let mut line_range = match collection_entry {
            Some(entry) => entry,
            None => {
                let span = self.spans.get(&canonical).ok_or_else(|| Error::Path {
                    pointer: canonical.clone(),
                    // SpanMap only records scalar leaves; walk the in-memory
                    // Value tree so the diagnostic surfaces the longest
                    // matching container chain.
                    matched_prefix: longest_existing_value_prefix(&self.value, pointer),
                    kind: PathErrorKind::MissingKey,
                    did_you_mean: Vec::new(),
                })?;
                // An entry of a JSON value or of a flow collection shares its
                // line with others: remove it with one separating comma.
                let in_flow = self.format == FormatTag::Json
                    || matches!(
                        span.context,
                        SpanContext::FlowMapValue | SpanContext::FlowSeqItem
                    );
                in_flow
                    .then(|| {
                        yaml_flow_entry.clone().or_else(|| {
                            let value = &span.value_range;
                            match layout
                                .as_ref()
                                .and_then(|layout| layout.flow_entries.get(&canonical))
                            {
                                Some(entry) => flow_entry_removal_from(
                                    &self.original_bytes,
                                    entry.start,
                                    value,
                                ),
                                None => flow_entry_removal(
                                    &self.original_bytes,
                                    value,
                                    in_map,
                                    self.format,
                                ),
                            }
                        })
                    })
                    .flatten()
                    .unwrap_or_else(|| span.line_range.clone())
            }
        };
        // `a.b = 1`, the only key under `a`: removing its line would remove
        // `a` too, so it becomes `a = {}`.
        if self.format == FormatTag::Toml
            && parent_loses_last_child
            && in_map
            && let Some(value) = &toml_flow_value
        {
            // Inside an inline table (`{x.y = 1, b = 2}`): the entry turns
            // into `x = {}`, its separators kept.
            let sep = skip_blanks_back(&self.original_bytes, value.start);
            if let Some(key) = (sep > 0 && self.original_bytes[sep - 1] == b'=')
                .then(|| {
                    flow_key_start(
                        &self.original_bytes,
                        skip_blanks_back(&self.original_bytes, sep - 1),
                        self.format,
                    )
                })
                .flatten()
                && let Some(prefix) = toml_dotted_key_parent(&self.original_bytes[key..])
            {
                line_range = key..value.end;
                inserted = prefix;
                inserted.extend_from_slice(b" = {}");
            }
        } else if self.format == FormatTag::Toml
            && parent_loses_last_child
            && in_map
            && !dotted_target
            && let Some(prefix) = toml_dotted_key_parent(&self.original_bytes[line_range.clone()])
        {
            inserted = prefix;
            inserted.extend_from_slice(b" = {}");
            inserted.extend_from_slice(match &self.original_bytes[line_range.clone()] {
                [.., b'\r', b'\n'] => b"\r\n",
                [.., b'\n'] => b"\n",
                _ => b"",
            });
        }
        // Lines removed from the top of the file: the blank lines after
        // them go too, rather than open what follows.
        if self.format != FormatTag::Json
            && inserted.is_empty()
            && matches!(
                &self.original_bytes[..line_range.start],
                b"" | b"\xef\xbb\xbf"
            )
            && line_range.end > line_range.start
            && self.original_bytes[line_range.end - 1] == b'\n'
        {
            let past = past_blank_lines(&self.original_bytes, line_range.end);
            if past < self.original_bytes.len() {
                line_range.end = past;
            }
        }
        let removed = line_range.clone();
        // An alias elsewhere would dangle — or resolve to an earlier
        // definition of its anchor name — once the bytes defining its anchor
        // are gone.
        if let Some(err) = self.anchor_removal_refused(cache, &removed, "delete", &canonical) {
            return Err(err);
        }
        // The line break the removed bytes end with, if any: `\r\n` or `\n`.
        let removed_line_break: &'static [u8] = match &self.original_bytes[removed.clone()] {
            [.., b'\r', b'\n'] => b"\r\n",
            [.., b'\n'] => b"\n",
            _ => b"",
        };
        let delta = SpanRecomputeDelta {
            at: line_range.start,
            old_len: line_range.end - line_range.start,
            new_len: inserted.len(),
        };
        self.original_bytes
            .splice(line_range, inserted.iter().copied());
        // Drop every span at or below the deleted pointer, then shift the
        // remaining spans by the negative delta. We collect the keys to
        // remove first so we don't borrow `self.spans` mutably and
        // immutably at the same time.
        let prefix = format!("{canonical}/");
        let to_remove: Vec<String> = self
            .spans
            .keys()
            .filter(|k| k.as_str() == canonical || k.starts_with(&prefix))
            .cloned()
            .collect();
        for key in to_remove {
            self.spans.shift_remove(&key);
        }
        apply_delta(&mut self.spans, delta);
        delete_value_at(&mut self.value, pointer)?;
        if parent_loses_last_child
            && let Some(parent) = layout
                .as_ref()
                .and_then(|layout| layout.containers.get(&parent_ptr.as_canonical()))
        {
            self.mark_emptied_yaml_collection(&parent_ptr, parent, &removed, removed_line_break);
        }
        // Mirror `set_at`: regenerate provenance from the post-edit span
        // map so the IR view stays consistent across writes.
        self.provenance = provenance_from_spans(&self.spans);
        Ok(())
    }

    /// `end`, where the YAML block collection at `canonical` ends, moved
    /// past the comment lines right after it that are indented as deep as
    /// its entries and deeper than its own key or `-`: they are inside it,
    /// and an edit that removes its entries takes them too.
    fn yaml_trailing_comments_end(
        &self,
        layout: &crate::parsers::yaml_spans::EditLayout,
        canonical: &str,
        end: usize,
    ) -> usize {
        let bytes = &self.original_bytes;
        let column = |at: usize| at - line_start(bytes, at);
        let Some(own) = layout.entries.get(canonical).map(|entry| column(entry.at)) else {
            return end;
        };
        let prefix = format!("{canonical}/");
        let first_child = layout
            .entries
            .iter()
            .filter(|(pointer, _)| {
                pointer
                    .strip_prefix(&prefix)
                    .is_some_and(|tail| !tail.contains('/'))
            })
            .map(|(_, entry)| entry.at)
            .min();
        match first_child {
            Some(child) => past_comment_lines(bytes, end, column(child).max(own + 1)),
            None => end,
        }
    }

    /// The line that writes the TOML table at `parent` again, empty, where
    /// the bytes removed from `first` on began — the bytes that alone made
    /// it: the dotted key of the line there cut down to the table
    /// (`a.b.c = 1` → `a = {}`), or a `[header]` of its own in place of a
    /// removed section. `None` when neither can be spelled.
    fn toml_emptied_table(
        &self,
        tables: &crate::parsers::toml::TomlTables,
        first: usize,
        parent: &Pointer,
    ) -> Option<Vec<u8>> {
        if let Some((pointer, line)) = tables
            .dotted_lines
            .iter()
            .find(|(_, line)| line.start == first)
        {
            let mut out =
                toml_dotted_key_prefix(&self.original_bytes[line.clone()], pointer, parent)?;
            out.extend_from_slice(b" = {}\n");
            return Some(out);
        }
        tables
            .sections
            .iter()
            .any(|(_, section)| section.start == first)
            .then(|| toml_header(&self.value, parent))
            .flatten()
    }

    /// Where `key = value` goes, and its bytes, in the TOML table at
    /// `parent` that has no header of its own and no key of its own to
    /// follow — made only by dotted keys (`a.d.x = 1`) or by the headers of
    /// tables under it (`[a.d]`): under the same dotted key after the last
    /// of its lines, or under a `[header]` of its own written before the
    /// first of those tables. `None` when neither fits.
    fn toml_implicit_table_insertion(
        &self,
        parent: &Pointer,
        key: &str,
        value: &Value,
        renderer: &dyn crate::textual_edit::InsertionRenderer,
    ) -> Option<(usize, Vec<u8>)> {
        let bytes = &self.original_bytes;
        let tables = crate::parsers::toml::toml_tables(bytes)?;
        let prefix = format!("{}/", parent.as_canonical());
        let entry = renderer.render_insertion(key, value, 1, SpanContext::BlockMapValue);
        // After the last dotted key that spells the table out, when it
        // reaches up to it (`a.d.x = 1` for `/a`, not `[a.d]`'s `x.y = 1`).
        if let Some((line, key)) = tables
            .dotted_lines
            .iter()
            .filter(|(pointer, _)| pointer.starts_with(&prefix))
            .max_by_key(|(_, line)| line.end)
            .and_then(|(pointer, line)| {
                toml_dotted_key_prefix(&bytes[line.clone()], pointer, parent).map(|key| (line, key))
            })
        {
            let mut out = Vec::new();
            if line.end > 0 && bytes[line.end - 1] != b'\n' {
                out.push(b'\n');
            }
            out.extend(key);
            out.push(b'.');
            out.extend(&entry);
            return Some((line.end, with_line_ending_at(bytes, line.end, out)));
        }
        // Otherwise a header of its own, before the first section under it
        // — among the sections of the array item it is under, if any.
        let at = tables
            .sections
            .iter()
            .filter(|(pointer, _)| pointer.starts_with(&prefix))
            .map(|(_, section)| section.start)
            .min()?;
        let mut out = toml_header(&self.value, parent)?;
        out.extend(entry);
        out.push(b'\n');
        Some((at, with_line_ending_at(bytes, at, out)))
    }

    /// The bytes `del` removes for the YAML value at `canonical` when its
    /// entry shares its line with its parent's `-` (`- key: v`, `- - v`):
    /// from the entry up to the next entry of the same parent, so that one
    /// takes the `-`; for the only entry, up to the end of its value — the
    /// caller writes `{}` / `[]` in its place. `None` for an entry that
    /// owns its line.
    ///
    /// # Errors
    ///
    /// [`Error::EditRejected`] when the end of the value is not known.
    fn compact_entry_removal(
        &self,
        layout: &crate::parsers::yaml_spans::EditLayout,
        canonical: &str,
        parent_ptr: &Pointer,
        target_is_collection: bool,
        only_child: bool,
    ) -> Result<Option<std::ops::Range<usize>>> {
        let Some(entry) = layout
            .entries
            .get(canonical)
            .filter(|entry| !entry.owns_line)
        else {
            return Ok(None);
        };
        let parent = parent_ptr.as_canonical();
        let siblings = format!("{parent}/");
        let next = layout
            .entries
            .iter()
            .filter(|(pointer, start)| {
                // An empty tail is the empty key `""`, a sibling too.
                pointer
                    .strip_prefix(&siblings)
                    .is_some_and(|tail| !tail.contains('/'))
                    && start.at > entry.at
            })
            .map(|(_, start)| start.at)
            .min();
        let value_end = if target_is_collection {
            // A block collection with the comment lines inside it after its
            // last entry, as `set` over it takes them.
            layout.containers.get(canonical).map(|collection| {
                if collection.block {
                    self.yaml_trailing_comments_end(layout, canonical, collection.end)
                } else {
                    collection.end
                }
            })
        } else if let Some(empty) = layout.empty_values.get(canonical) {
            Some(empty.end)
        } else {
            self.spans.get(canonical).map(|span| span.value_range.end)
        };
        if let Some(next) = next {
            // A comment line before the next key stays: the item's `-`
            // goes on to it (`- # mid`), the key on its own line below.
            let comment = value_end
                .and_then(|end| first_comment_line(&self.original_bytes, end.max(entry.at), next));
            return Ok(Some(entry.at..comment.unwrap_or(next)));
        }
        let end = if only_child { value_end } else { None };
        let Some(mut end) = end.filter(|&end| end > entry.at) else {
            return Err(Error::EditRejected {
                action: "delete",
                pointer: canonical.to_owned(),
                reason: "it shares its line with the entry around it".to_owned(),
            });
        };
        while end > entry.at && self.original_bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        Ok(Some(entry.at..end))
    }

    /// After `del_at` removed the bytes `removed` (ending in the line break
    /// `removed_line_break`, empty if none) holding the last child of the
    /// YAML block collection at `parent_ptr`, write an explicit `{}` / `[]`
    /// for it — past its key's `:` (`key: {}`), its `-`, or its node
    /// properties; where the child was for a document root — and record the
    /// empty-container span a fresh parse would.
    ///
    /// Leaves the bytes alone for a flow collection (its brackets stay),
    /// when the removed bytes do not hold the child's whole entry, and when
    /// the marker position is unknown or was removed with the child
    /// (`- key: value`, the child on its parent's `-` line).
    fn mark_emptied_yaml_collection(
        &mut self,
        parent_ptr: &Pointer,
        parent: &crate::parsers::yaml_spans::ContainerLayout,
        removed: &std::ops::Range<usize>,
        removed_line_break: &[u8],
    ) {
        let is_map = match parent_ptr.resolve(&self.value) {
            Ok(Value::Map(map)) if map.is_empty() => true,
            Ok(Value::Array(items)) if items.is_empty() => false,
            _ => return,
        };
        // Only once the child's whole entry is gone: the removed bytes must
        // start at or before the line its key or `-` is on.
        let entry_removed = !removed.is_empty()
            && parent
                .first_child_line
                .is_some_and(|line| removed.start <= line);
        if !parent.block || !entry_removed {
            return;
        }
        let marker: &[u8] = if is_map { b"{}" } else { b"[]" };
        let (at, inserted) = if parent.top_level {
            let mut inserted = marker.to_vec();
            inserted.extend_from_slice(removed_line_break);
            (removed.start, inserted)
        } else {
            match parent.empty_marker_at {
                Some(at) if at < removed.start => {
                    let mut inserted = b" ".to_vec();
                    inserted.extend_from_slice(marker);
                    (at, inserted)
                }
                _ => return,
            }
        };
        self.original_bytes.splice(at..at, inserted.iter().copied());
        apply_delta(
            &mut self.spans,
            SpanRecomputeDelta {
                at,
                old_len: 0,
                new_len: inserted.len(),
            },
        );
        // Past the separating space, if one was inserted.
        let marker_start = if parent.top_level { at } else { at + 1 };
        let marker_range = marker_start..marker_start + marker.len();
        let line_start = line_start(&self.original_bytes, marker_start);
        self.spans
            .entry(parent_ptr.as_canonical())
            .or_insert_with(|| ValueSpan {
                value_range: marker_range.clone(),
                line_range: marker_range,
                indent: u32::try_from(marker_start - line_start + 1).unwrap_or(u32::MAX),
                context: if is_map {
                    SpanContext::FlowMapValue
                } else {
                    SpanContext::FlowSeqItem
                },
            });
    }
}

/// Bytes to remove to delete one entry of a flow collection — any JSON
/// object or array, a YAML flow collection, a TOML inline table or array —
/// whose value is at `value`: the entry (its key and YAML node properties
/// included) and one separating comma, or everything between the brackets
/// for the only entry. `None` when the surroundings are not the expected
/// separators — a comment in between, say.
fn flow_entry_removal(
    bytes: &[u8],
    value: &std::ops::Range<usize>,
    map_entry: bool,
    format: FormatTag,
) -> Option<std::ops::Range<usize>> {
    let start = flow_entry_start(bytes, value, map_entry, format)?;
    flow_entry_removal_from(bytes, start, value)
}

/// Where the entry of a flow collection whose value is at `value` begins,
/// read back from the value: its key with any YAML node properties and
/// `?`, or the value's own node properties. A YAML edit takes it from the
/// layout when it can (`EditLayout::flow_entries`): reading back cannot
/// always tell a bracket of a collection key from one in a quoted scalar.
fn flow_entry_start(
    bytes: &[u8],
    value: &std::ops::Range<usize>,
    map_entry: bool,
    format: FormatTag,
) -> Option<usize> {
    let yaml = format == FormatTag::Yaml;
    let mut start = value.start;
    if yaml {
        start = yaml_properties_start(bytes, start);
    }
    if map_entry {
        let indicator = if format == FormatTag::Toml {
            b'='
        } else {
            b':'
        };
        let sep = skip_blanks_back(bytes, start);
        let key_end = if sep > 0 && bytes[sep - 1] == indicator {
            skip_blanks_back(bytes, sep - 1)
        } else if yaml && value.is_empty() && start == value.start && sep == start {
            // The empty value of a key with no `:` (`{n}`), spanned right
            // past the key.
            sep
        } else {
            return None;
        };
        start = flow_key_start(bytes, key_end, format)?;
        if yaml {
            start = yaml_properties_start(bytes, start);
            let explicit = skip_blanks_back(bytes, start);
            if explicit > 0 && bytes[explicit - 1] == b'?' {
                start = explicit - 1;
            }
        }
    }
    Some(start)
}

/// [`flow_entry_removal`] for the entry that begins at `start`.
fn flow_entry_removal_from(
    bytes: &[u8],
    start: usize,
    value: &std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    let after = skip_blanks(bytes, value.end);
    let before = skip_blanks_back(bytes, start);
    let preceding = before.checked_sub(1).map(|i| bytes[i]);
    match bytes.get(after) {
        Some(b',') => {
            let next = skip_blanks(bytes, after + 1);
            match bytes.get(next) {
                // Keep the blank that makes a comment a comment.
                Some(b'#') => return Some(start..after + 1),
                Some(b'}' | b']') => {}
                _ => return Some(start..next),
            }
            // A trailing comma: the entry is the last one.
            match preceding {
                Some(b',') => Some(before - 1..after + 1),
                Some(b'{' | b'[') => Some(before..next),
                _ => None,
            }
        }
        Some(b'}' | b']') => match preceding {
            Some(b',') => Some(before - 1..value.end),
            Some(b'{' | b'[') => Some(before..after),
            _ => None,
        },
        _ => None,
    }
}

/// The bytes `del` removes for the entry of the YAML flow collection at
/// `canonical`, from where the parse put the entries of its parent at
/// `parent` (which has `children` entries): the entry with the comma after
/// it, or — for the last one — with the comma before it and the blanks
/// before that comma; for the only one, everything between the brackets.
/// `None` when the layout does not account for every entry of the parent.
fn yaml_flow_entry_removal(
    bytes: &[u8],
    layout: &crate::parsers::yaml_spans::EditLayout,
    canonical: &str,
    parent: &str,
    children: usize,
) -> Option<std::ops::Range<usize>> {
    let entry = layout.flow_entries.get(canonical)?.clone();
    let brackets = layout.containers.get(parent)?.brackets.clone()?;
    let prefix = format!("{parent}/");
    let mut siblings: Vec<(&str, std::ops::Range<usize>)> = layout
        .flow_entries
        .iter()
        .filter(|(pointer, _)| {
            pointer
                .strip_prefix(&prefix)
                .is_some_and(|tail| !tail.contains('/'))
        })
        .map(|(pointer, range)| (pointer.as_str(), range.clone()))
        .collect();
    if siblings.len() != children {
        return None;
    }
    siblings.sort_by_key(|(_, range)| range.start);
    let at = siblings.iter().position(|(_, range)| *range == entry)?;
    let (pointers, siblings): (Vec<&str>, Vec<std::ops::Range<usize>>) =
        siblings.into_iter().unzip();
    // What follows the entry: its comma or the closing bracket.
    let after = skip_blanks_and_comments(bytes, entry.end);
    let close = brackets.end.checked_sub(1)?;
    if let Some(next) = siblings.get(at + 1) {
        if bytes.get(after) != Some(&b',') {
            return None;
        }
        let past = skip_blanks(bytes, after + 1);
        // Keep the blank that makes a comment a comment.
        return Some(if bytes.get(past) == Some(&b'#') {
            entry.start..after + 1
        } else {
            entry.start..next.start
        });
    }
    let end = match bytes.get(after) {
        // A trailing comma goes with the last entry.
        Some(b',') => after + 1,
        _ if after == close => entry.end,
        _ => return None,
    };
    let Some(previous) = at.checked_sub(1).map(|i| &siblings[i]) else {
        // The only entry: the collection is left empty, with the comments
        // before and after the entry.
        let open = brackets.start + 1;
        let start = if bytes[open..entry.start].contains(&b'#') {
            entry.start
        } else {
            open
        };
        let end = if bytes[end..close].contains(&b'#') {
            end
        } else {
            close
        };
        return (matches!(bytes.get(brackets.start), Some(b'{' | b'['))
            && matches!(bytes.get(close), Some(b'}' | b']')))
        .then_some(start..end);
    };
    let comma = skip_blanks_and_comments(bytes, previous.end);
    if bytes.get(comma) != Some(&b',') {
        return None;
    }
    // A comment after the comma stays, and the comma with it: a trailing
    // comma is no error in a flow collection.
    if bytes[comma..entry.start].contains(&b'#') {
        return Some(entry.start..end);
    }
    // The blanks before the comma go too (`{a: 1 , b: 2}`), a comment
    // there stays, and so does a blank after the entry before when it is
    // an empty node with a tag (`[!!str , 1]`): `!!str]` reads as one tag.
    let start = if layout.bare_flow_tags.contains(pointers[at - 1]) && comma > previous.end {
        previous.end + 1
    } else if bytes[previous.end..comma]
        .iter()
        .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    {
        previous.end
    } else {
        comma
    };
    Some(start.min(comma)..end)
}

/// Start of the key that ends at `end` in a flow mapping: a quoted key, a
/// TOML dotted key (`a."b".c`), a YAML flow collection (`[1, 2]`), or a
/// plain key back to the flow indicator before it.
fn flow_key_start(bytes: &[u8], end: usize, format: FormatTag) -> Option<usize> {
    let mut end = end;
    loop {
        let last = *bytes.get(end.checked_sub(1)?)?;
        if format == FormatTag::Yaml
            && matches!(last, b']' | b'}')
            && let Some(open) = yaml_flow_collection_start(bytes, end)
        {
            return Some(open);
        }
        let start = match last {
            b'"' => {
                // The opening quote: the nearest `"` before the closing one
                // that no odd run of backslashes escapes.
                let mut i = end - 1;
                loop {
                    i = bytes[..i].iter().rposition(|&b| b == b'"')?;
                    let backslashes = bytes[..i].iter().rev().take_while(|&&b| b == b'\\').count();
                    if backslashes % 2 == 0 {
                        break i;
                    }
                }
            }
            b'\'' if format != FormatTag::Json => {
                // `''` inside a single-quoted scalar is an escaped quote.
                let mut i = end - 1;
                loop {
                    let quote = bytes[..i].iter().rposition(|&b| b == b'\'')?;
                    if quote > 0 && bytes[quote - 1] == b'\'' {
                        i = quote - 1;
                        continue;
                    }
                    break quote;
                }
            }
            _ => {
                let toml = format == FormatTag::Toml;
                let stops = |b: u8| {
                    matches!(b, b'{' | b'[' | b',' | b'\n' | b'\r')
                        || (toml && matches!(b, b'.' | b' ' | b'\t'))
                };
                let mut i = end;
                while i > 0 && !stops(bytes[i - 1]) {
                    i -= 1;
                }
                while i < end && matches!(bytes[i], b' ' | b'\t') {
                    i += 1;
                }
                i
            }
        };
        if format == FormatTag::Toml {
            let dot = skip_blanks_back(bytes, start);
            if dot > 0 && bytes[dot - 1] == b'.' {
                end = skip_blanks_back(bytes, dot - 1);
                continue;
            }
        }
        return Some(start);
    }
}

/// Start of the YAML flow collection that ends at `end` (with its `]` or
/// `}`): the nearest `[` / `{` before it that opens a collection closing
/// exactly at `end` — [`yaml_flow_closes_at`] reads forward from each, as
/// quotes can only be told apart from a plain scalar's `'` that way.
fn yaml_flow_collection_start(bytes: &[u8], end: usize) -> Option<usize> {
    let mut at = end.checked_sub(1)?;
    while let Some(open) = bytes[..at].iter().rposition(|&b| matches!(b, b'[' | b'{')) {
        if yaml_flow_closes_at(bytes, open, end) {
            return Some(open);
        }
        at = open;
    }
    None
}

/// Whether the YAML flow collection opening at `open` closes exactly at
/// `end`: brackets matched by kind outside quoted scalars and comments. A quote
/// starts a scalar only where a node can start — after `[` `{` `,`, or a
/// `?` / `:` indicator — not inside a plain scalar (`it's`).
fn yaml_flow_closes_at(bytes: &[u8], open: usize, end: usize) -> bool {
    // The open brackets, innermost last: a `]` closes only a `[`.
    let mut open_brackets: Vec<u8> = Vec::new();
    let mut node_next = true;
    let mut i = open;
    while i < end {
        let b = bytes[i];
        i += 1;
        match b {
            b'[' | b'{' => {
                open_brackets.push(b);
                node_next = true;
            }
            b']' | b'}' => {
                let opener = if b == b']' { b'[' } else { b'{' };
                if open_brackets.pop() != Some(opener) {
                    return false;
                }
                if open_brackets.is_empty() {
                    return i == end;
                }
                node_next = false;
            }
            b',' => node_next = true,
            b' ' | b'\t' | b'\r' | b'\n' => {}
            b'#' if matches!(bytes[i - 2], b' ' | b'\t' | b'\r' | b'\n') => {
                while i < end && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' if node_next => {
                loop {
                    match bytes.get(i) {
                        Some(b'\\') => i += 2,
                        Some(b'"') => break,
                        Some(_) => i += 1,
                        None => return false,
                    }
                }
                i += 1;
                node_next = false;
            }
            b'\'' if node_next => {
                loop {
                    match (bytes.get(i), bytes.get(i + 1)) {
                        (Some(b'\''), Some(b'\'')) => i += 2,
                        (Some(b'\''), _) => break,
                        (Some(_), _) => i += 1,
                        (None, _) => return false,
                    }
                }
                i += 1;
                node_next = false;
            }
            b':' | b'?'
                if bytes.get(i).is_none_or(|&next| {
                    matches!(
                        next,
                        b' ' | b'\t' | b'\r' | b'\n' | b',' | b'[' | b']' | b'{' | b'}'
                    )
                }) =>
            {
                node_next = true;
            }
            _ => node_next = false,
        }
    }
    false
}

/// Start of the YAML node properties (`&anchor`, `!tag`) right before
/// `start`, or `start` without any.
fn yaml_properties_start(bytes: &[u8], start: usize) -> usize {
    let mut start = start;
    loop {
        let end = skip_blanks_back(bytes, start);
        let mut token = end;
        while token > 0
            && !matches!(
                bytes[token - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b'{' | b'[' | b','
            )
        {
            token -= 1;
        }
        // A verbatim tag (`!<tag:yaml.org,2002:map>`) may hold a `,`.
        if end > 0
            && bytes[end - 1] == b'>'
            && let Some(open) = bytes[..end]
                .windows(2)
                .rposition(|pair| pair == b"!<")
                .filter(|&open| !bytes[open..end].iter().any(u8::is_ascii_whitespace))
        {
            token = open;
        }
        if token < end && matches!(bytes[token], b'&' | b'!') {
            start = token;
        } else {
            return start;
        }
    }
}

/// First byte at or after `pos` that is neither blank, a line break nor
/// part of a `#` comment.
fn skip_blanks_and_comments(bytes: &[u8], pos: usize) -> usize {
    let mut pos = skip_blanks(bytes, pos);
    while bytes.get(pos) == Some(&b'#') {
        while bytes.get(pos).is_some_and(|&b| b != b'\n') {
            pos += 1;
        }
        pos = skip_blanks(bytes, pos);
    }
    pos
}

/// First byte at or after `pos` that is not a space, tab or line break.
fn skip_blanks(bytes: &[u8], pos: usize) -> usize {
    let mut pos = pos;
    while matches!(bytes.get(pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        pos += 1;
    }
    pos
}

/// `pos` moved back over spaces, tabs and line breaks: `bytes[result - 1]`
/// is the last other byte before `pos`.
fn skip_blanks_back(bytes: &[u8], pos: usize) -> usize {
    let mut pos = pos.min(bytes.len());
    while pos > 0 && matches!(bytes[pos - 1], b' ' | b'\t' | b'\n' | b'\r') {
        pos -= 1;
    }
    pos
}

/// Pointer (below `at`) of the first place `expected` and `actual`
/// differ, `None` when they are the same tree. Map order does not count,
/// NaN equals NaN, and finite floats compare by bits — so a writer that
/// drops the sign of `-0.0` is caught ([`Value::same_data`]).
fn first_difference(expected: &Value, actual: &Value, at: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Map(want), Value::Map(got)) => {
            let below = |key: &str| format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"));
            want.iter()
                .find_map(|(key, want)| match got.get(key) {
                    Some(got) => first_difference(want, got, &below(key)),
                    None => Some(below(key)),
                })
                .or_else(|| {
                    got.keys()
                        .find(|key| !want.contains_key(*key))
                        .map(|key| below(key))
                })
        }
        (Value::Array(want), Value::Array(got)) => {
            if want.len() != got.len() {
                return Some(at.to_owned());
            }
            want.iter()
                .zip(got)
                .enumerate()
                .find_map(|(i, (want, got))| first_difference(want, got, &format!("{at}/{i}")))
        }
        // NaN equals no float, YAML holds `.nan` / `.inf` as literals a
        // TOML `nan` / `inf` reads back as floats, and `-0.0 == 0.0`.
        (want, got) if want.same_data(got) => None,
        _ => Some(at.to_owned()),
    }
}

/// Build a [`ProvenanceMap`] from a [`SpanMap`] by emitting one
/// [`Provenance::Original`] entry per pointer, sharing the same canonical
/// keys.
///
/// Every populated span yields `Original { pointer, span: Some(span),
/// inline_offset: None }`; the read-only parser path does not call this
/// helper — it leaves `Document::provenance` empty so `as_ir().provenance`
/// reports "no provenance available" for every lookup.
///
/// Inline-offset population is the parser's job (`add-validation-and-extended-formats`
/// Phase 2): YAML block scalars and markdown fenced code blocks override
/// `inline_offset` post-construction by writing a richer entry directly
/// into `Document::provenance`. See `parsers::yaml_spans` and
/// `parsers::markdown` for the populated branches; every other format
/// keeps the `None` default produced here.
fn provenance_from_spans(spans: &SpanMap) -> ProvenanceMap {
    let mut map = ProvenanceMap::with_capacity(spans.len());
    for (canonical, span) in spans {
        // Re-parse the canonical pointer string so the structured
        // [`Pointer`] inside `Original` matches the canonical key. A span
        // under an empty key (`{"": 1}`, YAML `? &a`) has a pointer with an
        // empty segment, which `Pointer::parse` rejects: no pointer can
        // address that node, so it gets no provenance.
        let Ok(pointer) = Pointer::parse(canonical) else {
            tracing::debug!(pointer = %canonical, "no provenance for an unaddressable span");
            continue;
        };
        map.insert(
            canonical.clone(),
            Provenance::original(pointer, Some(span.clone())),
        );
    }
    map
}

/// Apply a signed shift to a `usize`, saturating on negative overflow.
fn signed_extend(value: usize, shift: isize) -> usize {
    if shift >= 0 {
        value.saturating_add(shift as usize)
    } else {
        value.saturating_sub(shift.unsigned_abs())
    }
}

/// Locate the byte offset where `scalar_bytes` begins inside `fragment`.
///
/// The insertion renderers always emit the value as the last meaningful
/// token in the fragment — `<prefix>: <value>` for JSON / YAML,
/// `<prefix> = <value>` for TOML, optionally followed by a trailing `\n`.
/// We therefore look for the rightmost occurrence of `scalar_bytes` inside
/// the fragment. Returns `0` when the scalar can't be located (should
/// never happen with the registered renderers, but the conservative
/// fallback keeps span arithmetic well-defined and observable in tests).
fn locate_value_in_fragment(fragment: &[u8], scalar_bytes: &[u8]) -> usize {
    if scalar_bytes.is_empty() || fragment.len() < scalar_bytes.len() {
        return 0;
    }
    // Search rightmost-first so a value that happens to also appear inside
    // an escaped key isn't mistakenly matched.
    let mut i = fragment.len() - scalar_bytes.len();
    loop {
        if &fragment[i..i + scalar_bytes.len()] == scalar_bytes {
            return i;
        }
        if i == 0 {
            return 0;
        }
        i -= 1;
    }
}

/// Walk back from `value_start` to the previous newline and forward through
/// leading whitespace to find the 1-indexed column of the sibling's key.
///
/// This mirrors what the YAML insertion renderer expects in its
/// `parent_indent` parameter: it wants the column of the children's keys,
/// but the recorded [`ValueSpan::indent`] is the column of the children's
/// *values*. Scanning the line directly is the most faithful — and parser-
/// agnostic — way to recover the key column from any leaf span.
///
/// Returns `1` when no leading whitespace is found (root-level keys at
/// column 1). For JSON / TOML the renderers ignore this value, so the
/// scan is harmless when called on those formats.
fn sibling_key_column(bytes: &[u8], value_start: usize) -> u32 {
    let cap = bytes.len();
    // Past a byte-order mark: it is no column.
    let line_start = line_start(bytes, value_start);
    let mut key_col = line_start;
    while key_col < cap && matches!(bytes[key_col], b' ' | b'\t') {
        key_col += 1;
    }
    // A YAML key of a compact mapping sits past its item's `-` (`- key:`,
    // `- - key:`). No JSON or TOML key starts with `-` and a blank.
    while key_col + 1 < cap && bytes[key_col] == b'-' && matches!(bytes[key_col + 1], b' ' | b'\t')
    {
        key_col += 1;
        while key_col < cap && matches!(bytes[key_col], b' ' | b'\t') {
            key_col += 1;
        }
    }
    // Column = (byte offset on line) + 1 to match the 1-indexed convention
    // saphyr-parser reports for `ValueSpan::indent`.
    u32::try_from(key_col - line_start + 1).unwrap_or(1)
}

/// Translate the per-format [`InsertionRenderer`] fragment into a final
/// `(splice_at, splice_bytes)` pair anchored against a recorded sibling
/// span. The caller locates the value bytes inside `splice_bytes` directly
/// via [`locate_value_in_fragment`] so this helper doesn't have to track
/// fragment-relative offsets.
///
/// Each format expects a different shape:
///
/// - **JSON**: splice at `sibling.value_range.end` (just past the value,
///   before any whitespace + comma + closing `}`/`]`). The renderer
///   produces `\n  "k": v` (no leading comma, no trailing newline). We
///   prepend `,` so the result is a valid sibling.
/// - **YAML**: splice at `sibling.line_range.end` (just past the trailing
///   newline of the sibling's line). The renderer produces `\n  k: v\n`;
///   we strip the leading newline so we don't introduce a blank line.
///   When the source has no trailing newline at that position (e.g. the
///   document ends without one) we re-add it.
/// - **TOML**: splice at `sibling.line_range.end`. The renderer already
///   produces a self-contained `key = value\n` fragment with no leading
///   newline, so we pass it through verbatim — with the same
///   trailing-newline guard as YAML.
///
/// Any format without a wired renderer should never reach this function
/// (callers gate on [`crate::textual_edit::insertion_renderer_for_format`])
/// — the match falls through to YAML's contract as a defensive default.
fn format_specific_splice(
    format: FormatTag,
    fragment: &[u8],
    sibling_span: &ValueSpan,
    original_bytes: &[u8],
) -> (usize, Vec<u8>) {
    match format {
        FormatTag::Json => {
            let mut out = Vec::with_capacity(fragment.len() + 1);
            out.push(b',');
            out.extend_from_slice(fragment);
            (sibling_span.value_range.end, out)
        }
        FormatTag::Yaml => {
            // Strip a leading `\n` from the renderer output if present —
            // the splice point is already past the sibling's `\n`.
            let trimmed: &[u8] = fragment.strip_prefix(b"\n").unwrap_or(fragment);
            let mut splice_at = past_line_break(original_bytes, sibling_span.line_range.end);
            let mut out = Vec::with_capacity(trimmed.len() + 1);
            // Guard: the sibling's `line_range.end` should point past a
            // `\n`. If the source ends without one (e.g. final value with
            // no trailing newline) we prepend `\n` so we don't smear into
            // the previous line.
            if splice_at == 0 || original_bytes.get(splice_at - 1) != Some(&b'\n') {
                out.push(b'\n');
            }
            out.extend_from_slice(trimmed);
            // Edge case: if we'd splice past EOF in a doc that ends mid-line,
            // clamp to EOF.
            if splice_at > original_bytes.len() {
                splice_at = original_bytes.len();
            }
            (splice_at, out)
        }
        FormatTag::Toml => {
            let mut splice_at = past_line_break(original_bytes, sibling_span.line_range.end);
            let mut out = Vec::with_capacity(fragment.len() + 1);
            if splice_at == 0 || original_bytes.get(splice_at - 1) != Some(&b'\n') {
                out.push(b'\n');
            }
            out.extend_from_slice(fragment);
            if splice_at > original_bytes.len() {
                splice_at = original_bytes.len();
            }
            (splice_at, out)
        }
        // Defensive default — every format reaching this point has a
        // registered renderer by construction. Fall through to YAML's
        // shape, which is the safest "block-style append" form.
        FormatTag::Jsonl
        | FormatTag::Hcl
        | FormatTag::Ini
        | FormatTag::DotEnv
        | FormatTag::Csv
        | FormatTag::Tsv
        | FormatTag::Dockerfile
        | FormatTag::IgnoreList
        | FormatTag::Frontmatter
        | FormatTag::Markdown
        | FormatTag::Xml => {
            let trimmed: &[u8] = fragment.strip_prefix(b"\n").unwrap_or(fragment);
            (sibling_span.line_range.end, trimmed.to_vec())
        }
    }
}

/// What replaces the node properties `props` of an empty node (empty
/// without any) when it gets the value `rendered`, and the offset of the
/// value in it. Anchors stay; tags of the core schema go, as they would
/// retype the new value (`!!str` turns `5` into `"5"`). Without
/// properties a blank separates the value from the `:` / `-` before it.
fn empty_node_replacement(props: &[u8], rendered: &[u8], tags: &CoreTags) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(props.len() + rendered.len() + 1);
    if props.is_empty() {
        out.push(b' ');
    }
    out.append(&mut kept_properties(props, tags));
    let offset = out.len();
    out.extend_from_slice(rendered);
    (out, offset)
}

/// The `#` of the first line between the line holding `from` and `to`
/// that holds only a comment; `None` when there is none.
fn first_comment_line(bytes: &[u8], from: usize, to: usize) -> Option<usize> {
    let line_start = line_start(bytes, from);
    // A span can end inside the next line's indentation.
    let mut line = if bytes[line_start..from]
        .iter()
        .all(|&b| matches!(b, b' ' | b'\t'))
    {
        line_start
    } else {
        from + bytes[from..to].iter().position(|&b| b == b'\n')? + 1
    };
    while line < to {
        let content = line
            + bytes[line..to]
                .iter()
                .position(|&b| !matches!(b, b' ' | b'\t'))?;
        if bytes[content] == b'#' {
            return Some(content);
        }
        line = content + bytes[content..to].iter().position(|&b| b == b'\n')? + 1;
    }
    None
}

/// Whether a TOML file puts a blank line before a table header — right
/// before it, or before the comment lines that lead into it.
fn blank_line_before_a_header(bytes: &[u8]) -> bool {
    // Whether the last line before the comment lines just seen is blank.
    let mut blank_before = false;
    // A header starts its line, as the items of a multi-line array
    // seldom do.
    for line in bytes.split(|&b| b == b'\n') {
        if line.starts_with(b"[") && blank_before {
            return true;
        }
        if !line.trim_ascii_start().starts_with(b"#") {
            blank_before = line.trim_ascii().is_empty();
        }
    }
    false
}

/// Byte offset of `pos` from the start of its line.
fn line_column(bytes: &[u8], pos: usize) -> usize {
    pos - line_start(bytes, pos)
}

/// How a file breaks its lines around a byte: the one detector every
/// textual edit asks before it writes a line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineBreak {
    /// `\n`.
    Lf,
    /// `\r\n`.
    CrLf,
    /// A lone `\r`, in a file with no `\n` at all.
    Cr,
}

impl LineBreak {
    /// The line break nearest `at` in `bytes` — the last one before it, or
    /// else the first after; [`LineBreak::Cr`] when there is no `\n` but a
    /// `\r`, [`LineBreak::Lf`] when there is no line break at all.
    pub(crate) fn at(bytes: &[u8], at: usize) -> Self {
        let at = at.min(bytes.len());
        let nearest = bytes[..at]
            .iter()
            .rposition(|&b| b == b'\n')
            .or_else(|| bytes[at..].iter().position(|&b| b == b'\n').map(|i| at + i));
        match nearest {
            Some(i) if i > 0 && bytes[i - 1] == b'\r' => Self::CrLf,
            Some(_) => Self::Lf,
            None if bytes.contains(&b'\r') => Self::Cr,
            None => Self::Lf,
        }
    }

    /// Whether every line of `bytes` ends in a lone `\r`.
    pub(crate) fn cr_only(bytes: &[u8]) -> bool {
        Self::at(bytes, 0) == Self::Cr
    }
}

/// `inserted`, to go in `bytes` at `at`, with each bare `\n` turned into
/// the line break nearest `at` ([`LineBreak::at`]): `\r\n` or a lone `\r`.
fn with_line_ending_at(bytes: &[u8], at: usize, inserted: Vec<u8>) -> Vec<u8> {
    with_line_break(LineBreak::at(bytes, at), inserted)
}

/// `inserted` with each bare `\n` turned into `line_break`.
fn with_line_break(line_break: LineBreak, inserted: Vec<u8>) -> Vec<u8> {
    if line_break == LineBreak::Lf || !inserted.contains(&b'\n') {
        return inserted;
    }
    let mut out = Vec::with_capacity(inserted.len() + 4);
    for (i, &b) in inserted.iter().enumerate() {
        if b == b'\n' && (i == 0 || inserted[i - 1] != b'\r') {
            out.push(b'\r');
            if line_break == LineBreak::Cr {
                continue;
            }
        }
        out.push(b);
    }
    out
}

/// Offsets of the dots between the segments of the TOML key that `text`
/// begins with (blanks first allowed), which ends at the first `=` or `]`
/// outside quotes. `None` without that end.
fn toml_key_dots(text: &[u8]) -> Option<Vec<usize>> {
    let mut quote = None;
    let mut dots = Vec::new();
    let mut escaped = false;
    for (i, &b) in text.iter().enumerate() {
        if std::mem::take(&mut escaped) {
            continue;
        }
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            // In a basic string `\"` is no closing quote.
            (Some(b'"'), b'\\') => escaped = true,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'.') => dots.push(i),
            (None, b'=' | b']') => return Some(dots),
            (None, b'#' | b'\n' | b'[' | b'{') => return None,
            (None, _) => {}
        }
    }
    None
}

/// The TOML line `line` that writes the value at `pointer` under a dotted
/// key, cut down to the part of the key that names the table at `parent`
/// (`  a.b.c = 1`, `/a/b/c`, `/a` → `  a`), its indentation kept. `None`
/// when the key does not reach up to `parent` within the line.
fn toml_dotted_key_prefix(line: &[u8], pointer: &str, parent: &Pointer) -> Option<Vec<u8>> {
    let dots = toml_key_dots(line)?;
    // Segments of the canonical pointer, an empty key (`a."".x`) included,
    // which `Pointer::parse` rejects.
    let depth = pointer.matches('/').count();
    let below = depth.checked_sub(parent.segments().len())?;
    // The key's last `below` segments lie under `parent`.
    let kept = (dots.len() + 1).checked_sub(below)?;
    let end = skip_blanks_back(line, *dots.get(kept.checked_sub(1)?)?);
    Some(line[..end].to_vec())
}

/// The `[header]` line of the TOML table at `table`, each segment a key
/// of a table or an index into an array of tables. A header names no
/// index — `[t.a]` is under the `[[t]]` item last opened above it — so
/// it is right only where it goes among that item's own sections. `None`
/// for the root and a path through anything else.
fn toml_header(root: &Value, table: &Pointer) -> Option<Vec<u8>> {
    let segments = table.segments();
    let mut keys = Vec::with_capacity(segments.len());
    let mut node = root;
    // An array item's own header is `[[t]]`, which opens a new one: the
    // path ends with a key.
    let mut ends_with_key = false;
    for segment in segments {
        (node, ends_with_key) = match (node, segment) {
            (Value::Map(map), Segment::Key(key)) => {
                keys.push(crate::parsers::toml::toml_key_token(key));
                (map.get(key)?, true)
            }
            (Value::Array(items), Segment::Index(index)) => (items.get(*index)?, false),
            (Value::Array(items), Segment::Key(index)) => {
                (items.get(index.parse::<usize>().ok()?)?, false)
            }
            _ => return None,
        };
    }
    if !ends_with_key {
        return None;
    }
    Some(format!("[{}]\n", keys.join(".")).into_bytes())
}

/// For the TOML line `line` holding a dotted key (`  a."b.c".d = 1`), the
/// key without its last part (`  a."b.c"`); `None` for a key of one part
/// or a line that is not `key = value`.
fn toml_dotted_key_parent(line: &[u8]) -> Option<Vec<u8>> {
    let mut quote = None;
    let mut last_dot = None;
    for (i, &b) in line.iter().enumerate() {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            (Some(b'"'), b'\\') => return None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'.') => last_dot = Some(i),
            (None, b'=') => {
                let mut end = last_dot?;
                while end > 0 && matches!(line[end - 1], b' ' | b'\t') {
                    end -= 1;
                }
                return Some(line[..end].to_vec());
            }
            (None, b'#' | b'\n' | b'[' | b'{') => return None,
            (None, _) => {}
        }
    }
    None
}

/// The anchor and other tags of the node properties `props`, each
/// followed by a space; the core-schema tags `tags` names are dropped.
fn kept_properties(props: &[u8], tags: &CoreTags) -> Vec<u8> {
    let mut out = Vec::with_capacity(props.len());
    for token in props
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty() && !tags.retypes(token))
    {
        out.extend_from_slice(token);
        out.push(b' ');
    }
    out
}

/// The YAML node properties right before `end` when one of them is a tag
/// of the core schema — which would retype a new value written after
/// them — and their bytes; `None` otherwise.
fn core_tag_removal<'a>(
    bytes: &'a [u8],
    end: usize,
    tags: &CoreTags,
) -> Option<(std::ops::Range<usize>, &'a [u8])> {
    let start = yaml_properties_start(bytes, end);
    let props = &bytes[start..end];
    tags.any_in(props).then_some((start..end, props))
}

/// The tags of a YAML document that name a type of the core schema
/// (`tag:yaml.org,2002:`), which a new value must not inherit: `!!map`,
/// the verbatim `!<tag:yaml.org,2002:map>`, a `%TAG` handle bound to that
/// prefix (`!e!map`), and the non-specific `!`, which makes `5` a string.
struct CoreTags {
    /// The `%TAG` directives of the document: handle, prefix.
    directives: Vec<(Vec<u8>, Vec<u8>)>,
}

impl CoreTags {
    const PREFIX: &'static [u8] = b"tag:yaml.org,2002:";

    /// The core tags of the document of the stream `bytes` that holds
    /// byte `at`: a `%TAG` directive binds its handle for the one document
    /// that follows it.
    fn at(bytes: &[u8], at: usize) -> Self {
        let start = bom_end(bytes);
        // Directives read since the last document began, and those of the
        // document `at` is in.
        let mut pending = Vec::new();
        let mut directives = Vec::new();
        for line in bytes[start..at.clamp(start, bytes.len())].split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let marker = |marker: &[u8]| {
                line.strip_prefix(marker)
                    .is_some_and(|rest| rest.first().is_none_or(u8::is_ascii_whitespace))
            };
            if marker(b"---") {
                directives = std::mem::take(&mut pending);
            } else if marker(b"...") {
                directives.clear();
            } else if marker(b"%TAG") {
                let mut fields = line[4..]
                    .split(u8::is_ascii_whitespace)
                    .filter(|field| !field.is_empty());
                if let (Some(handle), Some(prefix)) = (fields.next(), fields.next()) {
                    pending.push((handle.to_vec(), prefix.to_vec()));
                }
            }
        }
        Self { directives }
    }

    /// Whether the node property `token` is a tag that retypes the node.
    fn retypes(&self, token: &[u8]) -> bool {
        if token == b"!" {
            return true;
        }
        if let Some(uri) = token.strip_prefix(b"!<") {
            return uri.starts_with(Self::PREFIX);
        }
        if !token.starts_with(b"!") {
            return false;
        }
        // `!!x`, `!e!x`: the handle ends at the second `!`; `!x` has the
        // primary handle `!`.
        let handle = match token[1..].iter().position(|&b| b == b'!') {
            Some(i) => &token[..i + 2],
            None => b"!".as_slice(),
        };
        let prefix = self
            .directives
            .iter()
            .rev()
            .find(|(bound, _)| bound == handle)
            .map(|(_, prefix)| prefix.as_slice())
            .or_else(|| (handle == b"!!").then_some(Self::PREFIX));
        prefix.is_some_and(|prefix| prefix.starts_with(Self::PREFIX))
    }

    /// Whether any of the node properties `props` is such a tag.
    fn any_in(&self, props: &[u8]) -> bool {
        props
            .split(u8::is_ascii_whitespace)
            .any(|token| self.retypes(token))
    }
}

/// Past the blank lines from the line start `at` on.
fn past_blank_lines(bytes: &[u8], mut at: usize) -> usize {
    while let Some(end) = bytes[at..].iter().position(|&b| b == b'\n')
        && bytes[at..at + end].iter().all(u8::is_ascii_whitespace)
    {
        at += end + 1;
    }
    at
}

/// The offset just past a leading UTF-8 byte order mark, or 0.
pub(crate) fn bom_end(bytes: &[u8]) -> usize {
    if bytes.starts_with("\u{feff}".as_bytes()) {
        "\u{feff}".len()
    } else {
        0
    }
}

/// Start of the line holding byte `pos`; on the first line, past a
/// byte-order mark — so that columns count from the first character, and
/// bytes removed or inserted at a line start leave the mark first.
pub(crate) fn line_start(bytes: &[u8], pos: usize) -> usize {
    const BOM: &[u8] = "\u{feff}".as_bytes();
    let pos = pos.min(bytes.len());
    bytes[..pos].iter().rposition(|&b| b == b'\n').map_or_else(
        || {
            if bytes.starts_with(BOM) {
                BOM.len().min(pos)
            } else {
                0
            }
        },
        |i| i + 1,
    )
}

/// `end`, the start of a line, moved past the comment-only lines that
/// follow it with their `#` at column `column` (0-based) or deeper, and the
/// blank lines between them; blank lines after the last such comment stay.
fn past_comment_lines(bytes: &[u8], end: usize, column: usize) -> usize {
    let mut out = end;
    let mut pos = end;
    while pos < bytes.len() {
        let next = bytes[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |at| pos + at + 1);
        let line = &bytes[pos..next];
        let indent = line.iter().take_while(|&&b| b == b' ').count();
        match line[indent..].first() {
            None | Some(b'\n' | b'\r') => {}
            Some(b'#') if indent >= column => out = next,
            Some(_) => break,
        }
        pos = next;
    }
    out
}

/// Of the recorded `scalar` child and the `collection` child spanning its
/// brackets, the one that starts later, the collection with a span over
/// its brackets in `context`.
fn last_by_start(
    scalar: Option<(String, ValueSpan)>,
    collection: Option<(String, std::ops::Range<usize>)>,
    context: SpanContext,
) -> Option<(String, ValueSpan)> {
    match (scalar, collection) {
        (Some(scalar), Some((_, value))) if scalar.1.value_range.start > value.start => {
            Some(scalar)
        }
        (_, Some((pointer, value))) => Some((
            pointer,
            ValueSpan {
                line_range: value.clone(),
                value_range: value,
                indent: 1,
                context,
            },
        )),
        (scalar, None) => scalar,
    }
}

/// Past the bracket closing the YAML flow collection that opens at
/// `within.start`, found before `within.end`; quoted scalars and comments
/// skipped.
fn yaml_flow_end(bytes: &[u8], within: std::ops::Range<usize>) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = within.start;
    while i < within.end {
        let prev = i.checked_sub(1).map(|p| bytes[p]);
        // A quote opens a scalar only where a scalar can begin.
        let scalar_start = prev.is_none_or(|p| {
            matches!(
                p,
                b'[' | b'{' | b',' | b':' | b'?' | b' ' | b'\t' | b'\n' | b'\r'
            )
        });
        match bytes[i] {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b'"' if scalar_start => loop {
                i += 1;
                match bytes.get(i)? {
                    b'\\' => i += 1,
                    b'"' => break,
                    _ => {}
                }
            },
            b'\'' if scalar_start => loop {
                i += 1;
                if *bytes.get(i)? == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 1;
                    } else {
                        break;
                    }
                }
            },
            b'#' if prev.is_some_and(|p| p.is_ascii_whitespace()) => {
                while i < within.end && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `bytes` parsed again write-aware as `format`; `None` for a format
/// without textual edits.
fn parse_again(format: FormatTag, bytes: &[u8]) -> Option<Result<Document>> {
    match format {
        FormatTag::Yaml => Some(crate::parsers::parse_yaml_with_spans(bytes)),
        FormatTag::Json => Some(crate::parsers::parse_json_with_spans(bytes)),
        FormatTag::Toml => Some(crate::format::Format::parse(&crate::parsers::Toml, bytes)),
        _ => None,
    }
}

/// The YAML edit layout of a document before an edit, worked out at most
/// once per edit — see [`Document::edit_layout`].
type LayoutCache = std::cell::OnceCell<Option<crate::parsers::yaml_spans::EditLayout>>;

/// Why a JSON `set` of NaN or Infinity is refused — by the JSON writer
/// too, so with no way around it.
const JSON_NON_FINITE: &str = "JSON has no NaN or Infinity";

/// Why a TOML `set` of a number past `i64` / `f64` is refused — by the
/// TOML writer too, so with no way around it. The number follows.
const TOML_BIG_NUMBER: &str = "TOML cannot represent arbitrary-precision number";

/// Why a TOML `set` of the root to a value that is no table is refused.
const TOML_ROOT_TABLE: &str = "a TOML document is a table, so its root is one";

/// Why `value` cannot be the root of a TOML document — the textual `set`
/// of the root and the TOML writer both refuse it so; `None` for a table.
pub(crate) fn toml_root_refusal(value: &Value) -> Option<String> {
    if matches!(value, Value::Map(_)) {
        return None;
    }
    Some(format!(
        "{TOML_ROOT_TABLE}, not {}",
        value.kind_with_article()
    ))
}

/// The way around an edit the read-back check refuses.
const JQ_REWRITE_HINT: &str =
    "`dq set --jq` rewrites the whole file through its writer instead, dropping comments";

/// Where a new flow entry goes relative to the sibling it follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowLayout {
    /// On the sibling's line: `, k: v`.
    SameLine,
    /// On a line of its own, its key at `column` (0-based) like the
    /// sibling's, after the line break the sibling's line ends in.
    OwnLine { column: usize, crlf: bool },
}

/// `(splice_at, splice_bytes)` putting the entry the insertion renderer
/// wrote as `fragment` right after the flow `sibling`, behind a comma. The
/// renderers write a block entry (a line break, indentation, a trailing
/// line break): only its `key: value` / `key = value` text is kept. A
/// sibling that is an empty node with a tag (`n: !!null`, `bare_tag`)
/// gets a blank before the comma: YAML 1.1 readers take `!!null,` for one
/// tag.
fn flow_sibling_splice(
    fragment: &[u8],
    sibling: &ValueSpan,
    layout: FlowLayout,
    bare_tag: bool,
) -> (usize, Vec<u8>) {
    let start = fragment
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .unwrap_or(fragment.len());
    let end = fragment
        .iter()
        .rposition(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .map_or(start, |last| last + 1);
    let entry = &fragment[start..end.max(start)];
    let mut out = Vec::with_capacity(entry.len() + 3);
    if bare_tag {
        out.push(b' ');
    }
    match layout {
        FlowLayout::SameLine => out.extend_from_slice(b", "),
        FlowLayout::OwnLine { column, crlf } => {
            out.extend_from_slice(if crlf { b",\r\n" } else { b",\n" });
            out.extend(std::iter::repeat_n(b' ', column));
        }
    }
    out.extend_from_slice(entry);
    (sibling.value_range.end, out)
}

/// `pos`, or — when it is not at the start of a line — just past the line
/// break that ends its line: the `line_range` of an empty `{}` / `[]`
/// ends at its bracket. Stays at `pos` when no line break follows.
fn past_line_break(bytes: &[u8], pos: usize) -> usize {
    if pos == 0 || pos > bytes.len() || bytes[pos - 1] == b'\n' {
        return pos;
    }
    bytes[pos..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(pos, |at| pos + at + 1)
}

/// Translate the per-format [`InsertionRenderer`] fragment into a final
/// `(splice_at, splice_bytes)` pair anchored against the parent's
/// **empty-container** span (i.e. the recorded `{}` / `[]` body).
///
/// Mirrors [`format_specific_splice`] but for the empty-parent mkdir-p
/// branch where there is no sibling to anchor against. The new key is
/// spliced inside the parent's delimiters so the result is well-formed:
///
/// - **JSON** (`{}` body): splice between the `{` and `}` at
///   `parent.value_range.start + 1` (or scan inside for the position just
///   before the closing `}` if there's interior whitespace). The renderer
///   produces `\n  "k": v` (no leading comma, no trailing newline); the
///   empty body has no preceding sibling so no leading comma is needed.
///   We append a trailing `\n` (with the closing `}`'s indent) so the
///   result reads as a single-key formatted object.
/// - **YAML** (flow-style `{}` / `[]`, or block-empty `parent:`):
///   - Flow form (parent value starts with `{` or `[`): splice just before
///     the closing `}` / `]`, emitting the fragment without its leading
///     `\n` because we're inside a single-line literal.
///   - Block form (parent value is empty / `~` / null on its own line):
///     splice at the end of the parent's line, emitting the fragment
///     as-is.
/// - **TOML** (inline `{}` / `[]`): splice just before the closing `}` /
///   `]`. The renderer emits `key = value\n` which we strip the trailing
///   `\n` from for inline contexts.
fn format_specific_empty_parent_splice(
    format: FormatTag,
    fragment: &[u8],
    parent_span: &ValueSpan,
    original_bytes: &[u8],
    one_line: bool,
) -> (usize, Vec<u8>) {
    match format {
        FormatTag::Json => {
            json_empty_parent_splice(fragment, parent_span, original_bytes, one_line)
        }
        FormatTag::Yaml => yaml_empty_parent_splice(fragment, parent_span, original_bytes),
        FormatTag::Toml => toml_empty_parent_splice(fragment, parent_span, original_bytes),
        // Read-only formats don't reach here — callers gate on the
        // insertion-renderer factory which returns `None` for them.
        // Defensive fallback: splice at parent_span.value_range.end (the
        // safest "append after" anchor) with the unmodified fragment.
        FormatTag::Jsonl
        | FormatTag::Hcl
        | FormatTag::Ini
        | FormatTag::DotEnv
        | FormatTag::Csv
        | FormatTag::Tsv
        | FormatTag::Dockerfile
        | FormatTag::IgnoreList
        | FormatTag::Frontmatter
        | FormatTag::Markdown
        | FormatTag::Xml => (parent_span.value_range.end, fragment.to_vec()),
    }
}

/// JSON empty-parent splice. Parent's `value_range` covers `{}` (or `{}` with
/// interior whitespace like `{ }` / `{\n}`). We splice before the `}`: the
/// fragment (`\n` + indent + `"k": v`) and a line break with the indent of
/// the line the `{` is on, so the `}` lines up with that line — or, when
/// `one_line` (the container around the `{}` is written on one line), the
/// entry alone, `{"k": v}`.
fn json_empty_parent_splice(
    fragment: &[u8],
    parent_span: &ValueSpan,
    original_bytes: &[u8],
    one_line: bool,
) -> (usize, Vec<u8>) {
    // The literal `{}` has the closing brace at `value_range.end - 1`.
    let close_pos = parent_span
        .value_range
        .end
        .saturating_sub(1)
        .min(original_bytes.len());
    if one_line {
        let entry = fragment
            .iter()
            .position(|&b| !matches!(b, b'\n' | b' '))
            .unwrap_or(fragment.len());
        return (close_pos, fragment[entry..].to_vec());
    }
    // `{` and `}` on lines of their own already: the entry goes right
    // after the `{`, onto a line of its own, and the `}` keeps its line.
    let open = parent_span.value_range.start + 1;
    if original_bytes
        .get(open..close_pos)
        .is_some_and(|inside| inside.contains(&b'\n'))
    {
        return (open, fragment.to_vec());
    }
    let indent = json_line_indent(original_bytes, parent_span.value_range.start);
    let mut out = Vec::with_capacity(fragment.len() + indent + 1);
    out.extend_from_slice(fragment);
    out.push(b'\n');
    out.extend(std::iter::repeat_n(b' ', indent));
    (close_pos, out)
}

/// The number of blanks the line holding byte `at` begins with.
fn json_line_indent(bytes: &[u8], at: usize) -> usize {
    let start = line_start(bytes, at.min(bytes.len()));
    bytes[start..]
        .iter()
        .take_while(|&&b| matches!(b, b' ' | b'\t'))
        .count()
}

/// YAML empty-parent splice. The parent's recorded `value_range` either
/// covers `{}` / `[]` (flow form) or is the empty range after `parent:`
/// (block form, where the parent has no children).
fn yaml_empty_parent_splice(
    fragment: &[u8],
    parent_span: &ValueSpan,
    original_bytes: &[u8],
) -> (usize, Vec<u8>) {
    let body_first = original_bytes.get(parent_span.value_range.start).copied();
    match body_first {
        Some(b'{' | b'[') => {
            // Flow form: splice before the closing `}` / `]`. Strip the
            // fragment's leading `\n` so we stay on a single line, and
            // surround the fragment with `,` if needed. Empty body → no
            // leading comma. The flow renderer would emit something like
            // `\n  k: v\n`; we want `k: v` instead.
            let close_pos = parent_span.value_range.end.saturating_sub(1);
            let trimmed: &[u8] = fragment.strip_prefix(b"\n").unwrap_or(fragment);
            // Strip leading spaces (renderer indented for block context).
            let mut start = 0_usize;
            while start < trimmed.len() && trimmed[start] == b' ' {
                start += 1;
            }
            let trimmed = &trimmed[start..];
            // Strip trailing newline if any.
            let trimmed_end = trimmed.strip_suffix(b"\n").unwrap_or(trimmed);
            (close_pos, trimmed_end.to_vec())
        }
        _ => {
            // Block form: the parent's value_range is the empty range
            // after the colon. Splice at line_range.end (start of the
            // line after the parent's `parent:` line). The fragment is
            // emitted as-is (the renderer already produced
            // `\n  key: value\n` which we want to strip the leading `\n`
            // from because we're already on a fresh line).
            let trimmed: &[u8] = fragment.strip_prefix(b"\n").unwrap_or(fragment);
            let mut splice_at = parent_span.line_range.end;
            let mut out = Vec::with_capacity(trimmed.len() + 1);
            if splice_at == 0 || original_bytes.get(splice_at - 1) != Some(&b'\n') {
                out.push(b'\n');
            }
            out.extend_from_slice(trimmed);
            if splice_at > original_bytes.len() {
                splice_at = original_bytes.len();
            }
            (splice_at, out)
        }
    }
}

/// TOML empty-parent splice. The parent's recorded span covers an empty
/// inline `{}` or `[]`. Splice between the delimiters, on a single line.
fn toml_empty_parent_splice(
    fragment: &[u8],
    parent_span: &ValueSpan,
    original_bytes: &[u8],
) -> (usize, Vec<u8>) {
    let close_pos = parent_span.value_range.end.saturating_sub(1);
    // TomlInsertionRenderer emits `key = value\n`. For inline contexts we
    // want `key = value` (no trailing newline). Add a single space margin
    // inside the braces so the result reads as `{ key = value }` rather
    // than `{key = value}`.
    let trimmed = fragment.strip_suffix(b"\n").unwrap_or(fragment);
    // If the parent body is currently `{}` (adjacent braces) we need a
    // space before AND after the inserted key; if it's `{ }` (already
    // spaced), one side already has whitespace. Check the byte just before
    // the close.
    let needs_leading_space = !matches!(
        original_bytes.get(close_pos.saturating_sub(1)),
        Some(b' ' | b'\t')
    );
    let needs_trailing_space = !matches!(original_bytes.get(close_pos), Some(b' ' | b'\t'));
    let mut out = Vec::with_capacity(
        trimmed.len() + needs_leading_space as usize + needs_trailing_space as usize,
    );
    if needs_leading_space {
        out.push(b' ');
    }
    out.extend_from_slice(trimmed);
    if needs_trailing_space {
        out.push(b' ');
    }
    (close_pos, out)
}

/// Walk `pointer` through the in-memory `Value` tree and return the canonical
/// form of the longest existing prefix.
///
/// This is the write-side counterpart to [`longest_existing_prefix`]: parser
/// span maps only record scalar leaves (containers do not get their own
/// `ValueSpan`), so a SpanMap-only walk under-reports the matched prefix for
/// write errors whose pointer descends through container keys. The Value
/// tree has the truth and is the source the read-side [`Pointer::resolve`]
/// already uses.
/// How many leading segments of `pointer` resolve in `root`.
fn existing_prefix_len(root: &Value, pointer: &Pointer) -> usize {
    let segments = pointer.segments();
    (0..=segments.len())
        .rev()
        .find(|&len| Pointer::new(segments[..len].to_vec()).resolve(root).is_ok())
        .unwrap_or(0)
}

/// Whether the pointer segment `segment` is an array index.
fn is_index(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit())
}

/// The TOML insertion of a chain of missing tables as a dotted key: a
/// block entry `chain.leaf = value` (`b.c = 2`), whatever key and value
/// the mkdir-p insertion asks for — the first missing key holding the
/// rest; inside an inline table, that nested value as asked.
struct TomlDottedRenderer<'a> {
    chain: &'a [String],
    leaf: &'a str,
    value: &'a Value,
}

impl crate::textual_edit::InsertionRenderer for TomlDottedRenderer<'_> {
    fn render_insertion(
        &self,
        key: &str,
        value: &Value,
        parent_indent: u32,
        parent_context: SpanContext,
    ) -> Vec<u8> {
        let inner = crate::parsers::toml::TomlInsertionRenderer;
        if matches!(
            parent_context,
            SpanContext::FlowMapValue | SpanContext::FlowSeqItem
        ) {
            return inner.render_insertion(key, value, parent_indent, parent_context);
        }
        let mut out = inner.render_insertion(self.leaf, self.value, parent_indent, parent_context);
        let at = out.len() - out.trim_ascii_start().len();
        let mut prefix = Vec::new();
        for key in self.chain {
            prefix.extend(crate::parsers::toml::toml_key_token(key).into_bytes());
            prefix.push(b'.');
        }
        out.splice(at..at, prefix);
        out
    }
}

fn longest_existing_value_prefix(root: &Value, pointer: &Pointer) -> String {
    let mut current = root;
    let mut matched: Vec<Segment> = Vec::new();
    for seg in pointer.segments() {
        match (current, seg) {
            (Value::Map(map), Segment::Key(k)) => {
                if let Some(next) = map.get(k) {
                    current = next;
                    matched.push(Segment::Key(k.clone()));
                } else {
                    break;
                }
            }
            (Value::Array(items), Segment::Index(i)) => {
                if let Some(next) = items.get(*i) {
                    current = next;
                    matched.push(Segment::Index(*i));
                } else {
                    break;
                }
            }
            (Value::Array(items), Segment::Key(k)) => {
                // RFC 6901 keeps numeric segments as strings; coerce when the
                // container is an array, mirroring `Pointer::resolve`.
                if let Ok(idx) = k.parse::<usize>()
                    && let Some(next) = items.get(idx)
                {
                    current = next;
                    matched.push(Segment::Index(idx));
                    continue;
                }
                break;
            }
            _ => break,
        }
    }
    Pointer::new(matched).as_canonical()
}

/// Set the value at `pointer` inside `root`, extending the tree with empty
/// containers as needed (mkdir-p semantics).
///
/// This helper keeps the in-memory `Value` tree consistent with the source
/// buffer after `set_at` patches the bytes. It is intentionally simple —
/// the source-of-truth for write-pat is the byte buffer; this is
/// best-effort housekeeping so reads via `value()` reflect the write.
fn set_value_at(root: &mut Value, pointer: &Pointer, value: Value) -> Result<()> {
    let segs = pointer.segments();
    if segs.is_empty() {
        *root = value;
        return Ok(());
    }
    // Track every segment successfully descended into so `Error::Path`
    // surfaces the longest matching prefix, mirroring the read-side
    // `Pointer::resolve` accumulator.
    let mut matched: Vec<Segment> = Vec::new();
    let mut current = root;
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i + 1 == segs.len();
        match (&mut *current, seg) {
            (Value::Map(map), Segment::Key(k)) => {
                if is_last {
                    map.insert(k.clone(), value);
                    return Ok(());
                }
                if !map.contains_key(k) {
                    map.insert(k.clone(), Value::Map(IndexMap::new()));
                }
                current = map.get_mut(k).expect("just inserted or already present");
                matched.push(Segment::Key(k.clone()));
            }
            (Value::Array(items), Segment::Index(idx)) => {
                if *idx < items.len() {
                    if is_last {
                        items[*idx] = value;
                        return Ok(());
                    }
                    current = &mut items[*idx];
                    matched.push(Segment::Index(*idx));
                } else {
                    return Err(Error::Path {
                        pointer: pointer.as_canonical(),
                        matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                        kind: PathErrorKind::OutOfBounds,
                        did_you_mean: Vec::new(),
                    });
                }
            }
            (Value::Array(items), Segment::Key(k)) => {
                // RFC 6902 §4.1 array-append marker `-` resolves to "the
                // position past the end of the array". This is only meaningful
                // when the parent array already exists — M2's mkdir-p baseline
                // does NOT create new array containers — but for an existing
                // array we treat `-` as `items.len()` and append.
                if k == "-" {
                    if is_last {
                        items.push(value);
                        return Ok(());
                    }
                    return Err(Error::Path {
                        pointer: pointer.as_canonical(),
                        matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                        kind: PathErrorKind::TypeMismatch {
                            expected: "leaf segment",
                            found: "array-append marker '-' used mid-path",
                        },
                        did_you_mean: Vec::new(),
                    });
                }
                // RFC 6901 keeps numeric segments as strings at parse time;
                // resolve them to indices when the container is an array.
                let idx: usize = k.parse().map_err(|_| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "array index",
                        found: "non-numeric key",
                    },
                    did_you_mean: Vec::new(),
                })?;
                if idx < items.len() {
                    if is_last {
                        items[idx] = value;
                        return Ok(());
                    }
                    current = &mut items[idx];
                    matched.push(Segment::Index(idx));
                } else {
                    return Err(Error::Path {
                        pointer: pointer.as_canonical(),
                        matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                        kind: PathErrorKind::OutOfBounds,
                        did_you_mean: Vec::new(),
                    });
                }
            }
            (other, _) => {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "object or array",
                        found: other.type_name(),
                    },
                    did_you_mean: Vec::new(),
                });
            }
        }
    }
    Ok(())
}

/// Remove the value at `pointer` from `root`. Errors when the pointer
/// addresses a non-existent path or the root.
fn delete_value_at(root: &mut Value, pointer: &Pointer) -> Result<()> {
    let segs = pointer.segments();
    if segs.is_empty() {
        // Root deletion fires before any segment is consumed — empty
        // matched_prefix is the correct semantics here.
        return Err(Error::Path {
            pointer: String::new(),
            matched_prefix: String::new(),
            kind: PathErrorKind::DeleteRoot,
            did_you_mean: Vec::new(),
        });
    }
    // Walk to the parent, tracking every segment we descend into so
    // `Error::Path` surfaces the longest matching prefix.
    let (last, parent_segs) = segs.split_last().expect("segs non-empty");
    let mut matched: Vec<Segment> = Vec::new();
    let mut current: &mut Value = root;
    for seg in parent_segs {
        match (&mut *current, seg) {
            (Value::Map(map), Segment::Key(k)) => {
                current = map.get_mut(k).ok_or_else(|| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::MissingKey,
                    did_you_mean: Vec::new(),
                })?;
                matched.push(Segment::Key(k.clone()));
            }
            (Value::Array(items), Segment::Index(idx)) => {
                current = items.get_mut(*idx).ok_or_else(|| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                })?;
                matched.push(Segment::Index(*idx));
            }
            (Value::Array(items), Segment::Key(k)) => {
                let idx: usize = k.parse().map_err(|_| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "array index",
                        found: "non-numeric key",
                    },
                    did_you_mean: Vec::new(),
                })?;
                current = items.get_mut(idx).ok_or_else(|| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                })?;
                matched.push(Segment::Index(idx));
            }
            (other, _) => {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "object or array",
                        found: other.type_name(),
                    },
                    did_you_mean: Vec::new(),
                });
            }
        }
    }
    match (current, last) {
        (Value::Map(map), Segment::Key(k)) => {
            if map.shift_remove(k).is_none() {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::MissingKey,
                    did_you_mean: Vec::new(),
                });
            }
        }
        (Value::Array(items), Segment::Index(idx)) => {
            if *idx >= items.len() {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                });
            }
            items.remove(*idx);
        }
        (Value::Array(items), Segment::Key(k)) => {
            // RFC 6902 §4.1 reserves `-` as an array-append marker; it has no
            // meaning for delete operations. Surface a clear TypeMismatch so
            // callers don't have to introspect the pointer themselves.
            if k == "-" {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "array index",
                        found: "array-append marker '-' is not deletable",
                    },
                    did_you_mean: Vec::new(),
                });
            }
            let idx: usize = k.parse().map_err(|_| Error::Path {
                pointer: pointer.as_canonical(),
                matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                kind: PathErrorKind::TypeMismatch {
                    expected: "array index",
                    found: "non-numeric key",
                },
                did_you_mean: Vec::new(),
            })?;
            if idx >= items.len() {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                });
            }
            items.remove(idx);
        }
        (other, _) => {
            return Err(Error::Path {
                pointer: pointer.as_canonical(),
                matched_prefix: Pointer::new(matched.clone()).as_canonical(),
                kind: PathErrorKind::TypeMismatch {
                    expected: "object or array",
                    found: other.type_name(),
                },
                did_you_mean: Vec::new(),
            });
        }
    }
    Ok(())
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(n) => write!(f, "{n}"),
            Self::BigInt(s) | Self::BigFloat(s) => f.write_str(s),
            Self::Float(n) => f.write_str(&number::shortest_float(*n)),
            Self::String(s) => write!(f, "{s:?}"),
            Self::Array(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Self::Map(map) => {
                f.write_str("{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k:?}: {v}")?;
                }
                f.write_str("}")
            }
        }
    }
}

impl fmt::Display for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.multi_doc
            && let Value::Array(docs) = &self.value
        {
            for (i, d) in docs.iter().enumerate() {
                if i > 0 {
                    f.write_str("\n---\n")?;
                }
                write!(f, "{d}")?;
            }
            return Ok(());
        }
        write!(f, "{}", self.value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Self::Array(value)
    }
}

impl From<IndexMap<String, Value>> for Value {
    fn from(value: IndexMap<String, Value>) -> Self {
        Self::Map(value)
    }
}

impl serde::Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(b) => serializer.serialize_bool(*b),
            Self::Int(n) => serializer.serialize_i64(*n),
            Self::Float(n) => serializer.serialize_f64(*n),
            Self::String(s) => serializer.serialize_str(s),
            // Generic fallback for arbitrary-precision values: emit as a string
            // token. JSON byte-for-byte round-trip of `BigInt`/`BigFloat` is
            // handled by the dedicated JSON writer, not the generic
            // `Serialize` impl, because not every serializer can represent
            // them as numeric tokens without precision loss.
            // `.nan` / `.inf` are floats every serializer has a token for.
            Self::BigFloat(s) if let Some(f) = non_finite_literal(s) => serializer.serialize_f64(f),
            Self::BigInt(s) | Self::BigFloat(s) => serializer.serialize_str(s),
            Self::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Map(map) => {
                let mut m = serializer.serialize_map(Some(map.len()))?;
                for (k, v) in map {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Format;
    use pretty_assertions::assert_eq;

    /// Build a `ValueSpan` for a single-line block-mapping scalar covering
    /// `[start..end)` for the value and `[line_start..line_end)` for the
    /// physical line. Centralising the boilerplate keeps each test focused
    /// on the behaviour under inspection rather than struct construction.
    fn block_map_span(
        value_start: usize,
        value_end: usize,
        line_start: usize,
        line_end: usize,
    ) -> ValueSpan {
        ValueSpan {
            value_range: value_start..value_end,
            line_range: line_start..line_end,
            indent: 0,
            context: SpanContext::BlockMapValue,
        }
    }

    /// Helper: build a single-key Map with one `i64` entry. Used by tests
    /// that need a tree-shaped `Value` matching the manually-built
    /// `original_bytes` / `SpanMap`.
    fn map_one(key: &str, value: Value) -> Value {
        let mut m = IndexMap::new();
        m.insert(key.to_owned(), value);
        Value::Map(m)
    }

    #[test]
    fn type_name_covers_every_variant() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(Value::Int(0).type_name(), "int");
        assert_eq!(Value::BigInt("1".into()).type_name(), "int");
        assert_eq!(Value::Float(0.0).type_name(), "float");
        assert_eq!(Value::BigFloat("1.0".into()).type_name(), "float");
        assert_eq!(Value::String("x".into()).type_name(), "string");
        assert_eq!(Value::Array(vec![]).type_name(), "array");
        assert_eq!(Value::Map(IndexMap::new()).type_name(), "object");
    }

    #[test]
    fn document_leaf_type_name() {
        assert_eq!(Document::single(Value::Null).leaf_type_name(), "null");
        assert_eq!(Document::multi(vec![]).leaf_type_name(), "array");
    }

    #[test]
    fn from_conversions_produce_expected_variants() {
        assert!(matches!(Value::from(true), Value::Bool(true)));
        assert!(matches!(Value::from(7_i64), Value::Int(7)));
        assert!(matches!(Value::from("s"), Value::String(_)));
    }

    #[test]
    fn value_only_constructor_has_empty_metadata() {
        // Pre-Section 3 contract: documents built without spans must NOT
        // accept writes — the renderer factory cannot do anything with
        // them yet. This test pins the contract so Section 3 work cannot
        // accidentally regress it.
        let doc = Document::value_only(Value::Null, FormatTag::Yaml);
        assert!(doc.original_bytes().is_empty());
        assert!(doc.spans().is_empty());
        assert_eq!(doc.format(), FormatTag::Yaml);
        assert!(!doc.is_multi());
    }

    #[test]
    fn original_bytes_returns_empty_for_value_only() {
        // Distinct from `value_only_constructor_has_empty_metadata`: that
        // test pins the *constructor's* shape; this one pins the public
        // accessor. If a future refactor decoupled the two — e.g. by
        // synthesising bytes lazily — we want the accessor contract to
        // stay observable.
        let doc = Document::value_only(Value::Bool(true), FormatTag::Json);
        assert!(
            doc.original_bytes().is_empty(),
            "value_only documents must report empty original_bytes; got {} bytes",
            doc.original_bytes().len(),
        );
    }

    #[test]
    fn value_only_document_rejects_set_at() {
        // Section 2 smoke test: a read-only document must reject every
        // write attempt with `Error::WriteUnavailable`. Section 3 will add
        // a write-aware constructor that replaces this rejection with a
        // real splice; the contract for `value_only`-shaped documents
        // stays "no writes allowed".
        let mut doc = Document::value_only(Value::Null, FormatTag::Yaml);
        let pointer = Pointer::parse("/a").expect("/a parses");
        let result = doc.set_at(&pointer, Value::Bool(true));
        match result {
            Err(Error::WriteUnavailable { reason }) => {
                assert!(
                    reason.contains("read-only"),
                    "WriteUnavailable reason must mention `read-only` so users can act on it; got: {reason}",
                );
            }
            other => panic!("expected WriteUnavailable, got: {other:?}"),
        }
    }

    #[test]
    fn value_only_document_rejects_del_at() {
        // Mirror of `value_only_document_rejects_set_at` for the delete
        // path — a read-only document is symmetric on both write ops.
        let mut doc = Document::value_only(Value::Null, FormatTag::Yaml);
        let pointer = Pointer::parse("/a").expect("/a parses");
        let result = doc.del_at(&pointer);
        match result {
            Err(Error::WriteUnavailable { reason }) => {
                assert!(
                    reason.contains("read-only"),
                    "WriteUnavailable reason must mention `read-only`; got: {reason}",
                );
            }
            other => panic!("expected WriteUnavailable, got: {other:?}"),
        }
    }

    #[test]
    fn set_at_in_span_replaces_value_bytes_with_yaml_renderer() {
        // After M2 §3 the YAML renderer is registered, so an in-span
        // `set_at` with a populated `SpanMap` must perform the byte-splice
        // replacement and update both `original_bytes` and the in-memory
        // value tree. Pre-§3 this same assertion expected `WriteUnavailable`
        // — the renderer registration in `textual_edit::renderer_for_format`
        // is what flipped the contract.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(3, 4, 0, 5));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"a: 3\n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let pointer = Pointer::parse("/a").unwrap();
        doc.set_at(&pointer, Value::Int(5))
            .expect("set_at must succeed once YAML renderer is registered");
        assert_eq!(
            doc.original_bytes(),
            b"a: 5\n",
            "set_at must splice exactly the value range, leaving every other byte untouched",
        );
        // The in-memory tree mirrors the byte buffer.
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(5))),
            other => panic!("expected map, got: {other:?}"),
        }
        // The span for the modified pointer is patched in place; here the
        // new bytes happen to be the same length, so the range is unchanged.
        let span_a = doc.span_at(&pointer).expect("/a span still present");
        assert_eq!(
            span_a.value_range,
            3..4,
            "single-byte → single-byte replacement leaves the value_range identical",
        );
    }

    #[test]
    fn set_at_missing_pointer_for_xml_returns_error() {
        // XML has no registered `InsertionRenderer` (mkdir-p is not yet wired
        // for XML's conventional-key shape) and no registered scalar
        // renderer either — so a pointer the SpanMap doesn't contain must
        // surface either `Error::WriteUnavailable` or `Error::Path` rather
        // than mutating the bytes. This is the regression guard the
        // mkdir-p task spec mandates: read-only formats stay read-only.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(3, 4, 0, 5));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"<root><a>3</a></root>".to_vec(),
            spans,
            FormatTag::Xml,
        );
        let pointer = Pointer::parse("/x").unwrap();
        let result = doc.set_at(&pointer, Value::Int(7));
        match result {
            Err(Error::Path {
                pointer: ptr,
                kind: PathErrorKind::MissingKey,
                ..
            }) => assert_eq!(ptr, "/x"),
            Err(Error::WriteUnavailable { reason }) => assert!(
                reason.contains("Xml") || reason.contains("xml"),
                "WriteUnavailable reason must mention the format; got: {reason}",
            ),
            other => {
                panic!("expected Path/MissingKey or WriteUnavailable for XML, got: {other:?}",)
            }
        }
        // A failed `set_at` is a strict no-op on the source buffer.
        assert_eq!(
            doc.original_bytes(),
            b"<root><a>3</a></root>",
            "failed set_at must not mutate original_bytes",
        );
    }

    #[test]
    fn set_at_mkdir_p_creates_missing_key_in_json() {
        // Bug #1 contract: `dq set /a/c VAL` on `{"a":{"b":1}}` must succeed
        // by splicing the new sibling next to `/a/b`. Verifies both the
        // rendered bytes and the in-memory `Value` tree, plus that the new
        // pointer now has a SpanMap entry so a follow-up `set_at(/a/c, V2)`
        // routes through the in-span replace path (idempotency).
        let bytes = br#"{"a":{"b":1}}"#;
        let mut doc = crate::parsers::json::Json.parse(bytes).expect("parse JSON");
        let pointer = Pointer::parse("/a/c").unwrap();
        doc.set_at(&pointer, Value::Int(42))
            .expect("mkdir-p set_at on JSON sibling must succeed");
        // Bytes must round-trip as a parseable JSON document with the new key.
        let rendered = doc.original_bytes();
        let parsed: serde_json::Value =
            serde_json::from_slice(rendered).expect("rendered bytes must re-parse as JSON");
        assert_eq!(parsed.pointer("/a/c"), Some(&serde_json::json!(42)));
        assert_eq!(parsed.pointer("/a/b"), Some(&serde_json::json!(1)));
        // Tree mirrors the bytes.
        let added = pointer.resolve(doc.value()).expect("Value tree has /a/c");
        assert_eq!(added, &Value::Int(42));
        // SpanMap carries an entry for the new pointer.
        assert!(
            doc.span_at(&pointer).is_some(),
            "mkdir-p must record a SpanMap entry for the new pointer so subsequent set_at routes through the in-span replace path",
        );
    }

    #[test]
    fn set_at_mkdir_p_creates_missing_key_in_yaml() {
        // Same shape as the JSON test, exercised against the YAML parser so
        // we know the renderer's leading-`\n` strip + line_range.end splice
        // produces well-formed YAML.
        let bytes = b"a:\n  b: 1\n";
        let mut doc = crate::parsers::yaml_spans::parse_yaml_with_spans(bytes).expect("parse YAML");
        let pointer = Pointer::parse("/a/c").unwrap();
        doc.set_at(&pointer, Value::Int(42))
            .expect("mkdir-p set_at on YAML sibling must succeed");
        let rendered = doc.original_bytes().to_vec();
        // Round-trip: re-parse the rendered bytes through the same YAML
        // parser and confirm both keys are present at the right depths.
        let reparsed =
            crate::parsers::yaml_spans::parse_yaml_with_spans(&rendered).expect("re-parse YAML");
        let new_at_c = pointer
            .resolve(reparsed.value())
            .expect("reparsed tree has /a/c");
        assert_eq!(new_at_c, &Value::Int(42));
        let old_b = Pointer::parse("/a/b")
            .unwrap()
            .resolve(reparsed.value())
            .expect("reparsed tree still has /a/b");
        assert_eq!(old_b, &Value::Int(1));
        // In-memory tree mirrors the bytes.
        let added = pointer.resolve(doc.value()).expect("Value tree has /a/c");
        assert_eq!(added, &Value::Int(42));
        assert!(
            doc.span_at(&pointer).is_some(),
            "mkdir-p must record a SpanMap entry for the new pointer",
        );
    }

    #[test]
    fn set_at_mkdir_p_creates_missing_key_in_toml() {
        // TOML root-level mkdir-p: parent is the implicit root table.
        let bytes = b"a = 1\nb = 2\n";
        let mut doc = crate::parsers::toml::Toml.parse(bytes).expect("parse TOML");
        let pointer = Pointer::parse("/c").unwrap();
        doc.set_at(&pointer, Value::Int(42))
            .expect("mkdir-p set_at on TOML sibling must succeed");
        let rendered = doc.original_bytes().to_vec();
        // Round-trip: re-parse rendered bytes through the same TOML parser
        // and confirm structural integrity (all three keys present).
        let reparsed = crate::parsers::toml::Toml
            .parse(&rendered)
            .expect("re-parse TOML");
        let new_at_c = Pointer::parse("/c")
            .unwrap()
            .resolve(reparsed.value())
            .expect("reparsed tree has /c");
        assert_eq!(new_at_c, &Value::Int(42));
        let added = pointer.resolve(doc.value()).expect("Value tree has /c");
        assert_eq!(added, &Value::Int(42));
        assert!(
            doc.span_at(&pointer).is_some(),
            "mkdir-p must record a SpanMap entry for the new pointer",
        );
    }

    #[test]
    fn set_at_mkdir_p_is_idempotent_in_json() {
        // Idempotency contract: applying the same mkdir-p set twice must
        // yield byte-identical output (second call routes through the
        // in-span replace path because the first inserted a SpanMap entry).
        let bytes = br#"{"a":{"b":1}}"#;
        let mut doc = crate::parsers::json::Json.parse(bytes).expect("parse JSON");
        let pointer = Pointer::parse("/a/c").unwrap();
        doc.set_at(&pointer, Value::Int(42)).expect("first mkdir-p");
        let after_first = doc.original_bytes().to_vec();
        doc.set_at(&pointer, Value::Int(42))
            .expect("second set_at on now-existing key");
        assert_eq!(
            doc.original_bytes(),
            after_first.as_slice(),
            "idempotency: set_at(/a/c, 42) twice must produce identical bytes",
        );
    }

    #[test]
    fn set_at_mkdir_p_creates_a_missing_chain_of_mappings() {
        let bytes = br#"{"a":{"b":1}}"#;
        let mut doc = crate::parsers::json::Json.parse(bytes).expect("parse JSON");
        doc.set_at(&Pointer::parse("/a/missing/leaf").unwrap(), Value::Int(42))
            .expect("multi-level mkdir-p");
        assert_eq!(
            std::str::from_utf8(doc.original_bytes()).unwrap(),
            r#"{"a":{"b":1, "missing": {"leaf":42}}}"#
        );
        // Below a scalar, or with `-` on the way, nothing is made.
        for pointer in ["/a/b/x/y", "/a/-/y"] {
            let err = doc
                .set_at(&Pointer::parse(pointer).unwrap(), Value::Int(1))
                .expect_err(pointer);
            assert!(
                matches!(&err, Error::Path { pointer: at, kind: PathErrorKind::MissingKey, .. } if at == pointer),
                "{err:?}"
            );
        }
    }

    #[test]
    fn set_at_mkdir_p_creates_key_in_empty_json_nested() {
        // Nested empty-parent case: `{"a":{}}` → `set_at(/a/b, 42)` →
        // `{"a":{ ... "b": 42 ... }}`. Pre-fix this returned MissingKey
        // because the JSON scanner only recorded scalar leaf spans; the
        // fix added empty-container span recording so the splicer can
        // anchor inside the parent's `{}` body. RFC 7396 §2 conformance
        // (merging a non-empty object into an empty target) depends on
        // this path firing.
        let bytes = br#"{"a":{}}"#;
        let mut doc = crate::parsers::json::Json.parse(bytes).expect("parse JSON");
        let pointer = Pointer::parse("/a/b").unwrap();
        doc.set_at(&pointer, Value::Int(42))
            .expect("empty-parent mkdir-p on nested `{}` must succeed");
        let rendered = doc.original_bytes();
        let parsed: serde_json::Value =
            serde_json::from_slice(rendered).expect("rendered bytes must re-parse as JSON");
        assert_eq!(parsed.pointer("/a/b"), Some(&serde_json::json!(42)));
        // Tree mirrors the bytes.
        let added = pointer.resolve(doc.value()).expect("Value tree has /a/b");
        assert_eq!(added, &Value::Int(42));
        assert!(
            doc.span_at(&pointer).is_some(),
            "mkdir-p must record a SpanMap entry for the new pointer so a follow-up set_at routes through the in-span replace path",
        );
    }

    #[test]
    fn set_at_mkdir_p_creates_key_in_empty_json_root() {
        // Root-level empty-parent case: `{}` → `set_at(/a, 42)` →
        // `{"a": 42}`. This is the common merge-into-empty target case
        // that RFC 7396 §2 requires; pre-fix this returned MissingKey
        // with `matched_prefix = ''`.
        let bytes = b"{}";
        let mut doc = crate::parsers::json::Json.parse(bytes).expect("parse JSON");
        let pointer = Pointer::parse("/a").unwrap();
        doc.set_at(&pointer, Value::Int(42))
            .expect("empty-root mkdir-p must succeed");
        let rendered = doc.original_bytes();
        let parsed: serde_json::Value =
            serde_json::from_slice(rendered).expect("rendered bytes must re-parse as JSON");
        assert_eq!(parsed.pointer("/a"), Some(&serde_json::json!(42)));
        let added = pointer.resolve(doc.value()).expect("Value tree has /a");
        assert_eq!(added, &Value::Int(42));
        assert!(
            doc.span_at(&pointer).is_some(),
            "mkdir-p must record a SpanMap entry for the new pointer",
        );
    }

    #[test]
    fn set_at_mkdir_p_creates_key_in_empty_yaml_root() {
        // YAML root-empty case: `{}` (flow-style root) → `set_at(/a, 42)`
        // must succeed. Mirror of `set_at_mkdir_p_creates_key_in_empty_json_root`
        // exercised through the YAML span builder.
        let bytes = b"{}\n";
        let mut doc = crate::parsers::yaml_spans::parse_yaml_with_spans(bytes).expect("parse YAML");
        let pointer = Pointer::parse("/a").unwrap();
        doc.set_at(&pointer, Value::Int(42))
            .expect("empty-root YAML mkdir-p must succeed");
        let reparsed = crate::parsers::yaml_spans::parse_yaml_with_spans(doc.original_bytes())
            .expect("re-parse YAML");
        let added = pointer
            .resolve(reparsed.value())
            .expect("reparsed tree has /a");
        assert_eq!(added, &Value::Int(42));
        let added_in_doc = pointer.resolve(doc.value()).expect("doc tree has /a");
        assert_eq!(added_in_doc, &Value::Int(42));
        assert!(
            doc.span_at(&pointer).is_some(),
            "mkdir-p must record a SpanMap entry for the new pointer",
        );
    }

    #[test]
    fn set_at_mkdir_p_empty_parent_is_idempotent() {
        // Idempotency: running the same empty-parent set twice must yield
        // byte-identical output. The first call goes through the empty-
        // parent splice path; the second routes through the in-span
        // replace path because the first inserted a SpanMap entry for the
        // new leaf.
        let bytes = b"{}";
        let mut doc = crate::parsers::json::Json.parse(bytes).expect("parse JSON");
        let pointer = Pointer::parse("/a").unwrap();
        doc.set_at(&pointer, Value::Int(42))
            .expect("first empty-root mkdir-p");
        let after_first = doc.original_bytes().to_vec();
        doc.set_at(&pointer, Value::Int(42))
            .expect("second set_at on now-existing key");
        assert_eq!(
            doc.original_bytes(),
            after_first.as_slice(),
            "idempotency: empty-parent set followed by in-span replace of same value must produce identical bytes",
        );
    }

    #[test]
    fn set_at_in_span_renderer_missing_for_unregistered_format() {
        // Pinning the negative case: a write-aware document carrying a
        // format whose renderer hasn't landed yet (JSONL is line-oriented
        // and has no in-place mutation in M2) must still report
        // `WriteUnavailable` rather than silently no-op'ing. Pre-§5 the
        // same assertion targeted JSON; §5 flipped JSON to a registered
        // renderer so this test now covers JSONL.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(5, 6, 0, 7));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"{\"a\":3}\n".to_vec(),
            spans,
            FormatTag::Jsonl,
        );
        let pointer = Pointer::parse("/a").unwrap();
        let result = doc.set_at(&pointer, Value::Int(5));
        match result {
            Err(Error::WriteUnavailable { reason }) => {
                assert!(
                    reason.contains("jsonl has no textual edits"),
                    "JSONL write must say it has no textual edits; got: {reason}",
                );
            }
            other => panic!("expected WriteUnavailable, got: {other:?}"),
        }
        assert_eq!(
            doc.original_bytes(),
            b"{\"a\":3}\n",
            "failed set_at must not mutate original_bytes",
        );
    }

    #[test]
    fn set_at_in_span_replaces_value_bytes_with_json_renderer() {
        // §5 contract mirror of `set_at_in_span_replaces_value_bytes_with_yaml_renderer`:
        // a write-aware JSON document with a populated SpanMap must perform
        // the byte-splice replacement once the JSON renderer is registered.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(6, 7, 0, 8));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"{\"a\": 3}".to_vec(),
            spans,
            FormatTag::Json,
        );
        let pointer = Pointer::parse("/a").unwrap();
        doc.set_at(&pointer, Value::Int(5))
            .expect("set_at must succeed once JSON renderer is registered");
        assert_eq!(
            doc.original_bytes(),
            b"{\"a\": 5}",
            "set_at must splice exactly the value range",
        );
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(5))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn set_at_in_span_replaces_value_bytes_with_toml_renderer() {
        // §4 contract mirror of `set_at_in_span_replaces_value_bytes_with_yaml_renderer`:
        // a write-aware TOML document with a populated SpanMap must perform
        // the byte-splice replacement once the TOML renderer is registered.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(4, 5, 0, 6));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"a = 3\n".to_vec(),
            spans,
            FormatTag::Toml,
        );
        let pointer = Pointer::parse("/a").unwrap();
        doc.set_at(&pointer, Value::Int(5))
            .expect("set_at must succeed once TOML renderer is registered");
        assert_eq!(
            doc.original_bytes(),
            b"a = 5\n",
            "set_at must splice exactly the value range",
        );
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(5))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn del_at_removes_line_range() {
        // Three-line YAML: deleting `/b` must splice out the entire
        // `b: 2\n` line, leaving `a: 1\nc: 3\n`. The trailing newline is
        // included in `line_range` precisely so `del_at` doesn't leave
        // blank lines behind.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(3, 4, 0, 5));
        spans.insert("/b".into(), block_map_span(8, 9, 5, 10));
        spans.insert("/c".into(), block_map_span(13, 14, 10, 15));
        let mut value = IndexMap::new();
        value.insert("a".into(), Value::Int(1));
        value.insert("b".into(), Value::Int(2));
        value.insert("c".into(), Value::Int(3));
        let mut doc = Document::with_spans(
            Value::Map(value),
            b"a: 1\nb: 2\nc: 3\n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let pointer = Pointer::parse("/b").unwrap();
        doc.del_at(&pointer).expect("del_at /b must succeed");
        assert_eq!(
            doc.original_bytes(),
            b"a: 1\nc: 3\n",
            "del_at must splice out the whole physical line including trailing newline",
        );
    }

    #[test]
    fn del_at_recompute_shifts_subsequent_spans() {
        // After deleting `/b` (5 bytes for `b: 2\n`), span `/a` is to the
        // left of the edit and must NOT move; span `/c` was at byte 13
        // and must shift left by 5 to byte 8. Anchoring this test prevents
        // a regression where `apply_delta` is dropped from `del_at` (the
        // bug would manifest as stale spans referring to bytes past the
        // end of the splice'd buffer).
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(3, 4, 0, 5));
        spans.insert("/b".into(), block_map_span(8, 9, 5, 10));
        spans.insert("/c".into(), block_map_span(13, 14, 10, 15));
        let mut value = IndexMap::new();
        value.insert("a".into(), Value::Int(1));
        value.insert("b".into(), Value::Int(2));
        value.insert("c".into(), Value::Int(3));
        let mut doc = Document::with_spans(
            Value::Map(value),
            b"a: 1\nb: 2\nc: 3\n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let pointer = Pointer::parse("/b").unwrap();
        doc.del_at(&pointer).expect("del_at /b must succeed");

        let span_a = doc
            .span_at(&Pointer::parse("/a").unwrap())
            .expect("/a span");
        assert_eq!(
            span_a.value_range,
            3..4,
            "spans to the left of the edit must not move",
        );
        assert_eq!(span_a.line_range, 0..5);

        assert!(
            doc.span_at(&Pointer::parse("/b").unwrap()).is_none(),
            "deleted pointer's span must be removed",
        );

        let span_c = doc
            .span_at(&Pointer::parse("/c").unwrap())
            .expect("/c span");
        assert_eq!(
            span_c.value_range,
            8..9,
            "spans to the right of the edit shift left by the deleted line length",
        );
        assert_eq!(span_c.line_range, 5..10);
    }

    #[test]
    fn del_at_removes_subspans_of_deleted_pointer() {
        // When `/a` is deleted, every span recorded under `/a/...` must
        // also disappear. Otherwise downstream `set_at` calls would
        // resolve into bytes that no longer exist — and the in-memory
        // value tree, which IS pruned, would silently disagree with the
        // span map.
        //
        // Bytes: `a:\n  x: 1\n  y: 2\n` = 3 + 7 + 7 = 17 bytes total.
        // The `/a` span covers the whole structure (line_range 0..17);
        // `/a/x` lives at byte 8 in line `[3..10)`; `/a/y` at byte 15
        // in line `[10..17)`.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(0, 0, 0, 17));
        spans.insert("/a/x".into(), block_map_span(8, 9, 3, 10));
        spans.insert("/a/y".into(), block_map_span(15, 16, 10, 17));
        let mut inner = IndexMap::new();
        inner.insert("x".into(), Value::Int(1));
        inner.insert("y".into(), Value::Int(2));
        let mut doc = Document::with_spans(
            map_one("a", Value::Map(inner)),
            b"a:\n  x: 1\n  y: 2\n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let pointer = Pointer::parse("/a").unwrap();
        doc.del_at(&pointer).expect("del_at /a must succeed");

        for canonical in ["/a", "/a/x", "/a/y"] {
            let p = Pointer::parse(canonical).unwrap();
            assert!(
                doc.span_at(&p).is_none(),
                "span for {canonical} must be removed when its ancestor is deleted; spans = {:?}",
                doc.spans(),
            );
        }
    }

    #[test]
    fn blank_line_before_a_header_looks_past_its_comment_lines() {
        assert!(blank_line_before_a_header(b"a = 1\n\n[t]\n"));
        assert!(blank_line_before_a_header(b"a = 1\n\n# lead\n[t]\n"));
        assert!(blank_line_before_a_header(
            b"a = 1\r\n\r\n# lead\r\n[t]\r\n"
        ));
        assert!(!blank_line_before_a_header(b"a = 1\n# lead\n[t]\n"));
        assert!(!blank_line_before_a_header(b"a = [\n\n  [1],\n]\n"));
    }

    #[test]
    fn markdown_has_no_textual_edits() {
        let mut doc =
            crate::format::Format::parse(&crate::parsers::Markdown, b"# T\n\ntext\n").unwrap();
        let err = doc
            .set_at(&Pointer::parse("/a").unwrap(), Value::Int(1))
            .unwrap_err();
        assert!(
            err.to_string().contains("markdown has no textual edits"),
            "{err}"
        );
    }

    #[test]
    fn del_at_root_returns_delete_root() {
        // Even on a write-aware document the root pointer cannot be
        // deleted — that would empty the file. A populated `spans` map
        // proves the early `WriteUnavailable` check is past, so we know
        // the root rejection is happening for the right reason.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(3, 4, 0, 5));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"a: 3\n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let result = doc.del_at(&Pointer::default());
        match result {
            Err(Error::Path { kind, .. }) => assert_eq!(
                kind,
                PathErrorKind::DeleteRoot,
                "root pointer must be flagged distinctly"
            ),
            other => panic!("expected Path error, got: {other:?}"),
        }
    }

    #[test]
    fn del_at_missing_pointer_returns_missing_key() {
        // del_at on a populated SpanMap that doesn't contain the pointer
        // must surface a structured Path/MissingKey — not WriteUnavailable
        // (the document IS write-aware) and not a panic.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(3, 4, 0, 5));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"a: 3\n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let pointer = Pointer::parse("/x").unwrap();
        let result = doc.del_at(&pointer);
        match result {
            Err(Error::Path { kind, pointer, .. }) => {
                assert_eq!(pointer, "/x");
                assert_eq!(kind, PathErrorKind::MissingKey);
            }
            other => panic!("expected Path/MissingKey, got: {other:?}"),
        }
    }

    #[test]
    fn set_at_reports_matched_prefix_for_partial_path() {
        // Regression: `set_at` on a pointer whose tail descends past an
        // existing container key must surface the longest matching prefix in
        // `matched_prefix` — not an empty string. Bug #7 reported that
        // `dq set config.yaml /spec/strategy/type X` on a doc containing
        // `{"spec":{"replicas":3}}` produced `matched up to ''` because the
        // parser SpanMap only records scalar leaves; walking the Value tree
        // (which records every container) is required to compute the prefix.
        // A missing chain is created now; one under a scalar still fails.
        let mut doc = crate::parsers::parse_json_with_spans(b"{\"spec\":{\"replicas\":3}}")
            .expect("JSON parses");
        let pointer = Pointer::parse("/spec/replicas/strategy/type").expect("pointer parses");
        let result = doc.set_at(&pointer, Value::Int(42));
        match result {
            Err(Error::Path {
                pointer: ptr,
                matched_prefix,
                kind,
                ..
            }) => {
                assert_eq!(ptr, "/spec/replicas/strategy/type");
                assert_eq!(
                    matched_prefix, "/spec/replicas",
                    "matched_prefix must follow the longest container chain present in the Value tree",
                );
                assert_eq!(kind, PathErrorKind::MissingKey);
            }
            other => panic!("expected Path/MissingKey, got: {other:?}"),
        }
    }

    #[test]
    fn del_at_reports_matched_prefix_for_partial_path() {
        // Mirror of `set_at_reports_matched_prefix_for_partial_path` on the
        // delete path. `/spec/missing/key` descends into the existing
        // `/spec` container, so `matched_prefix` must be `/spec`.
        let mut doc = crate::parsers::parse_json_with_spans(b"{\"spec\":{\"replicas\":3}}")
            .expect("JSON parses");
        let pointer = Pointer::parse("/spec/missing/key").expect("pointer parses");
        let result = doc.del_at(&pointer);
        match result {
            Err(Error::Path {
                pointer: ptr,
                matched_prefix,
                kind,
                ..
            }) => {
                assert_eq!(ptr, "/spec/missing/key");
                assert_eq!(
                    matched_prefix, "/spec",
                    "matched_prefix must follow the longest container chain present in the Value tree",
                );
                assert_eq!(kind, PathErrorKind::MissingKey);
            }
            other => panic!("expected Path/MissingKey, got: {other:?}"),
        }
    }

    #[test]
    fn non_finite_float_reads_floats_and_literals() {
        assert!(
            Value::Float(f64::NAN)
                .non_finite_float()
                .is_some_and(f64::is_nan)
        );
        assert_eq!(
            Value::BigFloat("-.Inf".into()).non_finite_float(),
            Some(f64::NEG_INFINITY)
        );
        assert_eq!(Value::Float(1.5).non_finite_float(), None);
        assert_eq!(Value::BigFloat("1e400".into()).non_finite_float(), None);
        assert_eq!(Value::String(".nan".into()).non_finite_float(), None);
    }

    #[test]
    fn to_serde_json_spells_every_non_finite_float_the_yaml_way() {
        let cases = [
            (Value::Float(f64::NAN), ".nan"),
            (Value::Float(f64::INFINITY), ".inf"),
            (Value::Float(f64::NEG_INFINITY), "-.inf"),
            (Value::BigFloat(".NaN".into()), ".nan"),
            (Value::BigFloat("+inf".into()), ".inf"),
            (Value::BigFloat("-.Inf".into()), "-.inf"),
        ];
        for (value, spelling) in cases {
            assert_eq!(value.non_finite_spelling(), Some(spelling), "{value:?}");
            assert_eq!(
                value.to_serde_json(),
                serde_json::json!(spelling),
                "{value:?}"
            );
        }
        assert_eq!(Value::Float(1.5).non_finite_spelling(), None);
    }

    #[test]
    fn is_whole_number_reads_the_exact_value() {
        let big = |s: &str| Value::BigFloat(s.into());
        for whole in [
            Value::Int(-3),
            Value::BigInt("18446744073709551616".into()),
            Value::Float(2.0),
            big("1e400"),
            big("1.50e1"),
        ] {
            assert!(whole.is_whole_number(), "{whole:?}");
        }
        for other in [
            Value::Float(2.5),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NAN),
            big(".inf"),
            big("1e-400"),
            Value::String("1".into()),
        ] {
            assert!(!other.is_whole_number(), "{other:?}");
        }
    }

    #[test]
    fn compare_number_orders_values_across_variants() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let big = |s: &str| Value::BigFloat(s.to_owned());
        let cases = [
            (Value::Int(1), Value::Float(1.0), Some(Equal)),
            (Value::Float(0.1), big("0.10000000000000000001"), Some(Less)),
            (
                Value::BigInt("18446744073709551617".into()),
                big("1.8446744073709551616e19"),
                Some(Greater),
            ),
            (big(".inf"), big("1e400"), Some(Greater)),
            (
                Value::Float(f64::NEG_INFINITY),
                Value::Int(i64::MIN),
                Some(Less),
            ),
            (big("-.inf"), Value::Float(f64::NEG_INFINITY), Some(Equal)),
            (big(".nan"), Value::Int(1), None),
            (Value::Int(1), Value::Float(f64::NAN), None),
            (Value::Int(1), Value::String("1".into()), None),
            (big(".inf"), Value::Null, None),
        ];
        for (a, b, order) in cases {
            assert_eq!(a.compare_number(&b), order, "{a:?} vs {b:?}");
            assert_eq!(
                b.compare_number(&a),
                order.map(std::cmp::Ordering::reverse),
                "{b:?} vs {a:?}"
            );
        }
    }

    #[test]
    fn same_number_compares_values_across_variants() {
        let big = |s: &str| Value::BigFloat(s.into());
        assert!(Value::Int(1).same_number(&Value::Float(1.0)));
        assert!(Value::Int(1).same_number(&big("1e0")));
        assert!(Value::Float(-0.0).same_number(&Value::Int(0)));
        assert!(big("1e400").same_number(&big("1.0e+400")));
        assert!(Value::Float(0.1).same_number(&big("0.1")));
        assert!(!Value::Float(0.1).same_number(&big("0.1000000000000000000000000001")));
        assert!(Value::Float(f64::INFINITY).same_number(&big(".inf")));
        assert!(!Value::Float(f64::INFINITY).same_number(&big("-.inf")));
        assert!(!Value::Float(f64::NAN).same_number(&big(".nan")));
        assert!(!Value::Int(1).same_number(&Value::String("1".into())));
    }

    #[test]
    fn holds_non_finite_looks_inside_collections() {
        let tree = |json: serde_json::Value| Value::from_serde_json(&json);
        let mut doc = tree(serde_json::json!({"a": [1, {"b": 2}]}));
        assert!(!doc.holds_non_finite());
        if let Value::Map(map) = &mut doc {
            map.insert(
                "c".into(),
                Value::Array(vec![Value::BigFloat(".nan".into())]),
            );
        }
        assert!(doc.holds_non_finite());
    }

    #[test]
    fn from_number_literal_keeps_the_exact_value() {
        let big = |s: &str| Value::BigFloat(s.into());
        let cases = [
            ("12", Value::Int(12)),
            (
                "-9223372036854775809",
                Value::BigInt("-9223372036854775809".into()),
            ),
            (
                "+18446744073709551616",
                Value::BigInt("18446744073709551616".into()),
            ),
            ("1.10", Value::Float(1.1)),
            ("1e3", Value::Float(1000.0)),
            ("0.30000000000000004", Value::Float(0.30000000000000004)),
            ("-0.0", Value::Float(-0.0)),
            (
                "0.1000000000000000000000000001",
                big("0.1000000000000000000000000001"),
            ),
            ("1e400", big("1e400")),
            ("1E-400", big("1E-400")),
            ("+.5e400", big("0.5e400")),
            ("007.e+400", big("7.0e+400")),
        ];
        for (literal, want) in cases {
            let got = Value::from_number_literal(literal);
            assert!(
                got.as_ref().is_some_and(|got| got.same_data(&want)),
                "{literal}: {got:?}"
            );
            assert_eq!(got, Some(want), "{literal}");
        }
        for not_a_number in [
            "", ".", "-", "1e", "0x10", "1_000", ".inf", "nan", "1.2.3", "e5",
        ] {
            assert_eq!(
                Value::from_number_literal(not_a_number),
                None,
                "{not_a_number}"
            );
        }
    }

    #[test]
    fn same_data_compares_big_numbers_by_their_decimal_value() {
        let big = |s: &str| Value::BigFloat(s.into());
        assert!(big("1e400").same_data(&big("1.0e+400")));
        assert!(big("1e400").same_data(&big("10E399")));
        assert!(!big("1e400").same_data(&big("1e401")));
        assert!(!big("0.1000000000000000000000000001").same_data(&Value::Float(0.1)));
        assert!(big("0.10").same_data(&Value::Float(0.1)));
        assert!(
            Value::BigInt("18446744073709551616".into())
                .same_data(&Value::BigInt("+18446744073709551616".into()))
        );
        assert!(
            !Value::BigInt("18446744073709551616".into())
                .same_data(&Value::BigInt("18446744073709551617".into()))
        );
        assert!(!Value::BigInt("100".into()).same_data(&big("100.0")));
        assert!(!big(".nan").same_data(&big("1e400")));
        // An exponent past `i64` compares by spelling.
        assert!(big("1e99999999999999999999").same_data(&big("1e99999999999999999999")));
    }

    #[test]
    fn same_big_number_wants_a_big_number_on_one_side() {
        let big = |s: &str| Value::BigFloat(s.into());
        assert!(big("1e400").same_big_number(&big("1e+400")));
        assert!(big("0.10").same_big_number(&Value::Float(0.1)));
        assert!(!big("1e400").same_big_number(&big("1e401")));
        assert!(!Value::Float(f64::NAN).same_big_number(&Value::Float(f64::NAN)));
        let list = Value::Array(vec![big("1e400")]);
        assert!(!list.same_big_number(&Value::Array(vec![big("1e+400")])));
    }

    #[test]
    fn same_data_matches_nan_its_literal_and_reordered_maps() {
        let tree = |json: serde_json::Value| Value::from_serde_json(&json);
        assert!(Value::Float(f64::NAN).same_data(&Value::Float(f64::NAN)));
        assert!(Value::Float(f64::NAN).same_data(&Value::BigFloat(".nan".into())));
        assert!(Value::BigFloat("-.inf".into()).same_data(&Value::Float(f64::NEG_INFINITY)));
        assert!(!Value::Float(f64::INFINITY).same_data(&Value::Float(f64::NEG_INFINITY)));
        assert!(!Value::Float(f64::NAN).same_data(&Value::String(".nan".into())));
        assert!(!Value::Int(1).same_data(&Value::Float(1.0)));
        assert!(!Value::Float(-0.0).same_data(&Value::Float(0.0)));
        assert!(Value::Float(-0.0).same_data(&Value::Float(-0.0)));
        assert!(Value::Float(1.5).same_data(&Value::Float(1.5)));
        let a = tree(serde_json::json!({"a": 1, "b": [1, {"c": 2}]}));
        assert!(a.same_data(&tree(serde_json::json!({"b": [1, {"c": 2}], "a": 1}))));
        assert!(!a.same_data(&tree(serde_json::json!({"a": 1, "b": [1, {"c": 3}]}))));
        assert!(!a.same_data(&tree(serde_json::json!({"a": 1}))));
        assert!(!a.same_data(&tree(serde_json::json!([1]))));
    }

    #[test]
    fn first_difference_ignores_map_order_and_matches_nan() {
        let tree = |json: serde_json::Value| Value::from_serde_json(&json);
        let a = tree(serde_json::json!({"a": 1, "b": [1, {"c/d": 2}]}));
        let b = tree(serde_json::json!({"b": [1, {"c/d": 2}], "a": 1}));
        assert_eq!(first_difference(&a, &b, ""), None);
        let c = tree(serde_json::json!({"a": 1, "b": [1, {"c/d": 3}]}));
        assert_eq!(first_difference(&a, &c, ""), Some("/b/1/c~1d".to_owned()));
        assert_eq!(
            first_difference(&Value::Float(f64::NAN), &Value::Float(f64::NAN), ""),
            None
        );
        assert_eq!(
            first_difference(&Value::Int(1), &Value::Float(1.0), "/x"),
            Some("/x".to_owned())
        );
    }

    #[test]
    fn first_difference_tells_negative_zero_from_zero() {
        assert_eq!(
            first_difference(&Value::Float(-0.0), &Value::Float(0.0), "/x"),
            Some("/x".to_owned())
        );
        assert_eq!(
            first_difference(&Value::Float(-0.0), &Value::Float(-0.0), "/x"),
            None
        );
        assert_eq!(
            first_difference(&Value::Float(f64::NAN), &Value::BigFloat(".nan".into()), ""),
            None
        );
    }

    /// The bytes after `edit` on `src` parsed as `format`.
    fn edited(format: &str, src: &str, edit: impl FnOnce(&mut Document) -> Result<()>) -> String {
        // The YAML `Format` parses read-only; edits go through spans.
        let mut doc = match format {
            "yaml" => crate::parsers::parse_yaml_with_spans(src.as_bytes()),
            _ => crate::format::by_name(format)
                .expect("format")
                .parse(src.as_bytes()),
        }
        .expect("parse");
        edit(&mut doc).unwrap_or_else(|err| panic!("edit on {src:?}: {err}"));
        String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
    }

    fn del_edit(pointer: &str) -> impl FnOnce(&mut Document) -> Result<()> {
        let pointer = Pointer::parse(pointer).unwrap();
        move |doc| doc.del_at(&pointer)
    }

    fn set_edit(pointer: &str, value: Value) -> impl FnOnce(&mut Document) -> Result<()> {
        let pointer = Pointer::parse(pointer).unwrap();
        move |doc| doc.set_at(&pointer, value)
    }

    #[test]
    fn del_of_an_explicit_key_without_a_value_keeps_the_next_one() {
        assert_eq!(edited("yaml", "? k\n? j\n", del_edit("/k")), "? j\n");
        assert_eq!(
            edited("yaml", "? k\n? j\n? l\n", del_edit("/j")),
            "? k\n? l\n"
        );
    }

    #[test]
    fn del_of_a_compact_entry_before_an_empty_key() {
        assert_eq!(
            edited("yaml", "- a: 1\n  \"\": 2\n- b: 3\n", del_edit("/0/a")),
            "- \"\": 2\n- b: 3\n"
        );
    }

    #[test]
    fn del_of_a_compact_entry_takes_its_trailing_comments() {
        assert_eq!(
            edited(
                "yaml",
                "- a:\n    b: 1\n    # c\n  d: 2\n",
                del_edit("/0/a")
            ),
            "- d: 2\n"
        );
    }

    #[test]
    fn del_at_the_top_of_the_file_leaves_no_blank_line_first() {
        assert_eq!(
            edited("yaml", "a:\n  b: 1\n  # tail\n\nd: 3\n", del_edit("/a")),
            "d: 3\n"
        );
        assert_eq!(
            edited("yaml", "x: 0\na: 1\n\nd: 3\n", del_edit("/a")),
            "x: 0\n\nd: 3\n"
        );
    }

    #[test]
    fn comment_lines_count_columns_past_a_byte_order_mark() {
        let src = "\u{feff}a:\n  b: 1\n  # c\nd: 2\n";
        assert_eq!(edited("yaml", src, del_edit("/a")), "\u{feff}d: 2\n");
        assert_eq!(
            edited("yaml", src, set_edit("/a", Value::Int(5))),
            "\u{feff}a: 5\nd: 2\n"
        );
        assert_eq!(
            edited(
                "yaml",
                "\u{feff}- a:\n    b: 1\n    # c\n  d: 2\n",
                del_edit("/0/a")
            ),
            "\u{feff}- d: 2\n"
        );
        assert_eq!(line_start("\u{feff}ab".as_bytes(), 4), 3);
        assert_eq!(line_start("\u{feff}ab".as_bytes(), 0), 0);
    }

    #[test]
    fn toml_del_keeps_the_byte_order_mark_and_line_endings() {
        assert_eq!(
            edited("toml", "\u{feff}a=1\r\n\r\nb=2\r\n", del_edit("/a")),
            "\u{feff}b=2\r\n"
        );
        assert_eq!(
            edited("toml", "\u{feff}[a.b]\r\nc=1\r\n", del_edit("/a/b")),
            "\u{feff}[a]\r\n"
        );
    }

    #[test]
    fn set_over_a_tagged_yaml_collection_drops_the_core_tag() {
        let five = || Value::Int(5);
        assert_eq!(
            edited("yaml", "a: !!map\n  b: 1\nc: 2\n", set_edit("/a", five())),
            "a: 5\nc: 2\n"
        );
        assert_eq!(
            edited("yaml", "a: !!map {b: 1}\nc: 2\n", set_edit("/a", five())),
            "a: 5\nc: 2\n"
        );
        // An anchor stays.
        assert_eq!(
            edited(
                "yaml",
                "a: &x !!map\n  b: 1\nc: 2\n",
                set_edit("/a", five())
            ),
            "a: &x 5\nc: 2\n"
        );
    }

    #[test]
    fn core_tags_are_spelled_three_ways() {
        let hello = || Value::String("hello".into());
        for src in [
            "a: !<tag:yaml.org,2002:map> {b: 1}\nc: 2\n",
            "a: !<tag:yaml.org,2002:seq>\n  - 1\nc: 2\n",
        ] {
            assert_eq!(
                edited("yaml", src, set_edit("/a", hello())),
                "a: hello\nc: 2\n"
            );
        }
        assert_eq!(
            edited(
                "yaml",
                "%TAG !e! tag:yaml.org,2002:\n---\na: !e!map {b: 1}\n",
                set_edit("/a", hello())
            ),
            "%TAG !e! tag:yaml.org,2002:\n---\na: hello\n"
        );
        let src = b"%TAG !e! tag:example.com,2000:\n--- a\n";
        let tags = CoreTags::at(src, src.len());
        assert!(tags.retypes(b"!!str") && tags.retypes(b"!"));
        assert!(!tags.retypes(b"!e!x") && !tags.retypes(b"!<tag:example.com,2000:x>"));
        // A directive binds its handle for the next document only.
        let src = b"%TAG !e! tag:yaml.org,2002:\n--- a\n...\n%TAG !e! tag:x,1:\n--- b\n";
        assert!(CoreTags::at(src, "%TAG !e! tag:yaml.org,2002:\n--- ".len()).retypes(b"!e!map"));
        assert!(!CoreTags::at(src, src.len()).retypes(b"!e!map"));
        // `!!` can be bound elsewhere, and `!` to the core prefix.
        let src = b"%TAG !! tag:x,1:\n%TAG ! tag:yaml.org,2002:\n--- a\n";
        let tags = CoreTags::at(src, src.len());
        assert!(!tags.retypes(b"!!str") && tags.retypes(b"!str"));
        // A directive on the first line counts after a byte order mark.
        let src = "\u{feff}%TAG !e! tag:yaml.org,2002:\n--- a\n".as_bytes();
        assert!(CoreTags::at(src, src.len()).retypes(b"!e!map"));
        assert_eq!(yaml_properties_start(b"a: !<tag:x,1:y> {", 16), 3);
    }

    #[test]
    fn set_over_a_yaml_collection_takes_its_comment_lines() {
        let src = "a:\n  # note\n  b: 1\n  # tail\n# after\nd: 3\n";
        assert_eq!(
            edited("yaml", src, set_edit("/a", Value::Int(5))),
            "a: 5\n# after\nd: 3\n"
        );
    }

    #[test]
    fn set_of_a_new_key_in_a_toml_table_with_no_header() {
        assert_eq!(
            edited("toml", "[a.d]\nx = 1\n", set_edit("/a/z", Value::Int(3))),
            "[a]\nz = 3\n\n[a.d]\nx = 1\n"
        );
        assert_eq!(
            edited("toml", "a.d.x = 1\n", set_edit("/a/z", Value::Int(3))),
            "a.d.x = 1\na.z = 3\n"
        );
        // A dotted key that does not reach up to the table: a header.
        assert_eq!(
            edited("toml", "[a.d]\ne.f = 1\n", set_edit("/a/z", Value::Int(3))),
            "[a]\nz = 3\n\n[a.d]\ne.f = 1\n"
        );
        // After a byte-order mark, not before it.
        assert_eq!(
            edited(
                "toml",
                "\u{feff}[a.d]\nx=1\n",
                set_edit("/a/z", Value::Int(3))
            ),
            "\u{feff}[a]\nz = 3\n\n[a.d]\nx=1\n"
        );
        // The comment lines above the section, up to the byte-order mark.
        assert_eq!(
            edited(
                "toml",
                "\u{feff}# c\n[a.d]\nx=1\n",
                set_edit("/a/z", Value::Int(3))
            ),
            "\u{feff}[a]\nz = 3\n\n# c\n[a.d]\nx=1\n"
        );
    }

    #[test]
    fn toml_dotted_key_prefix_cuts_the_key_down_to_the_table() {
        let a = Pointer::parse("/a").unwrap();
        assert_eq!(
            toml_dotted_key_prefix(b"  a . \"b.c\".d = 1\n", "/a/b.c/d", &a),
            Some(b"  a".to_vec())
        );
        // The key does not reach up to `/a` inside `[a]`.
        assert_eq!(toml_dotted_key_prefix(b"d = 1\n", "/a/d", &a), None);
        assert_eq!(toml_key_dots(b"a.'x.y'.b]"), Some(vec![1, 7]));
        assert_eq!(toml_key_dots(b"a.b # no end"), None);
        assert_eq!(toml_key_dots(b"a.\"b\\\".c\".d ="), Some(vec![1, 9]));
        assert_eq!(
            toml_dotted_key_prefix(b"a.\"\".x = 1\n", "/a//x", &a),
            Some(b"a".to_vec())
        );
    }

    #[test]
    fn toml_header_spells_a_path_of_tables() {
        let root = Value::from_serde_json(&serde_json::json!({"a": {"b c": {}}, "t": [{}]}));
        assert_eq!(
            toml_header(&root, &Pointer::parse("/a/b c").unwrap()),
            Some(b"[a.\"b c\"]\n".to_vec())
        );
        assert_eq!(toml_header(&root, &Pointer::parse("/t/0").unwrap()), None);
        let root = Value::from_serde_json(&serde_json::json!({"t": [{"a": {}}]}));
        assert_eq!(
            toml_header(&root, &Pointer::parse("/t/0/a").unwrap()),
            Some(b"[t.a]\n".to_vec())
        );
        assert_eq!(toml_header(&root, &Pointer::default()), None);
    }

    #[test]
    fn past_comment_lines_stops_at_a_shallower_comment() {
        let src = b"  # deep\n\n    # deeper\n\n# shallow\n";
        // Past the deeper comment; the blank line after it stays.
        assert_eq!(
            past_comment_lines(src, 0, 2),
            "  # deep\n\n    # deeper\n".len()
        );
        assert_eq!(past_comment_lines(b"x: 1\n", 0, 0), 0);
    }

    /// Byte offset of the first occurrence of `needle` in `src`.
    fn at(src: &str, needle: &str) -> usize {
        src.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} in {src:?}"))
    }

    #[test]
    fn flow_key_start_finds_quoted_and_dotted_keys() {
        // (format, source, key text as written, the key ends where it ends)
        let cases = [
            (FormatTag::Json, r#"{"b": 2, "q\"x": 1}"#, r#""q\"x""#),
            (FormatTag::Json, r#"{"b": 2, "a\\\"": 1}"#, r#""a\\\"""#),
            (FormatTag::Json, r#"{"b": 2, "q\\": 1}"#, r#""q\\""#),
            (FormatTag::Yaml, "{b: 2, 'it''s': 1}", "'it''s'"),
            (FormatTag::Yaml, "{b: 2,'''': 1}", "''''"),
            (FormatTag::Yaml, "{b: 2, '': 1}", "''"),
            (FormatTag::Yaml, "{b: 2,  plain key: 1}", "plain key"),
            (FormatTag::Yaml, "{b: 2,\n  k: 1}", "k"),
            (FormatTag::Yaml, "{b: 2, [1]: 1}", "[1]"),
            (FormatTag::Yaml, "{[x, [1]]: 1}", "[x, [1]]"),
            (
                FormatTag::Yaml,
                "{b: 2, {k: ']', it's: '['}: 1}",
                "{k: ']', it's: '['}",
            ),
            (FormatTag::Yaml, "{b: 2, [\"a\\\"[\"]: 1}", "[\"a\\\"[\"]"),
            // A `{` / `[` inside a quoted scalar of the key opens nothing,
            // even where a bracket of the other kind closes after it.
            (FormatTag::Yaml, "{b: 2, ['{', b]: 1}", "['{', b]"),
            (FormatTag::Yaml, "{b: 2, {x: '['}: 1}", "{x: '['}"),
            (
                FormatTag::Toml,
                "{b = 2, x . \"y z\" . w = 1}",
                "x . \"y z\" . w",
            ),
            (FormatTag::Toml, "{b = 2, x.'y.z'.w = 1}", "x.'y.z'.w"),
            (FormatTag::Toml, "{b = 2, \"x\" = 1}", "\"x\""),
        ];
        for (format, src, key) in cases {
            let start = at(src, key);
            assert_eq!(
                flow_key_start(src.as_bytes(), start + key.len(), format),
                Some(start),
                "{format:?} {src:?}"
            );
        }
    }

    #[test]
    fn flow_entry_removal_takes_the_entry_and_one_comma() {
        // (format, source, value of the entry to remove, map entry, what
        // is left)
        let cases = [
            (FormatTag::Yaml, "{a: 1, b: 2}", "1", true, "{b: 2}"),
            (FormatTag::Yaml, "{a: 1, b: 2}", "2", true, "{a: 1}"),
            (FormatTag::Yaml, "{a: 1}", "1", true, "{}"),
            (FormatTag::Yaml, "[1, 2]", "2", false, "[1]"),
            // A flow collection as the key.
            (FormatTag::Yaml, "{[1]: a, z: 1}", "a", true, "{z: 1}"),
            (FormatTag::Yaml, "{z: 1, [1]: a}", "a", true, "{z: 1}"),
            (FormatTag::Yaml, "{? {k: v} : a}", "a", true, "{}"),
            // A trailing comma: the last entry takes the comma before it.
            (FormatTag::Yaml, "{a: 1, b: 2,}", "2", true, "{a: 1}"),
            (FormatTag::Yaml, "{a: 1,}", "1", true, "{}"),
            // The blank before a comment stays, so it is still a comment.
            (
                FormatTag::Yaml,
                "{a: 1, # c\n b: 2}",
                "1",
                true,
                "{ # c\n b: 2}",
            ),
            // Node properties and an explicit key go with the entry.
            (
                FormatTag::Yaml,
                "{? &k a : !!str 1, b: 2}",
                "1",
                true,
                "{b: 2}",
            ),
            (
                FormatTag::Json,
                r#"{"q\"x": 1, "b": 2}"#,
                "1",
                true,
                r#"{"b": 2}"#,
            ),
            (
                FormatTag::Toml,
                "{x . \"y z\" . w = 1, b = 2}",
                "1",
                true,
                "{b = 2}",
            ),
            (FormatTag::Toml, "{b = 2, x.y = 1}", "1", true, "{b = 2}"),
        ];
        for (format, src, value, map_entry, want) in cases {
            let start = src.rfind(value).expect("value in source");
            let range = flow_entry_removal(
                src.as_bytes(),
                &(start..start + value.len()),
                map_entry,
                format,
            )
            .unwrap_or_else(|| panic!("{format:?} {src:?}: no removal"));
            let mut left = src.to_owned();
            left.replace_range(range, "");
            assert_eq!(left, want, "{format:?} {src:?}");
        }
    }

    #[test]
    fn flow_entry_removal_refuses_what_it_does_not_recognise() {
        // A comment before the closing bracket, a comment before the comma,
        // a value that is not a flow entry at all.
        let cases = [
            ("{a: 1 # c\n}", "1"),
            ("{a: 1 # c\n, b: 2}", "1"),
            ("a: 1\n", "1"),
        ];
        for (src, value) in cases {
            let start = src.find(value).expect("value in source");
            assert_eq!(
                flow_entry_removal(
                    src.as_bytes(),
                    &(start..start + value.len()),
                    true,
                    FormatTag::Yaml
                ),
                None,
                "{src:?}"
            );
        }
    }

    #[test]
    fn past_line_break_moves_to_the_next_line_start() {
        let src = b"a: {}\r\nb: 1";
        // Mid-line: past the `\n` of a CRLF.
        assert_eq!(past_line_break(src, 5), 7);
        // At a line start, at 0, past the end: stays.
        assert_eq!(past_line_break(src, 7), 7);
        assert_eq!(past_line_break(src, 0), 0);
        assert_eq!(past_line_break(src, src.len() + 1), src.len() + 1);
        // No line break after: stays.
        assert_eq!(past_line_break(src, 9), 9);
    }

    #[test]
    fn skip_blanks_and_comments_stops_at_content() {
        let src = b"  # one\r\n\t# two\n\n  x # three";
        assert_eq!(
            skip_blanks_and_comments(src, 0),
            at("  # one\r\n\t# two\n\n  x", "x")
        );
        // A `#` inside content is not skipped from: content is content.
        assert_eq!(skip_blanks_and_comments(b"x # c", 0), 0);
        // A comment to the end of the input ends there.
        assert_eq!(skip_blanks_and_comments(b" # c", 0), 4);
        assert_eq!(skip_blanks_and_comments(b"", 0), 0);
    }

    #[test]
    fn del_at_recompute_only_shifts_spans_after_edit_end() {
        // Three sibling spans on synthesized bytes — `/a` left of edit,
        // `/b` is the edit target, `/c` to the right. After del_at(/b)
        // shrinks the buffer by 5 bytes, /a must be untouched and /c
        // must shift left by 5. This is the same shape as the
        // del_at_recompute_shifts_subsequent_spans test but uses a
        // tighter byte layout to exercise the apply_delta boundary
        // condition: span /c starts at byte 20, edit_end = 5+5 = 10, so
        // 20 > 10 means /c shifts. If apply_delta ever flipped the
        // comparison to `>` instead of `>=` for example, /c would still
        // shift here but `/at_boundary` (would-be at byte 10) wouldn't —
        // we don't add that span because del_at's caller never produces
        // an at-boundary span for the `del_at` path; this tests the
        // common case.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(0, 3, 0, 5));
        spans.insert("/b".into(), block_map_span(8, 9, 5, 10));
        spans.insert("/c".into(), block_map_span(20, 25, 10, 30));
        let mut value = IndexMap::new();
        value.insert("a".into(), Value::Int(1));
        value.insert("b".into(), Value::Int(2));
        value.insert("c".into(), Value::String("hello".into()));
        let mut doc = Document::with_spans(
            Value::Map(value),
            b"a: 123\nb: 2\n     hello                  \n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let pointer = Pointer::parse("/b").unwrap();
        // The bytes are a synthetic layout that does not read back as the
        // tree: exercise the splice without the read-back check.
        doc.del_at_unchecked(&pointer, &LayoutCache::new())
            .expect("del_at /b must succeed");

        let span_a = doc
            .span_at(&Pointer::parse("/a").unwrap())
            .expect("/a span");
        assert_eq!(
            span_a.value_range,
            0..3,
            "spans wholly before the edit must not move",
        );
        let span_c = doc
            .span_at(&Pointer::parse("/c").unwrap())
            .expect("/c span");
        assert_eq!(
            span_c.value_range,
            15..20,
            "spans after the edit shift by `-(old_len - new_len)` = -5",
        );
        assert_eq!(span_c.line_range, 5..25);
    }

    #[test]
    fn span_at_returns_none_for_unknown_pointer() {
        let doc = Document::value_only(Value::Null, FormatTag::Yaml);
        let pointer = Pointer::parse("/x").unwrap();
        assert!(doc.span_at(&pointer).is_none());
    }

    #[test]
    fn format_tag_from_name_round_trips() {
        assert_eq!(FormatTag::from_name("yaml"), Some(FormatTag::Yaml));
        assert_eq!(FormatTag::from_name("json"), Some(FormatTag::Json));
        assert_eq!(FormatTag::from_name("toml"), Some(FormatTag::Toml));
        assert_eq!(FormatTag::from_name("jsonl"), Some(FormatTag::Jsonl));
        assert_eq!(FormatTag::from_name("hcl"), Some(FormatTag::Hcl));
        assert_eq!(FormatTag::from_name("ini"), Some(FormatTag::Ini));
        assert_eq!(FormatTag::from_name("dotenv"), Some(FormatTag::DotEnv));
        assert_eq!(FormatTag::from_name("csv"), Some(FormatTag::Csv));
        assert_eq!(FormatTag::from_name("tsv"), Some(FormatTag::Tsv));
        assert_eq!(
            FormatTag::from_name("dockerfile"),
            Some(FormatTag::Dockerfile),
        );
        assert_eq!(
            FormatTag::from_name("ignore-list"),
            Some(FormatTag::IgnoreList),
        );
        assert_eq!(
            FormatTag::from_name("frontmatter"),
            Some(FormatTag::Frontmatter),
        );
        assert_eq!(FormatTag::from_name("markdown"), Some(FormatTag::Markdown),);
        assert_eq!(FormatTag::Markdown.name(), "markdown");
        assert_eq!(FormatTag::from_name("xml"), Some(FormatTag::Xml));
        assert_eq!(FormatTag::Xml.name(), "xml");
        assert_eq!(FormatTag::from_name("zzz"), None);
    }

    #[test]
    fn frontmatter_constructor_carries_body_and_kind() {
        // Stage-1 contract: the new `Document::frontmatter` constructor
        // tags the document as `FormatTag::Frontmatter` and stores the body
        // bytes under a `FrontmatterPayload` keyed by the inner format that
        // produced the header. Stage 2's writer relies on both pieces of
        // information being preserved end-to-end.
        let doc = Document::frontmatter(Value::Null, b"# body".to_vec(), FrontmatterKind::Yaml);
        assert_eq!(doc.format(), FormatTag::Frontmatter);
        let payload = doc
            .frontmatter_payload()
            .expect("frontmatter payload must be present on frontmatter documents");
        assert_eq!(payload.kind, FrontmatterKind::Yaml);
        assert_eq!(payload.body, b"# body");
    }

    #[test]
    fn value_only_has_no_frontmatter_payload() {
        // Symmetric anti-test: every non-frontmatter constructor leaves
        // `frontmatter_payload()` `None`. Pinning this prevents a future
        // refactor from accidentally synthesising an empty payload for
        // every document and confusing the writer dispatch.
        let doc = Document::value_only(Value::Null, FormatTag::Yaml);
        assert!(doc.frontmatter_payload().is_none());
    }
}

/// The fast paths of [`Value::compare_number`], [`Value::same_number`] and
/// [`Value::is_multiple_of`] against the exact, literal-reading ones.
#[cfg(test)]
mod number_fast_path_tests {
    use super::*;
    use proptest::prelude::*;

    /// A number the fast paths take: a float of any bit pattern, a short
    /// decimal, or an integer — some at 2^53, where an `f64` stops holding
    /// every integer.
    fn fast_number() -> impl Strategy<Value = Value> {
        const TWO_TO_53: i64 = 1 << 53;
        prop_oneof![
            any::<f64>().prop_map(Value::Float),
            (-1_000_000_i64..1_000_000, 0_i32..4)
                .prop_map(|(n, b)| Value::Float(n as f64 / 10_f64.powi(b))),
            any::<i64>().prop_map(Value::Int),
            (-3_i64..3).prop_map(|d| Value::Int(TWO_TO_53 + d)),
            (-3_i64..3).prop_map(|d| Value::Float((TWO_TO_53 + d) as f64)),
            (-3_i64..3).prop_map(Value::Int),
        ]
    }

    proptest! {
        #[test]
        fn fast_comparisons_agree_with_the_literals(a in fast_number(), b in fast_number()) {
            if let (Some(x), Some(y)) = (a.finite_literal(), b.finite_literal()) {
                prop_assert_eq!(a.compare_number(&b), number::compare_decimal(&x, &y));
                prop_assert_eq!(a.same_number(&b), number::same_decimal(&x, &y));
                prop_assert_eq!(a.is_multiple_of(&b), number::decimal_is_multiple(&x, &y));
            }
        }
    }

    #[test]
    fn fast_comparisons_keep_the_edges() {
        let two_to_53 = 1_i64 << 53;
        // 2^53 + 1 is no `f64`: the nearest, 2^53, is below it.
        assert_eq!(
            Value::Int(two_to_53 + 1).compare_number(&Value::Float(2f64.powi(53))),
            Some(std::cmp::Ordering::Greater)
        );
        assert!(Value::Float(-0.0).same_number(&Value::Int(0)));
        assert_eq!(
            Value::Float(f64::NAN).compare_number(&Value::Float(1.0)),
            None
        );
        assert_eq!(
            Value::Float(0.3).is_multiple_of(&Value::Float(0.1)),
            Some(true)
        );
        assert_eq!(Value::Int(0).is_multiple_of(&Value::Float(0.1)), Some(true));
        assert_eq!(Value::Int(3).is_multiple_of(&Value::Int(0)), None);
    }
}
