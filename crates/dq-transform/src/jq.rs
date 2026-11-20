//! `JqEngine` — embedded jq evaluator on top of the `jaq` crate family.
//!
//! ## Architecture
//!
//! Three layers are involved every time a jq expression runs:
//!
//! 1. The caller hands us a [`serde_json::Value`] (the lingua franca for
//!    cross-crate value interchange in `dq`).
//! 2. We deserialize it into a [`jaq_json::Val`] via the `serde` feature on
//!    `jaq-json` (see [`serde_to_val`]).
//! 3. The compiled [`jaq_core::Filter`] runs against the `Val` input,
//!    producing a stream of `Val` outputs that we materialise into a
//!    `Vec<serde_json::Value>` via [`val_to_serde`].
//!
//! [`JqEngine::run_value`] skips `serde_json` on both sides and converts a
//! [`dq_core::Value`] to and from `Val` directly: `serde_json::Value` has
//! no NaN or infinity, so a non-finite float survives jq only on that path.
//!
//! ## Feature gate
//!
//! The `embedded-jq` cargo feature (default-on) controls whether the heavy
//! `jaq-*` dependencies are linked. With the feature off, [`JqEngine`] still
//! exists as a small shell whose every method returns
//! [`JqError::FeatureDisabled`].

use thiserror::Error;

/// Errors returned by [`JqEngine`] and the value adapters.
///
/// `JqError::Compile` carries enough context (snippet + byte offset) for
/// callers to surface a caret-and-snippet diagnostic in CI output.
/// `JqError::Runtime` only carries a message because a runtime error is not
/// associated with a specific source-text position.
#[derive(Debug, Error)]
pub enum JqError {
    /// The jq expression failed to lex, parse, or compile.
    ///
    /// The `position` is the byte offset within the original expression
    /// where the error was detected (0-based). The `snippet` is a short
    /// excerpt of the expression around the offending position (at most
    /// roughly 60 characters with `...` ellipsis when truncated).
    #[error("jq compile error at byte offset {position}: {message}")]
    Compile {
        /// Short excerpt of the expression around the offending position.
        snippet: String,
        /// Byte offset within the original expression where the error was detected.
        position: usize,
        /// Diagnostic message from `jaq-core`'s loader/compiler.
        message: String,
    },
    /// The compiled filter failed at evaluation time (e.g. type mismatch,
    /// arithmetic on incompatible types, division by zero).
    #[error("jq runtime error: {message}")]
    Runtime {
        /// Diagnostic message from `jaq-core`'s exception type.
        message: String,
    },
    /// A value could not be converted between `serde_json::Value` and
    /// `jaq_json::Val`. Examples: a `Val::BStr` whose bytes are not valid
    /// UTF-8 cannot become a `serde_json::Value::String`; a non-string
    /// object key cannot be expressed in JSON.
    #[error("jq value conversion error: {message}")]
    Conversion {
        /// Description of what could not be converted.
        message: String,
    },
    /// `dq-transform` was built without the `embedded-jq` feature; the jq
    /// engine is unavailable in this build.
    #[error("dq-transform was built without `embedded-jq` ({hint})")]
    FeatureDisabled {
        /// Short hint shown to the user (e.g. how to rebuild with the feature).
        hint: &'static str,
    },
}

impl JqError {
    /// Stable, lowercase string identifying the error category.
    ///
    /// Used by callers (e.g. JSON output formats, log-aggregation rules)
    /// that want a stable key independent of the diagnostic message.
    /// Returns one of `"compile"`, `"runtime"`, `"conversion"`,
    /// `"feature_disabled"`.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Compile { .. } => "compile",
            Self::Runtime { .. } => "runtime",
            Self::Conversion { .. } => "conversion",
            Self::FeatureDisabled { .. } => "feature_disabled",
        }
    }
}

// ---------------------------------------------------------------------------
// Feature-disabled stubs: keep the public surface identical so downstream
// callers (e.g. `dq-cli`) compile regardless of the feature state and
// produce a deterministic error rather than a link-time failure.
// ---------------------------------------------------------------------------

/// Hint string returned in [`JqError::FeatureDisabled`] when the
/// `embedded-jq` feature is off. Only referenced in the off-state code
/// path and tests; gated to avoid a `dead_code` warning when the
/// feature is on.
#[cfg(not(feature = "embedded-jq"))]
const FEATURE_DISABLED_HINT: &str = "rebuild with --features embedded-jq";

#[cfg(not(feature = "embedded-jq"))]
mod stub {
    use super::{FEATURE_DISABLED_HINT, JqError};

    /// Feature-disabled stub for [`crate::JqEngine`]. Every method returns
    /// [`JqError::FeatureDisabled`].
    #[derive(Debug)]
    pub struct JqEngine;

    impl JqEngine {
        /// Always returns [`JqError::FeatureDisabled`] when the
        /// `embedded-jq` feature is off.
        pub fn compile(_expression: &str) -> Result<Self, JqError> {
            Err(JqError::FeatureDisabled {
                hint: FEATURE_DISABLED_HINT,
            })
        }

        /// Always returns [`JqError::FeatureDisabled`] when the
        /// `embedded-jq` feature is off.
        pub fn run(&self, _input: &serde_json::Value) -> Result<Vec<serde_json::Value>, JqError> {
            Err(JqError::FeatureDisabled {
                hint: FEATURE_DISABLED_HINT,
            })
        }

        /// Always returns [`JqError::FeatureDisabled`] when the
        /// `embedded-jq` feature is off.
        pub fn run_value(&self, _input: &dq_core::Value) -> Result<Vec<dq_core::Value>, JqError> {
            Err(JqError::FeatureDisabled {
                hint: FEATURE_DISABLED_HINT,
            })
        }
    }

    /// Feature-disabled stub.
    ///
    /// The return type is `Result<(), JqError>` (rather than
    /// `Result<jaq_json::Val, JqError>`) because `jaq_json::Val` is not in
    /// scope without the `embedded-jq` feature. Always returns
    /// [`JqError::FeatureDisabled`].
    pub fn serde_to_val(_input: &serde_json::Value) -> Result<(), JqError> {
        Err(JqError::FeatureDisabled {
            hint: FEATURE_DISABLED_HINT,
        })
    }

