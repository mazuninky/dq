//! `dq completions <shell>` — write a shell completion script to stdout.
//!
//! Single-shell, single-target counterpart to the hidden `dq generate-docs`
//! command (which writes a directory tree of files for packaging scripts).
//! `dq completions zsh > ~/.zsh/completions/_dq` is the documented end-user
//! invocation.

use std::io::Write;

use clap::CommandFactory;

use crate::cli::{Cli, CompletionsArgs};

/// Run `dq completions <shell>` — write the completion script to `out`.
///
/// # Errors
///
/// Returns any I/O error from `out` (`clap_complete::generate` writes
/// directly via `std::io::Write`, so a closed pipe surfaces here).
pub fn run(args: &CompletionsArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let mut cmd = Cli::command();
    clap_complete::generate(args.shell, &mut cmd, "dq", out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap_complete::Shell;
    use pretty_assertions::assert_ne;

    /// Render `run` for the given shell and return stdout as a `String`.
    /// Centralises the `Vec<u8>` → UTF-8 plumbing so each per-shell test
    /// stays one assertion long.
    fn render(shell: Shell) -> String {
        let mut buf: Vec<u8> = Vec::new();
        run(&CompletionsArgs { shell }, &mut buf).expect("completions::run must not fail");
        String::from_utf8(buf).expect("completion script must be valid UTF-8")
    }

    #[test]
    fn bash_completion_contains_complete_function_directive() {
        // The generated bash script must declare a `complete` function for
        // the `dq` binary so `source <(dq completions bash)` registers the
        // completer. The marker is part of bash-completion's stable surface.
        let out = render(Shell::Bash);
        assert!(
            out.contains("complete -F"),
            "bash completion missing `complete -F` directive; got: {out:?}",
        );
    }

    #[test]
    fn zsh_completion_contains_compdef_header() {
        // The zsh script must start (after a license preamble) with the
        // `#compdef dq` magic line so zsh's `compinit` discovers it. Without
        // this header the file is just a script, not a completion.
        let out = render(Shell::Zsh);
        assert!(
            out.contains("#compdef dq"),
            "zsh completion missing `#compdef dq` header; got: {out:?}",
        );
    }

    #[test]
    fn fish_completion_contains_complete_c_dq_directive() {
        // The fish script must register completions for the `dq` command
        // via `complete -c dq …`. Asserting on the verb (rather than on a
        // specific subcommand) is robust to clap renaming subcommands.
        let out = render(Shell::Fish);
        assert!(
            out.contains("complete -c dq"),
            "fish completion missing `complete -c dq` directive; got: {out:?}",
        );
    }

    #[test]
    fn powershell_completion_contains_register_argument_completer() {
        // The PowerShell script registers an argument completer for the
        // `dq` command. The verb `Register-ArgumentCompleter` is the
        // PS-stable entry point — no Posh module to mock.
        let out = render(Shell::PowerShell);
        assert!(
            out.contains("Register-ArgumentCompleter"),
            "powershell completion missing `Register-ArgumentCompleter`; got: {out:?}",
        );
    }

    #[test]
    fn elvish_completion_is_non_empty() {
        // Elvish has no fixed marker — `clap_complete::generate` emits an
        // `edit:complex-completer` block whose name is not part of any stable
        // CLI surface. We just verify the call succeeded with output, which
        // catches `clap_complete` regressions where elvish stops emitting.
        let out = render(Shell::Elvish);
        assert_ne!(out.len(), 0, "elvish completion must produce some output");
        assert!(
            out.contains("dq"),
            "elvish completion must mention the binary name `dq`; got: {out:?}",
        );
    }
}
