//! `dq convert FILE` — re-emit the document in the format selected by `-F`.
//!
//! Two modes:
//!
//! - **Read-only (default, M2 baseline)**: the rendered output is written to
//!   the supplied `out` writer (typically stdout). Same byte-identical
//!   contract as the M2 implementation.
//! - **In-place (M3 §8, `-i`)**: per design D9, the rendered bytes are
//!   atomically written to a sibling file with the target extension and the
//!   source file is removed (unless `--keep-source` is set). Combined with
//!   glob expansion, `dq convert 'manifests/*.yaml' -i -F json` rewrites
//!   every match to its `.json` counterpart.
//!
//! Routing rules (shared by both modes):
//! - For `yaml` / `toml` / `jsonl` / `json` output, we call
//!   [`dq_core::Format::write`] directly on the parsed [`Document`]. Going
//!   through the reporter pipeline would force a `Document` →
//!   `serde_json::Value` round-trip, losing big-int / big-float precision and
//!   reordering keys for JSONL.
//! - For `console` and `toon` output, we render via the supplied reporter
//!   (those formats don't have a corresponding `dq-core` writer). Console /
//!   Toon are rejected up-front in `-i` mode because they have no canonical
//!   file extension.
//!
//! ## Why convert has its own write loop instead of `bulk::run_per_file`
//!
//! The shared bulk driver assumes the per-file `output_bytes` are written
//! back to the same path the op was invoked with (the M2 in-place contract
//! for `set`/`del`). `convert -i` writes to a DIFFERENT path — the source's
//! sibling with a swapped extension — and additionally removes the source
//! file on success. Generalising the bulk driver to accept a
//! `target_path_for(source) -> Utf8PathBuf` hook is reserved for a future
//! refactor (M4+); for the M3 baseline we run a simple sequential loop here
//! that reuses [`crate::bulk::expand_glob`] for file expansion and
//! [`dq_core::atomic_write::write`] for atomicity. `--parallel` for
//! `convert` is a future enhancement.
//!
//! ## Lossy-conversion warnings (M2 placeholder)
//!
//! When converting between formats, source-only metadata such as YAML
//! comments, anchor names, and quote-style hints are dropped. M1's `Document`
//! model does not yet carry those fields — they arrive in M2 with the
//! event-API parser rewrite. For now, when the input format is YAML and the
//! output is something that cannot represent comments (json/toml), we emit a
//! `tracing::warn!` line as a forward-looking placeholder so the eventual
//! M2 work has a hook to extend.

use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use dq_core::{Document, FormatTag, FrontmatterKind, WriteOptions};

use super::io_helpers::load_document_with_path;
use crate::bulk;
use crate::cli::{Cli, ConvertArgs};
use crate::error::InvalidInput;
use crate::output::{ConsoleReporter, OutputFormat, Reporter, ToonReporter};

/// Run the `convert` command.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when:
///   - any non-`-i` write-mode flag is set in read mode (`--diff`,
///     `--backup`, `--check`, ...);
///   - `-i` is combined with `--diff` / `--check` / `--continue-on-error`
///     / `--parallel` (none of which `convert -i` supports);
///   - `--backup` is set without `-i`;
///   - `-i` is set with `OutputFormat::Console` (no canonical extension);
///   - the swapped target extension equals the source extension (no-op).
/// - `--check` for `convert -i` is rejected up front (exit 6) — the
///   "would the target be created or differ?" semantics are deferred to
///   M4+.
/// - `--continue-on-error` and `--parallel` are rejected up front for
///   `convert -i` — bulk parallelism for cross-target writes is not part
///   of M3 §8.
/// - The usual `dq_core::Error` family on I/O, parse, and write failures.
///
/// `use_color` controls whether the inner `ConsoleReporter` (used for the
/// console output branch only) emits ANSI escapes. The caller is responsible
/// for resolving the global `--no-color` flag through
/// [`crate::output::resolve_color`] before passing it in.
pub fn run(
    cli: &Cli,
    args: &ConvertArgs,
    input_format: Option<&str>,
    output_format: OutputFormat,
    use_color: bool,
    opts: &WriteOptions,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    if cli.in_place {
        run_in_place(cli, args, output_format, use_color, opts, out)
    } else {
        // Read-only mode: existing M2 contract. We do NOT call the generic
        // `Cli::ensure_no_write_flags` because `--keep-source` is not a
        // global flag and `-i` is the only write flag we *would* accept;
        // `ensure_no_write_flags` rejects every flag the read mode forbids
        // (everything except `-i`, which we already filtered above).
        cli.ensure_no_write_flags()?;
        run_to_stdout(args, input_format, output_format, use_color, opts, out)
    }
}