    /// Feature-disabled stub.
    ///
    /// The input type is `()` (rather than `&jaq_json::Val`) because
    /// `jaq_json::Val` is not in scope without the `embedded-jq` feature.
    /// Always returns [`JqError::FeatureDisabled`].
    pub fn val_to_serde(_val: &()) -> Result<serde_json::Value, JqError> {
        Err(JqError::FeatureDisabled {
            hint: FEATURE_DISABLED_HINT,
        })
    }

    /// Feature-disabled stub for the IR-aware adapter. Mirrors
    /// [`serde_to_val`]'s shape so the public surface stays consistent.
    /// Always returns [`JqError::FeatureDisabled`].
    pub fn ir_to_val(_input: &dq_core::Ir<'_>) -> Result<(), JqError> {
        Err(JqError::FeatureDisabled {
            hint: FEATURE_DISABLED_HINT,
        })
    }

    /// Feature-disabled stub for the IR-aware adapter. Mirrors
    /// [`val_to_serde`]'s shape so the public surface stays consistent.
    /// Always returns [`JqError::FeatureDisabled`].
    pub fn val_to_owned_ir(
        _val: &(),
        _format: dq_core::FormatTag,
    ) -> Result<dq_core::OwnedIr, JqError> {
        Err(JqError::FeatureDisabled {
            hint: FEATURE_DISABLED_HINT,
        })
    }
}

#[cfg(not(feature = "embedded-jq"))]
pub use stub::{JqEngine, ir_to_val, serde_to_val, val_to_owned_ir, val_to_serde};

// ---------------------------------------------------------------------------
// Real implementation behind the `embedded-jq` feature.
// ---------------------------------------------------------------------------

#[cfg(feature = "embedded-jq")]
mod paths;

#[cfg(feature = "embedded-jq")]
mod imp {
    use super::JqError;
    use super::paths;
    use core::str::FromStr;
    use jaq_core::load::{Arena, File, Loader};
    use jaq_core::{Compiler, Ctx, Vars, data, unwrap_valr};
    use jaq_json::{Num, Val};

    /// Compiled jq filter, designed to be shared across rayon workers via
    /// `Arc<JqEngine>`.
    ///
    /// The engine is `Send + Sync`; the `sync` feature on `jaq-json`
    /// (enabled at the workspace level) is what makes `Val: Send + Sync`,
    /// which is required for the trait bound on
    /// `Filter<JustLut<Val>>: Send + Sync`.
    ///
    /// `JqEngine` does **not** implement `Clone`. The underlying
    /// `jaq_core::Filter<Native<JustLut<Val>>>` is not `Clone` because
    /// `Native<D>` (a function-pointer record) doesn't derive `Clone` in
    /// jaq-core 3.0. Wrap the engine in [`std::sync::Arc`] to share it
    /// across threads — this is the documented approach for the
    /// rayon-driven bulk path in `dq set --jq`.
    pub struct JqEngine {
        filter: jaq_core::Filter<data::JustLut<Val>>,
    }

