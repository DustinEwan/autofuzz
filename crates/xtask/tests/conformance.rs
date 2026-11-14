//! The conformance suite, run against the repository's reference fake plugin.
//!
//! Two jobs. The first is the false-positive gate: `a_conforming_plugin_passes` fails if a
//! rule fires on a plugin that honors the contract, which would make the tool useless to a
//! third-party author. The second is the negative controls: each one scripts exactly one
//! defect into `afz-fakeplugin` and asserts that the named rule is what comes back, because
//! a rule that cannot fail proves nothing.
//!
//! Published harnesses are not validated here. Conformance is a property of a harness, so
//! each harness repository's own CI runs `afz install-harness "$GITHUB_WORKSPACE"` and
//! asserts its own report; the engine ships the suite those gates drive and tests it here
//! against a plugin the repository owns.

use std::path::PathBuf;

use afz_engine::rpc::client::Launch;
use afz_conformance::{self as conformance, Check};

/// The fake plugin is a binary of this crate's sibling package, so `CARGO_BIN_EXE_…` cannot reach
/// it.
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

fn check(script: Option<&str>) -> Check {
    let launch = match script {
        Some(script) => Launch::new([fakeplugin_path().display().to_string()]).with_env("AFZ_FAKE_SCRIPT", script),
        None => Launch::new([fakeplugin_path().display().to_string()]),
    };
    Check::new(launch, ".")
}

/// A script that replaces the handshake with one declaring exactly these capabilities, and adds
/// the given directives, so a test can claim a capability and then not honor it.
fn script_declaring(capabilities: &str, directives: &str) -> String {
    format!(
        r#"{{"handshake":{{"protocolVersion":"afz/1","name":"afz-fakeplugin","version":"0.1.0","targetClass":"design-time","capabilities":[{capabilities}],"languages":["fake"],"discoverRequiresExecution":false}},"directives":[{directives}]}}"#
    )
}

#[test]
fn a_conforming_plugin_passes() {
    let report = conformance::run(&check(None));
    assert!(
        report.violations.is_empty(),
        "the reference plugin must pass every rule it is applicable to: {:?}",
        report.violations
    );
    assert_eq!(report.plugin, "afz-fakeplugin 0.1.0");
    assert_eq!(report.protocol_version, "afz/1");
    // Six targets, one of which `prepare` refuses, so five are invoked twice.
    assert_eq!(report.targets, 6);
    assert_eq!(report.driven, 6);
    // The reference plugin's environment is its own process, so it honestly leaves
    // `readiness` undeclared and that rule is not applicable to it; both declared
    // branches are driven in `crates/afz-conformance/tests/readiness.rs`.
    assert_eq!(report.passed(), conformance::RULES.len() - 1, "readiness is the only rule the reference plugin does not claim");
}

#[test]
fn a_wrong_protocol_version_is_reported() {
    let script = r#"{"handshake":{"protocolVersion":"afz/0","name":"afz-fakeplugin","version":"0.1.0","targetClass":"design-time","capabilities":[],"languages":["fake"],"discoverRequiresExecution":false}}"#;
    let report = conformance::run(&check(Some(script)));
    assert_eq!(
        report.rules_failed(),
        vec!["protocol-version"],
        "a different protocol revision must be the only thing this plugin is failed for: {:?}",
        report.violations
    );
    assert!(
        report.violations[0].detail.contains("\"afz/0\"") && report.violations[0].detail.contains("afz/1"),
        "the message must name both revisions: {}",
        report.violations[0].detail
    );
}