/// Existing M2 read-only path: parse, render, write to `out`. Bit-identical
/// to the pre-M3 implementation when `opts == &WriteOptions::default()` —
/// must keep all golden tests passing. When `--sort-keys` / `--indent` are
/// set, the rendering path picks them up via `Format::write_with_options`.
fn run_to_stdout(
    args: &ConvertArgs,
    input_format: Option<&str>,
    output_format: OutputFormat,
    use_color: bool,
    opts: &WriteOptions,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let (input_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    maybe_warn_lossy(input_fmt.name(), output_format);
    let bytes = render_to_format(&doc, output_format, use_color, opts)?;
    out.write_all(&bytes)?;
    Ok(())
}

/// M3 §8 in-place mode: read source, render to target format, atomic-write
/// to a sibling file with the swapped extension, remove source on success.
///
/// Validates the `-i`-specific flag combination directly (the global
/// [`Cli::ensure_write_flags_consistent`] would reject `-i + -F`, which is
/// exactly what `convert -i` requires; the cli-shell spec calls out
/// `convert` as the one command that accepts that combination). We
/// hand-check the subset of write-flag rules that DO apply: `-i` ⊥ `--diff`,
/// `-i` ⊥ `--check` (TODO M4+: `--check` for convert means "would this
/// target be created/changed?"), and `--backup` requires `-i`.
fn run_in_place(
    cli: &Cli,
    args: &ConvertArgs,
    output_format: OutputFormat,
    use_color: bool,
    opts: &WriteOptions,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    // -- 1. Validate the convert-specific write-flag subset --------------
    //
    // Do NOT call `cli.ensure_write_flags_consistent()`: that helper
    // rejects `-i + -F`, which is the entire point of `convert -i`. The
    // cli-shell spec (`crates/dq-cli/specs/cli-shell/spec.md` in the
    // `add-bulk-and-ci` change) explicitly notes "convert is special: it
    // accepts `-i` (when paired with `-F`) but rejects the other write
    // modes".
    if cli.diff {
        return Err(anyhow::Error::new(InvalidInput::new(
            "-i/--in-place and --diff are mutually exclusive (convert -i has no diff mode)",
        )));
    }
    if cli.check {
        // M3 §8 baseline: `--check` for convert is rejected. The cleanest
        // semantics ("would the target be created or differ from existing
        // bytes?") would require comparing against a target file that may
        // not exist; we punt that to M4+. Reject explicitly so users notice.
        return Err(anyhow::Error::new(InvalidInput::new(
            "-i/--in-place and --check are mutually exclusive for `convert`",
        )));
    }
    if cli.continue_on_error {
        return Err(anyhow::Error::new(InvalidInput::new(
            "--continue-on-error is not supported for `convert -i` in this build",
        )));
    }
    if cli.parallel.is_some() {
        return Err(anyhow::Error::new(InvalidInput::new(
            "--parallel is not supported for `convert -i` in this build",
        )));
    }
    // `--backup` is meaningful only when paired with `-i`; we are in `-i`
    // mode, so no validation needed beyond the global rule. Note that
    // backup semantics for convert -i are: if the TARGET path already
    // exists, copy it to `<target>.bak` before overwriting. If the target
    // does not exist yet (the typical case — extension swap produces a new
    // path), `--backup` is a no-op. This is delegated to
    // `dq_core::atomic_write::write`, which only creates the backup when
    // the target file already exists.

    // -- 2. Console / Toon are not valid in-place targets ----------------
    //
    // Both render through reporters (no canonical extension). `-i` requires
    // a target file path; every other variant of `OutputFormat` has a
    // canonical extension via [`canonical_extension`].
    if matches!(output_format, OutputFormat::Console | OutputFormat::Toon) {
        return Err(anyhow::Error::new(InvalidInput::new(
            "-i/--in-place requires a format with a canonical file extension; console/toon have none",
        )));
    }

    // -- 3. Glob-expand the input pattern (single file fast path is built
    //       into `expand_glob`).
    let files = bulk::expand_glob(&args.file)?;

    // -- 4. Loop ---------------------------------------------------------
    //
    // Per-file: read → parse → render → atomic-write to target → remove
    // source. We do not call `bulk::run_per_file` because that helper
    // assumes target == source; convert -i breaks that invariant. See the
    // module doc for the rationale.
    let mut modified: usize = 0;
    let is_bulk = files.len() > 1;

    for source_path in &files {
        let target_path = compute_target_path(source_path, output_format)?;
        if target_path == *source_path {
            // Same source/target — covered both by the explicit extension
            // check below and by this defensive check, since some
            // pathological inputs (no extension, etc.) could otherwise
            // slip through.
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "convert -i to {}: target path '{source_path}' is identical to source — drop -F or pick a different format",
                output_format_label(output_format),
            ))));
        }

        // Parse + render.
        let (input_fmt, doc) = load_document_with_path(source_path, None)?;
        maybe_warn_lossy(input_fmt.name(), output_format);
        let output_bytes = render_to_format(&doc, output_format, use_color, opts)?;

        // Atomic write to target. `--backup` is honoured by atomic_write
        // when the target already exists.
        dq_core::atomic_write::write(&target_path, &output_bytes, cli.backup)
            .map_err(anyhow::Error::new)?;

        // Source removal — only if --keep-source is unset and the target
        // is genuinely a different path. The second guard is belt-and-
        // braces; `target_path == *source_path` was already rejected
        // above.
        if !args.keep_source && target_path != *source_path {
            std::fs::remove_file(source_path.as_std_path()).map_err(|source| {
                anyhow::Error::new(dq_core::Error::WriteIo {
                    path: source_path.clone(),
                    source,
                })
            })?;
        }

        modified += 1;
    }

    // Bulk runs print a one-line summary so users can confirm the count.
    // Single-file runs stay silent so scripts can pipe convert -i without
    // surprise output. (If a user hits zero matches, `expand_glob` already
    // returned an error.)
    if is_bulk {
        writeln!(out, "Modified: {modified}, Skipped: 0, Failed: 0")?;
    }

    Ok(())
}

