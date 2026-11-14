//! The `readiness` rule. The suite starts any service itself, so a plugin that declares
//! `readiness` and then answers `ready` with no is misreporting its own fixture, and a
//! plugin that declares the capability but never answers the method strands every scan of
//! it. Both are violations; a plugin that does not declare the capability is not
//! applicable, because its environment is the tree discovery read.

use std::path::PathBuf;

use afz_conformance::{self as conformance, Check};
use afz_engine::rpc::client::Launch;
use afz_protocol::PROTOCOL_VERSION;

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

/// The conforming fake handshake plus the `readiness` capability. `ready` is the scripted
/// answer, or absent to let the fake answer its always-ready default.
fn declaring_script(ready: Option<&str>) -> String {
    let override_ = ready.map(|r| format!(r#", "ready":{r}"#)).unwrap_or_default();
    format!(
        r#"{{"handshake":{{"protocolVersion":"{PROTOCOL_VERSION}","name":"afz-fakeplugin","version":"0.0.0","targetClass":"design-time","capabilities":["readiness"],"languages":["fake"]}}{override_}}}"#
    )
}

fn check(script: &str) -> Check {
    Check::new(Launch::new([fakeplugin_path().display().to_string()]).with_env("AFZ_FAKE_SCRIPT", script), ".")
}

#[test]
fn a_declared_capability_that_answers_not_ready_is_a_violation() {
    let report = conformance::run(&check(&declaring_script(Some(r#"{"ready":false,"detail":"no service is listening"}"#))));
    assert!(report.failed("readiness"), "a plugin answering not ready after the suite started the service must be caught: {:?}", report.violations);
    let violation = report.violations.iter().find(|v| v.rule == "readiness").expect("the readiness violation");
    assert!(violation.detail.contains("no service is listening"), "the plugin's own reason must be quoted: {}", violation.detail);
}

#[test]
fn a_declared_capability_that_answers_ready_passes() {
    // The fake answers its default `{ready: true}`; the whole suite must pass, which also
    // pins that declaring the capability does not disturb any other rule.
    let report = conformance::run(&check(&declaring_script(None)));
    assert_eq!(report.rules_failed(), Vec::<&'static str>::new(), "the fake declaring readiness must pass every rule: {:?}", report.violations);
}

#[test]
fn an_undeclared_capability_is_not_applicable() {
    // The default fake handshake does not declare readiness, so the suite must not call
    // `ready` and must not fail the plugin for it; the rule is counted not-applicable.
    let report = conformance::run(&check(""));
    assert!(!report.failed("readiness"), "a plugin that never declared readiness must not be failed for it: {:?}", report.violations);
    assert!(report.not_applicable >= 1, "the readiness rule must be counted not-applicable: {}", report.not_applicable);
}
