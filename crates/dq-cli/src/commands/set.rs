//! `dq set FILE POINTER [VALUE]` — mutate the value at a JSON Pointer.
//!
//! Layered on [`Document::set_at`] (textual-edit splice) and
//! [`crate::bulk::run_per_file`] (glob expansion, `--check`,
//! `--continue-on-error`, `--parallel`, summary).
//!
//! Pipeline:
//!
//! 1. Validate the write-mode flag combination via
//!    [`Cli::ensure_write_flags_consistent`].
//! 2. Resolve the value to write ONCE (it is identical for every matched
//!    file in bulk mode) — inline arg, `--value-from`, `@<path>` prefix, or
//!    `-` for stdin.
//! 3. Parse the pointer ONCE.
//! 4. Build a [`SetFileOp`] adapter that holds the resolved value/pointer +
//!    CLI flags by reference; hand it to [`crate::bulk::run_per_file`] which
//!    short-circuits to byte-identical M2 output when the glob matches a
//!    single file.
//!
//! Per-file work executed inside [`SetFileOp::apply`]:
//!
//! 1. Resolve the format (extension or `-F`).
//! 2. Read the original bytes.
//! 3. Run the Helm/Argo/GitHub Actions template guard
//!    ([`dq_core::template_guard`]) according to the global
//!    `--allow-templates` / `--raw-template-strings` flags.
//! 4. Parse to a write-aware [`Document`] using the format-specific
//!    write-aware parser.
//! 5. Call [`Document::set_at`] to splice the byte buffer in place.
//! 6. Restore template placeholders if the substitution pass ran.
//! 7. Compute a unified-diff string when `--diff` mode is active so the
//!    bulk driver can dispatch it; never write to disk here — `--check`,
//!    `--diff`, and `-i` are all driver concerns.

use std::io::{Read, Write};
use std::str::FromStr;
use std::sync::Arc;

use camino::Utf8Path;
use dq_core::{Document, Pointer, Value};
use indexmap::IndexMap;

use super::io_helpers::{load_document_with_path, pick_format, read_bytes, value_to_serde_json};
use super::query::jq_compile_to_parse;
use crate::bulk::{self, FileOp, FileOpResult};
use crate::cli::{Cli, SetArgs};
use crate::error::InvalidInput;

/// Run `dq set`.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) for inconsistent write flags or missing value
///   source.
/// - [`dq_core::Error::TemplatedFile`] (exit 3) when the file contains Go
///   template syntax and neither escape-hatch flag is set.
/// - [`dq_core::Error::Path`] (exit 2) when the pointer addresses a missing
///   node and `--no-create` is set (or when the M2 baseline insertion path
///   declines to mkdir-p — see [`Document::set_at`] for the inline TODO).
/// - [`dq_core::Error::WriteIo`] / [`dq_core::Error::WriteUnavailable`]
///   (exit 7) on write failure.
/// - [`crate::error::CheckPending`] (exit 1) when `--check` finds at least
///   one file that would be modified.
/// - [`crate::error::BulkPartialFailure`] (exit 7) when
///   `--continue-on-error` finishes with one or more failed files.
pub fn run(
    cli: &Cli,
    args: &SetArgs,
    input_format: Option<&str>,
    use_color: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_write_flags_consistent()?;

    // M4 §4 / design D5: `--sort-keys` and `--indent` are no-ops on the
    // textual-edit splice path. The set handler reads `doc.original_bytes()`
    // verbatim (after the splice mutates the buffer in place) and never
    // routes through `Format::write_with_options`, so the canonicalization
    // knobs would be silently dropped. Surface a `debug` line so users
    // running `-vv` see why their flags didn't take — and direct them to
    // `dq fmt` for the canonical re-emit step.
    //
    // The `--jq` branch DOES route through `write_with_options` (re-emit
    // path), so the `--sort-keys` / `--indent` flags ARE honoured there;
    // the debug message above is specifically about the splice path.
    if cli.sort_keys || cli.indent.is_some() {
        tracing::debug!(
            "--sort-keys / --indent are no-ops for textual-edit splice; use `dq fmt` to canonicalize after `set`",
        );
    }

    // M7: --jq branches to the re-emit transform path. Validate the
    // pointer-vs-jq pairing first (clap already rejected positional VALUE
    // via `conflicts_with`, so we only need to check the pointer here).
    if let Some(expression) = &args.jq {
        if let Some(p) = &args.pointer
            && !p.is_empty()
            && p != "/"
        {
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "--jq applies to the document root; positional POINTER is not accepted (got '{p}')",
            ))));
        }
        return run_jq_transform(cli, args, expression, input_format, use_color, out);
    }

    // The non-jq path requires a pointer. Surface a structured InvalidInput
    // error if it's missing — the value-source guard in `resolve_value` is
    // the closest precedent for "validate input source".
    let pointer_str = args.pointer.as_deref().ok_or_else(|| {
        anyhow::Error::new(InvalidInput::new(
            "missing pointer: provide a JSON Pointer or use --jq for a whole-document transform",
        ))
    })?;

    // Resolve the value to write ONCE — every matched file in the bulk run
    // sees the same value. (The inline JSON literal heuristic, stdin
    // capture, and `--value-from` file read all happen here, before the
    // per-file loop.)
    let value = resolve_value(args)?;

    // Parse the pointer ONCE — every file uses the same pointer.
    let pointer = Pointer::parse(pointer_str).map_err(anyhow::Error::new)?;

    // NOTE: `--no-create` is honored automatically by the M2 baseline
    // because `Document::set_at` returns `Path { kind: MissingKey }` for any
    // pointer that needs an intermediate node. Once mkdir-p lands in M2 §12,
    // we'll need to gate the insertion on `args.no_create == false` inside
    // `SetFileOp::apply`.
    let _no_create_will_matter_post_mkdirp = args.no_create;

    let op = SetFileOp {
        cli,
        input_format,
        use_color,
        pointer: &pointer,
        value: &value,
    };

    let files = bulk::expand_glob(&args.file)?;
    bulk::run_per_file(files, &op, cli, out)
}

