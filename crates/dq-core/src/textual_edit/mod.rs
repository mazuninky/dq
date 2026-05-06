//! Textual-edit infrastructure: renderer traits and the format → renderer
//! factory.
//!
//! [`Document::set_at`](crate::document::Document::set_at) needs to render
//! a replacement value back into bytes that match the surrounding
//! syntactic style — for YAML that means matching the original quote /
//! flow-vs-block context, for JSON that means matching the original indent
//! style. Each format registers a [`ScalarRenderer`] (for in-span
//! replacement) and an [`InsertionRenderer`] (for mkdir-p inserts) here.
//!
//! As of M2 §5 the YAML, TOML, and JSON renderers are wired up. JSONL
//! remains line-oriented and has no in-place mutation use case in M2.
//! Formats without a registered renderer surface
//! [`Error::WriteUnavailable`](crate::error::Error::WriteUnavailable) from
//! `Document::set_at`, which the CLI maps to the documented
//! `WRITE_FAILED` exit code.

use crate::document::spans::SpanContext;
use crate::document::{FormatTag, Value};

/// Render a [`Value`] back into bytes matching the syntactic context of
/// the original span being replaced.
///
/// The renderer receives the original bytes too — useful when the format
/// preserves quote style (e.g. `"foo"` vs `'foo'` vs `foo`) by reading the
/// first byte of the original. M2 implementations consult this to keep
/// quotes consistent unless the new value's type forces a different shape.
pub trait ScalarRenderer: Sync + Send {
    /// Produce the byte sequence to splice in place of the original span.
    ///
    /// `value` is the new value to encode; `context` distinguishes
    /// block-vs-flow and mapping-vs-sequence; `original` is the byte slice
    /// of the value being replaced (inclusive of surrounding quotes).
    fn render_replacement(&self, value: &Value, context: SpanContext, original: &[u8]) -> Vec<u8>;
}

/// Render a brand-new key/value pair to be inserted into a parent
/// container at a specific indentation.
///
/// Used by `Document::set_at`'s mkdir-p path. M2 baseline produces a
/// minimal valid output (e.g. `\n  key: value\n` for block-style YAML);
/// finer-grained formatting (matching the parent's existing newline /
/// indent style) is a polishing concern in later milestones.
pub trait InsertionRenderer: Sync + Send {
    /// Produce the byte sequence for a new `key: value` pair.
    ///
    /// `key` is the unescaped pointer segment; `value` is the new value;
    /// `parent_indent` is the indentation of the parent container in the
    /// source bytes; `parent_context` distinguishes block-vs-flow.
    fn render_insertion(
        &self,
        key: &str,
        value: &Value,
        parent_indent: u32,
        parent_context: SpanContext,
    ) -> Vec<u8>;
}

// Per-format renderer instances. They are zero-sized types implementing
// `Sync + Send`, so a single `static` lives for the whole program and the
// factory hands out `&'static dyn _` references.
static YAML_SCALAR_RENDERER: crate::parsers::yaml_spans::YamlScalarRenderer =
    crate::parsers::yaml_spans::YamlScalarRenderer;
static YAML_INSERTION_RENDERER: crate::parsers::yaml_spans::YamlInsertionRenderer =
    crate::parsers::yaml_spans::YamlInsertionRenderer;
static TOML_SCALAR_RENDERER: crate::parsers::toml::TomlScalarRenderer =
    crate::parsers::toml::TomlScalarRenderer;
static TOML_INSERTION_RENDERER: crate::parsers::toml::TomlInsertionRenderer =
    crate::parsers::toml::TomlInsertionRenderer;
static JSON_SCALAR_RENDERER: crate::parsers::json::JsonScalarRenderer =
    crate::parsers::json::JsonScalarRenderer;
static JSON_INSERTION_RENDERER: crate::parsers::json::JsonInsertionRenderer =
    crate::parsers::json::JsonInsertionRenderer;

