//! Repository helpers.
//!
//! The conformance suite lives in the `afz-conformance` crate, where the product CLI can reach
//! it; the `check-harness` task is its repository-side CLI. [`fake`] implements the scripted
//! harness plugin used by the engine's transport and supervisor tests, and by
//! `afz probe-plugin`. The plugin binary is a thin `main` over [`fake::serve`], so a test can
//! drive the same behavior in-process if it ever needs to.

pub mod fake;
pub mod schema;
