//! `dq patch FILE [OPS]` — apply an RFC 6902 JSON Patch.
//!
//! Layered on [`dq_core::apply_patch`] (clone-on-apply atomicity) and
//! [`crate::bulk::run_per_file`] (glob expansion, `--check`,
//! `--continue-on-error`, `--parallel`, summary).
//!
//! Pipeline (per file, executed inside [`PatchFileOp::apply`]):
//!
//! 1. Resolve the format (extension or `-F`).
//! 2. Read the original bytes.
//! 3. Run the template guard (mirrors `commands::set`).
//! 4. Parse to a write-aware [`Document`].
//! 5. Apply the pre-resolved op list via [`dq_core::apply_patch`] — atomic
//!    rollback on any failure (including a failing `test`) is the engine's
//!    responsibility.
//! 6. Restore template placeholders if the substitution pass ran.
//! 7. Compute the unified-diff string when `--diff` mode is active so the
//!    bulk driver can dispatch it; never write to disk here — `--check`,
//!    `--diff`, and `-i` are all driver concerns.

use std::io::{Read, Write};
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use dq_core::{Document, PatchOp, Pointer, Value};
use indexmap::IndexMap;

use crate::bulk::{self, FileOp, FileOpResult};
use crate::cli::{Cli, PatchArgs};
use crate::commands::io_helpers::{pick_format, read_bytes};
use crate::error::InvalidInput;

/// Run `dq patch`.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) for inconsistent write flags, missing ops
///   source, or a malformed line-format line.
/// - [`dq_core::Error::TemplatedFile`] (exit 3) when a file contains Go
///   template syntax and neither `--allow-templates` nor
///   `--raw-template-strings` is set.
/// - [`dq_core::Error::PatchTestFailed`] when an RFC 6902 `test` op
///   observes a mismatched value. The on-disk file is untouched (atomic
///   rollback in [`dq_core::apply_patch`]).
/// - [`crate::error::CheckPending`] (exit 1) when `--check` finds at least
///   one file that would be modified.
/// - [`crate::error::BulkPartialFailure`] (exit 7) when
///   `--continue-on-error` finishes with one or more failed files.
pub fn run(
    cli: &Cli,
    args: &PatchArgs,
    input_format: Option<&str>,
    use_color: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_write_flags_consistent()?;

    // M4 §4 / design D5: `--sort-keys` and `--indent` are no-ops on the
    // textual-edit splice path. See `commands::set::run` for the rationale.
    if cli.sort_keys || cli.indent.is_some() {
        tracing::debug!(
            "--sort-keys / --indent are no-ops for textual-edit splice; use `dq fmt` to canonicalize after `patch`",
        );
    }

    // Resolve the patch ops once, before iterating files. Every matched
    // file in the bulk run sees the same op list.
    let ops = resolve_ops(args)?;

    let op = PatchFileOp {
        ops: &ops,
        input_format,
        cli,
        use_color,
    };

    let files = bulk::expand_glob(&args.file)?;
    bulk::run_per_file(files, &op, cli, out)
}

/// `FileOp` adapter that holds the resolved ops + CLI flags by reference so
/// rayon can spread `apply` across worker threads without cloning.
struct PatchFileOp<'a> {
    ops: &'a [PatchOp],
    input_format: Option<&'a str>,
    cli: &'a Cli,
    use_color: bool,
}

impl<'a> FileOp for PatchFileOp<'a> {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
        // Mirror `commands::set::run` minus the value-resolution step:
        // pick_format -> read_bytes -> template guard -> parse_to_document
        // -> apply_patch -> restore_placeholders -> optional diff.
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

        // Apply the patch atomically — on any failure (including a failing
        // `test`), the engine leaves `document` untouched (clone-on-apply).
        dq_core::apply_patch(&mut document, self.ops).map_err(anyhow::Error::new)?;

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
/// TODO(M3 §10): this is a near-verbatim copy of
/// [`crate::commands::set::parse_to_document`]. The merge / diff handlers in
/// §5 / §6 will produce a third copy; once three call sites exist, extract
/// to a shared helper module under `commands/io_helpers` (the abstraction
/// is not yet load-bearing — each handler still owns its own template-guard
/// dance, so a half-shared helper would obscure rather than simplify).
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

/// Resolve the patch source (inline / stdin / `@<path>` / `--ops-from`) into
/// a parsed [`Vec<PatchOp>`].
///
/// Source-resolution priority mirrors `set`'s `resolve_value`:
/// `--ops-from` wins; then the inline arg's leading `-` reads stdin, leading
/// `@` reads a file path, and anything else is the inline JSON literal.
fn resolve_ops(args: &PatchArgs) -> anyhow::Result<Vec<PatchOp>> {
    let bytes = if let Some(path) = &args.ops_from {
        std::fs::read(path.as_std_path()).map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: path.clone(),
                source,
            })
        })?
    } else if let Some(s) = &args.ops {
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
            "missing ops: provide an inline JSON Patch, `-` for stdin, `@<path>`, or --ops-from",
        )));
    };

    if args.line_format {
        parse_line_format(&bytes)
    } else {
        // Standard JSON array of ops.
        serde_json::from_slice::<Vec<PatchOp>>(&bytes)
            .map_err(|e| anyhow::Error::new(InvalidInput::new(format!("invalid JSON Patch: {e}"))))
    }
}

