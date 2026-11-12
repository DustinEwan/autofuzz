//! The MCP server library: the six tools, the job registry, and the scan-project resolution
//! every tool runs before it acts on a path.
//!
//! The binary in `main.rs` is a thin `tokio` entry over [`server::run`]. The module tree
//! lives in this library target, and the binary is its only consumer.

pub mod jobs;
pub mod project;
pub mod server;
pub mod tools;
