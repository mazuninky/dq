//! `dq-exec` — rule runtime for the dq lint engine.
//!
//! Public surface:
//! - [`Diagnostic`] / [`Severity`] — the structured violation type.
//! - [`Rule`] / [`RuleSet`] / [`RuleSource`] — parsed rule schema.
//! - `Evaluator` — pre-compiled rule runner (filled in §3.2).
//! - `RuleLoader` — `--rules` and auto-discovery (filled in §3.3).
//! - `RuleTester` — `*.test.yml` fixture runner (filled in §3.4).
//! - [`ExecError`] — `thiserror`-based error enum with stable [`ExecError::kind_name`].

/// M11 Phase 4: composite-rule runtime — recursive cross-format
/// evaluation for `Check::Composite { extract, nested, message }`.
pub(crate) mod composite;
/// Structured diagnostic emitted by the lint engine — see `crates/dq-cli/src/output/sarif.rs`
/// for the canonical envelope shape this module mirrors.
pub mod diagnostic;
/// Domain error type — `thiserror` enum with a stable `kind_name()` selector.
pub mod error;
/// Pre-compiled rule runner — `Evaluator::evaluate_file` is the per-file entry point.
pub mod evaluator;
/// M10 autofix runtime — `Fixer::apply` runs every applicable rule's
/// `fix.jq` against a document.
pub mod fixer;
/// `--rules` resolution and implicit auto-binding — `RuleLoader::resolve`.
pub mod loader;
/// Parsed rule schema — `Rule`, `RuleMatch`, [`Check`], `RuleLoc`.
pub mod rule;
/// Multi-rule containers and the three loaders (`from_str` / `from_path` / `from_std`).
pub mod ruleset;
/// M11 Phase 3: JSON Schema 2020-12 compilation + validation for the
/// `Check::Schema` / `Check::SchemaFile` rule variants.
pub(crate) mod schema_check;
/// Mustache-style template renderer for `check.message` — `template::render`.
pub mod template;
/// `*.test.yml` fixture runner — `RuleTester::run_dir`.
pub mod test_runner;

pub use diagnostic::{Diagnostic, Severity};
pub use error::{ExecError, Result};
pub use evaluator::Evaluator;
pub use fixer::{FixOutcome, Fixer};
pub use loader::{LoaderArgs, RuleLoader};
pub use rule::{Check, Rule, RuleCheck, RuleFix, RuleLoc, RuleMatch};
pub use ruleset::{RuleSet, RuleSource};
pub use test_runner::{
    ExpectedOutcome, ExpectedViolation, RuleTestCase, RuleTestFile, RuleTester, TestOutcome,
};
