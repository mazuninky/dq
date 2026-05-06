//! `dq get FILE POINTER` — print the value addressed by an RFC 6901 JSON Pointer.
//!
//! Per spec, `get` rejects JSONPath input with a structured suggestion to use
//! `dq select`. The handler returns `dq_core::Error::Path` on a miss so the
//! top-level exit-code mapper produces `NOT_FOUND` (2). Successful resolution
//! is rendered through the supplied [`Reporter`].

use std::io::Write;

use dq_core::Pointer;

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, GetArgs};
use crate::output::Reporter;

/// Run the `get` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `get` is a read subcommand.
/// - `dq_core::Error::Path` when the pointer does not address a node — maps
///   to exit code 2 (`NOT_FOUND`).
/// - `dq_core::Error::Io` / `Parse` / `UnsupportedFormat` for I/O,
///   parsing, and unknown-format failures respectively.
/// - A plain `anyhow::Error` (exit 1) when the user passes a JSONPath
///   expression instead of a JSON Pointer.
pub fn run(
    cli: &Cli,
    args: &GetArgs,
    input_format: Option<&str>,
    doc_arg: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;
    if looks_like_jsonpath(&args.pointer) {
        anyhow::bail!(
            "`get` accepts RFC 6901 JSON Pointer; for JSONPath use `dq select`. \
             Suggested fix: dq get {file} {hint}",
            file = args.file,
            hint = jsonpath_to_pointer_hint(&args.pointer),
        );
    }

    let (_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    let view = select_document(&doc, doc_arg)?;
    let pointer = Pointer::parse(&args.pointer).map_err(anyhow::Error::new)?;
    let value = pointer.resolve(view.as_ref()).map_err(anyhow::Error::new)?;
    let json = value.to_serde_json();
    reporter.report(&json, out)?;
    Ok(())
}

/// Detect whether `s` is a JSONPath expression (so we can refuse it before parsing).
///
/// Matches the bare root `$`, dotted form `$.foo`, and bracketed form `$[0]`.
fn looks_like_jsonpath(s: &str) -> bool {
    s == "$" || s.starts_with("$.") || s.starts_with("$[")
}

/// Heuristic suggestion for users who passed JSONPath: convert the simplest
/// dotted form (`$.a.b.c`) into the equivalent JSON Pointer (`/a/b/c`). We
/// fall back to `<pointer>` when the input has square brackets or other
/// constructs we cannot translate cleanly.
///
/// Per RFC 6901, segments containing `~` or `/` must be escaped as `~0`
/// and `~1` respectively. The order matters: replace `~` first, then `/`,
/// otherwise the `~` in the freshly-emitted `~1` would be double-encoded.
fn jsonpath_to_pointer_hint(s: &str) -> String {
    // Bare `$` is the JSONPath root — equivalent to the JSON Pointer `/`.
    if s == "$" {
        return "/".to_owned();
    }
    if let Some(stripped) = s.strip_prefix("$.")
        && !stripped.contains('[')
        && !stripped.contains('*')
        && !stripped.contains('?')
    {
        let parts: String = stripped
            .split('.')
            .filter(|p| !p.is_empty())
            .map(escape_pointer_segment)
            .map(|p| format!("/{p}"))
            .collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    "<pointer>".to_owned()
}

/// Escape RFC 6901 reserved characters in a single pointer segment.
///
/// The replacement order is load-bearing: `~` becomes `~0` first, then `/`
/// becomes `~1`. Reversing the order would corrupt any `~1` produced by the
/// second step.
fn escape_pointer_segment(seg: &str) -> String {
    seg.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
    use crate::output::ConsoleReporter;
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

    /// Build a default `Cli` with no write flags set, suitable as the
    /// gating-only argument in handler unit tests.
    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "get", file, "/dummy"]).expect("clap parse")
    }

    #[test]
    fn get_returns_scalar() {
        let tmp = write_yaml("server:\n  port: 8080\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = GetArgs {
            file: path,
            pointer: "/server/port".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "8080\n");
    }

    #[test]
    fn get_missing_pointer_returns_path_error() {
        let tmp = write_yaml("server:\n  port: 8080\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = GetArgs {
            file: path,
            pointer: "/server/prot".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "path");
    }

    #[test]
    fn get_rejects_jsonpath_input() {
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = GetArgs {
            file: path,
            pointer: "$.a".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        assert!(err.to_string().contains("dq select"));
        assert!(err.to_string().contains("/a"));
    }

    #[test]
    fn get_rejects_in_place_flag_before_io() {
        // Construct a `Cli` with `-i` so the gate fires before the handler
        // attempts to read the (non-existent) file. Confirms the gate runs
        // first AND that the rejection carries the InvalidInput marker.
        let cli = Cli::try_parse_from(["dq", "-i", "get", "/nope/missing.yaml", "/foo"]).unwrap();
        let args = GetArgs {
            file: camino::Utf8PathBuf::from("/nope/missing.yaml"),
            pointer: "/foo".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker so exit-code mapper picks 6, got: {err:?}",
        );
        assert!(err.to_string().contains("--in-place"));
    }

    #[test]
    fn looks_like_jsonpath_recognises_bare_dollar() {
        assert!(looks_like_jsonpath("$"));
        assert!(looks_like_jsonpath("$.a"));
        assert!(looks_like_jsonpath("$[0]"));
        assert!(!looks_like_jsonpath("/a"));
        assert!(!looks_like_jsonpath("a.b"));
    }

    #[test]
    fn jsonpath_to_pointer_hint_root_is_slash() {
        assert_eq!(jsonpath_to_pointer_hint("$"), "/");
    }

    #[test]
    fn jsonpath_to_pointer_hint_basic_dotted() {
        assert_eq!(jsonpath_to_pointer_hint("$.a.b.c"), "/a/b/c");
    }

    #[test]
    fn jsonpath_to_pointer_hint_escapes_rfc6901_reserved() {
        // `~` and `/` are reserved per RFC 6901; segments must escape them.
        // Order check: `a~/b` -> first `~` becomes `~0`, then `/` becomes `~1`.
        assert_eq!(jsonpath_to_pointer_hint("$.a~b"), "/a~0b");
        assert_eq!(jsonpath_to_pointer_hint("$.a/b"), "/a~1b");
        // Combining both: tilde is replaced FIRST so the `~` produced by `~1`
        // is not double-encoded.
        assert_eq!(jsonpath_to_pointer_hint("$.a~/b"), "/a~0~1b");
    }

    #[test]
    fn jsonpath_to_pointer_hint_falls_back_for_brackets_and_filters() {
        assert_eq!(jsonpath_to_pointer_hint("$.a[0]"), "<pointer>");
        assert_eq!(jsonpath_to_pointer_hint("$.a.*"), "<pointer>");
        assert_eq!(jsonpath_to_pointer_hint("$.a[?(@.b)]"), "<pointer>");
    }
}