/// `FileOp` adapter that holds the resolved value/pointer + CLI flags by
/// reference so rayon can spread `apply` across worker threads without
/// cloning the value or pointer for every file.
struct SetFileOp<'a> {
    cli: &'a Cli,
    input_format: Option<&'a str>,
    use_color: bool,
    pointer: &'a Pointer,
    value: &'a Value,
}

impl<'a> FileOp for SetFileOp<'a> {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
        // 1. Resolve format up-front (before reading bytes) so we can fail
        //    fast on `-F xml` without doing any I/O.
        let format = pick_format(path, self.input_format)?;

        // 2. Read the original bytes. Used for the diff comparison and the
        //    template guard; parse input may be substituted for the guard's
        //    sake before being handed to the parser.
        let original_bytes = read_bytes(path)?;

        // 3. Template guard: detect / substitute / pass-through depending
        //    on which escape-hatch flag is active. Stored as
        //    `Option<HashMap>` because we only need the restoration map
        //    when substitution ran.
        let (parse_input, placeholder_map) = if self.cli.raw_template_strings {
            let (substituted, map) =
                dq_core::template_guard::substitute_placeholders(&original_bytes);
            (substituted, Some(map))
        } else if self.cli.allow_templates {
            (original_bytes.clone(), None)
        } else if let Some(marker) = dq_core::template_guard::detect_templates(&original_bytes) {
            return Err(anyhow::Error::new(dq_core::Error::templated_file(marker)));
        } else {
            (original_bytes.clone(), None)
        };

        // 4. Parse to a write-aware Document. YAML and JSON have dedicated
        //    span-collecting entry points; TOML and JSONL go through the
        //    `Format::parse` trait method (TOML produces a write-aware doc
        //    via `Toml::parse`; JSONL is read-only, so `set_at` will return
        //    `WriteUnavailable` and the exit-code mapper picks 7).
        let mut document = parse_to_document(format, &parse_input, path)?;

        // 5. Apply the write. Cloning the value is unavoidable because
        //    `set_at` consumes a `Value` (the engine takes ownership of the
        //    new node); we cannot share `&self.value` across worker threads
        //    that all call `set_at`.
        document
            .set_at(self.pointer, self.value.clone())
            .map_err(anyhow::Error::new)?;

        // 6. Compute the final output bytes, restoring template
        //    placeholders if the substitution pass ran.
        let mut final_bytes = document.original_bytes().to_vec();
        if let Some(map) = &placeholder_map {
            final_bytes = dq_core::template_guard::restore_placeholders(&final_bytes, map);
        }

