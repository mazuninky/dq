//! `dq generate-docs --output-dir DIR` — emit man pages and shell completions.
//!
//! Hidden command (`#[command(hide = true)]` lives on the `Cli::Command`
//! enum). Used by the build pipeline / packaging scripts; not surfaced in
//! `--help` to avoid confusing end users.
//!
//! Layout:
//!
//! ```text
//! <output_dir>/
//!   man/
//!     dq.1
//!     dq-get.1
//!     ...
//!   completions/
//!     dq.bash
//!     _dq            (zsh)
//!     dq.fish
//!     _dq.ps1        (PowerShell)
//! ```
//!
//! `clap_mangen::generate_to` walks the command tree itself; for completions
//! we call `clap_complete::generate` once per shell and pick a filename that
//! matches the shell's convention.

use std::fs;
use std::path::PathBuf;

use clap::CommandFactory;
use clap_complete::Shell;

use crate::cli::{Cli, GenerateDocsArgs};

/// Run the hidden `generate-docs` command.
///
/// # Errors
///
/// Returns `std::io::Error`-derived `anyhow::Error`s when the output
/// directory cannot be created or when writing a generated file fails.
pub fn run(args: &GenerateDocsArgs) -> anyhow::Result<()> {
    let root = args.output_dir.as_std_path();
    let man_dir = root.join("man");
    let comp_dir = root.join("completions");
    fs::create_dir_all(&man_dir)?;
    fs::create_dir_all(&comp_dir)?;

    // Man pages: clap_mangen handles the recursive walk for us.
    clap_mangen::generate_to(Cli::command(), &man_dir)?;

    // Completions: one call per shell.
    let mut cmd = Cli::command();
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
        write_completion(&mut cmd, shell, &comp_dir)?;
    }

    Ok(())
}

fn write_completion(
    cmd: &mut clap::Command,
    shell: Shell,
    out_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    clap_complete::generate(shell, cmd, "dq", &mut buf);
    let path = filename_for_shell(out_dir, shell);
    fs::write(path, buf)?;
    Ok(())
}

fn filename_for_shell(dir: &std::path::Path, shell: Shell) -> PathBuf {
    let leaf = match shell {
        Shell::Bash => "dq.bash",
        Shell::Zsh => "_dq",
        Shell::Fish => "dq.fish",
        Shell::PowerShell => "_dq.ps1",
        // `Shell` is `#[non_exhaustive]`; default to a generic filename so
        // forward-compat additions don't crash here.
        _ => "dq.completion",
    };
    dir.join(leaf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generate_docs_creates_man_and_completions() {
        let tmp = tempdir().unwrap();
        let out = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let args = GenerateDocsArgs {
            output_dir: out.clone(),
        };
        run(&args).unwrap();

        assert!(
            tmp.path().join("man").join("dq.1").is_file(),
            "expected man/dq.1"
        );
        assert!(
            tmp.path().join("completions").join("dq.bash").is_file(),
            "expected completions/dq.bash"
        );
        assert!(
            tmp.path().join("completions").join("_dq").is_file(),
            "expected completions/_dq (zsh)"
        );
        assert!(
            tmp.path().join("completions").join("dq.fish").is_file(),
            "expected completions/dq.fish"
        );
        assert!(
            tmp.path().join("completions").join("_dq.ps1").is_file(),
            "expected completions/_dq.ps1"
        );
    }

    #[test]
    fn generate_docs_creates_subcommand_man_pages() {
        let tmp = tempdir().unwrap();
        let out = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let args = GenerateDocsArgs { output_dir: out };
        run(&args).unwrap();
        // Spot-check a few subcommand pages.
        for sub in ["get", "exists", "convert", "paths"] {
            let p = tmp.path().join("man").join(format!("dq-{sub}.1"));
            assert!(p.is_file(), "expected {p:?} to exist");
        }
    }
}
