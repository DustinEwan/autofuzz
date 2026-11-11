//! Repository helpers.
//!
//! [`fake`] implements the scripted harness plugin used by the engine's transport and
//! supervisor tests, and by `afz probe-plugin`. The plugin binary is a thin `main` over
//! [`fake::serve`], so a test can drive the same behavior in-process if it ever needs to.

pub mod fake;
pub mod schema;
