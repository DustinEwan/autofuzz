//! The conformance suite, run against the repository's reference plugin and against every
//! harness the repository ships.
//!
//! Two jobs. The first is the false-positive gate: `a_conforming_plugin_passes` and the harness
//! tests fail if a rule fires on a plugin that honors the contract, which would make the
//! tool useless to a third-party author. The second is the negative controls: each one scripts
//! exactly one defect into `afz-fakeplugin` and asserts that the named rule is what comes back,
//! because a rule that cannot fail proves nothing.

use std::path::{Path, PathBuf};

use afz_engine::rpc::client::Launch;
use afz_conformance::{self as conformance, Check};

/// The fake plugin is a binary of this crate's sibling package, so `CARGO_BIN_EXE_…` cannot reach
/// it. A test binary sits in `<target>/<profile>/deps/`, one level below it.
fn fakeplugin_path() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    let profile_dir = exe.parent().and_then(|d| d.parent()).expect("target profile dir");
    let candidate = profile_dir.join("afz-fakeplugin");
    assert!(candidate.is_file(), "build the fake plugin: `cargo build --workspace` (looked in {})", candidate.display());
    candidate
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
    assert_eq!(report.passed(), conformance::RULES.len(), "the reference plugin declares every capability the suite can check");
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

// ------------------------------------------------------------------ shipped harnesses

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent).expect("workspace root").to_path_buf()
}

fn js_ts_launch() -> Launch {
    let package = repo_root().join("packages/harness-js");
    let server = package.join("dist/server.js");
    assert!(
        server.is_file(),
        "build the harness plugin first: npm --prefix packages/harness-js run build (looked for {})",
        server.display(),
    );
    let node = std::env::var("AFZ_NODE").unwrap_or_else(|_| "node".to_string());
    Launch::new([node, server.display().to_string()]).with_cwd(package.display().to_string())
}

#[test]
fn the_js_ts_harness_passes() {
    let fixture = repo_root().join("packages/harness-js/test-targets/billing");
    let loaded = afz_engine::config::load(&fixture.join(afz_engine::config::CONFIG_FILE_NAME)).expect("billing is a scan project");
    let scope = loaded.discovery_scope().expect("billing declares a codebase block");
    let mut check = Check::new(js_ts_launch(), scope.root.display().to_string());
    check.include = scope.include.unwrap_or_default();
    check.exclude = scope.exclude.unwrap_or_default();
    check.config_path = Some(loaded.path.display().to_string());

    let report = conformance::run(&check);
    assert_eq!(report.plugin, "js-ts 0.1.0", "the handshake must still name the harness");
    assert!(report.targets > 0, "billing must still yield targets");
    assert!(report.driven > 0, "billing must yield at least one target to drive");
    assert!(report.violations.is_empty(), "the shipped js-ts harness must conform: {:?}", report.violations);
}

fn python_launch() -> Launch {
    let package = repo_root().join("packages/harness-py");
    let server = package.join("src/server.py");
    assert!(server.is_file(), "the python harness ships in-tree (looked for {})", server.display());
    let python = std::env::var("AFZ_PYTHON").unwrap_or_else(|_| "python3".to_string());
    Launch::new([python, server.display().to_string()]).with_cwd(package.display().to_string())
}

#[test]
fn the_python_harness_passes() {
    let fixture = repo_root().join("packages/harness-py/test-targets/pricing");
    let loaded = afz_engine::config::load(&fixture.join(afz_engine::config::CONFIG_FILE_NAME)).expect("pricing is a scan project");
    let scope = loaded.discovery_scope().expect("pricing declares a codebase block");
    let mut check = Check::new(python_launch(), scope.root.display().to_string());
    check.include = scope.include.unwrap_or_default();
    check.exclude = scope.exclude.unwrap_or_default();
    check.config_path = Some(loaded.path.display().to_string());

    let report = conformance::run(&check);
    assert_eq!(report.plugin, "python 0.1.0", "the handshake must still name the harness");
    assert_eq!(report.targets, 5, "the pricing fixture must still yield its five fuzzable symbols");
    assert_eq!(report.driven, 5, "only the two constructors and the property are skipped");
    assert!(report.violations.is_empty(), "the shipped python harness must conform: {:?}", report.violations);
}

