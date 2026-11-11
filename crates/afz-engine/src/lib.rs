//! The autofuzz engine: generation, corpus, scheduling, supervision, oracles, minimization,
//! and the scan result.
//!
//! Nothing in this crate knows a language. It talks to a harness plugin over JSON-RPC and
//! reads only `TypeNode`, `ValueTree`, and `Outcome`.

pub mod boundaries;
pub mod config;
pub mod corpus;
pub mod generate;
pub mod mutate;
pub mod oracles;
pub mod rng;
pub mod rpc;
pub mod schedule;
pub mod supervise;