        // 7. The bulk driver handles `-i` and `--check` uniformly — we
        //    only need to compute the diff string when `--diff` mode is
        //    active so the driver can dispatch it to stdout.
        let diff = if self.cli.diff {
            // Diff against the user's original file bytes (NOT the
            // template-substituted version) so the user sees a diff that
            // maps back to what they have on disk.
            let original_str = String::from_utf8_lossy(&original_bytes);
            let modified_str = String::from_utf8_lossy(&final_bytes);
            Some(crate::diff::render_unified(
                &original_str,
                &modified_str,
                path.as_str(),
                self.use_color,
            ))
        } else {
            None
        };

        Ok(FileOpResult::Modified {
            output_bytes: final_bytes,
            diff,
        })
    }
}

/// `FileOp` adapter for the `--jq` transform path. Holds an
/// `Arc<JqEngine>` so the (non-`Clone`) engine can fan out to rayon workers.
struct JqFileOp<'a> {
    cli: &'a Cli,
    input_format: Option<&'a str>,
    use_color: bool,
    engine: Arc<dq_transform::JqEngine>,
}

impl<'a> FileOp for JqFileOp<'a> {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
        // Resolve format up-front so we can fail fast on `-F xml` without
        // doing any I/O.
        let format = pick_format(path, self.input_format)?;

        // Read the original bytes — we need them for the diff comparison and
        // for re-parsing through the value-only path.
        let original_bytes = read_bytes(path)?;
        let document = format.parse(&original_bytes).map_err(anyhow::Error::new)?;

        // Convert the document's top-level value into serde_json so the jq
        // engine can consume it.
        let serde_value = value_to_serde_json(document.value());

        // Evaluate the filter. Runtime / Conversion errors stay as plain
        // anyhow (exit 1) — they aren't compile failures and aren't IO
        // failures. The compile error is handled at the engine-construction
        // site in `run_jq_transform`, before we get here.
        let outputs = self
            .engine
            .run(&serde_value)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        // The single-output rule (design D5): collect outputs, require
        // exactly one. The error message names the count so users know to
        // wrap iterating filters in `[.[]]`.
        let single = match outputs.as_slice() {
            [v] => v,
            [] => {
                return Err(anyhow::Error::new(InvalidInput::new(
                    "--jq filter produced 0 outputs (document would become empty); use `[...]` to collect or pick a non-empty filter",
                )));
            }
            many => {
                return Err(anyhow::Error::new(InvalidInput::new(format!(
                    "--jq filter produced {} outputs (expected exactly 1); wrap iteration in `[...]` to collect into an array",
                    many.len(),
                ))));
            }
        };

        // Build a value-only document for the re-emit path.
        let new_value = serde_json_to_dq_value(single);
        let new_doc = Document::value_only(new_value, document.format());

        // Re-emit through `write_with_options` so global `--sort-keys` /
        // `--indent` are honoured (this is one of the differences vs the
        // splice path).
        let mut final_bytes = Vec::new();
        format
            .write_with_options(&new_doc, &mut final_bytes, &self.cli.write_options())
            .map_err(anyhow::Error::new)?;

        // Diff handling mirrors the splice path's `SetFileOp::apply`: only
        // compute the diff when `--diff` is set.
        let diff = if self.cli.diff {
            let original_str = String::from_utf8_lossy(&original_bytes);
            let modified_str = String::from_utf8_lossy(&final_bytes);
            Some(crate::diff::render_unified(
                &original_str,
                &modified_str,
                path.as_str(),
                self.use_color,
            ))
        } else {
            None
        };

        Ok(FileOpResult::Modified {
            output_bytes: final_bytes,
            diff,
        })
    }
}

