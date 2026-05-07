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
use crate::pointer::Pointer;
use crate::textual_edit::renderer_for_format;

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
    /// # Errors
    ///
    /// - [`Error::WriteUnavailable`] when the document was loaded
    ///   read-only (no spans or no renderer registered).
    /// - [`Error::Path`] when the pointer addresses a non-existent path
    ///   that cannot be created via mkdir-p (the M2 baseline does not yet
    ///   record spans for inserted nodes; see the inline TODO).
    pub fn set_at(&mut self, pointer: &Pointer, value: Value) -> Result<()> {
        if self.spans.is_empty() {
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
        if let Some(span) = self.spans.get(&canonical).cloned() {
            // Replace-in-span path: render the new value using the same
            // syntactic context as the original, splice into the source
            // buffer, then shift every span to the right of the edit.
            let original_slice = &self.original_bytes[span.value_range.clone()];
            let rendered = renderer.render_replacement(&value, span.context, original_slice);
            let delta = SpanRecomputeDelta {
                at: span.value_range.start,
                old_len: span.value_range.end - span.value_range.start,
                new_len: rendered.len(),
            };
            // `splice` requires an owned iterator; cloning into a Vec keeps
            // the operation straightforward and the cost is negligible for
            // human-scale documents.
            self.original_bytes
                .splice(span.value_range.clone(), rendered.iter().copied());
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
            let new_end = span.value_range.start + rendered.len();
            if let Some(updated) = self.spans.get_mut(&canonical) {
                updated.value_range = span.value_range.start..new_end;
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
        } else {
            // mkdir-p path: M2 baseline does not yet support inserting brand
            // new keys via the textual-edit pipeline. The full insertion
            // logic (locate nearest ancestor span, render `key: value`
            // chain, splice into the parent container's range, refresh
            // spans) lands in Section 3 alongside the per-format
            // `InsertionRenderer` impls — see design D14 for the tree of
            // edge cases that has to be covered.
            //
            // For now we surface a structured `Path` error so callers can
            // distinguish "node missing" from "format does not yet support
            // mkdir-p"; downstream M2 work will replace this branch with
            // the real insertion path.
            Err(Error::Path {
                pointer: canonical,
                matched_prefix: longest_existing_prefix(&self.spans, pointer),
                kind: PathErrorKind::MissingKey,
                did_you_mean: Vec::new(),
            })
        }
    }

    /// Remove the value at `pointer`, splicing out its physical line(s) and
    /// dropping every span beneath it.
    ///
    /// # Errors
    ///
    /// - [`Error::WriteUnavailable`] when the document was loaded
    ///   read-only.
    /// - [`Error::Path`] with `kind = TypeMismatch` when `pointer` is
    ///   the root (the root is never deletable — it would empty the file).
    /// - [`Error::Path`] with `kind = MissingKey` when no span exists for
    ///   the pointer.
    pub fn del_at(&mut self, pointer: &Pointer) -> Result<()> {
        if self.spans.is_empty() {
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
        let span = self
            .spans
            .get(&canonical)
            .cloned()
            .ok_or_else(|| Error::Path {
                pointer: canonical.clone(),
                matched_prefix: longest_existing_prefix(&self.spans, pointer),
                kind: PathErrorKind::MissingKey,
                did_you_mean: Vec::new(),
            })?;
        let line_range = span.line_range.clone();
        let delta = SpanRecomputeDelta {
            at: line_range.start,
            old_len: line_range.end - line_range.start,
            new_len: 0,
        };
        self.original_bytes
            .splice(line_range, std::iter::empty::<u8>());
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
        // Mirror `set_at`: regenerate provenance from the post-edit span
        // map so the IR view stays consistent across writes.
        self.provenance = provenance_from_spans(&self.spans);
        Ok(())
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
        // [`Pointer`] inside `Original` matches the canonical key. The
        // canonical form was produced by [`Pointer::as_canonical`], so
        // [`Pointer::parse`] is the exact inverse and cannot fail on
        // well-formed input. Defensive `expect` documents the invariant
        // for future readers.
        let pointer = Pointer::parse(canonical)
            .expect("canonical span keys are produced by Pointer::as_canonical");
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

/// Walk up `pointer`'s segments and return the longest prefix whose
/// canonical form has a recorded span. Used for the `matched_prefix`
/// diagnostic on `Error::Path`.
fn longest_existing_prefix(spans: &SpanMap, pointer: &Pointer) -> String {
    let segs = pointer.segments();
    if segs.is_empty() {
        return String::new();
    }
    // Strip one segment at a time from the tail; the first prefix that has
    // a span wins. This is `O(depth^2)` on canonical-string rebuild but
    // depth is tiny (≤ 10 in practice) so the simplicity wins over a
    // fancier walk.
    for end in (0..segs.len()).rev() {
        let prefix = Pointer::new(segs[..end].to_vec());
        let canon = prefix.as_canonical();
        if canon.is_empty() || spans.contains_key(&canon) {
            return canon;
        }
    }
    String::new()
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
    let mut current = root;
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i + 1 == segs.len();
        match (&mut *current, seg) {
            (Value::Map(map), crate::pointer::Segment::Key(k)) => {
                if is_last {
                    map.insert(k.clone(), value);
                    return Ok(());
                }
                if !map.contains_key(k) {
                    map.insert(k.clone(), Value::Map(IndexMap::new()));
                }
                current = map.get_mut(k).expect("just inserted or already present");
            }
            (Value::Array(items), crate::pointer::Segment::Index(idx)) => {
                if *idx < items.len() {
                    if is_last {
                        items[*idx] = value;
                        return Ok(());
                    }
                    current = &mut items[*idx];
                } else {
                    return Err(Error::Path {
                        pointer: pointer.as_canonical(),
                        matched_prefix: String::new(),
                        kind: PathErrorKind::OutOfBounds,
                        did_you_mean: Vec::new(),
                    });
                }
            }
            (Value::Array(items), crate::pointer::Segment::Key(k)) => {
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
                        matched_prefix: String::new(),
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
                    matched_prefix: String::new(),
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
                } else {
                    return Err(Error::Path {
                        pointer: pointer.as_canonical(),
                        matched_prefix: String::new(),
                        kind: PathErrorKind::OutOfBounds,
                        did_you_mean: Vec::new(),
                    });
                }
            }
            (other, _) => {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
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
    // Walk to the parent.
    let (last, parent_segs) = segs.split_last().expect("segs non-empty");
    let mut current: &mut Value = root;
    for seg in parent_segs {
        match (&mut *current, seg) {
            (Value::Map(map), crate::pointer::Segment::Key(k)) => {
                current = map.get_mut(k).ok_or_else(|| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::MissingKey,
                    did_you_mean: Vec::new(),
                })?;
            }
            (Value::Array(items), crate::pointer::Segment::Index(idx)) => {
                current = items.get_mut(*idx).ok_or_else(|| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                })?;
            }
            (Value::Array(items), crate::pointer::Segment::Key(k)) => {
                let idx: usize = k.parse().map_err(|_| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "array index",
                        found: "non-numeric key",
                    },
                    did_you_mean: Vec::new(),
                })?;
                current = items.get_mut(idx).ok_or_else(|| Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                })?;
            }
            (other, _) => {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
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
        (Value::Map(map), crate::pointer::Segment::Key(k)) => {
            if map.shift_remove(k).is_none() {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::MissingKey,
                    did_you_mean: Vec::new(),
                });
            }
        }
        (Value::Array(items), crate::pointer::Segment::Index(idx)) => {
            if *idx >= items.len() {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                });
            }
            items.remove(*idx);
        }
        (Value::Array(items), crate::pointer::Segment::Key(k)) => {
            // RFC 6902 §4.1 reserves `-` as an array-append marker; it has no
            // meaning for delete operations. Surface a clear TypeMismatch so
            // callers don't have to introspect the pointer themselves.
            if k == "-" {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "array index",
                        found: "array-append marker '-' is not deletable",
                    },
                    did_you_mean: Vec::new(),
                });
            }
            let idx: usize = k.parse().map_err(|_| Error::Path {
                pointer: pointer.as_canonical(),
                matched_prefix: String::new(),
                kind: PathErrorKind::TypeMismatch {
                    expected: "array index",
                    found: "non-numeric key",
                },
                did_you_mean: Vec::new(),
            })?;
            if idx >= items.len() {
                return Err(Error::Path {
                    pointer: pointer.as_canonical(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::OutOfBounds,
                    did_you_mean: Vec::new(),
                });
            }
            items.remove(idx);
        }
        (other, _) => {
            return Err(Error::Path {
                pointer: pointer.as_canonical(),
                matched_prefix: String::new(),
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
    fn set_at_missing_pointer_returns_path_missing_key() {
        // Once a renderer is registered for YAML the renderer-availability
        // check passes and the SpanMap lookup runs. A pointer the parser
        // never recorded falls through to the mkdir-p stub and surfaces
        // `Error::Path { kind: MissingKey }` — the contract every M2 caller
        // observes. Pre-§3 the same call returned `WriteUnavailable`; the
        // §3 work flipped this branch by registering the YAML renderer.
        let mut spans = SpanMap::new();
        spans.insert("/a".into(), block_map_span(3, 4, 0, 5));
        let mut doc = Document::with_spans(
            map_one("a", Value::Int(3)),
            b"a: 3\n".to_vec(),
            spans,
            FormatTag::Yaml,
        );
        let pointer = Pointer::parse("/x").unwrap();
        let result = doc.set_at(&pointer, Value::Int(7));
        match result {
            Err(Error::Path {
                pointer: ptr, kind, ..
            }) => {
                assert_eq!(ptr, "/x");
                assert_eq!(kind, PathErrorKind::MissingKey);
            }
            other => panic!("expected Path/MissingKey, got: {other:?}"),
        }
        // A failed `set_at` is a strict no-op on the source buffer.
        assert_eq!(
            doc.original_bytes(),
            b"a: 3\n",
            "failed set_at must not mutate original_bytes",
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
        doc.del_at(&pointer).expect("del_at /b must succeed");

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
