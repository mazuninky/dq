//! `dq man [PAGE]` — render a troff man page to stdout.
//!
//! With no `PAGE`, the top-level `dq.1` man page is rendered. With
//! `dq man get`, the `dq-get.1` page is rendered. An unknown subcommand name
//! produces a structured [`InvalidInput`] error so the exit-code mapper picks
//! 6 (`INVALID_INPUT`) instead of the catch-all 1.

use std::io::Write;

use clap::CommandFactory;

use crate::cli::{Cli, ManArgs};
use crate::error::InvalidInput;

/// Run `dq man [PAGE]` — render a man page to `out`.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when `args.page` names a subcommand that
///   does not exist.
/// - Any I/O error from the underlying `clap_mangen::Man::render` call.
pub fn run(args: &ManArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let cmd = Cli::command();
    let target = match &args.page {
        None => cmd,
        Some(name) => cmd.find_subcommand(name).cloned().ok_or_else(|| {
            anyhow::Error::new(InvalidInput::new(format!(
                "unknown subcommand '{name}' — try `dq man --help`",
            )))
        })?,
    };
    let man = clap_mangen::Man::new(target);
    man.render(out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit_code::{INVALID_INPUT, exit_code_for_error};

    /// Render `dq man [page]` and return stdout as a `String`.
    fn render(page: Option<&str>) -> String {
        let mut buf: Vec<u8> = Vec::new();
        run(
            &ManArgs {
                page: page.map(str::to_owned),
            },
            &mut buf,
        )
        .expect("man::run must not fail for a known page");
        String::from_utf8(buf).expect("troff must be valid UTF-8")
    }

    #[test]
    fn renders_top_level_page_with_th_header() {
        // The top-level page must carry the `.TH dq 1` troff directive so
        // `man -l -` accepts it. `clap_mangen` emits the page name unquoted;
        // the assertion mirrors that.
        let out = render(None);
        assert!(
            out.contains(".TH dq 1"),
            "expected `.TH dq 1` in man output; got: {out:?}",
        );
    }

    #[test]
    fn renders_get_subcommand_page_with_th_header() {
        // `dq man get` must produce a page whose `.TH` line names `get`.
        // Note: `clap_mangen` produces `.TH get 1`, NOT `.TH dq-get 1` —
        // the page filename convention `dq-get.1` is for the packaging
        // layer (see `commands::generate_docs`); the troff `.TH` field
        // takes the leaf subcommand name only.
        let out = render(Some("get"));
        assert!(
            out.contains(".TH get 1"),
            "expected `.TH get 1` in `dq man get` output; got: {out:?}",
        );
    }

    #[test]
    fn renders_set_subcommand_page_with_th_header() {
        // `dq man set` must also work — verifies `find_subcommand` walks
        // write-side commands too, not only the read-only ones. Catches a
        // future refactor that hides write subcommands behind a feature
        // flag without updating the man-page surface.
        let out = render(Some("set"));
        assert!(
            out.contains(".TH set 1"),
            "expected `.TH set 1` in `dq man set` output; got: {out:?}",
        );
    }

    #[test]
    fn top_level_output_is_substantial_not_empty() {
        // Guard against a regression where `clap_mangen::Man::render` writes
        // only the troff prelude (preamble plus an empty body). A real page
        // is well over 100 bytes — the prelude alone is ~70 bytes.
        let out = render(None);
        assert!(
            out.len() > 100,
            "man output suspiciously short ({} bytes); expected substantial troff body",
            out.len(),
        );
    }

    #[test]
    fn unknown_subcommand_returns_invalid_input() {
        let mut buf: Vec<u8> = Vec::new();
        let err = run(
            &ManArgs {
                page: Some("does-not-exist".to_owned()),
            },
            &mut buf,
        )
        .unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker, got: {err:?}",
        );
        assert!(
            err.to_string().contains("does-not-exist"),
            "error must name the missing page; got: {err}",
        );
    }

    #[test]
    fn unknown_subcommand_maps_to_exit_code_six() {
        // The InvalidInput marker must drive the exit-code mapper to 6
        // (`INVALID_INPUT`) so a typo in `dq man <name>` is distinguishable
        // from the catch-all exit 1. CI scripts that pipe through
        // `dq man <subcommand> > docs/<sub>.1` rely on this contract to
        // detect "you typed the wrong page name" vs "everything else".
        let mut buf: Vec<u8> = Vec::new();
        let err = run(
            &ManArgs {
                page: Some("does-not-exist".to_owned()),
            },
            &mut buf,
        )
        .unwrap_err();
        assert_eq!(
            exit_code_for_error(&err),
            INVALID_INPUT,
            "InvalidInput must map to INVALID_INPUT (6); got mapping for: {err:?}",
        );
    }
}