/// Parse the simplified line format `<op> <pointer> [json-value]`.
///
/// Empty lines and lines whose first non-whitespace character is `#` are
/// skipped as comments.
///
/// Format per op:
/// - `add /path <json-value>`
/// - `remove /path`
/// - `replace /path <json-value>`
/// - `move /dst /src` — note: the second pointer is the `from` source.
/// - `copy /dst /src`
/// - `test /path <json-value>`
fn parse_line_format(bytes: &[u8]) -> anyhow::Result<Vec<PatchOp>> {
    let text = std::str::from_utf8(bytes).map_err(|e| {
        anyhow::Error::new(InvalidInput::new(format!(
            "line-format input must be UTF-8: {e}"
        )))
    })?;

    let mut ops = Vec::new();
    for (line_idx, raw_line) in text.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Tokenise: op, pointer, rest. `splitn(3, char::is_whitespace)`
        // would collapse runs of whitespace as a single delimiter on the
        // first call, but each subsequent token may itself begin with
        // leading whitespace inside `splitn`'s "rest" — so we hand-roll a
        // 3-token split that consumes runs.
        let mut iter = trimmed.split_whitespace();
        let op = iter.next().ok_or_else(|| {
            anyhow::Error::new(InvalidInput::new(format!(
                "line {line_no}: missing op token"
            )))
        })?;
        let pointer_str = iter.next().ok_or_else(|| {
            anyhow::Error::new(InvalidInput::new(format!(
                "line {line_no}: missing pointer for op '{op}'"
            )))
        })?;

        // For ops that take a third field, we need the unsplit remainder
        // so JSON values containing whitespace (e.g. `{"a": 1}`) survive.
        // Compute the byte offset of the pointer inside the trimmed line,
        // then take everything after it and trim again.
        let pointer_end = trimmed
            .find(pointer_str)
            .map(|i| i + pointer_str.len())
            .unwrap_or(trimmed.len());
        let rest = trimmed[pointer_end..].trim();

        let pointer = Pointer::parse(pointer_str).map_err(anyhow::Error::new)?;

        let parsed_op = match op {
            "add" => PatchOp::Add {
                path: pointer,
                value: parse_json_value(rest, line_no)?,
            },
            "remove" => {
                if !rest.is_empty() {
                    return Err(anyhow::Error::new(InvalidInput::new(format!(
                        "line {line_no}: 'remove' op takes no value, got: {rest:?}"
                    ))));
                }
                PatchOp::Remove { path: pointer }
            }
            "replace" => PatchOp::Replace {
                path: pointer,
                value: parse_json_value(rest, line_no)?,
            },
            "move" => {
                if rest.is_empty() {
                    return Err(anyhow::Error::new(InvalidInput::new(format!(
                        "line {line_no}: 'move' op requires a source pointer"
                    ))));
                }
                let from = Pointer::parse(rest).map_err(anyhow::Error::new)?;
                PatchOp::Move {
                    from,
                    path: pointer,
                }
            }
            "copy" => {
                if rest.is_empty() {
                    return Err(anyhow::Error::new(InvalidInput::new(format!(
                        "line {line_no}: 'copy' op requires a source pointer"
                    ))));
                }
                let from = Pointer::parse(rest).map_err(anyhow::Error::new)?;
                PatchOp::Copy {
                    from,
                    path: pointer,
                }
            }
            "test" => PatchOp::Test {
                path: pointer,
                value: parse_json_value(rest, line_no)?,
            },
            other => {
                return Err(anyhow::Error::new(InvalidInput::new(format!(
                    "line {line_no}: unknown op '{other}' (expected add/remove/replace/move/copy/test)"
                ))));
            }
        };
        ops.push(parsed_op);
    }
    Ok(ops)
}

/// Parse the value field of an `add` / `replace` / `test` line as a JSON
/// literal and convert it to a [`Value`].
fn parse_json_value(rest: &str, line_no: usize) -> anyhow::Result<Value> {
    if rest.is_empty() {
        return Err(anyhow::Error::new(InvalidInput::new(format!(
            "line {line_no}: missing value (expected a JSON literal)"
        ))));
    }
    let json: serde_json::Value = serde_json::from_str(rest).map_err(|e| {
        anyhow::Error::new(InvalidInput::new(format!(
            "line {line_no}: invalid JSON value: {e}"
        )))
    })?;
    Ok(serde_json_to_dq_value(&json))
}