    impl core::fmt::Debug for JqEngine {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("JqEngine").finish_non_exhaustive()
        }
    }

    impl JqEngine {
        /// Parse + compile a jq expression once.
        ///
        /// Includes the `jaq-core`, `jaq-std`, and `jaq-json` definition
        /// sets so that filters like `map`, `select`, `length`, `keys`,
        /// `to_entries`, `tojson`, regex, time, math etc. are available.
        ///
        /// # Errors
        ///
        /// Returns [`JqError::Compile`] if the expression fails to lex,
        /// parse, or compile. The error carries the byte offset within
        /// `expression` where the failure was detected, plus a short
        /// snippet around that position.
        pub fn compile(expression: &str) -> Result<Self, JqError> {
            tracing::trace!(expression, "compiling jq expression");

            // The program as written is compiled first, so a compile error
            // points into the expression the user wrote.
            let filter = compile_filter(expression)?;
            let Some(rewritten) = paths::rewrite(expression) else {
                return Ok(Self { filter });
            };
            tracing::trace!(
                rewritten,
                "jq assignments rewritten to create missing paths"
            );
            match compile_filter(&rewritten) {
                Ok(filter) => Ok(Self { filter }),
                Err(err) => {
                    // A printer bug, not the user's: keep jaq's own semantics.
                    tracing::warn!(%err, expression, "jq assignment rewrite failed to compile; running the expression as written");
                    Ok(Self { filter })
                }
            }
        }

        /// Evaluate the compiled filter against one input value.
        ///
        /// The full output stream is materialised into a `Vec`.
        ///
        /// # Errors
        ///
        /// - [`JqError::Conversion`] if `input` cannot be converted to
        ///   `Val` or one of the outputs cannot be converted back to
        ///   `serde_json::Value` (e.g. it contains a `Val::BStr` with
        ///   non-UTF-8 bytes).
        /// - [`JqError::Runtime`] if the filter raises an exception during
        ///   evaluation (e.g. type mismatch, division by zero).
        pub fn run(&self, input: &serde_json::Value) -> Result<Vec<serde_json::Value>, JqError> {
            let val = serde_to_val(input)?;
            let ctx = Ctx::<data::JustLut<Val>>::new(&self.filter.lut, Vars::new([]));

            let mut out = Vec::new();
            for result in self.filter.id.run((ctx, val)).map(unwrap_valr) {
                match result {
                    Ok(v) => out.push(val_to_serde(&v)?),
                    Err(e) => {
                        return Err(JqError::Runtime {
                            message: e.to_string(),
                        });
                    }
                }
            }
            Ok(out)
        }

        /// Evaluate the compiled filter against one [`dq_core::Value`].
        ///
        /// Unlike [`JqEngine::run`], nothing passes through
        /// `serde_json::Value`, which cannot hold a NaN or an infinity: a
        /// non-finite float (a YAML `.nan`, a TOML `inf`, jq's own `nan` or
        /// `infinite`) is a jq number inside the filter and comes back as a
        /// [`dq_core::Value::Float`]. A value the filter passes through
        /// untouched comes back as the same data (see
        /// [`dq_core::Value::same_data`]).
        ///
        /// Numbers come back as: an `Int` when an `i64` holds the result,
        /// else a `BigInt` literal; a `Float` for any number jq computed in
        /// floating point. A float literal — one in the input, or one the
        /// filter spells or makes with `tonumber` — is a `Float` when the
        /// nearest `f64` has the same decimal value (`1.10`, `1e3`), and
        /// otherwise keeps its literal as a `BigFloat` (a 30-digit `π`,
        /// `1e400`) until the filter computes with it.
        ///
        /// # Errors
        ///
        /// - [`JqError::Conversion`] if an output cannot become a
        ///   [`dq_core::Value`] (a `Val::BStr`, an object key that formats
        ///   to an empty string).
        /// - [`JqError::Runtime`] if the filter raises an exception.
        pub fn run_value(&self, input: &dq_core::Value) -> Result<Vec<dq_core::Value>, JqError> {
            let ctx = Ctx::<data::JustLut<Val>>::new(&self.filter.lut, Vars::new([]));
            let mut out = Vec::new();
            for result in self
                .filter
                .id
                .run((ctx, value_to_val(input)))
                .map(unwrap_valr)
            {
                match result {
                    Ok(v) => out.push(val_to_value(&v)?),
                    Err(e) => {
                        return Err(JqError::Runtime {
                            message: e.to_string(),
                        });
                    }
                }
            }
            Ok(out)
        }
    }

    /// Load and compile `expression` with jaq's definitions and natives,
    /// then dq's (see [`paths`]), which shadow jaq's `setpath`,
    /// `delpaths` and `del`.
    fn compile_filter(expression: &str) -> Result<jaq_core::Filter<data::JustLut<Val>>, JqError> {
        let defs = jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(jaq_json::defs())
            .chain(paths::defs());
        let funs = jaq_core::funs::<data::JustLut<Val>>()
            .chain(jaq_std::funs())
            .chain(jaq_json::funs())
            .chain(paths::funs());

        let loader = Loader::new(defs);
        let arena = Arena::default();
        let program = File {
            code: expression,
            path: (),
        };

        let modules = loader
            .load(&arena, program)
            .map_err(|errs| compile_error_from_load_errors(&errs, expression))?;

        Compiler::default()
            .with_funs(funs)
            .compile(modules)
            .map_err(|errs| compile_error_from_compile_errors(&errs, expression))
    }

    /// Convert a [`serde_json::Value`] into a [`jaq_json::Val`].
    ///
    /// Goes through [`dq_core::Value::from_serde_json`], so a number
    /// literal the `arbitrary_precision` feature keeps (a big integer, a
    /// float literal no `f64` holds) reaches jq as a number with its
    /// literal intact.
    ///
    /// # Errors
    ///
    /// Never fails today; the `Result` is kept for the feature-disabled
    /// stub's sake, which returns [`JqError::FeatureDisabled`].
    pub fn serde_to_val(v: &serde_json::Value) -> Result<Val, JqError> {
        Ok(value_to_val(&dq_core::Value::from_serde_json(v)))
    }

    /// Convert a [`dq_core::Value`] into a [`jaq_json::Val`].
    ///
    /// - `Int` and `Float` (NaN and the infinities included) become the
    ///   matching jq number.
    /// - `BigInt` becomes a big integer.
    /// - `BigFloat` spelling a NaN or an infinity (`.nan`, `-.inf`, `nan`)
    ///   becomes that float; any other `BigFloat` becomes a decimal number
    ///   that keeps its literal until jq computes with it.
    /// - A number literal neither reading accepts stays a string, as
    ///   [`dq_core::Value::to_serde_json`] keeps it.
    fn value_to_val(v: &dq_core::Value) -> Val {
        match v {
            dq_core::Value::Null => Val::Null,
            dq_core::Value::Bool(b) => Val::Bool(*b),
            dq_core::Value::Int(i) => Val::Num(Num::from_integral(*i)),
            dq_core::Value::Float(f) => Val::Num(Num::Float(*f)),
            dq_core::Value::BigInt(s) | dq_core::Value::BigFloat(s) => literal_to_val(v, s),
            dq_core::Value::String(s) => Val::from(s.clone()),
            dq_core::Value::Array(items) => items.iter().map(value_to_val).collect(),
            dq_core::Value::Map(map) => Val::obj(
                map.iter()
                    .map(|(k, child)| (Val::from(k.clone()), value_to_val(child)))
                    .collect(),
            ),
        }
    }

    /// The jq number a `BigInt` / `BigFloat` literal spells.
    fn literal_to_val(v: &dq_core::Value, literal: &str) -> Val {
        if let Some(f) = v.non_finite_float() {
            return Val::Num(Num::Float(f));
        }
        if let Some(n) = Num::from_str_radix(literal, 10) {
            return Val::Num(n);
        }
        // jaq reads a decimal back with `str::parse::<f64>` and unwraps
        // the result, so only a literal that parses may become one.
        if literal.parse::<f64>().is_ok() {
            return Val::Num(Num::Dec(literal.to_owned().into()));
        }
        Val::from(literal.to_owned())
    }

    /// Convert a [`jaq_json::Val`] into a [`dq_core::Value`] — the inverse
    /// of [`value_to_val`].
    ///
    /// A float becomes a [`dq_core::Value::Float`], NaN and the infinities
    /// included; a decimal keeps its literal the way
    /// [`dq_core::Value::from_serde_json`] keeps a number's. Strings and
    /// object keys follow [`val_to_serde`].
    ///
    /// # Errors
    ///
    /// [`JqError::Conversion`] for a `Val::BStr`, an object key that
    /// formats to an empty string, or a decimal that is not a number.
    fn val_to_value(v: &Val) -> Result<dq_core::Value, JqError> {
        match v {
            Val::Null => Ok(dq_core::Value::Null),
            Val::Bool(b) => Ok(dq_core::Value::Bool(*b)),
            Val::Num(n) => num_to_value(n),
            Val::TStr(bytes) => Ok(dq_core::Value::String(bytes_to_utf8_string(bytes.as_ref()))),
            Val::BStr(_) => Err(bstr_error()),
            Val::Arr(items) => items
                .iter()
                .map(val_to_value)
                .collect::<Result<_, _>>()
                .map(dq_core::Value::Array),
            Val::Obj(map) => map
                .iter()
                .map(|(k, child)| Ok((key_to_string(k)?, val_to_value(child)?)))
                .collect::<Result<_, _>>()
                .map(dq_core::Value::Map),
        }
    }

    /// The [`dq_core::Value`] a jq number is.
    fn num_to_value(n: &Num) -> Result<dq_core::Value, JqError> {
        match n {
            Num::Int(i) => Ok(i64::try_from(*i).map_or_else(
                |_| dq_core::Value::BigInt(i.to_string()),
                dq_core::Value::Int,
            )),
            Num::BigInt(b) => {
                let literal = b.to_string();
                Ok(literal
                    .parse::<i64>()
                    .map_or(dq_core::Value::BigInt(literal), dq_core::Value::Int))
            }
            Num::Float(f) => Ok(dq_core::Value::Float(*f)),
            Num::Dec(literal) => dec_to_value(literal),
        }
    }

    /// The [`dq_core::Value`] a decimal literal is: as a reader reads it
    /// ([`dq_core::Value::from_number_literal`] — an `i64` is an `Int`, a
    /// longer integer a `BigInt`, a float literal an `f64` holds a `Float`,
    /// any other a `BigFloat` carrying it: a 30-digit `π`, `1e400`), and a
    /// word jq parses as a float (`NaN`, `inf`) that float.
    fn dec_to_value(literal: &str) -> Result<dq_core::Value, JqError> {
        if let Some(value) = dq_core::Value::from_number_literal(literal) {
            return Ok(value);
        }
        match literal.parse::<f64>() {
            Ok(f) if !literal.bytes().any(|b| b.is_ascii_digit()) => Ok(dq_core::Value::Float(f)),
            _ => Err(JqError::Conversion {
                message: format!("jaq_json::Num::Dec({literal}) is not a number"),
            }),
        }
    }

    /// JSON has no byte-string type, and neither has [`dq_core::Value`].
    fn bstr_error() -> JqError {
        JqError::Conversion {
            message: "byte-string (BStr) values cannot be represented in JSON; convert via @text or @base64 inside the filter".into(),
        }
    }

    /// Convert an [`Ir<'_>`](dq_core::Ir) into a [`jaq_json::Val`].
    ///
    /// IR-aware variant of [`serde_to_val`]: converts the underlying
    /// [`dq_core::Value`] tree directly, so a NaN or an infinity reaches jq
    /// as a number (see [`JqEngine::run_value`]).
    ///
    /// # Provenance is discarded
    ///
    /// `jaq_json::Val` cannot carry pointer-keyed provenance — it is a
    /// plain JSON shape. The conversion therefore drops the input
    /// [`dq_core::ProvenanceMap`]. Callers that need pointer-based
    /// attribution should re-emit RFC 6901 pointer strings from the jq
    /// expression itself and look them up against the **input** `Ir` via
    /// [`dq_core::Ir::span_for`] / [`dq_core::Ir::provenance_for`] — see
    /// the `data-query-exec` capability's `loc.pointer` requirement.
    ///
    /// # Errors
    ///
    /// Never fails today; the `Result` is kept for the feature-disabled
    /// stub's sake, which returns [`JqError::FeatureDisabled`].
    pub fn ir_to_val(input: &dq_core::Ir<'_>) -> Result<Val, JqError> {
        Ok(value_to_val(input.value()))
    }

    /// Convert a [`jaq_json::Val`] into a fresh [`dq_core::OwnedIr`].
    ///
    /// IR-aware variant of [`val_to_serde`]: converts the `Val` directly
    /// into a [`dq_core::Value`] (a NaN or an infinity stays a float) to
    /// produce an [`dq_core::OwnedIr`] whose [`dq_core::ProvenanceMap`]
    /// marks every value-tree node as
    /// [`dq_core::Provenance::Synthetic`] with reason
    /// [`dq_core::SyntheticReason::Computed`]. The supplied `format` is
    /// stamped onto the resulting `OwnedIr`.
    ///
    /// # Why every node is `Synthetic { Computed }`
    ///
    /// jaq's `Val` shape carries no source-location metadata — by the time
    /// a value reaches this function the input pointer has been lost (a
    /// generic jq filter such as `.x + .y` emits a value with no clear
    /// input correspondence). The `Computed` reason is the explicit "I do
    /// not know" signal callers use to suppress span lookup. Callers that
    /// need pointer attribution should opt in by emitting pointer-tagged
    /// shapes (`[pointer, value]` pairs) from their jq expressions and
    /// reconstructing provenance host-side.
    ///
    /// # Pointer enumeration
    ///
    /// Every reachable [`dq_core::Pointer`] in the produced `Value` tree
    /// gets one entry in the resulting [`dq_core::ProvenanceMap`]. The
    /// walk is inline (rather than going through
    /// [`dq_core::enumerate_pointers`], which operates on a
    /// [`dq_core::Document`]) — keying the map directly off the canonical
    /// pointer strings produced by [`dq_core::Pointer::as_canonical`]
    /// matches the same convention used by
    /// [`dq_core::Document::with_spans`] for [`dq_core::ProvenanceMap`].
    ///
    /// # Errors
    ///
    /// Returns [`JqError::Conversion`] when the underlying [`val_to_serde`]
    /// conversion fails (e.g. a non-UTF-8 [`jaq_json::Val::BStr`]).
    pub fn val_to_owned_ir(
        val: &Val,
        format: dq_core::FormatTag,
    ) -> Result<dq_core::OwnedIr, JqError> {
        let dq_value = val_to_value(val)?;
        let mut provenance = dq_core::ProvenanceMap::new();
        record_synthetic_provenance(&dq_value, &mut Vec::new(), &mut provenance);
        Ok(dq_core::OwnedIr::new(dq_value, provenance, format))
    }

    /// Walk `value` in pre-order and record one
    /// [`dq_core::Provenance::Synthetic`] entry per addressable node into
    /// `out`, keyed by the canonical RFC 6901 form of the running path.
    ///
    /// Mirrors the leaf-and-container traversal used by
    /// [`dq_core::enumerate_pointers`] but operates on a bare
    /// [`dq_core::Value`] (not a [`dq_core::Document`]) so it can run
    /// against the freshly-built tree from [`val_to_value`] without
    /// needing a document envelope.
    fn record_synthetic_provenance(
        value: &dq_core::Value,
        path: &mut Vec<dq_core::Segment>,
        out: &mut dq_core::ProvenanceMap,
    ) {
        let pointer = dq_core::Pointer::new(path.clone());
        out.insert(
            pointer.as_canonical(),
            dq_core::Provenance::Synthetic {
                reason: dq_core::SyntheticReason::Computed,
            },
        );
        match value {
            dq_core::Value::Array(items) => {
                for (idx, item) in items.iter().enumerate() {
                    path.push(dq_core::Segment::Index(idx));
                    record_synthetic_provenance(item, path, out);
                    path.pop();
                }
            }
            dq_core::Value::Map(map) => {
                for (k, v) in map {
                    path.push(dq_core::Segment::Key(k.clone()));
                    record_synthetic_provenance(v, path, out);
                    path.pop();
                }
            }
            dq_core::Value::Null
            | dq_core::Value::Bool(_)
            | dq_core::Value::Int(_)
            | dq_core::Value::BigInt(_)
            | dq_core::Value::Float(_)
            | dq_core::Value::BigFloat(_)
            | dq_core::Value::String(_) => {}
        }
    }

    /// Convert a [`jaq_json::Val`] back into a [`serde_json::Value`].
    ///
    /// Walks the `Val` enum manually:
    ///
    /// - `Val::Null` / `Val::Bool` → trivial.
    /// - `Val::Num` is rendered via `Display` and re-parsed through
    ///   `serde_json::Number::from_str`. The workspace's
    ///   `arbitrary_precision` feature on `serde_json` keeps the textual
    ///   numeric literal intact across the round-trip.
    /// - `Val::TStr` is interpreted as UTF-8 text. Bytes that are not
    ///   valid UTF-8 fall back to `String::from_utf8_lossy`, which
    ///   substitutes the Unicode replacement character (U+FFFD) for
    ///   invalid sequences. This is a documented tradeoff for M7.
    /// - `Val::BStr` (arbitrary bytes) is **rejected** as
    ///   [`JqError::Conversion`] — JSON has no byte-string type and
    ///   base64-encoding bytes silently would surprise callers.
    /// - `Val::Arr` / `Val::Obj` recurse over their items.
    ///
    /// Object key handling: jaq's `Val::Obj` allows arbitrary `Val` keys
    /// (it is a superset of JSON). For the common JSON case where keys
    /// are `Val::TStr`, we extract the underlying UTF-8 text directly so
    /// keys round-trip without spurious quoting. For non-string keys we
    /// fall back to `format!("{key}")` (jaq's `Display` impl) and reject
    /// keys that produce an empty string.
    ///
    /// Object iteration order matches the underlying `IndexMap` (which is
    /// insertion order); this preserves key order across the round-trip.
    ///
    /// # Errors
    ///
    /// - [`JqError::Conversion`] for `Val::BStr`, for non-UTF-8 keys
    ///   that produce empty `Display` output, or for numbers that fail
    ///   to parse as a `serde_json::Number`.
    pub fn val_to_serde(v: &Val) -> Result<serde_json::Value, JqError> {
        match v {
            Val::Null => Ok(serde_json::Value::Null),
            Val::Bool(b) => Ok(serde_json::Value::Bool(*b)),
            Val::Num(n) => num_to_serde_value(n),
            Val::TStr(bytes) => Ok(serde_json::Value::String(bytes_to_utf8_string(
                bytes.as_ref(),
            ))),
            Val::BStr(_) => Err(bstr_error()),
            Val::Arr(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items.iter() {
                    out.push(val_to_serde(item)?);
                }
                Ok(serde_json::Value::Array(out))
            }
            Val::Obj(map) => {
                let mut out = serde_json::Map::with_capacity(map.len());
                for (k, value) in map.iter() {
                    let key = key_to_string(k)?;
                    out.insert(key, val_to_serde(value)?);
                }
                Ok(serde_json::Value::Object(out))
            }
        }
    }

    /// Render a numeric `Val` into a `serde_json::Value::Number`.
    ///
    /// Uses `Display` (which produces a plain decimal literal for ints,
    /// `ryu`-style for finite floats, and the underlying string for
    /// `Dec`) then re-parses through `serde_json::Number::from_str`. The
    /// workspace `arbitrary_precision` feature keeps the literal intact;
    /// non-finite floats (NaN / ±Infinity) produce a `Conversion` error
    /// because JSON has no representation for them.
    fn num_to_serde_value(n: &Num) -> Result<serde_json::Value, JqError> {
        let literal = n.to_string();
        let number = serde_json::Number::from_str(&literal).map_err(|e| JqError::Conversion {
            message: format!("jaq_json::Num({literal}) -> serde_json::Number: {e}"),
        })?;
        Ok(serde_json::Value::Number(number))
    }

    /// Decode raw bytes as UTF-8 text. Falls back to lossy decoding
    /// (`U+FFFD` substitution) for invalid sequences so the data is
    /// preserved as best as JSON allows.
    fn bytes_to_utf8_string(bytes: &[u8]) -> String {
        match core::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    /// Coerce an object key to a string for `serde_json::Map`.
    ///
    /// Strings (`Val::TStr` with valid UTF-8) round-trip directly to
    /// preserve the JSON shape. Other key types are rendered via
    /// `Display` and rejected if the formatted output is empty.
    fn key_to_string(k: &Val) -> Result<String, JqError> {
        match k {
            Val::TStr(bytes) => Ok(bytes_to_utf8_string(bytes.as_ref())),
            other => {
                let formatted = format!("{other}");
                if formatted.is_empty() {
                    return Err(JqError::Conversion {
                        message:
                            "object key formatted to empty string; cannot become a JSON object key"
                                .into(),
                    });
                }
                Ok(formatted)
            }
        }
    }

    // -- compile error plumbing ----------------------------------------

    /// Maximum half-window for the snippet either side of `position`.
    const SNIPPET_HALF: usize = 30;

    /// Build a compile-error `snippet` from the source expression and the
    /// byte position of the failure. Adds `...` prefix/suffix when the
    /// snippet has been truncated.
    fn snippet_around(expression: &str, position: usize) -> String {
        let len = expression.len();
        let pos = position.min(len);
        // Find a UTF-8-safe lower bound at most `SNIPPET_HALF` bytes before pos.
        let mut start = pos.saturating_sub(SNIPPET_HALF);
        while start > 0 && !expression.is_char_boundary(start) {
            start -= 1;
        }
        // And an upper bound at most `SNIPPET_HALF` bytes after pos.
        let mut end = (pos + SNIPPET_HALF).min(len);
        while end < len && !expression.is_char_boundary(end) {
            end += 1;
        }
        let mut s = String::new();
        if start > 0 {
            s.push_str("...");
        }
        s.push_str(&expression[start..end]);
        if end < len {
            s.push_str("...");
        }
        s
    }

    /// Convert a `Vec<load::Errors<&str, ()>>` into a flat
    /// [`JqError::Compile`].
    ///
    /// `load` errors fall into three buckets (Io / Lex / Parse). For each
    /// we surface the first error's source span as the `position`, falling
    /// back to `0` when no span is available.
    fn compile_error_from_load_errors(
        errs: &jaq_core::load::Errors<&str, (), jaq_core::load::Error<&str>>,
        expression: &str,
    ) -> JqError {
        // We only ever submit one main file; `errs` may still contain entries
        // for any (transitively-resolved) prelude module, so we walk all
        // entries but format messages compactly.
        let mut messages: Vec<String> = Vec::new();
        let mut position: Option<usize> = None;

        for (file, err) in errs {
            // Prefer spans that come from the main file (the user's expression).
            let is_main = core::ptr::eq(file.code as *const str, expression as *const str)
                || file.code == expression;
            match err {
                jaq_core::load::Error::Io(items) => {
                    for (path, msg) in items {
                        messages.push(format!("io error loading `{path}`: {msg}"));
                    }
                }
                jaq_core::load::Error::Lex(items) => {
                    for (expect, span) in items {
                        if is_main && position.is_none() {
                            position = Some(jaq_core::load::span(expression, span).start);
                        }
                        messages.push(format!("expected {} (lex error)", expect.as_str()));
                    }
                }
                jaq_core::load::Error::Parse(items) => {
                    for (expect, found) in items {
                        if is_main && position.is_none() {
                            position = Some(jaq_core::load::span(expression, found).start);
                        }
                        messages.push(format!(
                            "expected {} (parse error, got `{}`)",
                            expect.as_str(),
                            found
                        ));
                    }
                }
            }
        }

        if messages.is_empty() {
            messages.push("unknown jq compile error".into());
        }
        let position = position.unwrap_or(0);
        JqError::Compile {
            snippet: snippet_around(expression, position),
            position,
            message: messages.join("; "),
        }
    }

    /// Convert a `compile::Errors<&str, ()>` into a flat
    /// [`JqError::Compile`].
    ///
    /// Each compile error carries an `(S, Undefined)` tuple where `S` is
    /// a source slice; we recover its byte offset via
    /// [`jaq_core::load::span`].
    fn compile_error_from_compile_errors(
        errs: &jaq_core::compile::Errors<&str, ()>,
        expression: &str,
    ) -> JqError {
        let mut messages: Vec<String> = Vec::new();
        let mut position: Option<usize> = None;

        for (file, file_errs) in errs {
            let is_main = core::ptr::eq(file.code as *const str, expression as *const str)
                || file.code == expression;
            for (slice, undef) in file_errs {
                if is_main && position.is_none() {
                    position = Some(jaq_core::load::span(expression, slice).start);
                }
                messages.push(format!("undefined {}: `{slice}`", undef.as_str()));
            }
        }

        if messages.is_empty() {
            messages.push("unknown jq compile error".into());
        }
        let position = position.unwrap_or(0);
        JqError::Compile {
            snippet: snippet_around(expression, position),
            position,
            message: messages.join("; "),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn dec_to_value_classifies_like_a_json_number() {
            assert_eq!(dec_to_value("12").ok(), Some(dq_core::Value::Int(12)));
            assert_eq!(dec_to_value("1.5").ok(), Some(dq_core::Value::Float(1.5)));
            assert_eq!(
                dec_to_value("1e400").ok(),
                Some(dq_core::Value::BigFloat("1e400".into()))
            );
            assert_eq!(
                dec_to_value("123456789012345678901234567890").ok(),
                Some(dq_core::Value::BigInt(
                    "123456789012345678901234567890".into()
                ))
            );
            // A literal no f64 holds keeps it; one an f64 holds exactly,
            // however spelled, is a float.
            for exact in [
                "3.14159265358979323846264338327950288",
                "12345678901234567890123.0",
                "12345678901234567890.25",
                "0.1000000000000000000001",
            ] {
                assert_eq!(
                    dec_to_value(exact).ok(),
                    Some(dq_core::Value::BigFloat(exact.into()))
                );
            }
            for (spelled, f) in [
                ("0.10000000000000001", 0.1),
                ("37.774929499999999", 37.7749295),
                ("1.10", 1.1),
                ("1e3", 1000.0),
                ("-0.0", -0.0),
                ("2.5E-1", 0.25),
            ] {
                let Ok(dq_core::Value::Float(got)) = dec_to_value(spelled) else {
                    panic!("{spelled} is an f64");
                };
                assert_eq!(got.to_bits(), f64::to_bits(f), "{spelled}");
            }
            assert!(matches!(
                dec_to_value("NaN"),
                Ok(dq_core::Value::Float(f)) if f.is_nan()
            ));
            assert_eq!(
                dec_to_value("not-a-number").map_err(|e| e.kind_name()),
                Err("conversion")
            );
        }

        #[test]
        fn literal_to_val_keeps_an_unreadable_literal_as_a_string() {
            let odd = dq_core::Value::BigFloat("1_0.5x".into());
            let Val::TStr(bytes) = literal_to_val(&odd, "1_0.5x") else {
                panic!("an unreadable literal stays a string");
            };
            assert_eq!(&bytes[..], b"1_0.5x");
        }
    }
}

#[cfg(feature = "embedded-jq")]
pub use imp::{JqEngine, ir_to_val, serde_to_val, val_to_owned_ir, val_to_serde};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "embedded-jq"))]
mod tests_enabled {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    // -- value adapter round-trips -------------------------------------