/// Look up the registered [`ScalarRenderer`] for a format tag.
///
/// YAML wired up in M2 §3, TOML in §4, JSON in §5. JSONL is line-oriented
/// and has no in-place mutation use case in M2.
/// Callers (specifically
/// [`Document::set_at`](crate::document::Document::set_at)) translate
/// the `None` return into
/// [`Error::WriteUnavailable`](crate::error::Error::WriteUnavailable),
/// so the absence of a renderer is observable as a structured error rather
/// than a panic.
#[must_use]
pub fn renderer_for_format(format: FormatTag) -> Option<&'static dyn ScalarRenderer> {
    match format {
        FormatTag::Yaml => Some(&YAML_SCALAR_RENDERER),
        FormatTag::Toml => Some(&TOML_SCALAR_RENDERER),
        FormatTag::Json => Some(&JSON_SCALAR_RENDERER),
        // Line-oriented and M5 formats: no in-place textual-edit renderer.
        // Writes go through the format's `Format::write` whole-document path.
        FormatTag::Jsonl
        | FormatTag::Hcl
        | FormatTag::Ini
        | FormatTag::DotEnv
        | FormatTag::Csv
        | FormatTag::Tsv
        | FormatTag::Dockerfile
        | FormatTag::IgnoreList
        | FormatTag::Frontmatter
        | FormatTag::Markdown => None,
    }
}

/// Look up the registered [`InsertionRenderer`] for a format tag.
///
/// Mirrors [`renderer_for_format`] for the mkdir-p path. `Document::set_at`
/// only consults this when the target pointer has no existing span; until
/// the M2 mkdir-p splice implementation lands, the lookup is unused but the
/// factory is part of the public surface so §3.3 callers compile against
/// the final shape.
#[must_use]
pub fn insertion_renderer_for_format(format: FormatTag) -> Option<&'static dyn InsertionRenderer> {
    match format {
        FormatTag::Yaml => Some(&YAML_INSERTION_RENDERER),
        FormatTag::Toml => Some(&TOML_INSERTION_RENDERER),
        FormatTag::Json => Some(&JSON_INSERTION_RENDERER),
        // Line-oriented and M5 formats: no in-place insertion renderer.
        FormatTag::Jsonl
        | FormatTag::Hcl
        | FormatTag::Ini
        | FormatTag::DotEnv
        | FormatTag::Csv
        | FormatTag::Tsv
        | FormatTag::Dockerfile
        | FormatTag::IgnoreList
        | FormatTag::Frontmatter
        | FormatTag::Markdown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_renderer_is_registered() {
        // §3 contract: YAML scalar + insertion renderers are live. The next
        // sections (§4 TOML, §5 JSON) will flip the corresponding `None`
        // arms to `Some(_)` and add their own assertions here.
        assert!(
            renderer_for_format(FormatTag::Yaml).is_some(),
            "YAML scalar renderer must be registered after §3"
        );
        assert!(
            insertion_renderer_for_format(FormatTag::Yaml).is_some(),
            "YAML insertion renderer must be registered after §3"
        );
    }

    #[test]
    fn toml_renderer_is_registered() {
        // §4 contract: TOML scalar + insertion renderers are live. Mirror
        // of `yaml_renderer_is_registered`; landing this assertion alongside
        // the implementation pins the registration so §5's JSON work can
        // be reviewed in isolation.
        assert!(
            renderer_for_format(FormatTag::Toml).is_some(),
            "TOML scalar renderer must be registered after §4"
        );
        assert!(
            insertion_renderer_for_format(FormatTag::Toml).is_some(),
            "TOML insertion renderer must be registered after §4"
        );
    }

    #[test]
    fn json_renderer_is_registered() {
        // §5 contract: JSON scalar + insertion renderers are live. Mirror of
        // `yaml_renderer_is_registered` / `toml_renderer_is_registered`.
        assert!(
            renderer_for_format(FormatTag::Json).is_some(),
            "JSON scalar renderer must be registered after §5"
        );
        assert!(
            insertion_renderer_for_format(FormatTag::Json).is_some(),
            "JSON insertion renderer must be registered after §5"
        );
    }

    #[test]
    fn jsonl_renderer_remains_unregistered() {
        // JSONL is line-oriented (one JSON value per line) and has no
        // in-place mutation use case in M2 — the splice unit is the line,
        // not a sub-line span. Pinning it as `None` keeps the contract
        // explicit; if M3 ever wires JSONL textual-edit, this test must be
        // updated alongside the registration.
        let tag = FormatTag::Jsonl;
        assert!(
            renderer_for_format(tag).is_none(),
            "{tag:?} scalar renderer must remain unregistered until its section lands"
        );
        assert!(
            insertion_renderer_for_format(tag).is_none(),
            "{tag:?} insertion renderer must remain unregistered until its section lands"
        );
    }
}
