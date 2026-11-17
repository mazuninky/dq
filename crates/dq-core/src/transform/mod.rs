//! Operations-as-data transformations on a parsed [`Document`].
//!
//! M3 §1 introduces two RFC-defined transformation engines that build on top
//! of the M2 textual-edit primitives [`Document::set_at`] and
//! [`Document::del_at`]:
//!
//! - [`apply_patch`] applies an RFC 6902 JSON Patch ([`PatchOp`] sequence)
//!   atomically — clone-on-apply, so a failed `test` op or any other error
//!   leaves the caller's document untouched.
//! - [`apply_merge`] applies an RFC 7396 JSON Merge Patch — recursive object
//!   merge, `null` removes, scalars / arrays replace.
//!
//! All three engines preserve M2's textual round-trip guarantees by going
//! through the same `set_at` / `del_at` primitives the M2 `dq set` / `dq del`
//! commands use; they do NOT bypass the textual-edit pipeline.
//!
//! M3 §2 adds the structural [`diff`] engine — the third leg of the M3
//! transform tripod. It walks two [`Value`] trees in parallel and produces a
//! minimal `Vec<PatchOp>` (no `Move` / `Copy` / `Test` ops; see the module
//! docs for the rationale and minimality rules).
//!
//! [`Document`]: crate::Document
//! [`Document::set_at`]: crate::Document::set_at
//! [`Document::del_at`]: crate::Document::del_at
//! [`Value`]: crate::Value

pub mod diff;
pub mod merge;
pub mod patch;

pub use diff::diff;
pub use merge::apply_merge;
pub use patch::{PatchOp, apply_patch};