    fn roundtrip(input: serde_json::Value) -> serde_json::Value {
        let val = serde_to_val(&input).expect("serde_to_val");
        val_to_serde(&val).expect("val_to_serde")
    }

    #[test]
    fn roundtrip_null() {
        assert_eq!(roundtrip(json!(null)), json!(null));
    }

    #[test]
    fn roundtrip_bool() {
        assert_eq!(roundtrip(json!(true)), json!(true));
        assert_eq!(roundtrip(json!(false)), json!(false));
    }

    #[test]
    fn roundtrip_integer() {
        let out = roundtrip(json!(42));
        assert_eq!(out, json!(42));
        // The literal stays intact (arbitrary_precision keeps the original
        // textual form, so an integer stays integer-shaped).
        assert!(out.is_number());
    }

    #[test]
    fn roundtrip_float() {
        // A finite float that round-trips through ryu's decimal printer
        // without precision loss. Avoid `3.14` because clippy thinks
        // we're trying to approximate `PI`.
        let out = roundtrip(json!(2.5));
        assert_eq!(out.as_f64(), Some(2.5));
    }

    #[test]
    fn roundtrip_string() {
        assert_eq!(roundtrip(json!("hello")), json!("hello"));
    }

    #[test]
    fn roundtrip_array_preserves_order() {
        assert_eq!(roundtrip(json!([1, 2, 3])), json!([1, 2, 3]));
    }

