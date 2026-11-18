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

pub mod spans;

use std::fmt;
use std::str::FromStr;

use indexmap::IndexMap;
use serde::ser::{SerializeMap, SerializeSeq, Serializer};

use crate::Result;
use crate::error::{Error, PathErrorKind};
use crate::ir::{Ir, Provenance, ProvenanceMap};
use crate::pointer::{Pointer, Segment};
use crate::textual_edit::{insertion_renderer_for_format, renderer_for_format};

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
    /// tsv, dockerfile, ignore-list, markdown body) — see
    /// [`Document::as_ir`] for how this surfaces to callers.
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

/// The NaN or infinite float `value` is, as a float or a literal.
fn non_finite(value: &Value) -> Option<f64> {
    match value {
        Value::Float(f) if !f.is_finite() => Some(*f),
        Value::BigFloat(s) => non_finite_literal(s),
        _ => None,
    }
}

/// Whether `value` is or holds a NaN or infinite float.
fn holds_non_finite(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(holds_non_finite),
        Value::Map(map) => map.values().any(holds_non_finite),
        scalar => non_finite(scalar).is_some(),
    }
}

impl Value {
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
    /// carrying the textual rendering (`"NaN"`, `"inf"`, `"-inf"`) so the
    /// value survives reporting instead of silently becoming `Null`.
    #[must_use]
    pub fn to_serde_json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::Int(n) => serde_json::Value::Number((*n).into()),
            Self::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| serde_json::Value::String(f.to_string())),
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

/// Convert a [`serde_json::Number`] into a [`Value`] that preserves
/// big-int / big-float literals.
///
/// With the workspace's `arbitrary_precision` feature on `serde_json`,
/// `Number::to_string()` returns the original textual literal verbatim —
/// `as_i64()` / `as_f64()` lossily collapse a 22-digit integer to a
/// `Float(4.7e21)`. Mirror the parsing heuristic
/// `dq-core::parsers::json::json_number_to_value` uses on the read side:
/// try `i64` first, then float-with-round-trip, falling back to `BigInt` /
/// `BigFloat` literals.
fn serde_json_number_to_value(n: &serde_json::Number) -> Value {
    let literal = n.to_string();
    if let Ok(i) = literal.parse::<i64>() {
        return Value::Int(i);
    }
    if literal.contains('.') || literal.contains('e') || literal.contains('E') {
        if let Ok(f) = f64::from_str(&literal)
            && f.is_finite()
            && literal_round_trips_to(&literal, f)
        {
            return Value::Float(f);
        }
        return Value::BigFloat(literal);
    }
    Value::BigInt(literal)
}

