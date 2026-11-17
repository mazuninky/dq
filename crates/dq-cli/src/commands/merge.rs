//! `dq merge FILE [PATCH]` — apply an RFC 7396 JSON Merge Patch.
//!
//! Layered on [`dq_core::apply_merge`] (clone-on-apply atomicity) and
//! [`crate::bulk::run_per_file`] (glob expansion, `--check`,
//! `--continue-on-error`, `--parallel`, summary).
//!
//! Pipeline (per file, executed inside [`MergeFileOp::apply`]):
//!
//! 1. Resolve the format (extension or `-F`).
//! 2. Read the original bytes.
//! 3. Run the template guard (mirrors `commands::set` / `commands::patch`).
//! 4. Parse to a write-aware [`Document`].
//! 5. Apply the pre-resolved merge patch via [`dq_core::apply_merge`] —
//!    atomic rollback on any failure is the engine's responsibility.
//! 6. Restore template placeholders if the substitution pass ran.
//! 7. Compute the unified-diff string when `--diff` mode is active so the
//!    bulk driver can dispatch it; never write to disk here — `--check`,
//!    `--diff`, and `-i` are all driver concerns.

use std::io::{Read, Write};
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use dq_core::{Document, Value};
use indexmap::IndexMap;

use crate::bulk::{self, FileOp, FileOpResult};
use crate::cli::{Cli, MergeArgs};
use crate::commands::io_helpers::{pick_format, read_bytes};
use crate::error::InvalidInput;

/// Run `dq merge`.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) for inconsistent write flags or a missing
///   patch source.
/// - [`dq_core::Error::TemplatedFile`] (exit 3) when a file contains Go
///   template syntax and neither `--allow-templates` nor
///   `--raw-template-strings` is set.
/// - [`dq_core::Error::Path`] (exit 2) when the patch addresses a path
///   that cannot be resolved against the target document during merge.
/// - [`crate::error::CheckPending`] (exit 1) when `--check` finds at least
///   one file that would be modified.
/// - [`crate::error::BulkPartialFailure`] (exit 7) when
///   `--continue-on-error` finishes with one or more failed files.
pub fn run(
    cli: &Cli,
    args: &MergeArgs,
    input_format: Option<&str>,
    use_color: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_write_flags_consistent()?;

    // M4 §4 / design D5: `--sort-keys` and `--indent` are no-ops on the
    // textual-edit splice path. See `commands::set::run` for the rationale.
    if cli.sort_keys || cli.indent.is_some() {
        tracing::debug!(
            "--sort-keys / --indent are no-ops for textual-edit splice; use `dq fmt` to canonicalize after `merge`",
        );
    }

    // Resolve the merge patch once, before iterating files. Every matched
    // file in the bulk run sees the same patch value.
    let patch = resolve_patch(args)?;

    let op = MergeFileOp {
        patch: &patch,
        input_format,
        cli,
        use_color,
    };

    let files = bulk::expand_glob(&args.file)?;
    bulk::run_per_file(files, &op, cli, out)
}

/// `FileOp` adapter that holds the resolved patch + CLI flags by reference
/// so rayon can spread `apply` across worker threads without cloning.
struct MergeFileOp<'a> {
    patch: &'a Value,
    input_format: Option<&'a str>,
    cli: &'a Cli,
    use_color: bool,
}

impl<'a> FileOp for MergeFileOp<'a> {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
        // Mirror `commands::patch::PatchFileOp::apply`, swapping
        // `apply_patch(ops)` for `apply_merge(patch)`.
        let format = pick_format(path, self.input_format)?;
        let original_bytes = read_bytes(path)?;

        // Template guard: detect / substitute / pass-through depending on
        // which escape-hatch flag is active.
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

        let mut document = parse_to_document(format, &parse_input, path)?;

        // Apply the merge patch atomically — on any failure, the engine
        // leaves `document` untouched (clone-on-apply).
        dq_core::apply_merge(&mut document, self.patch).map_err(anyhow::Error::new)?;