    #[test]
    fn roundtrip_object_preserves_key_order() {
        let input = json!({"z": 1, "a": 2, "m": 3});
        let out = roundtrip(input.clone());
        assert_eq!(out, input);

        // Verify iteration order survives the round-trip; the workspace
        // enables `serde_json/preserve_order` and `jaq-json`'s `Map` is
        // an `IndexMap`, so insertion order is preserved end-to-end.
        let serde_json::Value::Object(map) = out else {
            panic!("expected object");
        };
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }

    #[test]
    fn roundtrip_nested_array_of_objects() {
        let input = json!([{"x": 1}, {"x": 2}]);
        assert_eq!(roundtrip(input.clone()), input);
    }

    // -- engine compile + run ------------------------------------------

    #[test]
    fn engine_identity_filter() {
        let engine = JqEngine::compile(".").expect("compile");
        let out = engine.run(&json!({"a": 1})).expect("run");
        assert_eq!(out, vec![json!({"a": 1})]);
    }

    #[test]
    fn engine_path_filter() {
        let engine = JqEngine::compile(".foo").expect("compile");
        let out = engine.run(&json!({"foo": 42})).expect("run");
        assert_eq!(out, vec![json!(42)]);
    }

    #[test]
    fn engine_update_assignment() {
        let engine = JqEngine::compile(".count |= . + 1").expect("compile");
        let out = engine.run(&json!({"count": 1})).expect("run");
        assert_eq!(out, vec![json!({"count": 2})]);
    }

