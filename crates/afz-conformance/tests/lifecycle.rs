//! The runtime lifecycle against a loopback stub. The stub is `python3 -m http.server`:
//! a real process binding the suite-allocated port, answering the health URL, and dying to
//! teardown. These tests pin the two promises the lifecycle makes: the port is the suite's
//! choice, and every failure names the package's own argv, never the suite's internals.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use afz_conformance::lifecycle::{Service, health_ready, substitute};
use afz_conformance::template::{Lifecycle, Manifest, ManifestProperty};
use afz_conformance::{self as conformance, Check};
use afz_engine::rpc::client::Launch;

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

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("afz-lifecycle-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A manifest that declares every template property not-applicable and carries `lifecycle`.
/// The lifecycle tests are about the service, not the fixture.
fn all_not_applicable() -> BTreeMap<String, ManifestProperty> {
    let mut properties = BTreeMap::new();
    for spec in afz_conformance::template::template_v1().properties {
        properties.insert(spec.id, ManifestProperty { symbol: None, input: None, inputs: None, blocked_by_type: None, not_applicable: Some("the lifecycle tests drive the service, not the fixture".to_string()) });
    }
    properties
}

/// A manifest that declares every template property not-applicable and carries `lifecycle`.
/// The lifecycle tests are about the service, not the fixture.
fn manifest_with(lifecycle: Lifecycle) -> Manifest {
    Manifest { template_version: 1, properties: all_not_applicable(), lifecycle: Some(lifecycle), validator: Vec::new() }
}

fn stub_service(sentinel: &Path) -> Lifecycle {
    Lifecycle {
        launch: vec!["python3".to_string(), "-m".to_string(), "http.server".to_string(), "{{port}}".to_string()],
        health: "http://127.0.0.1:{{port}}/".to_string(),
        teardown: vec!["python3".to_string(), "-c".to_string(), format!("open(r'{}', 'w').close()", sentinel.display())],
        nonce: "afz-nonce-42".to_string(),
        probe: "healthProbe".to_string(),
        health_timeout_secs: Some(20),
    }
}

fn dead_service() -> Lifecycle {
    Lifecycle {
        launch: vec!["python3".to_string(), "-c".to_string(), "raise SystemExit(3)".to_string()],
        health: "http://127.0.0.1:{{port}}/health".to_string(),
        teardown: Vec::new(),
        nonce: "afz-nonce-42".to_string(),
        probe: "healthProbe".to_string(),
        health_timeout_secs: Some(2),
    }
}

/// The health URL must carry the port the suite allocated, and teardown must end the
/// listener: after `stop`, the same URL must refuse the connection. (Loopback ports are
/// not reserved, so a rebind by another process in this window is possible in theory and
/// has not been observed.)
#[test]
fn the_suite_allocates_the_port_and_teardown_ends_the_listener() {
    let dir = temp_dir("port");
    let sentinel = dir.join("teardown-ran");
    let lifecycle = stub_service(&sentinel);
    let service = Service::start(&lifecycle, &dir).expect("http.server must answer its health URL on the suite's port");
    let url = substitute(std::slice::from_ref(&lifecycle.health), service.port()).remove(0);
    assert!(url.contains(&service.port().to_string()), "the health URL must carry the allocated port");
    assert!(health_ready(&url).is_ok(), "the service must stay ready after start");
    service.stop().expect("teardown succeeds");
    assert!(health_ready(&url).is_err(), "the listener must be gone after teardown");
}

#[test]
fn teardown_runs_the_packages_command() {
    let dir = temp_dir("teardown");
    let sentinel = dir.join("teardown-ran");
    let service = Service::start(&stub_service(&sentinel), &dir).expect("the stub service becomes ready");
    service.stop().expect("the teardown argv succeeds");
    assert!(sentinel.is_file(), "the package's teardown command must actually run");
}

#[test]
fn a_service_that_never_starts_is_attributed_to_the_package() {
    let error = Service::start(&dead_service(), Path::new(".")).expect_err("nothing will answer the health URL");
    assert!(error.contains("never became ready"), "{error}");
    assert!(error.contains("python3 -c raise SystemExit(3)"), "the failure must name the package's launch argv: {error}");
}

/// The regression this pins: a rendered conformance project lives in a temporary directory,
/// and its `lifecycle.launch` is relative to it (`node service/server.mjs`). The suite's
/// own cwd is the harness checkout, so a launch that ignores the project root spawns a
/// process that cannot find its script, and the health poll says only "connection refused".
#[test]
fn a_relative_launch_resolves_inside_the_project_root() {
    let dir = temp_dir("relative");
    std::fs::create_dir_all(dir.join("service")).expect("service dir");
    std::fs::write(
        dir.join("service").join("server.py"),
        "import sys\nfrom http.server import BaseHTTPRequestHandler, ThreadingHTTPServer\n\nclass Health(BaseHTTPRequestHandler):\n    def do_GET(self):\n        self.send_response(200)\n        self.end_headers()\n        self.wfile.write(b'ok')\n    def log_message(self, *args):\n        pass\n\nThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), Health).serve_forever()\n",
    )
    .expect("service source");
    let lifecycle = Lifecycle {
        launch: vec!["python3".to_string(), "service/server.py".to_string(), "{{port}}".to_string()],
        health: "http://127.0.0.1:{{port}}/health".to_string(),
        teardown: Vec::new(),
        nonce: "afz-nonce-rel".to_string(),
        probe: "healthProbe".to_string(),
        health_timeout_secs: Some(20),
    };
    let service = Service::start(&lifecycle, &dir).expect("the relative script must resolve inside the project root, not the suite's cwd");
    service.stop().expect("dropping the child ends the listener");

    let wrong_root = Service::start(&lifecycle, dir.parent().expect("temp dir has a parent")).expect_err("the same relative path does not exist one level up");
    assert!(wrong_root.contains("never became ready"), "{wrong_root}");
}

#[test]
fn a_dead_service_ends_the_run_before_the_plugin_is_spawned() {
    let mut check = Check::new(Launch::new([fakeplugin_path().display().to_string()]), ".");
    check.template = Some(manifest_with(dead_service()));
    let report = conformance::run(&check);
    assert_eq!(
        report.rules_failed(),
        vec!["lifecycle-service"],
        "the dead service must be the only violation, and the plugin must not be blamed for it: {:?}",
        report.violations
    );
    assert!(report.violations[0].detail.contains("raise SystemExit(3)"), "the message must name the package's argv: {}", report.violations[0].detail);
}

/// Discovery inventory carrying exactly the nonce probe, so the fake can answer it.
const PROBE_DISCOVERY: &str = r#"{"targets":[{"id":"afz:t:9100","targetClass":"runtime","origin":{"http":{"method":"GET","path":"/health"}},"name":"healthProbe","parameters":[],"returns":null,"effects":{"async":true},"materialization":"constructible"}],"skipped":[]}"#;

fn probe_script(directives: &str) -> String {
    format!(r#"{{"discovery":{PROBE_DISCOVERY},"directives":[{directives}]}}"#)
}

fn probe_check(script: &str, lifecycle: Lifecycle) -> Check {
    let launch = Launch::new([fakeplugin_path().display().to_string()]).with_env("AFZ_FAKE_SCRIPT", probe_script(script));
    let mut check = Check::new(launch, ".");
    check.template = Some(manifest_with(lifecycle));
    check
}

#[test]
fn a_probe_that_does_not_echo_the_nonce_is_reported() {
    // The fake's default answer is a number; the nonce cannot be in it, which is exactly the
    // plugin that fakes discovery from a table and never calls the service.
    let dir = temp_dir("nonce-missing");
    let sentinel = dir.join("teardown-ran");
    let report = conformance::run(&probe_check("", stub_service(&sentinel)));
    assert!(report.failed("lifecycle-nonce"), "a probe that answers without the nonce must be caught: {:?}", report.violations);
    assert!(!report.failed("lifecycle-service"), "the service itself came up and must not be blamed: {:?}", report.violations);
}

#[test]
fn a_probe_that_echoes_the_nonce_passes() {
    let dir = temp_dir("nonce-echoed");
    let sentinel = dir.join("teardown-ran");
    let directive = r#"{"at":1,"to":10000,"outcome":{"kind":"ok","durationMs":1.0,"value":{"node":"string","value":"service says afz-nonce-42","provenance":"user"}}}"#;
    let report = conformance::run(&probe_check(directive, stub_service(&sentinel)));
    assert!(!report.failed("lifecycle-nonce"), "the echoed nonce must satisfy the probe: {:?}", report.violations);
    assert!(!report.failed("lifecycle-service"), "the lifecycle must complete cleanly: {:?}", report.violations);
    assert!(sentinel.is_file(), "the run must have torn the service down");
}

/// The suite hands the plugin the port it allocated, as `AFZ_PORT`. The guard below is the
/// documented harness rule from `docs/harness-authoring.md` section 12: a harness that reads
/// `{{port}}` from `AFZ_PORT` refuses to run without it. With no `lifecycle` block nothing was
/// allocated, so the guard must surface as the `spawn` rule; with one, the same guard starts.
#[test]
fn the_suite_hands_the_plugin_the_allocated_port() {
    let guarded = || {
        Launch::new([
            "sh".to_string(),
            "-c".to_string(),
            "if [ -z \"${AFZ_PORT:-}\" ]; then exit 9; fi; exec \"$0\" \"$@\"".to_string(),
            fakeplugin_path().display().to_string(),
        ])
        .with_env("AFZ_FAKE_SCRIPT", probe_script(""))
    };

    let mut check = Check::new(guarded(), ".");
    check.template = Some(Manifest { template_version: 1, properties: all_not_applicable(), lifecycle: None, validator: Vec::new() });
    let report = conformance::run(&check);
    assert!(report.failed("spawn"), "with no lifecycle block there is no AFZ_PORT, and the guard must surface as `spawn`: {:?}", report.violations);

    let dir = temp_dir("port-handoff");
    let sentinel = dir.join("teardown-ran");
    let mut check = Check::new(guarded(), ".");
    check.template = Some(manifest_with(stub_service(&sentinel)));
    let report = conformance::run(&check);
    assert!(!report.failed("spawn"), "the suite must hand the plugin the allocated port: {:?}", report.violations);
}

#[test]
fn substitution_replaces_every_token() {
    let argv = substitute(&["java".to_string(), "-jar".to_string(), "app.jar".to_string(), "--port={{port}}".to_string()], 8123);
    assert_eq!(argv[3], "--port=8123");
    assert!(health_ready("not-a-url").is_err(), "a non-http health URL must be refused, not guessed at");
}