/// Compile the jq expression once and dispatch the bulk driver against the
/// resulting [`JqFileOp`]. The engine is wrapped in an [`Arc`] so each rayon
/// worker shares the same compiled filter (the engine is `Send + Sync` but
/// not `Clone`).
fn run_jq_transform(
    cli: &Cli,
    args: &SetArgs,
    expression: &str,
    input_format: Option<&str>,
    use_color: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    // The re-emit path does not preserve `{{ ... }}` template positions, so
    // neither template-guard escape hatch can round-trip through `--jq`.
    // Reject the combination explicitly with `InvalidInput` (exit 6) instead
    // of letting the parser fail later on the raw template markers — the
    // error message names BOTH flags so users (and audit grepping) can map
    // the rejection back to the responsible CLI surface.
    if cli.allow_templates {
        return Err(anyhow::Error::new(InvalidInput::new(
            "--jq is incompatible with --allow-templates: the re-emit path does not preserve template placeholders. Use point-edits (`dq set FILE POINTER VALUE`) for templated files.",
        )));
    }
    if cli.raw_template_strings {
        return Err(anyhow::Error::new(InvalidInput::new(
            "--jq is incompatible with --raw-template-strings: the re-emit path does not restore template placeholders. Use point-edits (`dq set FILE POINTER VALUE`) for templated files.",
        )));
    }

    tracing::debug!(
        "dq set --jq routes through Format::write_with_options; comments will be lost in re-emit",
    );

    // Compile once; bubble compile failures up as `dq_core::Error::Parse`
    // (exit 3) the same way `dq query` does.
    let engine = dq_transform::JqEngine::compile(expression)
        .map_err(|e| anyhow::Error::new(jq_compile_to_parse(e, &args.file, expression)))?;

    let op = JqFileOp {
        cli,
        input_format,
        use_color,
        engine: Arc::new(engine),
    };

    let files = bulk::expand_glob(&args.file)?;
    bulk::run_per_file(files, &op, cli, out)
}

/// Parse `parse_input` as `format` into a write-aware [`Document`].
///
/// YAML and JSON are routed through the dedicated span-collecting parsers
/// (`parse_yaml_with_spans` / `parse_json_with_spans`); every other format
/// falls through to the generic `Format::parse` trait method (which is
/// write-aware for TOML and read-only for JSONL).
fn parse_to_document(
    format: &'static dyn dq_core::Format,
    parse_input: &[u8],
    file: &Utf8Path,
) -> anyhow::Result<Document> {
    let path_label = camino::Utf8PathBuf::from(file);
    let doc_result: dq_core::Result<Document> = match format.name() {
        "yaml" => dq_core::parse_yaml_with_spans(parse_input),
        "json" => dq_core::parse_json_with_spans(parse_input),
        _ => format.parse(parse_input),
    };
    doc_result
        .map_err(|mut e| {
            if let dq_core::Error::Parse { ref mut file, .. } = e
                && file.is_none()
            {
                *file = Some(path_label.clone());
            }
            e
        })
        .map_err(anyhow::Error::new)
}

/// Resolve the value-source flags into a [`Value`].
///
/// Priority is the spec's: `--value-from <path>` wins over the inline
/// argument; the inline argument's leading `-` reads stdin; a `@`-prefix
/// treats the rest as a file path; otherwise the inline string is passed
/// through the JSON-literal heuristic.
fn resolve_value(args: &SetArgs) -> anyhow::Result<Value> {
    if let Some(path) = &args.value_from {
        return value_from_file(path);
    }
    let Some(inline) = &args.value else {
        return Err(anyhow::Error::new(InvalidInput::new(
            "missing value: provide an inline arg, --value-from, or '-' for stdin",
        )));
    };
    if inline == "-" {
        return value_from_stdin(args.value_string);
    }
    if let Some(path_str) = inline.strip_prefix('@') {
        let path = camino::Utf8PathBuf::from(path_str);
        return value_from_file(&path);
    }
    Ok(parse_inline_value(inline, args.value_string))
}

/// Parse a file at `path` as a structured document and return its top-level
/// value. Used by both `--value-from` and the `@<path>` shorthand.
fn value_from_file(path: &Utf8Path) -> anyhow::Result<Value> {
    let (_fmt, doc) = load_document_with_path(path, None)?;
    Ok(doc.value().clone())
}

/// Read all of stdin and apply the JSON-literal heuristic to the result.
///
/// Stdin is required to be UTF-8 — non-UTF-8 input produces an
/// [`InvalidInput`] error rather than silently lossy-decoding (which would
/// be a hidden footgun for users piping binary data into a text-oriented
/// CLI).
fn value_from_stdin(force_string: bool) -> anyhow::Result<Value> {
    let mut buf = Vec::new();
    std::io::stdin().lock().read_to_end(&mut buf)?;
    let s = String::from_utf8(buf).map_err(|_| {
        InvalidInput::new("stdin must be UTF-8 (binary input is not supported by `dq set -`)")
    })?;
    Ok(parse_inline_value(&s, force_string))
}