    #[test]
    fn engine_array_iteration_yields_each_item() {
        let engine = JqEngine::compile(".[]").expect("compile");
        let out = engine.run(&json!([1, 2, 3])).expect("run");
        assert_eq!(out, vec![json!(1), json!(2), json!(3)]);
    }

    #[test]
    fn engine_unknown_function_is_compile_error() {
        let err = JqEngine::compile("nonexistent_fn").expect_err("expected compile error");
        assert_eq!(err.kind_name(), "compile");
        match err {
            JqError::Compile { message, .. } => {
                assert!(
                    message.to_lowercase().contains("nonexistent_fn")
                        || message.to_lowercase().contains("undefined"),
                    "expected message to mention the unknown identifier or 'undefined', got: {message}"
                );
            }
            other => panic!("expected JqError::Compile, got {other:?}"),
        }
    }

    #[test]
    fn engine_unterminated_update_is_compile_error() {
        let err = JqEngine::compile(".foo |=").expect_err("expected compile error");
        assert_eq!(err.kind_name(), "compile");
        // Snippet should include some context from around the offending
        // position; we don't pin the exact bytes because jaq's diagnostic
        // wording can shift between point releases.
        match err {
            JqError::Compile { snippet, .. } => {
                assert!(
                    !snippet.is_empty(),
                    "compile error snippet should not be empty",
                );
            }
            other => panic!("expected JqError::Compile, got {other:?}"),
        }
    }

