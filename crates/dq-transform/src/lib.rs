//! `dq-transform` — embedded jq engine (via `jaq`) and the value adapters
//! that bridge `dq-core::Value` ↔ `jaq_json::Val` ↔ `serde_json::Value`.
//!
//! The public entry point is [`JqEngine`], which compiles a jq expression
//! once and evaluates it against `serde_json::Value` inputs. The
//! `embedded-jq` cargo feature (default-on) gates the heavy implementation;
//! when off, every method returns [`JqError::FeatureDisabled`].
//!
//! # Example
//!
//! ```
//! # #[cfg(feature = "embedded-jq")]
//! # fn main() -> Result<(), dq_transform::JqError> {
//! use dq_transform::JqEngine;
//!
//! let engine = JqEngine::compile(".count |= . + 1")?;
//! let out = engine.run(&serde_json::json!({"count": 1}))?;
//! assert_eq!(out, vec![serde_json::json!({"count": 2})]);
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "embedded-jq"))]
//! # fn main() {}
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod jq;

pub use jq::{JqEngine, JqError, serde_to_val, val_to_serde};