/// Compute the target path for `convert -i` by swapping the file's
/// extension to the canonical extension of `output_format`. Console and Toon
/// have no canonical extension and are rejected up-front by the caller.
fn compute_target_path(
    source: &Utf8Path,
    output_format: OutputFormat,
) -> anyhow::Result<Utf8PathBuf> {
    let target_ext = canonical_extension(output_format).ok_or_else(|| {
        anyhow::Error::new(InvalidInput::new(
            "convert -i requires a format with a canonical file extension; console/toon have none",
        ))
    })?;
    Ok(source.with_extension(target_ext))
}

/// Canonical extension for an output format, or `None` for formats without
/// a single canonical extension (Console / Toon / Sarif).
fn canonical_extension(output_format: OutputFormat) -> Option<&'static str> {
    match output_format {
        OutputFormat::Json => Some("json"),
        OutputFormat::Yaml => Some("yaml"),
        OutputFormat::Toml => Some("toml"),
        OutputFormat::Jsonl => Some("jsonl"),
        OutputFormat::Hcl => Some("hcl"),
        OutputFormat::Ini => Some("ini"),
        OutputFormat::DotEnv => Some("env"),
        OutputFormat::Csv => Some("csv"),
        OutputFormat::Tsv => Some("tsv"),
        OutputFormat::Frontmatter => Some("md"),
        OutputFormat::Markdown => Some("md"),
        OutputFormat::Xml => Some("xml"),
        OutputFormat::Toon => None,
        OutputFormat::Console => None,
        // M6 / M8: SARIF / JUnit / TAP are diagnostic-only output formats and
        // are not valid `convert` targets. The convert handler rejects them
        // before calling this function — the `None` here is a safety net.
        OutputFormat::Sarif | OutputFormat::Junit | OutputFormat::Tap => None,
    }
}

