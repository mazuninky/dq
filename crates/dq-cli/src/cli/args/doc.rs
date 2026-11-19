//! `--doc` — which document of a multi-document stream a command works on.

use std::fmt;
use std::str::FromStr;

/// The value of the global `--doc` flag.
///
/// Parsed by clap through [`FromStr`], so a value that is neither `all` nor
/// a non-negative integer is a usage error (exit 2) before any handler
/// runs. A file that is not a multi-document stream is a stream of one
/// document: `Index(0)` is that document, any other index is out of range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocSelector {
    /// `--doc N` — the `N`-th document, counting from 0.
    Index(usize),
    /// `--doc all` — every document.
    All,
}

impl FromStr for DocSelector {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            return Ok(Self::All);
        }
        s.parse::<usize>()
            .map(Self::Index)
            .map_err(|_| "expected a document index (0, 1, ...) or 'all'".to_owned())
    }
}

impl fmt::Display for DocSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(index) => write!(f, "{index}"),
            Self::All => f.write_str("all"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_index_and_all() {
        assert_eq!("0".parse::<DocSelector>(), Ok(DocSelector::Index(0)));
        assert_eq!("12".parse::<DocSelector>(), Ok(DocSelector::Index(12)));
        assert_eq!("all".parse::<DocSelector>(), Ok(DocSelector::All));
    }

    #[test]
    fn rejects_anything_else() {
        for bad in ["", "-1", "x", "ALL", "1.5", " 1"] {
            assert!(
                bad.parse::<DocSelector>().is_err(),
                "{bad:?} must not parse"
            );
        }
    }

    #[test]
    fn displays_as_typed() {
        assert_eq!(DocSelector::Index(3).to_string(), "3");
        assert_eq!(DocSelector::All.to_string(), "all");
    }
}