    #[test]
    fn engine_arithmetic_on_string_is_runtime_error() {
        let engine = JqEngine::compile(". + 1").expect("compile");
        let err = engine
            .run(&json!("string"))
            .expect_err("expected runtime error");
        assert_eq!(err.kind_name(), "runtime");
    }

    #[test]
    fn engine_shared_via_arc_works() {
        // `JqEngine` does not implement `Clone` (jaq-core's `Native<D>`
        // record of function pointers doesn't derive `Clone` in 3.0), so
        // sharing across rayon workers goes through `Arc<JqEngine>`. This
        // smoke confirms an `Arc`-wrapped engine still runs.
        use std::sync::Arc;
        let engine = Arc::new(JqEngine::compile(".foo").expect("compile"));
        let shared = Arc::clone(&engine);
        let out = shared.run(&json!({"foo": 99})).expect("run on Arc clone");
        assert_eq!(out, vec![json!(99)]);
    }

    /// Static assertion that [`JqEngine`] is `Send + Sync`.
    ///
    /// This is the canary for the `sync` feature on `jaq-json`: forgetting
    /// the feature surfaces here as a non-trivial trait-bound error.
    #[test]
    fn assert_engine_send_sync() {
        fn require_send_sync<T: Send + Sync>(_: &T) {}
        let engine = JqEngine::compile(".").expect("compile");
        require_send_sync(&engine);
    }

    // -- IR-aware adapters (Phase 2 of add-ir-foundation) --------------

