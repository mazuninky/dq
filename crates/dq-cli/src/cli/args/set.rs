//! `dq set FILE [POINTER] [VALUE]` — mutate the value at a JSON Pointer.
//!
//! M7 introduces the `--jq EXPR` flag: when set, `dq set` becomes a transform
//! that re-emits the document through its native writer rather than splicing
//! the original byte buffer. The pointer + positional VALUE arguments are
//! mutually exclusive with `--jq` (pointer must be empty or `/`, and VALUE is
//! rejected by clap at parse time via `conflicts_with`).

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq set`.
#[derive(Debug, Args)]
pub struct SetArgs {
    /// File to mutate (format detected by extension or `-F`).
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer (RFC 6901). Optional when `--jq` is used (the
    /// transform applies to the document root); required otherwise.
    pub pointer: Option<String>,

    /// Inline value, `-` for stdin, or `@<path>` to read the value from a file.
    /// Mutually exclusive with `--value-from` and with `--jq`.
    #[arg(conflicts_with = "value_from", conflicts_with = "jq")]
    pub value: Option<String>,

    /// Read the value from a file (alternative to inline / stdin / `@<path>`).
    #[arg(long = "value-from", value_parser = clap::value_parser!(Utf8PathBuf), conflicts_with = "jq")]
    pub value_from: Option<Utf8PathBuf>,

    /// Treat the value as a string even if it parses as a JSON literal.
    #[arg(long = "value-string")]
    pub value_string: bool,

    /// Reject the operation if any intermediate node along the pointer is missing.
    #[arg(long = "no-create")]
    pub no_create: bool,

    /// Apply a jq transform to the entire document.
    ///
    /// Mutually exclusive with the positional VALUE and with `--value-from`.
    /// The pointer argument MUST be omitted (or be `/`) when `--jq` is used —
    /// the transform is applied to the document root.
    ///
    /// NOTE: `--jq` routes through the format's native re-emit path
    /// (`Format::write_with_options`), which drops YAML comments and any
    /// other formatting the M2 textual-edit splice would have preserved. Use
    /// point-edits (`dq set FILE POINTER VALUE`) when comment preservation
    /// matters; reach for `--jq` when you need conditional or
    /// structure-changing transforms.
    ///
    /// The filter must produce exactly one output value — multi-output
    /// streams (e.g. `.[]`) and empty streams (e.g. `empty`) are rejected
    /// with `INVALID_INPUT` so users do not silently lose data.
    #[arg(
        long = "jq",
        value_name = "EXPR",
        conflicts_with = "value",
        conflicts_with = "value_from"
    )]
    pub jq: Option<String>,
}
