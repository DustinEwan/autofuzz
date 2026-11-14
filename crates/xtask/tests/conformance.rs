//! The conformance suite, run against the repository's reference fake plugin and against
//! every harness shipped for autofuzz.
//!
//! Two jobs. The first is the false-positive gate: `a_conforming_plugin_passes` and the
//! template gates fail if a rule fires on a plugin that honors the contract, which would make
//! the tool useless to a third-party author. The second is the negative controls: each one
//! scripts exactly one defect into `afz-fakeplugin` and asserts that the named rule is what
//! comes back, because a rule that cannot fail proves nothing.
//!
//! The five template gates read their plugin, emitter, and project root from the install
//! registry, so make them pass by installing: `afz install-harness
//! https://github.com/DustinEwan/autofuzz-harness-js-ts.git`, and the same for
//! `autofuzz-harness-python`, `autofuzz-harness-wasm`, `autofuzz-harness-starter`, and
//! `autofuzz-harness-openapi`.

use std::path::{Path, PathBuf};

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

// ------------------------------------------------------------------ template gates
//
// Each shipped harness renders a conformance project with its own emitter and is validated
// against the embedded template. The gates read the plugin, the emitter, and the root from the
// install record — the engine's dependency on another repository is a dependency on the
// install contract only. The applicability set is the golden the CI asserts: python realizes
// all ten properties; js-ts justifies exactly one absence (private-symbol); wasm realizes
// four and accounts for six; starter justifies two (method-receiver, coverage-digest-live);
// openapi justifies three (crash-unguarded, coverage-digest-live, stdout-purity). Every
// not-applicable line shows in the report, so drift is
// visible, never a silent pass.

fn installed(name: &str) -> afz_engine::harnesses::InstalledHarness {
    afz_engine::harnesses::lookup(name).unwrap_or_else(|| panic!("install the {name} harness first: afz install-harness <git url>"))
}

/// The record's argv with its program replaced by the runtime under test: for the
/// interpreted gates the runtime build is the point, so `AFZ_NODE` and `AFZ_PYTHON`
/// override what the install recorded.
fn relaunch(record: &afz_engine::harnesses::InstalledHarness, runtime: &str) -> Launch {
    let mut argv = record.argv.clone();
    argv[0] = runtime.to_string();
    Launch::new(argv).with_cwd(record.root.display().to_string())
}

fn render_conformance_project(emitter: &[PathBuf], out: &Path) {
    let (program, args) = emitter.split_first().expect("the manifest names an emitter");
    let status = std::process::Command::new(program).args(args).arg("--out").arg(out).status().expect("the emitter runs");
    assert!(status.success(), "the emitter must render the project into {}", out.display());
}

fn run_template_gate(label: &str, launch: Launch, out: &Path, deadline_ms: u64) -> conformance::Report {
    let loaded = afz_engine::config::load(&out.join(afz_engine::config::CONFIG_FILE_NAME)).expect("the rendered project is a scan project");
    let scope = loaded.discovery_scope().expect("the rendered project declares a harness block");
    // The same environment `LoadedConfig::launch` gives a real scan: the wasm harness reads its module
    // from `AFZ_ROOT` before the first request, so a gate that omits these kills the plugin.
    let launch = launch
        .with_env("AFZ_ROOT", out.display().to_string())
        .with_env("AFZ_CONFIG", loaded.path.display().to_string());
    let mut check = Check::new(launch, scope.root.display().to_string());
    check.include = scope.include.unwrap_or_default();
    check.exclude = scope.exclude.unwrap_or_default();
    check.config_path = Some(loaded.path.display().to_string());
    let manifest = afz_conformance::template::load_manifest(&out.join("afz-conformance-manifest.json")).expect("the rendered manifest loads");
    // The project's own fixture validator, with the same runner `afz validate-harness` uses:
    // a template section whose ground truth could not be produced is not a pass.
    if !manifest.validator.is_empty() {
        let argv = afz_engine::harnesses::resolve_argv(out, &manifest.validator).into_iter().map(PathBuf::from).collect::<Vec<_>>();
        check.validator = Some(afz_conformance::template::run_validator(&argv, out, deadline_ms).expect("the fixture validator runs"));
    }
    check.template = Some(manifest);
    let report = conformance::run(&check);
    assert!(report.violations.is_empty(), "the {label} harness must conform in template mode: {:?}", report.violations);
    report
}