    #[test]
    fn ir_to_val_preserves_value_and_discards_provenance() {
        // Construct an `Ir` whose ProvenanceMap is non-empty (so we can
        // observe that the provenance is dropped on the way to `Val`); the
        // round-trip back through `val_to_owned_ir` should yield a value
        // equal to the input plus a Synthetic-only provenance map.
        use dq_core::{
            FormatTag, Ir, OwnedIr, Pointer, Provenance, ProvenanceMap, SyntheticReason, Value,
        };

        // Build the value tree via the `from_serde_json` pathway so we don't
        // need to import `indexmap` (which is not a direct dependency of
        // dq-transform).
        let value = Value::from_serde_json(&serde_json::json!({"a": 1}));

        let mut provenance = ProvenanceMap::new();
        provenance.insert(
            String::new(),
            Provenance::Synthetic {
                reason: SyntheticReason::Constructed,
            },
        );
        let pointer = Pointer::parse("/a").expect("/a parses");
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Synthetic {
                reason: SyntheticReason::Constructed,
            },
        );

        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        let val = ir_to_val(&ir).expect("ir_to_val");
        let owned: OwnedIr = val_to_owned_ir(&val, FormatTag::Yaml).expect("val_to_owned_ir");
        let (round_value, round_provenance, round_format) = owned.into_parts();
        assert_eq!(round_value, value, "value must round-trip structurally");
        assert_eq!(round_format, FormatTag::Yaml);
        // Every provenance entry must be `Synthetic { Computed }` per the
        // documented contract — even though the input map carried
        // `Synthetic { Constructed }`, ir_to_val drops it.
        assert!(
            !round_provenance.is_empty(),
            "round-tripped provenance must enumerate every node",
        );
        for (_, prov) in &round_provenance {
            match prov {
                Provenance::Synthetic { reason } => {
                    assert_eq!(*reason, SyntheticReason::Computed);
                }
                other => panic!("expected Synthetic{{Computed}}, got: {other:?}"),
            }
        }
    }

    // -- run_value: no serde_json in between ---------------------------

    /// The value `src` parses to in `format` — the shape a command
    /// hands to jq (a YAML `.nan` is a `BigFloat(".nan")`, a TOML `nan`
    /// a `Float(NaN)`).
    fn parsed(format: &str, src: &str) -> dq_core::Value {
        let format = dq_core::by_name(format).expect("known format");
        format
            .parse(src.as_bytes())
            .expect("parses")
            .value()
            .clone()
    }

    fn run_value(expression: &str, input: &dq_core::Value) -> Vec<dq_core::Value> {
        JqEngine::compile(expression)
            .expect("compile")
            .run_value(input)
            .expect("run_value")
    }

    fn string(s: &str) -> dq_core::Value {
        dq_core::Value::String(s.to_owned())
    }

    #[test]
    fn run_value_sees_yaml_and_toml_non_finite_floats_as_numbers() {
        for input in [
            parsed("yaml", "x: .nan\ny: -.inf\n"),
            parsed("toml", "x = nan\ny = -inf\n"),
        ] {
            assert_eq!(
                run_value("[.x, .y] | map(type)", &input),
                vec![dq_core::Value::Array(vec![
                    string("number"),
                    string("number")
                ])],
                "{input:?}"
            );
            assert_eq!(
                run_value(".x | isnan", &input),
                vec![dq_core::Value::Bool(true)]
            );
            assert_eq!(
                run_value(".y | isinfinite", &input),
                vec![dq_core::Value::Bool(true)]
            );
            let [dq_core::Value::Float(sum)] = run_value(".x + 1", &input)[..] else {
                panic!("NaN + 1 must be a float");
            };
            assert!(sum.is_nan());
        }
    }

    #[test]
    fn run_value_hands_untouched_non_finite_floats_back_as_the_same_data() {
        let input = parsed("yaml", "x: .nan\ny: -.inf\nz: .Inf\nn: 1\n");
        let [out] = &run_value(".", &input)[..] else {
            panic!("identity yields one output");
        };
        assert!(out.same_data(&input), "{out:?} vs {input:?}");
        assert_eq!(
            run_value(".x", &input)[0]
                .non_finite_float()
                .map(f64::is_nan),
            Some(true)
        );
    }

    #[test]
    fn run_value_returns_jq_made_non_finite_floats_as_floats() {
        let out = run_value("[nan, infinite, -infinite]", &dq_core::Value::Null);
        let [dq_core::Value::Array(items)] = &out[..] else {
            panic!("one array, got {out:?}");
        };
        let floats: Vec<Option<f64>> = items.iter().map(dq_core::Value::non_finite_float).collect();
        assert!(matches!(floats[0], Some(f) if f.is_nan()));
        assert_eq!(floats[1..], [Some(f64::INFINITY), Some(f64::NEG_INFINITY)]);
    }

    #[test]
    fn run_value_keeps_big_integer_and_big_float_literals() {
        let input = parsed(
            "json",
            r#"{"i": 12345678901234567890123, "f": 1e400, "s": 1.5}"#,
        );
        assert_eq!(run_value(".", &input), vec![input.clone()]);
        assert_eq!(
            run_value("[.i, .f] | map(type)", &input),
            vec![dq_core::Value::Array(vec![
                string("number"),
                string("number")
            ])]
        );
        assert_eq!(
            run_value(".i + 1", &input),
            vec![dq_core::Value::BigInt("12345678901234567890124".into())]
        );
    }

    #[test]
    fn run_value_rejects_a_byte_string_output() {
        let err = JqEngine::compile("\"a\" | tobytes")
            .expect("compile")
            .run_value(&dq_core::Value::Null)
            .expect_err("a byte string has no dq value");
        assert_eq!(err.kind_name(), "conversion", "{err}");
    }

    #[test]
    fn serde_run_reads_an_arbitrary_precision_float_as_a_number() {
        let input: serde_json::Value = serde_json::from_str("1e400").expect("parses");
        let engine = JqEngine::compile("type").expect("compile");
        assert_eq!(engine.run(&input).expect("run"), vec![json!("number")]);
    }

    #[test]
    fn val_to_owned_ir_carries_format_tag_through() {
        use dq_core::FormatTag;
        let val = serde_to_val(&serde_json::json!({"x": 1})).expect("serde_to_val");
        let owned = val_to_owned_ir(&val, FormatTag::Json).expect("val_to_owned_ir");
        let (_v, _p, fmt) = owned.into_parts();
        assert_eq!(fmt, FormatTag::Json);
    }
}

#[cfg(all(test, not(feature = "embedded-jq")))]
mod tests_disabled {
    use super::*;

    #[test]
    fn compile_returns_feature_disabled() {
        let err = JqEngine::compile(".").expect_err("expected feature_disabled error");
        assert_eq!(err.kind_name(), "feature_disabled");
        match err {
            JqError::FeatureDisabled { hint } => assert_eq!(hint, FEATURE_DISABLED_HINT),
            other => panic!("expected FeatureDisabled, got {other:?}"),
        }
    }

    #[test]
    fn feature_disabled_kind_name() {
        let err = JqError::FeatureDisabled {
            hint: FEATURE_DISABLED_HINT,
        };
        assert_eq!(err.kind_name(), "feature_disabled");
    }

    #[test]
    fn run_value_returns_feature_disabled() {
        let err = JqEngine
            .run_value(&dq_core::Value::Null)
            .expect_err("expected feature_disabled error");
        assert_eq!(err.kind_name(), "feature_disabled");
    }

    #[test]
    fn serde_to_val_returns_feature_disabled() {
        let err =
            serde_to_val(&serde_json::Value::Null).expect_err("expected feature_disabled error");
        assert_eq!(err.kind_name(), "feature_disabled");
    }
}