/// Apply the JSON-literal heuristic to an inline string.
///
/// - `--value-string` short-circuits to `Value::String`.
/// - Trimmed input that begins with `{`, `[`, a digit, or `-`, or matches
///   `true`/`false`/`null` exactly, is passed through `serde_json` and
///   converted to a `dq_core::Value`. Failures fall back to `Value::String`.
/// - Anything else is `Value::String`.
fn parse_inline_value(s: &str, force_string: bool) -> Value {
    if force_string {
        return Value::String(s.to_owned());
    }
    let trimmed = s.trim();
    let looks_like_json = matches!(trimmed.chars().next(), Some('{' | '[' | '-'))
        || trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
        || matches!(trimmed, "true" | "false" | "null");
    if !looks_like_json {
        return Value::String(s.to_owned());
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(json) => serde_json_to_dq_value(&json),
        Err(_) => Value::String(s.to_owned()),
    }
}

/// Convert a [`serde_json::Value`] into a [`Value`].
///
/// This is the inverse of `io_helpers::value_to_serde_json`. Numbers that
/// don't fit `i64` / `f64` are preserved as `BigInt` / `BigFloat` literals
/// using the original textual representation so a `set` of a 22-digit
/// integer round-trips byte-for-byte through `get`.
fn serde_json_to_dq_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => number_to_value(n),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().map(serde_json_to_dq_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, child) in map {
                out.insert(k.clone(), serde_json_to_dq_value(child));
            }
            Value::Map(out)
        }
    }
}

fn number_to_value(n: &serde_json::Number) -> Value {
    // With the workspace's `arbitrary_precision` feature on `serde_json`,
    // `Number::to_string()` returns the original textual literal verbatim —
    // `as_i64()` / `as_f64()` lossily parse from that string and would
    // collapse a 22-digit integer to a `Float(4.7e21)`. Mirror the parsing
    // heuristic `dq-core::parsers::json::json_number_to_value` uses on the
    // read side: try `i64` first, then float-with-round-trip, falling back
    // to `BigInt` / `BigFloat` literals.
    let literal = n.to_string();
    if let Ok(i) = literal.parse::<i64>() {
        return Value::Int(i);
    }
    if literal.contains('.') || literal.contains('e') || literal.contains('E') {
        if let Ok(f) = f64::from_str(&literal)
            && f.is_finite()
            && f64_matches_literal(f, &literal)
        {
            return Value::Float(f);
        }
        return Value::BigFloat(literal);
    }
    Value::BigInt(literal)
}

/// Lossless-round-trip check for a parsed `f64` against its source literal.
///
/// Mirrors `dq_core::parsers::json::f64_matches_literal`: re-parse the
/// shortest float formatting and compare for exact equality so cosmetic
/// reformatting (e.g. `1e2` vs `100`) doesn't trigger the BigFloat branch.
fn f64_matches_literal(f: f64, literal: &str) -> bool {
    let formatted = format!("{f}");
    f64::from_str(&formatted).is_ok_and(|round_trip| round_trip.to_bits() == f.to_bits())
        && literal_round_trips_to(literal, f)
}

fn literal_round_trips_to(literal: &str, f: f64) -> bool {
    f64::from_str(literal).is_ok_and(|parsed| parsed.to_bits() == f.to_bits())
}

#[cfg(test)]
mod tests {
    // `super::*` already brings `std::io::Write` into scope, so
    // `tmp.write_all(...)` inside `write_yaml` resolves through it.
    use super::*;
    use clap::Parser;
    use tempfile::NamedTempFile;

    /// Build a default `Cli` with no write flags set, suitable for handler
    /// tests that only need a parsed `Cli` to satisfy the `cli: &Cli`
    /// parameter.
    fn cli_for(extra: &[&str]) -> Cli {
        let mut argv = vec!["dq"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["set", "ignored.yaml", "/x", "1"]);
        Cli::try_parse_from(argv).expect("clap parse")
    }

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    #[test]
    fn set_scalar_replaces_value_in_place() {
        // Smoke test for the `-i` path: the file content must match exactly
        // after the splice, with no spurious whitespace / encoding drift.
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli =
            Cli::try_parse_from(["dq", "-i", "set", path.as_str(), "/spec/replicas", "5"]).unwrap();
        let args = SetArgs {
            file: path.clone(),
            pointer: Some("/spec/replicas".to_owned()),
            value: Some("5".to_owned()),
            value_from: None,
            value_string: false,
            no_create: false,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("set should succeed");
        assert!(out.is_empty(), "in-place mode must not write to stdout");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "spec:\n  replicas: 5\n");
    }