        // Compute final bytes, restoring template placeholders when the
        // substitution pass ran.
        let mut final_bytes = document.original_bytes().to_vec();
        if let Some(map) = &placeholder_map {
            final_bytes = dq_core::template_guard::restore_placeholders(&final_bytes, map);
        }

        // The bulk driver handles `-i` and `--check` uniformly — we only
        // need to compute the diff string when `--diff` mode is active so
        // the driver can dispatch it to stdout.
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

/// Parse `parse_input` as `format` into a write-aware [`Document`].
///
/// TODO(M3 §10): third copy of `parse_to_document` — extract.
/// (First copy: `crate::commands::set`. Second copy:
/// `crate::commands::patch`. This is the third.) Each handler still owns
/// its own template-guard dance, so a half-shared helper would obscure
/// rather than simplify until the §10 cleanup pass.
fn parse_to_document(
    format: &'static dyn dq_core::Format,
    parse_input: &[u8],
    file: &Utf8Path,
) -> anyhow::Result<Document> {
    let path_label = Utf8PathBuf::from(file);
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

/// Resolve the merge-patch source (inline / stdin / `@<path>` /
/// `--patch-from`) into a parsed [`Value`].
///
/// Source-resolution priority mirrors `set::resolve_value` and
/// `patch::resolve_ops`: `--patch-from` wins; then the inline arg's leading
/// `-` reads stdin, leading `@` reads a file path, and anything else is the
/// inline JSON literal.
fn resolve_patch(args: &MergeArgs) -> anyhow::Result<Value> {
    let bytes = if let Some(path) = &args.patch_from {
        std::fs::read(path.as_std_path()).map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: path.clone(),
                source,
            })
        })?
    } else if let Some(s) = &args.patch {
        if s == "-" {
            let mut buf = Vec::new();
            std::io::stdin().lock().read_to_end(&mut buf)?;
            buf
        } else if let Some(path_str) = s.strip_prefix('@') {
            let path = Utf8PathBuf::from(path_str);
            std::fs::read(path.as_std_path()).map_err(|source| {
                anyhow::Error::new(dq_core::Error::Io {
                    path: path.clone(),
                    source,
                })
            })?
        } else {
            s.as_bytes().to_vec()
        }
    } else {
        return Err(anyhow::Error::new(InvalidInput::new(
            "missing patch: provide an inline JSON merge patch, `-` for stdin, `@<path>`, or --patch-from",
        )));
    };

    let json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        anyhow::Error::new(InvalidInput::new(format!("invalid JSON merge patch: {e}")))
    })?;
    Ok(serde_json_to_dq_value(&json))
}