/// Human-friendly label for an output format, used in error messages.
fn output_format_label(output_format: OutputFormat) -> &'static str {
    match output_format {
        OutputFormat::Json => "json",
        OutputFormat::Yaml => "yaml",
        OutputFormat::Toml => "toml",
        OutputFormat::Jsonl => "jsonl",
        OutputFormat::Hcl => "hcl",
        OutputFormat::Ini => "ini",
        OutputFormat::DotEnv => "dotenv",
        OutputFormat::Csv => "csv",
        OutputFormat::Tsv => "tsv",
        OutputFormat::Frontmatter => "frontmatter",
        OutputFormat::Markdown => "markdown",
        OutputFormat::Xml => "xml",
        OutputFormat::Toon => "toon",
        OutputFormat::Console => "console",
        OutputFormat::Sarif => "sarif",
        OutputFormat::Junit => "junit",
        OutputFormat::Tap => "tap",
    }
}

/// Project `doc` for cross-format rendering when the source carries
/// non-empty `original_bytes` but the target format would interpret those
/// bytes verbatim.
///
/// M9 surfaced this: the `Markdown` parser stores the markdown source in
/// `original_bytes` so its own writer can do the verbatim round-trip
/// described in the M9 spec. When `dq convert post.md -F json` calls the
/// JSON writer on that document, the JSON writer's "non-empty original
/// bytes ⇒ emit verbatim" shortcut fires and the user gets markdown text
/// in a `.json` output. Stripping `original_bytes` for the cross-format
/// case forces every writer onto its value-tree path, which is what the
/// caller actually wants from a `convert -F <other>` invocation.
///
/// Same-format renders (`Markdown::write` for a markdown-source doc) keep
/// the original bytes — that's the verbatim contract.
fn project_for_cross_format_render(doc: &Document, output_format: OutputFormat) -> Document {
    let target_tag = match output_format {
        OutputFormat::Json => Some(FormatTag::Json),
        OutputFormat::Yaml => Some(FormatTag::Yaml),
        OutputFormat::Toml => Some(FormatTag::Toml),
        OutputFormat::Jsonl => Some(FormatTag::Jsonl),
        OutputFormat::Hcl => Some(FormatTag::Hcl),
        OutputFormat::Ini => Some(FormatTag::Ini),
        OutputFormat::DotEnv => Some(FormatTag::DotEnv),
        OutputFormat::Csv => Some(FormatTag::Csv),
        OutputFormat::Tsv => Some(FormatTag::Tsv),
        OutputFormat::Xml => Some(FormatTag::Xml),
        // Console / Toon / Sarif / Junit / Tap / Markdown / Frontmatter
        // either render via reporters (no doc baseline) or have their own
        // explicit cross-format guard upstream.
        _ => None,
    };
    let Some(target) = target_tag else {
        return doc.clone();
    };
    if doc.format() == target {
        return doc.clone();
    }
    if doc.original_bytes().is_empty() {
        return doc.clone();
    }
    if let Some(values) = doc.values() {
        Document::multi_value_only(values.to_vec(), target)
    } else {
        Document::value_only(doc.value().clone(), target)
    }
}