fn conformance_out(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("afz-conformance-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The ids the report marks not-applicable, in report order.
fn not_applicable_ids(report: &afz_conformance::template::TemplateReport) -> Vec<&str> {
    report.properties.iter().filter(|p| p.verdict == afz_conformance::template::Verdict::NotApplicable).map(|p| p.id.as_str()).collect()
}

#[test]
fn the_python_harness_passes_template_v1() {
    let record = installed("python");
    let conformance_record = record.conformance.as_ref().expect("afz-harness.json declares conformance");
    let out = conformance_out("py-gate");
    render_conformance_project(&conformance_record.emitter, &out);
    let python = std::env::var("AFZ_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let report = run_template_gate("python", relaunch(&record, &python), &out, conformance_record.deadline_ms.unwrap_or(1000));
    let template = report.template.expect("a template-driven run carries the template section");
    assert_eq!(template.version, 1);
    assert_eq!(
        (template.passed(), template.failed(), template.not_applicable()),
        (10, 0, 0),
        "python realizes every property of v1; the applicability golden is all ten: {:?}",
        template.properties
    );
    let validator = report.validator.as_ref().expect("the conformance project shipped no fixture validator");
    assert_eq!(
        (validator.passed(), validator.failed(), validator.not_applicable()),
        (7, 0, 0),
        "the python fixture validator judges all seven fixture-shaped properties and passes each: {:?}",
        validator.properties
    );
}

#[test]
fn the_js_ts_harness_passes_template_v1() {
    let record = installed("js-ts");
    let conformance_record = record.conformance.as_ref().expect("afz-harness.json declares conformance");
    let out = conformance_out("js-gate");
    render_conformance_project(&conformance_record.emitter, &out);
    let node = std::env::var("AFZ_NODE").unwrap_or_else(|_| "node".to_string());
    let report = run_template_gate("js-ts", relaunch(&record, &node), &out, conformance_record.deadline_ms.unwrap_or(1000));
    let template = report.template.expect("a template-driven run carries the template section");
    assert_eq!(template.version, 1);
    assert_eq!(not_applicable_ids(&template), ["private-symbol"], "the js-ts golden is exactly one justified absence: {:?}", template.properties);
    assert_eq!((template.passed(), template.failed()), (9, 0), "the other nine properties must pass for js-ts: {:?}", template.properties);
    let validator = report.validator.as_ref().expect("the conformance project shipped no fixture validator");
    assert_eq!(
        (validator.passed(), validator.failed(), validator.not_applicable()),
        (6, 0, 1),
        "the js-ts fixture validator passes six and justifies one absence: {:?}",
        validator.properties
    );
    assert_eq!(not_applicable_ids(validator), ["private-symbol"], "the fixture-side absence is the same symbol the protocol side names: {:?}", validator.properties);
}

#[test]
fn the_wasm_harness_passes_template_v1() {
    let record = installed("wasm");
    let conformance_record = record.conformance.as_ref().expect("afz-harness.json declares conformance");
    let out = conformance_out("wasm-gate");
    render_conformance_project(&conformance_record.emitter, &out);
    // No runtime override: the plugin is the Rust binary the install step built.
    let launch = Launch::new(record.argv.clone()).with_cwd(record.root.display().to_string());
    let report = run_template_gate("wasm", launch, &out, conformance_record.deadline_ms.unwrap_or(1000));
    let template = report.template.expect("a template-driven run carries the template section");
    assert_eq!(template.version, 1);
    assert_eq!(
        (template.passed(), template.failed(), template.not_applicable()),
        (4, 0, 6),
        "a bare module realizes four properties of v1: {:?}",
        template.properties
    );
    assert_eq!(
        not_applicable_ids(&template),
        ["guard-refusal", "private-symbol", "method-receiver", "stdout-purity", "reproducer-script", "deadline-sleep"],
        "the wasm golden is the six absences the emitter's manifest justifies, in template order: {:?}",
        template.properties
    );
    let validator = report.validator.as_ref().expect("the conformance project shipped no fixture validator");
    assert_eq!(
        (validator.passed(), validator.failed(), validator.not_applicable()),
        (4, 0, 3),
        "the wasm fixture validator judges the module for four ids and finds no source-level shape for three: {:?}",
        validator.properties
    );
    assert_eq!(
        not_applicable_ids(validator),
        ["guard-refusal", "private-symbol", "method-receiver"],
        "a module carries no guard, private name, or method receiver to inspect: {:?}",
        validator.properties
    );
}

#[test]
fn the_starter_harness_passes_template_v1() {
    let record = installed("starter");
    let conformance_record = record.conformance.as_ref().expect("afz-harness.json declares conformance");
    let out = conformance_out("starter-gate");
    render_conformance_project(&conformance_record.emitter, &out);
    // No runtime override: the scaffold is the POSIX shell script the install recorded,
    // and `sh` is the runtime under test.
    let launch = Launch::new(record.argv.clone()).with_cwd(record.root.display().to_string());
    let report = run_template_gate("starter", launch, &out, conformance_record.deadline_ms.unwrap_or(1000));
    let template = report.template.expect("a template-driven run carries the template section");
    assert_eq!(template.version, 1);
    assert_eq!(
        not_applicable_ids(&template),
        ["method-receiver", "coverage-digest-live"],
        "the scaffold golden is exactly two justified absences: {:?}",
        template.properties
    );
    assert_eq!((template.passed(), template.failed()), (8, 0), "the other eight properties must pass for starter: {:?}", template.properties);
    let validator = report.validator.as_ref().expect("the conformance project shipped no fixture validator");
    assert_eq!(
        (validator.passed(), validator.failed(), validator.not_applicable()),
        (5, 0, 2),
        "the starter fixture validator passes five and justifies the same two absences: {:?}",
        validator.properties
    );
    assert_eq!(not_applicable_ids(validator), ["method-receiver", "coverage-digest-live"], "the fixture-side absences are the same properties the protocol side names: {:?}", validator.properties);
}

#[test]
fn the_openapi_harness_passes_template_v1() {
    let record = installed("openapi");
    let conformance_record = record.conformance.as_ref().expect("afz-harness.json declares conformance");
    let out = conformance_out("openapi-gate");
    render_conformance_project(&conformance_record.emitter, &out);
    let node = std::env::var("AFZ_NODE").unwrap_or_else(|_| "node".to_string());
    // deadline_ms 1500 comes from the record, so the 10-second /slow route always outlives
    // the cut the deadline-sleep property asserts.
    let report = run_template_gate("openapi", relaunch(&record, &node), &out, conformance_record.deadline_ms.unwrap_or(1000));
    let template = report.template.expect("a template-driven run carries the template section");
    assert_eq!(template.version, 1);
    assert_eq!(
        not_applicable_ids(&template),
        ["crash-unguarded", "coverage-digest-live", "stdout-purity"],
        "the openapi golden is exactly three justified absences: {:?}",
        template.properties
    );
    assert_eq!((template.passed(), template.failed()), (7, 0), "the other seven properties must pass for openapi: {:?}", template.properties);
    let validator = report.validator.as_ref().expect("the conformance project shipped no fixture validator");
    assert_eq!(
        (validator.passed(), validator.failed(), validator.not_applicable()),
        (5, 0, 2),
        "the openapi fixture validator judges seven ids, passes five, and repeats the two document-side absences: {:?}",
        validator.properties
    );
    assert_eq!(not_applicable_ids(validator), ["crash-unguarded", "coverage-digest-live"], "the fixture-side absences are the two fixture-shaped properties a runtime harness cannot realize: {:?}", validator.properties);
}
