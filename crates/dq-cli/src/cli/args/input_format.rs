//! `--input-format` — the format a file, or stdin `-`, is read in.

/// The value of the global `--input-format` flag: one variant per parser in
/// [`dq_core::parsers::registry`], named as the parser names itself.
///
/// A `ValueEnum`, so an unknown name is a usage error (exit 2) before any
/// handler runs. `-F/--format` is the output format and never reaches the
/// parser; this is the only way to override detection.
#[derive(Debug, Clone, Copy, Eq, PartialEq, clap::ValueEnum)]
pub enum InputFormat {
    /// JSON.
    Json,
    /// YAML (a `---`-separated stream reads as several documents).
    Yaml,
    /// TOML.
    Toml,
    /// Newline-delimited JSON.
    Jsonl,
    /// HCL (HashiCorp Configuration Language).
    Hcl,
    /// INI / Java `.properties`.
    Ini,
    /// `.env` files (`KEY=VALUE`).
    #[value(name = "dotenv", alias = "dot-env")]
    Dotenv,
    /// CSV (comma-separated values).
    Csv,
    /// TSV (tab-separated values).
    Tsv,
    /// Dockerfile (read-only).
    Dockerfile,
    /// `.gitignore`-style lists (read-only).
    IgnoreList,
    /// CommonMark + GFM markdown.
    Markdown,
    /// Markdown read as its YAML / TOML / JSON frontmatter.
    Frontmatter,
    /// XML 1.0.
    Xml,
}

impl InputFormat {
    /// The name of the [`dq_core`] parser this variant selects, as
    /// [`dq_core::by_name`] looks it up.
    #[must_use]
    pub fn parser_name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Jsonl => "jsonl",
            Self::Hcl => "hcl",
            Self::Ini => "ini",
            Self::Dotenv => "dotenv",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Dockerfile => "dockerfile",
            Self::IgnoreList => "ignore-list",
            Self::Markdown => "markdown",
            Self::Frontmatter => "frontmatter",
            Self::Xml => "xml",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn every_variant_names_a_registered_parser_and_every_parser_has_one() {
        let names: Vec<&str> = InputFormat::value_variants()
            .iter()
            .map(|v| v.parser_name())
            .collect();
        for name in &names {
            assert!(dq_core::by_name(name).is_some(), "no parser named {name}");
        }
        let registry: Vec<&str> = dq_core::parsers::registry()
            .iter()
            .map(|f| f.name())
            .collect();
        let mut sorted_names = names.clone();
        let mut sorted_registry = registry.clone();
        sorted_names.sort_unstable();
        sorted_registry.sort_unstable();
        assert_eq!(sorted_names, sorted_registry);
    }

    #[test]
    fn the_flag_value_is_the_parser_name() {
        for variant in InputFormat::value_variants() {
            let shown = variant.to_possible_value().expect("not skipped");
            assert_eq!(shown.get_name(), variant.parser_name());
        }
    }

    #[test]
    fn dot_env_is_accepted_as_an_alias() {
        assert_eq!(
            InputFormat::from_str("dot-env", false),
            Ok(InputFormat::Dotenv)
        );
    }
}