#[test]
fn a_declared_capability_must_show_up_in_the_answer() {
    let script = script_declaring("\"coverage\"", r#"{"dropCoverage":true}"#);
    let report = conformance::run(&check(Some(&script)));
    assert!(
        report.failed("coverage-declared"),
        "a plugin that declares `coverage` and answers `ok` with no digest must be caught: {:?}",
        report.violations
    );
}

#[test]
fn an_edge_the_coverage_map_does_not_name_is_reported() {
    // 999 is outside the range the reference plugin's coverage map names. The map is the
    // denominator of `coverageRatio` and the table `coverage.json` is written from, so a digest
    // edge it cannot name is a covered line no reader can locate.
    let report = conformance::run(&check(Some(r#"{"directives":[{"coverage":[999]}]}"#)));
    assert!(
        report.failed("coverage-edges-known"),
        "a digest edge the coverage map does not name must be caught: {:?}",
        report.violations
    );
}

#[test]
fn a_negative_duration_is_reported() {
    // A missing `durationMs` cannot be scripted here: `OkOutcome` carries `#[serde(default)]` and
    // serializes the field as `0.0` even when the script omits it, so the rule is pinned on the
    // value the deadline oracle cannot use.
    let report = conformance::run(&check(Some(r#"{"directives":[{"outcome":{"kind":"ok","durationMs":-1.0}}]}"#)));
    assert!(
        report.failed("outcome-duration"),
        "a negative duration must be caught: {:?}",
        report.violations
    );
}

// ------------------------------------------------------------------ template negative controls
//
// Each control realizes exactly one property of the embedded template against a scripted
// defect in the fake plugin and declares the other nine not-applicable, so the named rule
// is the only one the run can fail. A template rule that cannot fail proves nothing.

/// A Check in template mode: `defect` is realized by `realization`, every other property
/// carries a not-applicable line, and `script` is the fake's scripted defect.
fn template_control(defect: &str, realization: &str, script: Option<&str>) -> Check {
    let mut properties = serde_json::Map::new();
    for spec in afz_conformance::template::template_v1().properties {
        let entry = if spec.id == defect {
            serde_json::from_str(realization).expect("the control's realization is a JSON object")
        } else {
            serde_json::json!({"notApplicable": "the control realizes exactly one property"})
        };
        properties.insert(spec.id, entry);
    }
    let document = serde_json::json!({"templateVersion": 1, "properties": properties});
    let dir = std::env::temp_dir().join(format!("afz-template-control-{}-{}", std::process::id(), defect));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir for the control manifest");
    let file = dir.join("afz-conformance-manifest.json");
    std::fs::write(&file, document.to_string()).expect("write the control manifest");
    let mut check = check(script);
    check.template = Some(afz_conformance::template::load_manifest(&file).expect("the control manifest loads"));
    check
}

fn num(n: i64) -> serde_json::Value {
    serde_json::json!({"node": "number", "value": n, "provenance": "user"})
}

#[test]
fn a_symbol_that_never_crashes_fails_the_crash_property() {
    let report = conformance::run(&template_control("crash-unguarded", &format!(r#"{{"symbol":"plain","input":[{}]}}"#, num(1)), None));
    assert!(report.failed("crash-unguarded"), "a fixture whose crash symbol answers ok must be caught: {:?}", report.violations);
}

#[test]
fn an_undocumented_refusal_channel_fails_the_guard_property() {
    let script = r#"{"directives":[{"at":1,"to":10000,"for":"afz:t:0001","outcome":{"kind":"rejected","durationMs":1.0,"via":"custom-channel","detail":"refused through a channel the contract does not define"}}]}"#;
    let report = conformance::run(&template_control("guard-refusal", &format!(r#"{{"symbol":"plain","input":[{}]}}"#, num(-5)), Some(script)));
    assert!(report.failed("guard-refusal"), "a refusal outside guard-site/reject-class must be caught: {:?}", report.violations);
}

#[test]
fn a_fuzzable_symbol_fails_the_private_property() {
    let report = conformance::run(&template_control("private-symbol", r#"{"symbol":"plain"}"#, None));
    assert!(report.failed("private-symbol"), "a manifest naming a discovered target as private must be caught: {:?}", report.violations);
}

#[test]
fn a_fuzzable_symbol_fails_the_unrepresentable_property() {
    let report = conformance::run(&template_control("unrepresentable-symbol", r#"{"symbol":"plain"}"#, None));
    assert!(report.failed("unrepresentable-symbol"), "a manifest naming a discovered target as unrepresentable must be caught: {:?}", report.violations);
}

#[test]
fn the_wrong_blocking_type_fails_the_receiver_property() {
    // Discovery says the blocker is `Ledger`; the manifest claims `Clock`. The task board a
    // human reads would send them to build the wrong type.
    let script = r#"{"discovery":{"targets":[],"skipped":[{"targetId":"afz:t:9001","name":"post","reason":"unconstructible-parameter","blockedBy":{"type":"Ledger","path":"arguments[0]"}}]}}"#;
    let report = conformance::run(&template_control("unconstructible-receiver", r#"{"symbol":"post","blockedByType":"Clock"}"#, Some(script)));
    assert!(report.failed("unconstructible-receiver"), "a manifest naming the wrong blocking type must be caught: {:?}", report.violations);
}

#[test]
fn a_skipped_method_fails_the_receiver_construction_property() {
    let report = conformance::run(&template_control("method-receiver", &format!(r#"{{"symbol":"needsDb","input":[{}]}}"#, num(1)), None));
    assert!(report.failed("method-receiver"), "a manifest declaring an unbuildable receiver constructible must be caught: {:?}", report.violations);
}

#[test]
fn identical_digests_fail_the_coverage_property() {
    // Every call answers the same single edge: the digest cannot tell the two inputs apart,
    // which is what an armed-once or reset-before-the-call probe looks like. The range covers
    // every invocation, because a bare directive governs only its first index.
    let script = r#"{"directives":[{"at":1,"to":10000,"coverage":[7]}]}"#;
    let report = conformance::run(&template_control(
        "coverage-digest-live",
        &format!(r#"{{"symbol":"plain","inputs":[[{}],[{}]]}}"#, num(1), num(2)),
        Some(script),
    ));
    assert!(report.failed("coverage-digest-live"), "identical digests for two inputs must be caught: {:?}", report.violations);
}

#[test]
fn a_process_that_dies_on_the_noise_symbol_fails_the_stdout_property() {
    // The fake counts `invoke` per process: connection A answers the main pass's 10 calls
    // (five prepared targets, twice each), and the template's stdout drive is its 11th. The
    // worker connection is a second process with its own counter, so it never hits this index.
    let script = r#"{"directives":[{"at":11,"to":11,"for":"afz:t:0001","exit":1}]}"#;
    let report = conformance::run(&template_control("stdout-purity", r#"{"symbol":"plain","input":[]}"#, Some(script)));
    assert!(report.failed("stdout-purity"), "a plugin that dies while its target writes to stdout must be caught: {:?}", report.violations);
}

#[test]
fn a_reproducer_that_ignores_the_fixture_fails_the_reproducer_property() {
    // The fake's script is a generic pointer at the engine's finding files; it never names
    // the fixture the fault came from, which is exactly the unreproducible artifact a triager
    // cannot use.
    let report = conformance::run(&template_control("reproducer-script", &format!(r#"{{"symbol":"plain","input":[{}]}}"#, num(1)), None));
    assert!(report.failed("reproducer-script"), "a reproducer script that does not name the fixture must be caught: {:?}", report.violations);
}

#[test]
fn an_instant_answer_fails_the_deadline_property() {
    let report = conformance::run(&template_control("deadline-sleep", &format!(r#"{{"symbol":"plain","input":[{}]}}"#, num(1)), None));
    assert!(report.failed("deadline-sleep"), "a sleep fixture that answers ok inside the deadline must be caught: {:?}", report.violations);
}

#[test]
fn a_swallowed_parse_error_is_reported() {
    let report = conformance::run(&check(Some(r#"{"swallowParseError":true}"#)));
    assert!(report.failed("framing-intact"), "silence after an unparseable request must be caught: {:?}", report.violations);
    assert!(
        report.violations.iter().any(|v| v.rule == "framing-intact" && v.detail.contains("unparseable")),
        "the message must name the unparseable request: {:?}",
        report.violations
    );
}

#[test]
fn a_connection_that_only_discovered_can_answer() {
    // The engine's worker connections never receive `handshake` (`supervise.rs` `Worker::start`),
    // so a plugin that requires `discover` — which both shipped harnesses do — must still answer
    // `prepare` and `invoke` there.
    let report = conformance::run(&check(Some(r#"{"requireDiscover":true}"#)));
    assert!(
        !report.failed("connection-ready"),
        "a connection that only received discover must be able to prepare and invoke: {:?}",
        report.violations
    );
    assert!(report.violations.is_empty(), "and nothing else may break: {:?}", report.violations);
}
