//! `dq-core` — data model, format trait, parsers, and JSON Pointer for the `dq` CLI.

pub mod atomic_write;
pub mod document;
pub mod enumerate;
pub mod error;
pub mod format;
pub mod ir;
pub mod parsers;
pub mod pointer;
pub mod template_guard;
pub mod textual_edit;
pub mod transform;
pub mod write_options;

pub use document::{Document, FormatTag, FrontmatterKind, FrontmatterPayload, Value};
pub use enumerate::enumerate_pointers;
pub use error::{Error, PathErrorKind};
pub use format::{Format, by_name, detect};
pub use ir::{Ir, OwnedIr, Provenance, ProvenanceMap, SyntheticReason};
pub use parsers::{parse_json_with_spans, parse_yaml_with_spans};
pub use pointer::{Pointer, Segment};
pub use transform::{PatchOp, apply_merge, apply_patch, diff};
pub use write_options::{WriteOptions, canonicalize_keys};

/// Convenience alias used throughout `dq-core`.
pub type Result<T> = std::result::Result<T, Error>;