// ------------------------------------------------------------------ template gates
//
// Each shipped harness renders a conformance project with its own emitter and is validated
// against the embedded template. The applicability set is the golden the CI asserts: python
// realizes all ten properties; js-ts justifies exactly one absence (private-symbol). Every
// not-applicable line shows in the report, so drift is visible, never a silent pass.

fn render_conformance_project(program: &str, emitter: &Path, out: &Path) {
    let status = std::process::Command::new(program).arg(emitter).arg("--out").arg(out).status().expect("the emitter runs");
    assert!(status.success(), "the emitter must render the project into {}", out.display());
}

fn run_template_gate(label: &str, launch: Launch, out: &Path) -> conformance::Report {
    let loaded = afz_engine::config::load(&out.join(afz_engine::config::CONFIG_FILE_NAME)).expect("the rendered project is a scan project");
    let scope = loaded.discovery_scope().expect("the rendered project declares a codebase block");
    let mut check = Check::new(launch, scope.root.display().to_string());
    check.include = scope.include.unwrap_or_default();
    check.exclude = scope.exclude.unwrap_or_default();
    check.config_path = Some(loaded.path.display().to_string());
    check.template = Some(afz_conformance::template::load_manifest(&out.join("afz-conformance-manifest.json")).expect("the rendered manifest loads"));
    let report = conformance::run(&check);
    assert!(report.violations.is_empty(), "the {label} harness must conform in template mode: {:?}", report.violations);
    report
}

fn conformance_out(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("afz-conformance-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn the_python_harness_passes_template_v1() {
    let python = std::env::var("AFZ_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let out = conformance_out("py-gate");
    render_conformance_project(&python, &repo_root().join("packages/harness-py/conformance/emit.py"), &out);
    let report = run_template_gate("python", python_launch(), &out);
    let template = report.template.expect("a template-driven run carries the template section");
    assert_eq!(template.version, 1);
    assert_eq!(
        (template.passed(), template.failed(), template.not_applicable()),
        (10, 0, 0),
        "python realizes every property of v1; the applicability golden is all ten: {:?}",
        template.properties
    );
}

#[test]
fn the_js_ts_harness_passes_template_v1() {
    let node = std::env::var("AFZ_NODE").unwrap_or_else(|_| "node".to_string());
    let out = conformance_out("js-gate");
    render_conformance_project(&node, &repo_root().join("packages/harness-js/conformance/emit.mjs"), &out);
    let report = run_template_gate("js-ts", js_ts_launch(), &out);
    let template = report.template.expect("a template-driven run carries the template section");
    let not_applicable: Vec<&str> = template.properties.iter().filter(|p| p.verdict == afz_conformance::template::Verdict::NotApplicable).map(|p| p.id.as_str()).collect();
    assert_eq!(not_applicable, ["private-symbol"], "the js-ts golden is exactly one justified absence: {:?}", template.properties);
    assert_eq!((template.passed(), template.failed()), (9, 0), "the other nine properties must pass for js-ts: {:?}", template.properties);
}

#[test]
#[ignore = "needs cargo build --release -p afz-wasm and a wasm32-unknown-unknown probe module"]
fn the_wasm_harness_passes() {
    let root = repo_root();
    let project = root.join("targets/wasm-probe/autofuzz.json");
    let text = std::fs::read_to_string(&project).expect("targets/wasm-probe/autofuzz.json");
    let document: serde_json::Value = serde_json::from_str(&text).expect("the probe project is JSON");
    // The launch is resolved here rather than through `launch_for`, because that helper resolves a
    // relative argv against the *process* cwd, which is `crates/xtask/` under `cargo test`.
    let argv: Vec<String> = document["codebase"]["command"]
        .as_array()
        .expect("the probe project names a command")
        .iter()
        .map(|argument| argument.as_str().expect("argv is strings").to_string())
        .map(|argument| {
            let candidate = root.join(&argument);
            if candidate.exists() { candidate.display().to_string() } else { argument }
        })
        .collect();
    let launch = Launch::new(argv).with_cwd(root.display().to_string());
    let mut check = Check::new(launch, root.join("targets/wasm-probe").display().to_string());
    check.include = vec!["**/*.wasm".to_string()];

    let report = conformance::run(&check);
    assert_eq!(report.plugin, "wasm 0.1.0", "the handshake must still name the harness");
    assert!(report.targets > 0, "the probe module must yield targets");
    assert!(report.violations.is_empty(), "the shipped wasm harness must conform: {:?}", report.violations);
}