/// Convert a [`serde_json::Value`] into a [`Value`].
///
/// TODO(M3 §10): extract serde_json_to_dq_value to a shared module —
/// fourth copy site. (First copy: `crate::commands::set`. Second copy:
/// `dq-core::transform::patch` for serde plumbing. Third copy:
/// `crate::commands::patch`. This is the fourth.) Three copies was the
/// "we'll see" threshold; four is the trigger to extract a
/// `commands::value_convert` module in the §10 cleanup pass. The same
/// extraction batch also covers `f64_matches_literal`,
/// `literal_round_trips_to`, and `number_to_value`, which are duplicated
/// alongside this helper.
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
    use super::*;
    use clap::Parser;
    use tempfile::NamedTempFile;

    /// Build a `Cli` for tests. `extra` flags go before the subcommand;
    /// `merge` plus dummy positionals satisfy clap's required-argument
    /// validation.
    fn cli_for(extra: &[&str]) -> Cli {
        let mut argv = vec!["dq"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["merge", "ignored.yaml", "{}"]);
        Cli::try_parse_from(argv).expect("clap parse")
    }

    // Returns a `TempPath` (not a `NamedTempFile`) so the underlying `File`
    // handle is released after writing. Required for Windows: production
    // atomic-write uses `MoveFileEx` which fails with `Access is denied` if
    // the target is still held open elsewhere in the same process.
    fn write_yaml(content: &str) -> tempfile::TempPath {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.into_temp_path()
    }

    fn temp_path(tmp: &tempfile::TempPath) -> Utf8PathBuf {
        // `TempPath` derefs to `Path`, so `to_path_buf` resolves directly.
        Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap()
    }

    // ---- handler-level tests through `commands::merge::run` ----

    #[test]
    fn merge_recursive_map_merge_updates_nested_fields_and_preserves_others() {
        // RFC 7396 §1: object values merge recursively. Existing fields
        // outside the patch must survive untouched.
        let src = "spec:\n  replicas: 3\n  strategy:\n    type: Recreate\n  paused: false\n";
        let tmp = write_yaml(src);
        let path = temp_path(&tmp);
        let cli = cli_for(&["-i"]);
        let args = MergeArgs {
            file: path.clone(),
            patch: Some(
                r#"{"spec":{"replicas":5,"strategy":{"type":"RollingUpdate"}}}"#.to_owned(),
            ),
            patch_from: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("merge should succeed");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        // `replicas` is updated; `strategy.type` is updated; `paused` and the
        // strategy mapping body are preserved structurally.
        assert!(
            on_disk.contains("replicas: 5"),
            "replicas not updated: {on_disk}"
        );
        assert!(
            on_disk.contains("type: RollingUpdate"),
            "strategy.type not updated: {on_disk}"
        );
        assert!(
            on_disk.contains("paused: false"),
            "paused field must be preserved: {on_disk}"
        );
    }

    #[test]
    fn merge_null_removes_existing_key() {
        // RFC 7396 §1: null in the patch removes the addressed key. We only
        // exercise the *remove* arm here because adding a brand-new sibling
        // key would require mkdir-p, which the M2 textual-edit substrate
        // does not yet support — `Document::set_at` returns
        // `Path { kind: MissingKey }` for any pointer not already in
        // `SpanMap`. The spec's "null removes one key, sets a sibling" case
        // is covered end-to-end at the engine level in
        // `dq-core::transform::merge::tests::apply_merge_null_removes_existing_key`;
        // CLI coverage of the new-key-creation arm lands once the
        // textual-edit pipeline gains mkdir-p (M3 §later or M4).
        let src = "metadata:\n  annotations:\n    old: \"x\"\n    keep: \"y\"\n";
        let tmp = write_yaml(src);
        let path = temp_path(&tmp);
        let cli = cli_for(&["-i"]);
        let args = MergeArgs {
            file: path.clone(),
            patch: Some(r#"{"metadata":{"annotations":{"old":null}}}"#.to_owned()),
            patch_from: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("null-removing merge should succeed");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("old:"),
            "annotations.old should be removed: {on_disk}"
        );
        assert!(
            on_disk.contains("keep:"),
            "annotations.keep must survive removal of a sibling: {on_disk}"
        );
    }

    #[test]
    #[ignore = "array wholesale replacement requires container-level spans (deferred to post-M3)"]
    fn merge_array_in_patch_replaces_target_array_wholesale() {
        // RFC 7396 §1: arrays do not element-merge — the whole array is
        // replaced wholesale.
        //
        // BLOCKED: end-to-end CLI verification of "wholesale replacement
        // of a YAML/JSON sequence" requires a container-level span on
        // the array so `Document::set_at` can splice a new array body in
        // place of the old one. The M2/M3 YAML and JSON span recorders
        // only emit spans for scalar leaves
        // (see `crates/dq-core/src/parsers/yaml_spans.rs`'s
        // `Frame::Sequence` bookkeeping and `parsers/json.rs`'s
        // `record_scalar`). With no container span, `set_at` returns
        // `Path { kind: MissingKey }` and the merge aborts via the
        // engine's clone-on-apply rollback — the on-disk file is left
        // byte-identical, but the CLI surface is unable to fulfil the
        // RFC contract.
        //
        // The wholesale-replace contract IS exercised at the engine
        // level, where the test fixtures construct `Document`s with
        // explicit container spans:
        // `dq-core::transform::merge::tests::apply_merge_replaces_when_target_not_a_map`
        // covers the same code path. CLI-level coverage of this
        // scenario lands once the textual-edit pipeline records
        // container spans (a follow-up change in `data-query-write`).
        let src = "spec:\n  containers:\n    - name: old-a\n    - name: old-b\n  paused: false\n";
        let tmp = write_yaml(src);
        let path = temp_path(&tmp);
        let cli = cli_for(&["-i"]);
        let args = MergeArgs {
            file: path.clone(),
            patch: Some(r#"{"spec":{"containers":[{"name":"app"}]}}"#.to_owned()),
            patch_from: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("array-replacing merge should succeed");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("old-a"),
            "old container must be removed: {on_disk}"
        );
        assert!(
            !on_disk.contains("old-b"),
            "old container must be removed: {on_disk}"
        );
        assert!(
            on_disk.contains("name: app"),
            "new container must be present: {on_disk}"
        );
        assert!(
            on_disk.contains("paused: false"),
            "sibling field must survive: {on_disk}"
        );
    }

    #[test]
    fn merge_null_on_missing_key_is_silent_nop() {
        // RFC 7396 §1: a null value against a missing key is a silent NOP —
        // the merge succeeds and the document is structurally unchanged.
        let src = "a: 1\nb: 2\n";
        let tmp = write_yaml(src);
        let path = temp_path(&tmp);
        let cli = cli_for(&["-i"]);
        let args = MergeArgs {
            file: path.clone(),
            patch: Some(r#"{"absent":null}"#.to_owned()),
            patch_from: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out)
            .expect("null on missing key must be a silent NOP per RFC 7396 §1");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        // Both keys are still present and intact — the merge produced no
        // structural change.
        assert!(on_disk.contains("a: 1"), "key a must survive: {on_disk}");
        assert!(on_disk.contains("b: 2"), "key b must survive: {on_disk}");
    }

    #[test]
    fn merge_at_path_source_reads_patch_from_file() {
        // `@<path>` is the third source shape: write the patch JSON to a
        // tempfile and pass `@<path>` as the inline arg.
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = temp_path(&tmp);
        let mut patch_file = NamedTempFile::with_suffix(".json").unwrap();
        patch_file
            .write_all(br#"{"spec":{"replicas":11}}"#)
            .unwrap();
        let patch_path = patch_file.path().to_str().unwrap().to_owned();
        let at_arg = format!("@{patch_path}");
        let cli = cli_for(&["-i"]);
        let args = MergeArgs {
            file: path.clone(),
            patch: Some(at_arg),
            patch_from: None,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("merge from @path should succeed");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("replicas: 11"),
            "patch from @path was not applied: {on_disk}"
        );
    }

    #[test]
    fn merge_check_with_changes_returns_check_pending() {
        // `--check` mode: a patch that would modify the file → CheckPending,
        // file untouched on disk.
        let src = "spec:\n  replicas: 3\n";
        let tmp = write_yaml(src);
        let path = temp_path(&tmp);
        let cli = cli_for(&["--check"]);
        let args = MergeArgs {
            file: path.clone(),
            patch: Some(r#"{"spec":{"replicas":99}}"#.to_owned()),
            patch_from: None,
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("check mode with pending changes must error");
        let pending = err
            .downcast_ref::<crate::error::CheckPending>()
            .expect("expected CheckPending marker");
        assert_eq!(pending.count, 1);

        // File on disk must be untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), src);
    }

    #[test]
    fn merge_missing_patch_source_returns_invalid_input() {
        // No inline arg, no --patch-from, no stdin redirection set up —
        // resolver must surface InvalidInput so the exit-code mapper
        // produces 6.
        let tmp = write_yaml("a: 1\n");
        let path = temp_path(&tmp);
        let cli = cli_for(&[]);
        let args = MergeArgs {
            file: path,
            patch: None,
            patch_from: None,
        };
        let mut out: Vec<u8> = Vec::new();
        let err =
            run(&cli, &args, None, false, &mut out).expect_err("missing patch source must error");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker so exit-code mapper picks 6, got: {err:?}"
        );
    }

    // TODO(M3 §9 integration): cover the stdin path (`patch == "-"`) via
    // `assert_cmd` in `tests/cli_smoke.rs`. Unit tests can't redirect
    // `std::io::stdin()` cleanly, so the stdin source is exercised
    // end-to-end through the binary instead.
}