    #[test]
    fn set_outputs_full_doc_to_stdout_by_default() {
        // No `-i`, no `--diff` → stdout receives the full document, the
        // file on disk is untouched. CRITICAL: this is the M2 byte-identical
        // path. The bulk driver's single-file fast path must NOT add a
        // `=== <path> ===` marker.
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_for(&[]);
        let args = SetArgs {
            file: path.clone(),
            pointer: Some("/spec/replicas".to_owned()),
            value: Some("5".to_owned()),
            value_from: None,
            value_string: false,
            no_create: false,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("set should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("replicas: 5"), "got: {s}");
        assert!(
            !s.contains("==="),
            "single-file mode must not emit per-file markers, got: {s}",
        );
        assert!(
            !s.contains("Modified:"),
            "single-file mode must not emit a summary, got: {s}",
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, "spec:\n  replicas: 3\n",
            "file must be unchanged when -i is not set",
        );
    }

    #[test]
    fn set_diff_mode_renders_unified_diff() {
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli =
            Cli::try_parse_from(["dq", "--diff", "set", path.as_str(), "/spec/replicas", "5"])
                .unwrap();
        let args = SetArgs {
            file: path,
            pointer: Some("/spec/replicas".to_owned()),
            value: Some("5".to_owned()),
            value_from: None,
            value_string: false,
            no_create: false,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("set should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("-  replicas: 3"),
            "expected `-  replicas: 3` in diff, got:\n{s}",
        );
        assert!(
            s.contains("+  replicas: 5"),
            "expected `+  replicas: 5` in diff, got:\n{s}",
        );
    }

    #[test]
    fn set_no_create_rejects_missing_pointer() {
        // Per spec: `--no-create` forces a Path error when any intermediate
        // segment is missing. Until M2 §12 wires mkdir-p into the textual-edit
        // pipeline, `Document::set_at` already returns `MissingKey` for any
        // missing pointer, so this test exercises both the no-create flag
        // path AND the baseline mkdir-p stub.
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_for(&[]);
        let args = SetArgs {
            file: path,
            pointer: Some("/nonexistent/deep/path".to_owned()),
            value: Some("hello".to_owned()),
            value_from: None,
            value_string: false,
            no_create: true,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "path");
    }

    #[test]
    fn set_value_string_forces_string_interpretation() {
        // `8080` looks like an integer literal — without `--value-string` it
        // would parse as `Value::Int(8080)`. With the flag, it must be a
        // string in the rendered output.
        let tmp = write_yaml("port: 80\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_for(&[]);
        let args = SetArgs {
            file: path,
            pointer: Some("/port".to_owned()),
            value: Some("8080".to_owned()),
            value_from: None,
            value_string: true,
            no_create: false,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("set should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("'8080'") || s.contains("\"8080\""),
            "expected quoted string in output, got:\n{s}",
        );
    }

    #[test]
    fn set_inline_json_literal_heuristic_parses_int() {
        // The complementary case to `set_value_string_forces_string_interpretation`:
        // `8080` without the flag MUST end up as an integer.
        let tmp = write_yaml("port: 80\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_for(&[]);
        let args = SetArgs {
            file: path,
            pointer: Some("/port".to_owned()),
            value: Some("8080".to_owned()),
            value_from: None,
            value_string: false,
            no_create: false,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("set should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("port: 8080"), "got:\n{s}");
        assert!(
            !s.contains("'8080'") && !s.contains("\"8080\""),
            "got quoted, expected unquoted int:\n{s}",
        );
    }

    #[test]
    fn set_rejects_templated_file_by_default() {
        // Without an escape-hatch flag, a Helm-style template must produce
        // a structured `TemplatedFile` error before any parse attempt.
        let tmp = write_yaml("image:\n  tag: {{ .Values.image.tag }}\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_for(&[]);
        let args = SetArgs {
            file: path,
            pointer: Some("/image/tag".to_owned()),
            value: Some("v2".to_owned()),
            value_from: None,
            value_string: false,
            no_create: false,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "templated_file");
    }

    #[test]
    fn set_with_raw_template_strings_substitutes_and_restores() {
        // Round-trip property: with `--raw-template-strings`, set an
        // unrelated value and confirm the template block is preserved
        // verbatim in the output.
        let src = "name: my-app\nimage:\n  tag: {{ .Values.image.tag }}\n";
        let tmp = write_yaml(src);
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from([
            "dq",
            "--raw-template-strings",
            "set",
            path.as_str(),
            "/name",
            "your-app",
        ])
        .unwrap();
        let args = SetArgs {
            file: path,
            pointer: Some("/name".to_owned()),
            value: Some("your-app".to_owned()),
            value_from: None,
            value_string: false,
            no_create: false,
            jq: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("set should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("{{ .Values.image.tag }}"),
            "template block must round-trip, got:\n{s}",
        );
        assert!(
            s.contains("your-app"),
            "edited value must be present, got:\n{s}",
        );
    }

    // ---------------------------------------------------------------------
    // M7: --jq transform branch
    // ---------------------------------------------------------------------

    #[test]
    fn set_jq_increments_field_in_place() {
        // The headline use case: `dq set f.yaml --jq '.spec.replicas |= .
        // + 1' -i` increments the field on disk through the re-emit path.
        // Comments would be lost — the test fixture has none on purpose.
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from([
            "dq",
            "-i",
            "set",
            path.as_str(),
            "--jq",
            ".spec.replicas |= . + 1",
        ])
        .unwrap();
        let args = SetArgs {
            file: path.clone(),
            pointer: None,
            value: None,
            value_from: None,
            value_string: false,
            no_create: false,
            jq: Some(".spec.replicas |= . + 1".to_owned()),
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("--jq increment should succeed");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("replicas: 4"),
            "expected replicas to be 4 on disk, got:\n{on_disk}",
        );
    }

    #[test]
    fn set_jq_multi_output_filter_is_rejected() {
        // `.[]` yields each item separately — when applied to an array
        // document, it produces N outputs. The single-output rule (D5)
        // rejects this with `InvalidInput` so the user cannot silently lose
        // data; the message names the count to point them at `[.[]]`.
        let tmp = write_yaml("- 1\n- 2\n- 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "-i", "set", path.as_str(), "--jq", ".[]"]).unwrap();
        let args = SetArgs {
            file: path,
            pointer: None,
            value: None,
            value_from: None,
            value_string: false,
            no_create: false,
            jq: Some(".[]".to_owned()),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("multi-output stream must be rejected");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "rejection must carry InvalidInput so exit code is 6, got: {err:?}",
        );
        assert!(
            err.to_string().contains("3"),
            "error should name the output count, got: {err}",
        );
    }

    #[test]
    fn set_jq_empty_output_filter_is_rejected() {
        // `empty` produces zero outputs — accepting it would silently make
        // the document empty. Rejected with `InvalidInput`.
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "-i", "set", path.as_str(), "--jq", "empty"]).unwrap();
        let args = SetArgs {
            file: path,
            pointer: None,
            value: None,
            value_from: None,
            value_string: false,
            no_create: false,
            jq: Some("empty".to_owned()),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("empty output stream must be rejected");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "rejection must carry InvalidInput so exit code is 6, got: {err:?}",
        );
        assert!(
            err.to_string().contains('0') || err.to_string().to_lowercase().contains("empty"),
            "error should mention zero / empty, got: {err}",
        );
    }

    #[test]
    fn set_jq_rejected_with_allow_templates() {
        // The re-emit path can't preserve template placeholder positions, so
        // `--jq` + `--allow-templates` is a contract violation: reject before
        // any I/O with `InvalidInput` (exit 6). The error MUST name both
        // flags so users (and `cargo deny`-style grep audits) can trace the
        // rejection back to the CLI surface.
        let tmp = write_yaml("foo: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from([
            "dq",
            "--allow-templates",
            "set",
            path.as_str(),
            "--jq",
            ".foo |= 2",
            "-i",
        ])
        .unwrap();
        let args = SetArgs {
            file: path,
            pointer: None,
            value: None,
            value_from: None,
            value_string: false,
            no_create: false,
            jq: Some(".foo |= 2".to_owned()),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out).unwrap_err();
        assert!(
            err.downcast_ref::<crate::error::InvalidInput>().is_some(),
            "must carry InvalidInput marker so exit-code mapper picks 6, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("--jq"), "error must name --jq: {msg}");
        assert!(
            msg.contains("--allow-templates"),
            "error must name --allow-templates: {msg}"
        );
    }

    #[test]
    fn set_jq_rejected_with_raw_template_strings() {
        // Same contract as `set_jq_rejected_with_allow_templates`, but for
        // the substitute-and-restore escape hatch. The re-emit path doesn't
        // round-trip placeholder positions, so accepting this combination
        // would silently drop the restored markers; reject up front.
        let tmp = write_yaml("foo: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from([
            "dq",
            "--raw-template-strings",
            "set",
            path.as_str(),
            "--jq",
            ".foo |= 2",
            "-i",
        ])
        .unwrap();
        let args = SetArgs {
            file: path,
            pointer: None,
            value: None,
            value_from: None,
            value_string: false,
            no_create: false,
            jq: Some(".foo |= 2".to_owned()),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out).unwrap_err();
        assert!(err.downcast_ref::<crate::error::InvalidInput>().is_some());
        let msg = err.to_string();
        assert!(msg.contains("--jq"));
        assert!(msg.contains("--raw-template-strings"), "got: {msg}");
    }

    #[test]
    fn set_jq_with_positional_value_is_rejected_at_parse_time() {
        // `--jq` `conflicts_with = "value"` at the clap level: passing both
        // a positional VALUE AND `--jq` must fail at parse time before the
        // handler runs.
        let result = Cli::try_parse_from(["dq", "set", "f.yaml", "/x", "5", "--jq", ". + 1"]);
        assert!(
            result.is_err(),
            "clap should reject --jq with positional VALUE, got: {result:?}",
        );
    }

    // TODO(M2): Add a stdin test once `run` accepts a `&mut dyn Read` for
    // the stdin source. The current implementation reads from
    // `std::io::stdin().lock()` which makes process-level fixtures the
    // only way to drive it from a unit test. The integration tests in
    // `tests/cli_*.rs` cover the stdin path end-to-end via `assert_cmd`.

    #[test]
    fn parse_inline_value_int_heuristic() {
        match parse_inline_value("42", false) {
            Value::Int(42) => {}
            other => panic!("expected Int(42), got: {other:?}"),
        }
    }

    #[test]
    fn parse_inline_value_force_string() {
        match parse_inline_value("42", true) {
            Value::String(s) if s == "42" => {}
            other => panic!("expected String(\"42\"), got: {other:?}"),
        }
    }

    #[test]
    fn parse_inline_value_object_literal() {
        match parse_inline_value("{\"a\":1}", false) {
            Value::Map(m) => {
                assert_eq!(m.get("a"), Some(&Value::Int(1)));
            }
            other => panic!("expected Map, got: {other:?}"),
        }
    }

    #[test]
    fn parse_inline_value_falls_back_to_string_on_invalid_json() {
        // `{not json}` triggers the heuristic (leading `{`) but fails to
        // parse — the fallback must be the literal string, not a parse error.
        match parse_inline_value("{not json}", false) {
            Value::String(s) => assert_eq!(s, "{not json}"),
            other => panic!("expected String fallback, got: {other:?}"),
        }
    }

    #[test]
    fn parse_inline_value_recognises_keywords() {
        assert_eq!(parse_inline_value("true", false), Value::Bool(true));
        assert_eq!(parse_inline_value("false", false), Value::Bool(false));
        assert_eq!(parse_inline_value("null", false), Value::Null);
    }

    #[test]
    fn serde_json_to_dq_value_preserves_big_int() {
        // 22-digit integer is outside `i64::MAX` range — must round-trip as
        // a `BigInt` literal so the bytes survive a set→get pair byte-for-byte.
        let raw = "4722366482869645213696";
        let json: serde_json::Value = serde_json::from_str(raw).unwrap();
        match serde_json_to_dq_value(&json) {
            Value::BigInt(s) => assert_eq!(s, raw),
            other => panic!("expected BigInt, got: {other:?}"),
        }
    }
}
