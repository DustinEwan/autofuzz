//! Every connection the engine brings up for `prepare` must have discovered first.
//!
//! `prepare` resolves a `targetId` only against a `discover` that ran in the same plugin process,
//! and no protocol method carries an inventory from one process to another. Both shipped harnesses
//! resolve a target against their own inventory (`src/server.ts` `target()` in
//! `autofuzz-harness-js-ts`, `src/main.rs` `target()` in `autofuzz-harness-wasm`), so a connection the engine spawned without
//! discovering answers every `prepare` with an RPC error, and the scan files that as a
//! `plugin-lost` finding against the target rather than a harness defect.

use std::path::PathBuf;
use std::time::Duration;

use afz_engine::rpc::client::{spawn_discovered, Launch};
use afz_protocol::{DiscoverReq, PrepareReq, PrepareResp, ValueTree};

/// The fake plugin is a binary of `afz-xtask`, so this crate's tests cannot use
/// `CARGO_BIN_EXE_…`, which only covers the current package.
fn fakeplugin_path() -> PathBuf {
    // cargo places a test executable under `<target>/<profile>/deps/` or under
    // `<target>/<profile>/build/<package>/<hash>/out/` depending on its version: walk every
    // ancestor for the plugin instead of assuming the depth.
    let exe = std::env::current_exe().expect("test executable");
    let mut dir = exe.parent().expect("test executable in a directory");
    loop {
        let candidate = dir.join("afz-fakeplugin");
        if candidate.is_file() {
            return candidate;
        }
        let Some(parent) = dir.parent() else {
            panic!("build the fake plugin: `cargo build --workspace` (no afz-fakeplugin above {})", exe.display());
        };
        dir = parent;
    }
}

#[test]
fn every_connection_the_engine_prepares_on_has_discovered() {
    // `requireDiscover` is the plugin-side statement of the contract: `prepare` and `invoke` are
    // refused until a `discover` has been served on this connection. A helper that only spawned
    // would answer the `prepare` below with an RPC error and fail the `PrepareResp::Ok` match.
    let launch = Launch::new([fakeplugin_path().display().to_string()])
        .with_env("AFZ_FAKE_SCRIPT", r#"{"requireDiscover":true}"#);
    let discovery = DiscoverReq { root: ".".to_string(), ..Default::default() };
    let (client, lost) = spawn_discovered(&launch, &discovery).expect("the fake plugin starts");
    assert!(lost.is_none(), "the plugin answered its own inventory: {}", lost.unwrap().detail());
    assert!(
        matches!(
            client.prepare(&PrepareReq { target_id: "afz:t:0001".into(), values: vec![ValueTree::number(1.0)] }, Duration::from_secs(30)),
            Ok(PrepareResp::Ok { .. }),
        ),
        "a connection the engine brought up for `prepare` must be able to answer `prepare`",
    );
}