/// Render `doc` to a byte buffer in the requested output format.
///
/// Mirrors the `match output_format` block from the original M2 `run` so
/// both the read-only path and the in-place path share a single rendering
/// implementation.
///
/// # Errors
///
/// Bubbles up any `dq_core::Error` from the underlying writers (TOML
/// rejecting top-level scalars, JSONL needing an array root, ...). Reporter
/// errors are propagated through `anyhow`.
fn render_to_format(
    doc: &Document,
    output_format: OutputFormat,
    use_color: bool,
    opts: &WriteOptions,
) -> anyhow::Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    // Project the source document onto the target format when the two
    // differ. This avoids cross-format writers re-emitting source bytes
    // verbatim through their `original_bytes` shortcut — most prominent for
    // the M9 markdown source path. See [`project_for_cross_format_render`]
    // for the full rationale.
    let projected_owned = project_for_cross_format_render(doc, output_format);
    let doc: &Document = &projected_owned;
    match output_format {
        OutputFormat::Json => {
            // Use the dq-core JSON writer directly — preserves big-numeric
            // precision and key order. M4: route through `write_with_options`
            // so `--sort-keys` / `--indent` are honoured by the JSON writer.
            let json = dq_core::by_name("json").expect("json format always registered");
            json.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
            // The dq-core JSON writer does not append a trailing newline;
            // shell-friendly output gets one.
            writeln!(buf)?;
        }
        OutputFormat::Yaml => {
            let yaml = dq_core::by_name("yaml").expect("yaml format always registered");
            yaml.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Toml => {
            let toml = dq_core::by_name("toml").expect("toml format always registered");
            toml.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Jsonl => {
            let jsonl = dq_core::by_name("jsonl").expect("jsonl format always registered");
            jsonl
                .write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Hcl => {
            let f = dq_core::by_name("hcl").expect("hcl format registered in M5");
            f.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Ini => {
            let f = dq_core::by_name("ini").expect("ini format registered in M5");
            f.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::DotEnv => {
            let f = dq_core::by_name("dotenv").expect("dotenv format registered in M5");
            f.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Csv => {
            let f = dq_core::by_name("csv").expect("csv format registered in M5");
            f.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Tsv => {
            let f = dq_core::by_name("tsv").expect("tsv format registered in M5");
            f.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Xml => {
            // M11: XML write target. The XML writer demands a top-level
            // map containing exactly one element-shaped key — when the
            // source is JSON / YAML / TOML carrying an arbitrary tree,
            // the value must already be in that shape (i.e. the caller
            // has prepared a `{"root": [{"#text": "..."}]}` envelope).
            // Documents that don't fit this contract surface a
            // structured `Error::Format` from the writer, which the
            // exit-code mapper routes to WRITE_FAILED.
            let f = dq_core::by_name("xml").expect("xml format registered in M11");
            f.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Markdown => {
            // M9: markdown convert target is only meaningful when the source
            // is also markdown. The `Markdown` writer's `original_bytes`
            // round-trip path requires a `Document` whose format tag is
            // `Markdown` and whose source bytes were produced by the markdown
            // parser; cross-format conversion (e.g. `-F json -F markdown`)
            // would have no source-text baseline.
            if doc.format() != FormatTag::Markdown {
                return Err(anyhow::Error::new(InvalidInput::new(
                    "convert to markdown is only supported when input is also markdown — \
                     pass `-F markdown` on the input or convert from a markdown source file",
                )));
            }
            let f = dq_core::by_name("markdown").expect("markdown format registered in M9");
            f.write_with_options(doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Frontmatter => {
            // Frontmatter writer requires `Document::frontmatter_payload()`
            // to be `Some(...)`. When the source is already a frontmatter
            // document we hand it through unchanged; otherwise we wrap it in
            // a synthetic Frontmatter doc with an empty body so generic
            // documents can still be exported as a markdown file with YAML
            // frontmatter and no content. This is design Option A from the
            // M5 Stage 3 spec — chosen over erroring out so
            // `dq convert config.yaml -F frontmatter` round-trips through
            // `dq get out.md /...`.
            let f = dq_core::by_name("frontmatter").expect("frontmatter format registered in M5");
            let owned;
            let target_doc: &Document = if doc.format() == FormatTag::Frontmatter {
                doc
            } else {
                owned = synthesize_frontmatter_doc(doc);
                &owned
            };
            f.write_with_options(target_doc, &mut buf, opts)
                .map_err(anyhow::Error::new)?;
        }
        OutputFormat::Toon => {
            // toon-format only accepts `serde::Serialize` types; route via
            // serde_json::Value as the lowest-common-denominator
            // representation.
            let v = if let Some(values) = doc.values() {
                serde_json::Value::Array(values.iter().map(dq_core::Value::to_serde_json).collect())
            } else {
                doc.value().to_serde_json()
            };
            let reporter = ToonReporter;
            reporter.report(&v, &mut buf)?;
        }
        OutputFormat::Console => {
            // Console is the default; for `convert` it produces a "what
            // does this look like" view rather than a parseable output. We
            // still route through ConsoleReporter so the rendering matches
            // every other command's console mode.
            let v = if let Some(values) = doc.values() {
                serde_json::Value::Array(values.iter().map(dq_core::Value::to_serde_json).collect())
            } else {
                doc.value().to_serde_json()
            };
            let reporter = ConsoleReporter::new(use_color);
            reporter.report(&v, &mut buf)?;
        }
        OutputFormat::Sarif => {
            // M6 §5: SARIF is a diagnostic-only reporter (used by `validate`
            // and the future M8 lint engine). `convert` produces source-shape
            // documents, not diagnostics, so `-F sarif` makes no sense as a
            // convert target. Reject explicitly with InvalidInput so the
            // exit-code mapper produces 6.
            return Err(anyhow::Error::new(InvalidInput::new(
                "-F sarif is a diagnostic-only output format; not a valid `convert` target \
                 (use `dq validate -F sarif <file>` instead)",
            )));
        }
        OutputFormat::Junit => {
            // M8 §5: JUnit XML is a diagnostic-only reporter consumed by
            // GitLab CI / Jenkins. Same rejection discipline as SARIF.
            return Err(anyhow::Error::new(InvalidInput::new(
                "-F junit is a diagnostic-only output format; not a valid `convert` target \
                 (use the M8 `dq lint -F junit <files>` instead)",
            )));
        }
        OutputFormat::Tap => {
            // M8 §5: TAP 13 is a diagnostic-only reporter consumed by
            // `prove` / GitLab CI. Same rejection discipline as SARIF.
            return Err(anyhow::Error::new(InvalidInput::new(
                "-F tap is a diagnostic-only output format; not a valid `convert` target \
                 (use the M8 `dq lint -F tap <files>` instead)",
            )));
        }
    }
    Ok(buf)
}

/// Build a fresh Frontmatter [`Document`] that wraps `input_doc`'s value as
/// the YAML header with an empty body. Used when `convert -F frontmatter` is
/// asked to emit a non-frontmatter source document — without this wrapping,
/// the frontmatter writer would error out because `frontmatter_payload()` is
/// `None` for a JSON / YAML / TOML document.
///
/// Always synthesises a YAML header (`---\n…\n---\n`) regardless of the
/// source format. The body is empty (`Vec::new()`) so the output is exactly
/// `---\n<yaml>\n---\n` — round-trippable by `dq get` because the YAML
/// header carries the full value.
fn synthesize_frontmatter_doc(input_doc: &Document) -> Document {
    Document::frontmatter(input_doc.value().clone(), Vec::new(), FrontmatterKind::Yaml)
}

/// Emit a `tracing::warn!` line when the conversion drops source-format
/// metadata that the destination format cannot express. M1's `Document`
/// model does not yet carry comments / anchors, so the warning is forward-
/// looking — the actual data preservation arrives in M2.
fn maybe_warn_lossy(input: &str, output: OutputFormat) {
    let target = match output {
        // Formats that can carry comments or are otherwise lossless w.r.t.
        // YAML's source-only metadata don't need a warning. SARIF / JUnit /
        // TAP are also listed here so the warning suppressor compiles — the
        // render path rejects those diagnostic-only formats for `convert`
        // before reaching this function.
        OutputFormat::Console
        | OutputFormat::Yaml
        | OutputFormat::Toon
        | OutputFormat::Jsonl
        | OutputFormat::Frontmatter
        | OutputFormat::Markdown
        | OutputFormat::Sarif
        | OutputFormat::Junit
        | OutputFormat::Tap => {
            return;
        }
        OutputFormat::Json => "JSON",
        OutputFormat::Toml => "TOML",
        OutputFormat::Hcl => "HCL",
        OutputFormat::Ini => "INI",
        OutputFormat::DotEnv => "dotenv",
        OutputFormat::Csv => "CSV",
        OutputFormat::Tsv => "TSV",
        OutputFormat::Xml => "XML",
    };
    if input == "yaml" {
        tracing::warn!(
            "conversion from {input} to {target} drops comments, anchors, and quote-style metadata (M2 will preserve them)",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
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
        Cli::try_parse_from(["dq", "convert", file]).expect("clap parse")
    }

    #[test]
    fn convert_yaml_to_json_emits_valid_json() {
        let tmp = write_yaml("server:\n  port: 8080\n  host: x\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ConvertArgs {
            file: path,
            keep_source: false,
        };
        let opts = WriteOptions::default();
        let mut out: Vec<u8> = Vec::new();
        run(
            &cli,
            &args,
            None,
            OutputFormat::Json,
            false,
            &opts,
            &mut out,
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!({"server":{"port":8080,"host":"x"}})
        );
    }

    #[test]
    fn convert_preserves_big_int_through_json() {
        let mut json_tmp = NamedTempFile::with_suffix(".json").unwrap();
        let big = "4722366482869645213696";
        let json_in = format!("{{\"id\":{big}}}");
        json_tmp.write_all(json_in.as_bytes()).unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(json_tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ConvertArgs {
            file: path,
            keep_source: false,
        };
        let opts = WriteOptions::default();
        let mut out: Vec<u8> = Vec::new();
        run(
            &cli,
            &args,
            None,
            OutputFormat::Json,
            false,
            &opts,
            &mut out,
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains(big),
            "big int literal should survive convert: got {s:?}"
        );
    }

    #[test]
    fn convert_to_toon_renders_toon_output() {
        let tmp = write_yaml("name: alice\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ConvertArgs {
            file: path,
            keep_source: false,
        };
        let opts = WriteOptions::default();
        let mut out: Vec<u8> = Vec::new();
        run(
            &cli,
            &args,
            None,
            OutputFormat::Toon,
            false,
            &opts,
            &mut out,
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("name"), "toon output should mention key: {s:?}");
    }

    #[test]
    fn convert_to_console_honors_use_color_false() {
        // When use_color is false the convert command's console branch must
        // NOT emit ANSI escapes — this proves the global --no-color flag is
        // threaded through the dispatcher.
        let tmp = write_yaml("name: alice\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ConvertArgs {
            file: path,
            keep_source: false,
        };
        let opts = WriteOptions::default();
        let mut out: Vec<u8> = Vec::new();
        run(
            &cli,
            &args,
            None,
            OutputFormat::Console,
            false,
            &opts,
            &mut out,
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("\x1b["), "expected no ANSI escapes: {s:?}");
    }

    #[test]
    fn convert_rejects_in_place_with_console_output() {
        // M3 §8: `convert -i` accepts `-i + -F`, but Console output has no
        // canonical extension to write to. The convert handler must reject
        // this combination before any I/O happens. (Pre-M3 this test
        // asserted that `-i` itself was rejected; that assertion has
        // moved — `-i` is now accepted with a non-Console format.)
        let cli = Cli::try_parse_from(["dq", "-i", "convert", "/nope.yaml"]).unwrap();
        let args = ConvertArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            keep_source: false,
        };
        let opts = WriteOptions::default();
        let mut out: Vec<u8> = Vec::new();
        let err = run(
            &cli,
            &args,
            None,
            OutputFormat::Console,
            false,
            &opts,
            &mut out,
        )
        .unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn compute_target_path_swaps_extension_for_known_format() {
        let target = compute_target_path(
            camino::Utf8Path::new("/tmp/deploy.yaml"),
            OutputFormat::Json,
        )
        .unwrap();
        assert_eq!(target, camino::Utf8PathBuf::from("/tmp/deploy.json"));
    }

    #[test]
    fn compute_target_path_rejects_console_output() {
        let err = compute_target_path(
            camino::Utf8Path::new("/tmp/deploy.yaml"),
            OutputFormat::Console,
        )
        .unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    // ---- M5 Stage 3 sanity tests for the new write-target formats. -----
    //
    // These are minimal round-trips through `render_to_format`: they prove
    // the dispatcher arm is wired to `dq_core::by_name(...)` correctly. The
    // comprehensive byte-level tests for each format land in Stage 4.

    #[test]
    fn render_to_format_emits_hcl_for_a_simple_map() {
        // HCL is registered in the M5 parsers::registry(); a flat
        // string-keyed map is the simplest shape its writer accepts.
        let tmp = write_yaml("region: eu-west-1\nzone: a\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let (_fmt, doc) = super::super::io_helpers::load_document_with_path(&path, None).unwrap();
        let opts = WriteOptions::default();
        let bytes = render_to_format(&doc, OutputFormat::Hcl, false, &opts).unwrap();
        let s = String::from_utf8(bytes).expect("hcl output must be utf-8");
        assert!(
            s.contains("region"),
            "hcl render should mention the key: got {s:?}"
        );
    }

    #[test]
    fn render_to_format_emits_csv_for_an_array_of_maps() {
        // CSV's writer requires Array<Map<String, scalar>>. We build the
        // input through YAML to avoid hand-rolling a Document — the YAML
        // parser produces the right shape.
        let tmp = write_yaml("- name: alice\n  age: 30\n- name: bob\n  age: 25\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let (_fmt, doc) = super::super::io_helpers::load_document_with_path(&path, None).unwrap();
        let opts = WriteOptions::default();
        let bytes = render_to_format(&doc, OutputFormat::Csv, false, &opts).unwrap();
        let s = String::from_utf8(bytes).expect("csv output must be utf-8");
        assert!(
            s.contains("name"),
            "csv header row should include 'name': got {s:?}"
        );
        assert!(
            s.contains("alice"),
            "csv body should include 'alice': got {s:?}"
        );
    }

    #[test]
    fn render_to_format_emits_frontmatter_with_synthesised_body_for_yaml_source() {
        // Option A from the M5 Stage 3 spec: when the source is a plain YAML
        // (or other non-frontmatter) document, the frontmatter target wraps
        // it in a `---\n…\n---\n` block with an empty body. This proves
        // `synthesize_frontmatter_doc` is wired up — the writer would
        // otherwise return Error::Format("document is not a frontmatter
        // document").
        let tmp = write_yaml("title: hello\nauthor: alice\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let (_fmt, doc) = super::super::io_helpers::load_document_with_path(&path, None).unwrap();
        let opts = WriteOptions::default();
        let bytes = render_to_format(&doc, OutputFormat::Frontmatter, false, &opts).unwrap();
        let s = String::from_utf8(bytes).expect("frontmatter output must be utf-8");
        assert!(
            s.starts_with("---"),
            "frontmatter output should start with `---`: got {s:?}"
        );
        assert!(
            s.contains("title"),
            "frontmatter output should preserve the value: got {s:?}"
        );
    }
}
