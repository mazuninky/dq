//! Concrete `Format` implementations and the dispatcher registry.

pub mod csv;
pub mod dockerfile;
pub mod dotenv;
pub mod frontmatter;
pub mod hcl;
pub mod ignore_list;
pub mod ini;
pub mod json;
pub mod jsonl;
pub mod markdown;
pub mod toml;
pub mod xml;
pub mod yaml;
pub mod yaml_spans;

pub use csv::{Csv, Tsv};
pub use dockerfile::Dockerfile;
pub use dotenv::DotEnv;
pub use frontmatter::Frontmatter;
pub use hcl::Hcl;
pub use ignore_list::IgnoreList;
pub use ini::Ini;
pub use json::{
    Json, JsonInsertionRenderer, JsonScalarRenderer, parse_json_with_spans,
    parse_with_spans as parse_json_with_spans_pair,
};
pub use jsonl::Jsonl;
pub use markdown::Markdown;
pub use toml::{Toml, TomlInsertionRenderer, TomlScalarRenderer};
pub use xml::Xml;
pub use yaml::Yaml;
pub use yaml_spans::{
    YamlInsertionRenderer, YamlScalarRenderer, parse_with_spans as parse_yaml_with_spans_pair,
    parse_yaml_with_spans,
};

use crate::format::Format;

/// All formats known to `dq-core`. Order is the precedence used when scanning
/// for a format by extension.
#[must_use]
pub fn registry() -> &'static [&'static dyn Format] {
    // M9: `Markdown` precedes `Frontmatter` so default extension dispatch
    // for `.md` / `.markdown` resolves to the AST-aware parser. The
    // `Frontmatter` parser stays in the registry for explicit
    // `-F frontmatter` reachability (it claims the same extensions).
    static FORMATS: &[&dyn Format] = &[
        &Json,
        &Yaml,
        &Toml,
        &Jsonl,
        &Hcl,
        &Ini,
        &DotEnv,
        &Csv,
        &Tsv,
        &Dockerfile,
        &IgnoreList,
        &Markdown,
        &Frontmatter,
        &Xml,
    ];
    FORMATS
}
