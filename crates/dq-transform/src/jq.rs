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
}

#[cfg(not(feature = "embedded-jq"))]
pub use stub::{JqEngine, serde_to_val, val_to_serde};

// ---------------------------------------------------------------------------
// Real implementation behind the `embedded-jq` feature.
// ---------------------------------------------------------------------------

#[cfg(feature = "embedded-jq")]
mod imp {
    use super::JqError;
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

            let defs = jaq_core::defs()
                .chain(jaq_std::defs())
                .chain(jaq_json::defs());
            let funs = jaq_core::funs::<data::JustLut<Val>>()
                .chain(jaq_std::funs())
                .chain(jaq_json::funs());

            let loader = Loader::new(defs);
            let arena = Arena::default();
            let program = File {
                code: expression,
                path: (),
            };

            let modules = loader
                .load(&arena, program)
                .map_err(|errs| compile_error_from_load_errors(&errs, expression))?;

            let filter = Compiler::default()
                .with_funs(funs)
                .compile(modules)
                .map_err(|errs| compile_error_from_compile_errors(&errs, expression))?;

            Ok(Self { filter })
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
    }

    /// Convert a [`serde_json::Value`] into a [`jaq_json::Val`].
    ///
    /// Uses the `Val: Deserialize` impl provided by the `serde` feature on
    /// `jaq-json`. The conversion is total: any value that round-trips
    /// through `serde_json::from_value` will produce a valid `Val`.
    ///
    /// # Errors
    ///
    /// Returns [`JqError::Conversion`] if `serde_json::from_value` fails
    /// (e.g. a numeric literal under the `arbitrary_precision` feature
    /// that is neither integer nor finite float — an unusual case in
    /// practice).
    pub fn serde_to_val(v: &serde_json::Value) -> Result<Val, JqError> {
        serde_json::from_value::<Val>(v.clone()).map_err(|e| JqError::Conversion {
            message: format!("serde_json -> jaq_json::Val: {e}"),
        })
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
            Val::BStr(_) => Err(JqError::Conversion {
                message: "byte-string (BStr) values cannot be represented in JSON; convert via @text or @base64 inside the filter".into(),
            }),
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
}

#[cfg(feature = "embedded-jq")]
pub use imp::{JqEngine, serde_to_val, val_to_serde};

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
    fn serde_to_val_returns_feature_disabled() {
        let err =
            serde_to_val(&serde_json::Value::Null).expect_err("expected feature_disabled error");
        assert_eq!(err.kind_name(), "feature_disabled");
    }
}