/// Convert a [`serde_json::Value`] into a [`Value`].
///
/// TODO(M3 §10): this is a third copy of
/// [`crate::commands::set::serde_json_to_dq_value`] (the second copy lives
/// in `dq-core::transform::patch` for serde plumbing). Extract to a shared
/// helper once §5 (merge) lands its own copy — three call sites makes the
/// case for a `commands::value_convert` module. The same extraction batch
/// also covers `f64_matches_literal`, `literal_round_trips_to`, and
/// `number_to_value`, which are duplicated alongside this helper.
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
    /// `patch` plus dummy positionals satisfy clap's required-argument
    /// validation.
    fn cli_for(extra: &[&str]) -> Cli {
        let mut argv = vec!["dq"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["patch", "ignored.yaml", "[]"]);
        Cli::try_parse_from(argv).expect("clap parse")
    }

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    fn temp_path(tmp: &NamedTempFile) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap()
    }

    // ---- handler-level tests through `commands::patch::run` ----

    #[test]
    fn patch_inline_json_ops_in_place_updates_file() {
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = temp_path(&tmp);
        let cli = Cli::try_parse_from([
            "dq",
            "-i",
            "patch",
            path.as_str(),
            r#"[{"op":"replace","path":"/spec/replicas","value":5}]"#,
        ])
        .unwrap();
        let args = PatchArgs {
            file: path.clone(),
            ops: Some(r#"[{"op":"replace","path":"/spec/replicas","value":5}]"#.to_owned()),
            ops_from: None,
            line_format: false,
            no_create: false,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("patch should succeed");
        assert!(out.is_empty(), "in-place mode must not write to stdout");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "spec:\n  replicas: 5\n");
    }

    #[test]
    fn patch_at_path_source_reads_ops_from_file() {
        // `@<path>` is the second source shape: write ops to a tempfile,
        // pass `@<path>` as the inline arg.
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = temp_path(&tmp);
        let mut ops_file = NamedTempFile::with_suffix(".json").unwrap();
        ops_file
            .write_all(br#"[{"op":"replace","path":"/spec/replicas","value":7}]"#)
            .unwrap();
        let ops_path = ops_file.path().to_str().unwrap().to_owned();
        let at_arg = format!("@{ops_path}");
        let cli = cli_for(&["-i"]);
        let args = PatchArgs {
            file: path.clone(),
            ops: Some(at_arg),
            ops_from: None,
            line_format: false,
            no_create: false,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("patch from @path should succeed");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "spec:\n  replicas: 7\n");
    }

    #[test]
    fn patch_line_format_via_ops_from_applies_each_line() {
        // Line-format file with one `replace` op. `--ops-from` is the
        // canonical way to pass a multi-line file (the inline arg makes
        // `\n` round-tripping through the shell awkward).
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = temp_path(&tmp);
        let mut ops_file = NamedTempFile::with_suffix(".txt").unwrap();
        ops_file
            .write_all(b"# comment skipped\nreplace /spec/replicas 9\n")
            .unwrap();
        let ops_path = Utf8PathBuf::from_path_buf(ops_file.path().to_path_buf()).unwrap();
        let cli = cli_for(&["-i"]);
        let args = PatchArgs {
            file: path.clone(),
            ops: None,
            ops_from: Some(ops_path),
            line_format: true,
            no_create: false,
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("line-format patch should succeed");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "spec:\n  replicas: 9\n");
    }

    #[test]
    fn patch_test_op_failure_leaves_file_unchanged() {
        // RFC 6902 §5: a failing `test` op aborts the whole patch
        // atomically — the on-disk file must be byte-identical to its
        // pre-call state.
        let src = "spec:\n  replicas: 3\n";
        let tmp = write_yaml(src);
        let path = temp_path(&tmp);
        let original_on_disk = std::fs::read(&path).unwrap();
        let cli = cli_for(&["-i"]);
        let args = PatchArgs {
            file: path.clone(),
            ops: Some(
                // First op would replace; second op is a wrong-value test.
                r#"[
                    {"op":"replace","path":"/spec/replicas","value":5},
                    {"op":"test","path":"/spec/replicas","value":99}
                ]"#
                .to_owned(),
            ),
            ops_from: None,
            line_format: false,
            no_create: false,
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("failing test op must produce an error");
        let domain = err
            .downcast_ref::<dq_core::Error>()
            .expect("error should downcast to dq_core::Error");
        assert_eq!(domain.kind_name(), "patch_test_failed");

        // File on disk MUST be byte-identical.
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original_on_disk,
            "atomicity: failed test must not modify the on-disk file"
        );
    }

    #[test]
    fn patch_check_with_changes_returns_check_pending() {
        // `--check` mode: ops would modify the file → CheckPending error,
        // file untouched on disk.
        let src = "spec:\n  replicas: 3\n";
        let tmp = write_yaml(src);
        let path = temp_path(&tmp);
        let cli = cli_for(&["--check"]);
        let args = PatchArgs {
            file: path.clone(),
            ops: Some(r#"[{"op":"replace","path":"/spec/replicas","value":5}]"#.to_owned()),
            ops_from: None,
            line_format: false,
            no_create: false,
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("check mode with pending changes must error");
        let pending = err
            .downcast_ref::<crate::error::CheckPending>()
            .expect("expected CheckPending marker");
        assert_eq!(pending.count, 1);

        // File on disk untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), src);
    }

    #[test]
    fn patch_missing_ops_source_returns_invalid_input() {
        // No inline arg, no --ops-from, no stdin redirection set up —
        // resolver must surface InvalidInput so the exit-code mapper
        // produces 6.
        let tmp = write_yaml("a: 1\n");
        let path = temp_path(&tmp);
        let cli = cli_for(&[]);
        let args = PatchArgs {
            file: path,
            ops: None,
            ops_from: None,
            line_format: false,
            no_create: false,
        };
        let mut out: Vec<u8> = Vec::new();
        let err =
            run(&cli, &args, None, false, &mut out).expect_err("missing ops source must error");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker so exit-code mapper picks 6, got: {err:?}"
        );
    }

    // TODO(M3 §9 integration): cover the stdin path (`ops == "-"`) via
    // `assert_cmd` in `tests/cli_smoke.rs`. Unit tests can't redirect
    // `std::io::stdin()` cleanly, so the stdin source is exercised
    // end-to-end through the binary instead.

    // ---- parse_line_format unit tests ----

    #[test]
    fn parse_line_format_handles_each_op_variant() {
        // One line per op kind; assert variant tags survive the parser.
        let input = b"add /a 1\n\
                      remove /b\n\
                      replace /c \"hello\"\n\
                      move /dst /src\n\
                      copy /dst2 /src2\n\
                      test /d 42\n";
        let ops = parse_line_format(input).expect("all six ops must parse");
        assert_eq!(ops.len(), 6);
        assert!(matches!(ops[0], PatchOp::Add { .. }));
        assert!(matches!(ops[1], PatchOp::Remove { .. }));
        assert!(matches!(ops[2], PatchOp::Replace { .. }));
        assert!(matches!(ops[3], PatchOp::Move { .. }));
        assert!(matches!(ops[4], PatchOp::Copy { .. }));
        assert!(matches!(ops[5], PatchOp::Test { .. }));
    }

    #[test]
    fn parse_line_format_skips_empty_and_comment_lines() {
        // Blank lines and `#`-prefixed lines must be silently skipped so
        // hand-written ops files can carry inline notes.
        let input = b"\n\
                      # leading comment\n\
                      replace /a 1\n\
                      \n\
                      # trailing comment\n";
        let ops = parse_line_format(input).expect("comments and blanks ok");
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], PatchOp::Replace { .. }));
    }

    #[test]
    fn parse_line_format_unknown_op_errors_with_invalid_input() {
        let input = b"frobnicate /a 1\n";
        let err = parse_line_format(input).expect_err("unknown op must error");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker"
        );
        assert!(err.to_string().contains("unknown op"));
    }

    #[test]
    fn parse_line_format_missing_pointer_errors() {
        let input = b"replace\n";
        let err = parse_line_format(input).expect_err("missing pointer must error");
        assert!(err.to_string().contains("missing pointer"));
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn parse_line_format_remove_with_extra_value_errors() {
        // `remove` takes no value field — passing one indicates a typo
        // that's worth surfacing rather than silently ignoring.
        let input = b"remove /a 1\n";
        let err = parse_line_format(input).expect_err("remove with value must error");
        assert!(err.to_string().contains("'remove' op takes no value"));
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn parse_line_format_supports_object_value() {
        // Multi-character JSON values (objects, arrays) must round-trip
        // through the parser even though they contain whitespace.
        let input = br#"add /spec/containers/- {"name":"sidecar"}"#;
        let ops = parse_line_format(input).expect("object value must parse");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOp::Add { value, .. } => match value {
                Value::Map(m) => {
                    assert_eq!(m.get("name"), Some(&Value::String("sidecar".to_owned())))
                }
                other => panic!("expected map value, got: {other:?}"),
            },
            other => panic!("expected Add op, got: {other:?}"),
        }
    }
}
