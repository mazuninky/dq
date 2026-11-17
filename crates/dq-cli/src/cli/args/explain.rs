//! `dq explain RULE_ID` — print a rule's description, severity, and references.

use clap::Args;

/// Arguments for `dq explain`.
#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// Rule id (e.g. `k8s.no-latest-tag` or `@std/k8s.no-latest-tag`).
    pub rule_id: String,
}