/// Lossless round-trip check for a parsed `f64` against its source
/// literal. Mirrors `dq_core::parsers::json::f64_matches_literal` and
/// `dq-cli::commands::set::f64_matches_literal`: re-parse the shortest
/// float formatting and compare for exact equality so cosmetic
/// reformatting (e.g. `1e2` vs `100`) does not trigger the BigFloat
/// branch.
fn literal_round_trips_to(literal: &str, f: f64) -> bool {
    f64::from_str(literal).is_ok_and(|parsed| parsed.to_bits() == f.to_bits())
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
    /// csv, tsv, dockerfile, ignore-list, frontmatter body, markdown body)
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
    /// Multi-level mkdir-p chains (`/a/missing1/missing2/key` where every
    /// `missingN` also needs creating) remain out of scope and fall
    /// through to [`Error::Path`] with `kind = MissingKey`.
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
    ///   that cannot be created via the single-level mkdir-p path.
    /// - [`Error::EditRejected`] when the edited bytes fail the read-back
    ///   check.
    pub fn set_at(&mut self, pointer: &Pointer, value: Value) -> Result<()> {
        let before = self.clone();
        let layout = LayoutCache::new();
        let result = self.set_at_unchecked(pointer, value.clone(), &layout);
        self.check_edit(before, result, "set", pointer, &layout, |tree, at| {
            set_value_at(tree, at, value.clone())
        })
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

    /// [`Document::set_at`] without the read-back check.
    fn set_at_unchecked(
        &mut self,
        pointer: &Pointer,
        value: Value,
        layout: &LayoutCache,
    ) -> Result<()> {
        // "Read-only" really means "no source bytes captured" — read-only
        // parsers go through `value_only` / `multi_value_only` and leave
        // `original_bytes` empty. A document with bytes but an empty
        // SpanMap (e.g. `{"a":{}}` whose only key is itself an empty
        // container) is still write-aware; its mkdir-p path will fail
        // through to `Error::Path { MissingKey }` further down, but the
        // top-level capability check should not falsely block it. An empty
        // file parsed write-aware has no bytes but a span for its root.
        if self.original_bytes.is_empty() && self.spans.is_empty() {
            return Err(Error::WriteUnavailable {
                reason: format!(
                    "{} document was loaded read-only; reload via a write-aware parser to enable set",
                    self.format.name(),
                ),
            });
        }
        let renderer = renderer_for_format(self.format).ok_or_else(|| Error::WriteUnavailable {
            reason: format!(
                "renderer not registered for format {:?}; this format will gain write support in Section 3+",
                self.format,
            ),
        })?;

        let canonical = pointer.as_canonical();
        if self.format == FormatTag::Json && holds_non_finite(&value) {
            return Err(Error::EditRejected {
                action: "set",
                pointer: canonical,
                reason: JSON_NON_FINITE.to_owned(),
            });
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
            let rendered = renderer.render_replacement(&value, span.context, &[]);
            let (mut replacement, mut value_offset) =
                empty_node_replacement(&self.original_bytes[at.clone()], &rendered);
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
            // A `!!` tag fixes the type of what follows it (`!!int`,
            // `!!binary`): the new value is written untagged, typed by what
            // it is. dq's tree carries no tags; local tags (`!Ref`) and
            // anchors stay.
            let mut value_start = replaced.start;
            if self.format == FormatTag::Yaml {
                let props_start = yaml_properties_start(&self.original_bytes, replaced.start);
                let props = &self.original_bytes[props_start..replaced.start];
                if props
                    .split(u8::is_ascii_whitespace)
                    .any(|token| token.starts_with(b"!!"))
                {
                    let mut kept = kept_properties(props);
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
            // mkdir-p path: try the single-level insertion fallback before
            // surfacing `Error::Path { MissingKey }`. Multi-level chains
            // and empty-parent inserts fall through to the structured Path
            // error — the parsers do not yet record container-level spans
            // we'd need to splice into an empty `{}` body.
            self.try_single_level_mkdir_p(pointer, &canonical, value, layout)
        }
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
    /// Returns `Error::Path { MissingKey }` whenever the single-level
    /// path cannot be taken (multi-level missing chain, array parent,
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

        let Some(insertion_renderer) = insertion_renderer_for_format(self.format) else {
            // Read-only formats keep the legacy `MissingKey` diagnostic so
            // callers can still distinguish "format does not support
            // mkdir-p" from "wrong path" via the `Format::insertion_renderer`
            // capability check. XML, CSV, INI, .env, Dockerfile,
            // ignore-list and markdown body all land here.
            return Err(missing_key());
        };

        // Single-level only: the parent must resolve in the value tree
        // and the missing segment must be the last one. Multi-level
        // mkdir-p (where intermediate keys also need creating) is out of
        // scope for this baseline — see set_at's doc comment.
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
            let line_start = self.original_bytes[..sibling_span.value_range.start]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1);
            if let Some(prefix) = toml_dotted_key_parent(&self.original_bytes[line_start..]) {
                let prefix = prefix.trim_ascii_start();
                let at = fragment.len() - fragment.trim_ascii_start().len();
                fragment.splice(at..at, prefix.iter().copied().chain(*b"."));
            }
        }

        // Compute the actual splice point + final bytes, format-aware so
        // we don't smear `\n`s or forget a `,` separator.
        let (splice_at, splice_bytes) = match flow_layout {
            Some(layout) => flow_sibling_splice(&fragment, &sibling_span, layout),
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
                .map(|range| (range, SpanContext::BlockMapValue)),
            FormatTag::Toml => crate::parsers::toml::toml_inline_collections(&self.original_bytes)
                .and_then(|mut ranges| ranges.remove(canonical))
                .map(|(range, in_flow)| {
                    let context = match (in_flow, in_map) {
                        (true, true) => SpanContext::FlowMapValue,
                        (true, false) => SpanContext::FlowSeqItem,
                        (false, _) => SpanContext::BlockMapValue,
                    };
                    (range, context)
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
                    Some((brackets, context))
                } else {
                    // Its children go; the key or `-`, its properties and
                    // any comment on that line stay as an empty node.
                    let marker = collection.empty_marker_at.ok_or_else(rejected)?;
                    let first = collection.first_child_line.ok_or_else(rejected)?;
                    let children = if first >= marker {
                        first..collection.end
                    } else {
                        // `- key: …` / `- - …`: the first child shares the
                        // line with the `-`.
                        let mut end = collection.end;
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
                    self.adopt_reparse("set", canonical)?;
                    if !matches!(pointer.resolve(&self.value), Ok(Value::Null)) {
                        return Err(rejected());
                    }
                    return self.set_at_unchecked(pointer, value, &LayoutCache::new());
                }
            }
            _ => None,
        };
        if let Some((range, context)) = brackets {
            let rendered = renderer.render_replacement(&value, context, &[]);
            self.original_bytes.splice(range, rendered);
            return set_value_at(&mut self.value, pointer, value);
        }
        if self.format == FormatTag::Toml {
            let ranges = crate::parsers::toml::toml_tables(&self.original_bytes)
                .map(|tables| tables.ranges_of(canonical))
                .unwrap_or_default();
            if ranges.is_empty() {
                return Err(rejected());
            }
            for range in ranges.into_iter().rev() {
                self.original_bytes.drain(range);
            }
            self.adopt_reparse("set", canonical)?;
            if pointer.resolve(&self.value).is_ok() {
                return Err(rejected());
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
            return self
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
        if at > 0 && self.original_bytes[at - 1] != b'\n' {
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
        let renderer_indent = match self.format {
            FormatTag::Json => parent_span.indent.saturating_sub(1),
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

        let (splice_at, splice_bytes) = format_specific_empty_parent_splice(
            self.format,
            &fragment,
            &parent_span,
            &self.original_bytes,
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
                .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
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
                    let line_start = self.original_bytes[..entry.at]
                        .iter()
                        .rposition(|&b| b == b'\n')
                        .map_or(0, |i| i + 1);
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
                let tail = &k.as_str()[sibling_prefix.len()..];
                !tail.is_empty() && !tail.contains('/')
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
        let before = self.clone();
        let layout = LayoutCache::new();
        let result = self.del_at_unchecked(pointer, &layout);
        self.check_edit(before, result, "delete", pointer, &layout, delete_value_at)
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
                } if !reason.contains(JQ_REWRITE_HINT) && reason != JSON_NON_FINITE => {
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
        // "Read-only" is signalled by empty `original_bytes` (`value_only`
        // constructor); an all-container document with zero leaf scalars
        // has an empty SpanMap but populated bytes — its del would
        // legitimately fall through to MissingKey, not WriteUnavailable. An
        // empty file parsed write-aware has a span for its root.
        if self.original_bytes.is_empty() && self.spans.is_empty() {
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
                kind: PathErrorKind::TypeMismatch {
                    expected: "non-root pointer",
                    found: "root",
                },
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
                    self.original_bytes[span.line_range.start..]
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
        let collection = layout
            .as_ref()
            .filter(|_| target_is_collection)
            .and_then(|layout| layout.containers.get(&canonical));
        let collection_entry = collection.and_then(|collection| {
            collection.entry.clone().or_else(|| {
                collection.flow_value.as_ref().and_then(|value| {
                    flow_entry_removal(&self.original_bytes, value, in_map, self.format)
                })
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
                    let line_start = self.original_bytes[..value.start]
                        .iter()
                        .rposition(|&b| b == b'\n')
                        .map_or(0, |i| i + 1);
                    Some(line_start..past_line_break(&self.original_bytes, value.end))
                }
            });
        // A TOML `[table]`, `[[array]]` item or dotted table: its sections
        // and lines, wherever they are.
        if self.format == FormatTag::Toml
            && target_is_collection
            && toml_collection_entry.is_none()
            && let Some(ranges) = crate::parsers::toml::toml_tables(&self.original_bytes)
                .map(|tables| tables.ranges_of(&canonical))
                .filter(|ranges| !ranges.is_empty())
        {
            for range in ranges.into_iter().rev() {
                self.original_bytes.drain(range);
            }
            self.adopt_reparse("delete", &canonical)?;
            // The last `[[array]]` item, or the last table under a table
            // with no header of its own: the parent goes with it unless
            // written as `key = []` / `key = {}`.
            if parent_loses_last_child && parent_ptr.resolve(&self.value).is_err() {
                let empty = if in_map {
                    Value::Map(IndexMap::new())
                } else {
                    Value::Array(Vec::new())
                };
                return self
                    .set_at_unchecked(&parent_ptr, empty, &LayoutCache::new())
                    .map_err(|_| Error::EditRejected {
                        action: "delete",
                        pointer: canonical.clone(),
                        reason: "dq cannot keep the table it empties".to_owned(),
                    });
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
                        flow_entry_removal(
                            &self.original_bytes,
                            &span.value_range,
                            in_map,
                            self.format,
                        )
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
                pointer
                    .strip_prefix(&siblings)
                    .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
                    && start.at > entry.at
            })
            .map(|(_, start)| start.at)
            .min();
        let value_end = if target_is_collection {
            layout
                .containers
                .get(canonical)
                .map(|collection| collection.end)
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
        let line_start = self.original_bytes[..marker_start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
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
        if sep == 0 || bytes[sep - 1] != indicator {
            return None;
        }
        start = flow_key_start(bytes, skip_blanks_back(bytes, sep - 1), format)?;
        if yaml {
            start = yaml_properties_start(bytes, start);
            let explicit = skip_blanks_back(bytes, start);
            if explicit > 0 && bytes[explicit - 1] == b'?' {
                start = explicit - 1;
            }
        }
    }
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

/// Start of the key that ends at `end` in a flow mapping: a quoted key, a
/// TOML dotted key (`a."b".c`), or a plain key back to the flow indicator
/// before it.
fn flow_key_start(bytes: &[u8], end: usize, format: FormatTag) -> Option<usize> {
    let mut end = end;
    loop {
        let last = *bytes.get(end.checked_sub(1)?)?;
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
/// and NaN equals NaN.
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
        // NaN equals no float, and YAML holds `.nan` / `.inf` as literals
        // a TOML `nan` / `inf` reads back as floats.
        (want, got)
            if let (Some(want), Some(got)) = (non_finite(want), non_finite(got))
                && (want == got || (want.is_nan() && got.is_nan())) =>
        {
            None
        }
        (want, got) if want == got => None,
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
    let mut line_start = value_start.min(cap);
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
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
/// value in it. Anchors stay; `!!` tags of the core schema go, as they
/// would retype the new value (`!!str` turns `5` into `"5"`). Without
/// properties a blank separates the value from the `:` / `-` before it.
fn empty_node_replacement(props: &[u8], rendered: &[u8]) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(props.len() + rendered.len() + 1);
    if props.is_empty() {
        out.push(b' ');
    }
    out.append(&mut kept_properties(props));
    let offset = out.len();
    out.extend_from_slice(rendered);
    (out, offset)
}

/// The `#` of the first line between the line holding `from` and `to`
/// that holds only a comment; `None` when there is none.
fn first_comment_line(bytes: &[u8], from: usize, to: usize) -> Option<usize> {
    let line_start = bytes[..from]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
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

/// Byte offset of `pos` from the start of its line.
fn line_column(bytes: &[u8], pos: usize) -> usize {
    pos - bytes[..pos]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1)
}

/// `inserted`, to go in `bytes` at `at`, with each bare `\n` turned into
/// `\r\n` when the line break nearest `at` — the last one before it, or
/// else the first after — is one.
fn with_line_ending_at(bytes: &[u8], at: usize, inserted: Vec<u8>) -> Vec<u8> {
    let at = at.min(bytes.len());
    let nearest = bytes[..at]
        .iter()
        .rposition(|&b| b == b'\n')
        .or_else(|| bytes[at..].iter().position(|&b| b == b'\n').map(|i| at + i));
    let crlf = nearest.is_some_and(|i| i > 0 && bytes[i - 1] == b'\r');
    if !crlf || !inserted.contains(&b'\n') {
        return inserted;
    }
    let mut out = Vec::with_capacity(inserted.len() + 4);
    for (i, &b) in inserted.iter().enumerate() {
        if b == b'\n' && (i == 0 || inserted[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
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

/// The anchor and local tags of the node properties `props`, each followed
/// by a space; `!!` tags are dropped.
fn kept_properties(props: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(props.len());
    for token in props
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty() && !token.starts_with(b"!!"))
    {
        out.extend_from_slice(token);
        out.push(b' ');
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
/// line break): only its `key: value` / `key = value` text is kept.
fn flow_sibling_splice(
    fragment: &[u8],
    sibling: &ValueSpan,
    layout: FlowLayout,
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
    let mut out = Vec::with_capacity(entry.len() + 2);
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
) -> (usize, Vec<u8>) {
    match format {
        FormatTag::Json => json_empty_parent_splice(fragment, parent_span, original_bytes),
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
/// interior whitespace like `{ }` / `{\n}`). We splice between the `{` and
/// `}` so the result is `{<fragment>\n}` (or analog).
fn json_empty_parent_splice(
    fragment: &[u8],
    parent_span: &ValueSpan,
    original_bytes: &[u8],
) -> (usize, Vec<u8>) {
    // Find the closing `}` (or `]`) inside the parent's body, scanning
    // back from `value_range.end - 1`. The literal `{}` has the closing
    // brace at exactly `value_range.end - 1`; flexibility for `{ }` etc.
    // The trailing fragment must keep the `}` on its own line at the
    // parent's column for readability.
    let close_pos = parent_span
        .value_range
        .end
        .saturating_sub(1)
        .min(original_bytes.len());
    // The fragment already starts with `\n` + indent + `"k": v`; we append
    // `\n` + parent_indent_spaces + `}` worth of suffix. Actually we just
    // need to ensure the closing brace ends up on its own line at the
    // parent's column. Easier: append `\n` + (parent_indent - 1) spaces
    // before splicing; the existing `}` stays put. parent_span.indent is
    // 1-indexed column of the opening `{`, so the closing `}` lands at the
    // same column. Children are at indent + 2 (which the renderer used).
    let parent_col = parent_span.indent.saturating_sub(1) as usize;
    let mut out = Vec::with_capacity(fragment.len() + parent_col + 1);
    out.extend_from_slice(fragment);
    out.push(b'\n');
    out.extend(std::iter::repeat_n(b' ', parent_col));
    (close_pos, out)
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
            kind: PathErrorKind::TypeMismatch {
                expected: "non-root pointer",
                found: "root",
            },
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
            Self::Float(n) => write!(f, "{n}"),
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
    fn set_at_mkdir_p_fails_for_multi_level_missing_chain() {
        // Single-level baseline only — `/a/missing1/missing2` is out of
        // scope until container-level spans land. Verifies the structured
        // `MissingKey` fallback so a future multi-level enhancement has a
        // failing test to point at.
        let bytes = br#"{"a":{"b":1}}"#;
        let mut doc = crate::parsers::json::Json.parse(bytes).expect("parse JSON");
        let pointer = Pointer::parse("/a/missing/leaf").unwrap();
        let err = doc
            .set_at(&pointer, Value::Int(42))
            .expect_err("multi-level mkdir-p must fail in the single-level baseline");
        assert!(
            matches!(
                err,
                Error::Path {
                    kind: PathErrorKind::MissingKey,
                    ..
                }
            ),
            "expected Path/MissingKey for multi-level mkdir-p, got: {err:?}",
        );
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
                    reason.contains("renderer not registered"),
                    "JSONL write must surface a renderer-missing diagnostic; got: {reason}",
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
    fn del_at_root_returns_type_mismatch() {
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
            Err(Error::Path { kind, .. }) => match kind {
                PathErrorKind::TypeMismatch { expected, found } => {
                    assert_eq!(found, "root", "root pointer must be flagged distinctly");
                    assert_eq!(expected, "non-root pointer");
                }
                other => panic!("expected TypeMismatch, got: {other:?}"),
            },
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
        let mut doc = crate::parsers::parse_json_with_spans(b"{\"spec\":{\"replicas\":3}}")
            .expect("JSON parses");
        let pointer = Pointer::parse("/spec/strategy/type").expect("pointer parses");
        let result = doc.set_at(&pointer, Value::Int(42));
        match result {
            Err(Error::Path {
                pointer: ptr,
                matched_prefix,
                kind,
                ..
            }) => {
                assert_eq!(ptr, "/spec/strategy/type");
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
