//! Conformance suite for harness plugins: drive a plugin over the protocol and say, rule by
//! rule, whether it conforms. `afz validate-harness` and `afz-xtask check-harness` are its CLIs.
//!
//! The engine treats a plugin as a hostile, fallible peer, so every one of its failures already
//! has a defined meaning inside a scan. That is why a conformance report is not a single
//! pass/fail: a plugin that answers an RPC error where an `Outcome` belongs, and a plugin whose
//! coverage map does not name its own digests, both survive a scan and both corrupt the report
//! quietly. Each rule here is one observable property of the contract with one message that names
//! the consequence.
//!
//! Every response is validated against the committed JSON Schema under `docs/schema/` *before* it
//! is deserialized, so a plugin that drifts from the published contract is told so in that
//! contract's own vocabulary rather than with a serde message.
//!
//! When [`Check::template`] carries a manifest, the suite adds a second pass that judges the ten
//! semantic properties of the embedded template against the plugin's own rendered fixture (see
//! [`template`]). A published template version is immutable - new properties add version 2, they
//! never edit version 1 - so a validation result stays reproducible after this tool ships.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use afz_engine::generate::generate_arguments;
use afz_engine::rng::Rng;
use afz_engine::rpc::client::{Call, Launch, PluginClient};
use afz_engine::supervise::DISCOVERY_TIMEOUT;
use afz_protocol::*;
use serde::Serialize;

pub mod lifecycle;
pub mod template;

/// The engine's `PREPARE_TIMEOUT` is `pub(crate)` (`crates/afz-engine/src/scan.rs:79`); the same
/// bound applies here, because `prepare` imports the target's module graph and the invocation
/// deadline deliberately does not constrain it.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(120);
/// The bound `PluginClient::handshake` uses.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// The supervisor's `KILL_GRACE`: the engine waits this far past `deadlineMs` before it kills.
const KILL_GRACE: Duration = Duration::from_millis(250);
/// The scan loop's `COVERAGE_TIMEOUT`. `coverageMap` answers from a table the plugin already
/// holds, so this only bounds writing it over a pipe.
const COVERAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// The readiness probe is a network dialogue the plugin runs; same bound as the
/// coverage call, because a plugin that cannot answer it is stuck.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// The scan loop's `MINIMIZE_SLACK`, on top of the invocation deadline.
const MINIMIZE_SLACK: Duration = Duration::from_secs(10);
/// The id the unknown-method probe sends, clear of the small ids a plugin sees in normal traffic.
const PROBE_ID: u64 = 9001;
/// The JSON-RPC parse-error code. `framing-intact` requires the bad request to receive this
/// frame rather than silence; the engine discards unparseable lines on both directions.
const PARSE_ERROR_CODE: i64 = -32700;

/// Every rule the suite can report, in the order it drives them. A rule that does not apply to a
/// plugin — `minimize-schema` for a plugin that does not declare `minimize` — is counted as not
/// applicable rather than reported as passed.
pub const RULES: &[&str] = &[
    "spawn",
    "protocol-version",
    "handshake-schema",
    "discover-schema",
    "target-ids",
    "skipped-complete",
    "discover-repeatable",
    "discover-ids-stable",
    "readiness",
    "connection-ready",
    "prepare-answers",
    "prepare-schema",
    "invoke-answers",
    "invoke-schema",
    "outcome-duration",
    "coverage-declared",
    "coverage-map-offered",
    "coverage-edges-known",
    "coverage-map-schema",
    "minimize-schema",
    "reproducer-schema",
    "unknown-method-id",
    "framing-intact",
    "process-alive",
];

/// One rule the plugin broke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub rule: &'static str,
    pub detail: String,
}

/// One observation the suite cannot fail a plugin for, because it is also true of a correct
/// plugin on arbitrary third-party code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub rule: &'static str,
    pub detail: String,
}

/// What one run of the suite found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// `<handshake.name> <handshake.version>`, exactly as the plugin reported it.
    pub plugin: String,
    pub protocol_version: String,
    /// `discovery.targets.len()`.
    pub targets: usize,
    /// `discovery.skipped.len()`.
    pub skipped: usize,
    /// Targets `prepare` and `invoke` were called on.
    pub driven: usize,
    pub violations: Vec<Violation>,
    pub notes: Vec<Note>,
    /// Rules that could not be evaluated, because the plugin does not declare the capability they
    /// need or an earlier rule ended the run. Kept out of `passed()` so the summary never claims a
    /// rule was checked that was not.
    pub not_applicable: usize,
    /// The template section, present only when the run was template-driven.
    pub template: Option<template::TemplateReport>,
    /// The project's own fixture-side verdicts, absent when it ships no validator.
    pub validator: Option<template::TemplateReport>,
}

impl Report {
    /// The rules that ran and reported nothing.
    pub fn passed(&self) -> usize {
        RULES.len().saturating_sub(self.violations.len()).saturating_sub(self.not_applicable)
    }

    /// The ids of every rule this plugin violated, for a test that asserts on the exact set.
    pub fn rules_failed(&self) -> Vec<&'static str> {
        self.violations.iter().map(|v| v.rule).collect()
    }

    /// Whether `rule` appears among the violations.
    pub fn failed(&self, rule: &str) -> bool {
        self.violations.iter().any(|v| v.rule == rule)
    }
}

/// One run of the suite.
#[derive(Debug, Clone)]
pub struct Check {
    /// How to start the plugin.
    pub launch: Launch,
    /// The directory `discover` walks.
    pub root: String,
    /// `DiscoverReq.include`, sent verbatim.
    pub include: Vec<String>,
    /// `DiscoverReq.exclude`, sent verbatim.
    pub exclude: Vec<String>,
    /// `DiscoverReq.configPath`, so a plugin can read its own harness settings.
    pub config_path: Option<String>,
    /// The generator seed, so two runs drive the same inputs.
    pub seed: u32,
    /// How many targets `prepare` and `invoke` run on. Each one costs the plugin a `prepare`,
    /// which `docs/examples/yaml.md:58-59` measures at 517 ms cold and 92 ms primed, so this is
    /// the knob that keeps the suite inside a CI job.
    pub drive_targets: usize,
    /// `deadlineMs` for every driven invocation.
    pub deadline_ms: u64,
    /// Template mode: a manifest naming the fixture symbols for the embedded template's ten
    /// properties. `None` runs schema-only: the protocol rules and nothing about semantics.
    pub template: Option<template::Manifest>,
    /// The conformance project's own verdicts on its fixture, when it ships a
    /// validator. `run` compares them with the protocol-side verdicts.
    pub validator: Option<template::TemplateReport>,
}

impl Check {
    /// A check that drives `launch` over `root` with the repository's usual seed and budget.
    pub fn new(launch: Launch, root: impl Into<String>) -> Check {
        Check {
            launch,
            root: root.into(),
            include: Vec::new(),
            exclude: Vec::new(),
            config_path: None,
            seed: 0x5e3a_91c4,
            drive_targets: 8,
            deadline_ms: 1000,
            template: None,
            validator: None,
        }
    }

    fn discovery_request(&self) -> DiscoverReq {
        DiscoverReq {
            root: self.root.clone(),
            include: (!self.include.is_empty()).then(|| self.include.clone()),
            exclude: (!self.exclude.is_empty()).then(|| self.exclude.clone()),
            config_path: self.config_path.clone(),
        }
    }
}

/// One committed schema, loaded once per run.
struct Schema {
    file: &'static str,
    validator: jsonschema::Validator,
}

impl Schema {
    /// Build a validator from embedded text. The schemas are compiled in with `include_str!`
    /// rather than read from disk, so validation works wherever `afz` was installed, including
    /// a harness repository's CI that ran `cargo install --git`. `crates/xtask/tests/schema.rs`
    /// fails on drift between these copies and the Rust types.
    fn new(file: &'static str, text: &'static str) -> Schema {
        let document: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("docs/schema/{file} is not valid JSON: {e}"));
        Schema { file, validator: jsonschema::validator_for(&document).expect("the committed schema is a valid JSON Schema") }
    }

    /// `{subject} does not match docs/schema/{file}: {first error}`, when the document breaks it.
    fn mismatch(&self, subject: &str, document: &serde_json::Value) -> Option<String> {
        self.validator
            .iter_errors(document)
            .next()
            .map(|error| format!("{subject} does not match docs/schema/{}: {error}", self.file))
    }
}

struct Schemas {
    handshake: Schema,
    discovery: Schema,
    prepare_resp: Schema,
    outcome: Schema,
    coverage_map: Schema,
    minimize_resp: Schema,
    reproducer_resp: Schema,
    ready_resp: Schema,
}

impl Schemas {
    fn load() -> Schemas {
        Schemas {
            handshake: Schema::new("handshake.json", include_str!("../../../docs/schema/handshake.json")),
            discovery: Schema::new("discovery.json", include_str!("../../../docs/schema/discovery.json")),
            prepare_resp: Schema::new("prepare-resp.json", include_str!("../../../docs/schema/prepare-resp.json")),
            outcome: Schema::new("outcome.json", include_str!("../../../docs/schema/outcome.json")),
            coverage_map: Schema::new("coverage-map.json", include_str!("../../../docs/schema/coverage-map.json")),
            minimize_resp: Schema::new("minimize-resp.json", include_str!("../../../docs/schema/minimize-resp.json")),
            reproducer_resp: Schema::new("reproducer-resp.json", include_str!("../../../docs/schema/reproducer-resp.json")),
            ready_resp: Schema::new("ready-resp.json", include_str!("../../../docs/schema/ready-resp.json")),
        }
    }
}

/// Run the whole suite. Plugin behavior never panics here: a plugin that will not start, will not
/// answer, or dies mid-call is a violation carrying the reason the transport gave.
pub fn run(check: &Check) -> Report {
    Runner::new(check).run()
}

/// The aligned-column summary the CLI prints, in the shape `afz probe-plugin` uses.
pub fn render(report: &Report) -> String {
    let mut out = format!(
        "plugin      {} ({})\nprotocol    {}\ndiscover    {} target(s), {} skipped\ndriven      {} target(s)\ncheck       {} rules\n",
        report.plugin, report.protocol_version, report.protocol_version, report.targets, report.skipped, report.driven, RULES.len(),
    );
    for violation in &report.violations {
        out.push_str(&format!("FAIL  {}: {}\n", violation.rule, violation.detail));
    }
    for note in &report.notes {
        out.push_str(&format!("note  {}: {}\n", note.rule, note.detail));
    }
    if report.violations.is_empty() {
        out.push_str(&format!("ok  {} rules passed\n", report.passed()));
        if report.not_applicable > 0 {
            out.push_str(&format!("      {} rule(s) not applicable to this plugin\n", report.not_applicable));
        }
    } else {
        out.push_str(&format!("{} rule(s) violated\n", report.violations.len()));
    }
    if let Some(section) = &report.template {
        out.push_str(&format!(
            "template    v{}: {} pass, {} fail, {} not-applicable\n",
            section.version,
            section.passed(),
            section.failed(),
            section.not_applicable(),
        ));
        for property in section.properties.iter().filter(|p| p.verdict == template::Verdict::NotApplicable) {
            out.push_str(&format!("n/a   {}: {}\n", property.id, property.detail));
        }
    }
    if report.template.is_some() {
        match &report.validator {
            Some(section) => out.push_str(&format!(
                "validator: {} pass, {} fail, {} not-applicable\n",
                section.passed(),
                section.failed(),
                section.not_applicable(),
            )),
            None => out.push_str("validator: the project ships none\n"),
        }
    }
    out
}

/// The word the report shows for a verdict, matching the kebab-case wire spelling.
fn verdict_word(verdict: template::Verdict) -> &'static str {
    match verdict {
        template::Verdict::Pass => "pass",
        template::Verdict::Fail => "fail",
        template::Verdict::NotApplicable => "not-applicable",
    }
}

/// Compare the fixture-side verdicts with the protocol-side ones. Empty when
/// every fixture-shaped property agrees.
pub fn compare_validator(fixture: &template::TemplateReport, protocol: &template::TemplateReport) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut fail = |detail: String| violations.push(Violation { rule: "fixture-validator", detail });
    if fixture.version != protocol.version {
        fail(format!("the fixture validator targets template v{}, the suite targets v{}", fixture.version, protocol.version));
    }
    for property in &fixture.properties {
        if !template::is_fixture_shaped(&property.id) {
            fail(format!("the fixture validator reports {}, which is a process property and only the protocol judge may judge it", property.id));
        }
    }
    for id in template::FIXTURE_SHAPED {
        if !fixture.properties.iter().any(|p| p.id == *id) {
            fail(format!("the fixture validator does not report {id}"));
        }
    }
    let mut seen = BTreeSet::new();
    for property in &fixture.properties {
        if !seen.insert(property.id.clone()) {
            fail(format!("the fixture validator reports {} twice", property.id));
        }
    }
    for property in &fixture.properties {
        if !template::is_fixture_shaped(&property.id) {
            continue;
        }
        // `template_pass` reports all ten ids, so a missing protocol-side line only happens
        // for a hand-built report; the disagreement is still real and is reported below.
        let Some(plugin) = protocol.properties.iter().find(|p| p.id == property.id) else { continue };
        if plugin.verdict != property.verdict {
            fail(format!(
                "the fixture validator says {} for {} and the plugin says {}: {}",
                verdict_word(property.verdict),
                property.id,
                verdict_word(plugin.verdict),
                property.detail
            ));
        }
    }
    violations
}

/// The state one run accumulates: the report's parts, plus the counters the notes report.
struct Runner {
    /// Owned so `run` can add `AFZ_PORT` to the launch environment after the suite has
    /// allocated the service port; every plugin connection spawns from this copy.
    check: Check,
    schemas: Schemas,
    violations: Vec<Violation>,
    notes: Vec<Note>,
    not_applicable: usize,
    plugin: String,
    protocol_version: String,
    targets: usize,
    skipped: usize,
    driven: usize,
    /// `invoke` answers connection A gave, which is what `coverage-map-offered` counts.
    invocations: usize,
    /// Every edge id connection A's digests named, against the target that named it first.
    digests: BTreeMap<EdgeId, String>,
    /// Wall time of the `prepare` that ran last, read by the `prepare-ms` note.
    last_prepare_ms: u128,
    /// Driven answers that arrived inside `deadlineMs`, which is what `self-cut` reports.
    inside_deadline: usize,
    /// The template section, built by the second pass when the check carries a manifest.
    template: Option<template::TemplateReport>,
    /// The fixture-side verdicts carried in from the check, reported verbatim.
    validator: Option<template::TemplateReport>,
}

impl Runner {
    fn new(check: &Check) -> Runner {
        Runner {
            check: check.clone(),
            schemas: Schemas::load(),
            violations: Vec::new(),
            notes: Vec::new(),
            not_applicable: 0,
            plugin: String::new(),
            protocol_version: String::new(),
            targets: 0,
            skipped: 0,
            driven: 0,
            invocations: 0,
            digests: BTreeMap::new(),
            last_prepare_ms: 0,
            inside_deadline: 0,
            template: None,
            validator: None,
        }
    }

    fn fail(&mut self, rule: &'static str, detail: impl Into<String>) {
        self.violations.push(Violation { rule, detail: detail.into() });
    }

    fn note(&mut self, rule: &'static str, detail: impl Into<String>) {
        self.notes.push(Note { rule, detail: detail.into() });
    }

    fn skip(&mut self, count: usize) {
        self.not_applicable += count;
    }

    /// `process-alive`: a phase that finished on a process the transport has since given up on.
    fn alive(&mut self, client: &PluginClient, phase: &str) {
        if let Some(reason) = client.lost() {
            self.fail("process-alive", format!("the plugin process {} while {phase} was in flight", reason.detail()));
        }
    }

    /// Ask for the raw document, so the schema check sees exactly what the plugin wrote and the
    /// typed parse sees the same bytes.
    fn raw(&self, client: &PluginClient, method: Method, params: serde_json::Value, timeout: Duration) -> Call<serde_json::Value> {
        client.request::<serde_json::Value>(method, Some(params), timeout)
    }

    fn report(&mut self) -> Report {
        Report {
            plugin: std::mem::take(&mut self.plugin),
            protocol_version: std::mem::take(&mut self.protocol_version),
            targets: self.targets,
            skipped: self.skipped,
            driven: self.driven,
            violations: std::mem::take(&mut self.violations),
            notes: std::mem::take(&mut self.notes),
            not_applicable: self.not_applicable,
            template: self.template.take(),
            validator: self.validator.take(),
        }
    }

    fn run(&mut self) -> Report {
        // A runtime harness's service belongs to the package, so the suite starts it before
        // spawning the plugin: discovery may need it, and every failure below names the
        // package's own launch argv. An in-process manifest carries no lifecycle.
        let lifecycle = self.check.template.as_ref().and_then(|manifest| manifest.lifecycle.clone());
        let mut service = match &lifecycle {
            Some(spec) => match lifecycle::Service::start(spec, std::path::Path::new(&self.check.root)) {
                Ok(service) => Some(service),
                Err(detail) => {
                    self.fail("lifecycle-service", detail);
                    self.skip(RULES.len());
                    return self.report();
                }
            },
            None => None,
        };
        // The plugin reaches the service the suite started, so it needs the port the suite
        // allocated: every connection below spawns from `self.check.launch`, and the harness
        // substitutes `AFZ_PORT` into its own `harness.options` strings. An in-process manifest
        // carries no lifecycle, so `service` is `None` and the environment is untouched.
        if let Some(service) = &service {
            self.check.launch.env.insert("AFZ_PORT".to_string(), service.port().to_string());
        }
        // Connection A is the probe shape: `scan.rs` spawns, handshakes, and discovers on one
        // process, and it is the only connection the engine ever handshakes.
        let a = match PluginClient::spawn(&self.check.launch) {
            Ok(client) => client,
            Err(reason) => {
                self.fail("spawn", reason.detail());
                self.skip(RULES.len() - 1);
                return self.report();
            }
        };
        let Some(handshake) = self.handshake(&a) else {
            return self.report();
        };
        let Some(discovery) = self.discover(&a) else {
            return self.report();
        };
        // The nonce closes the loop between suite and service: if the probe target cannot
        // echo it, the plugin is answering from a table, not from the service this suite
        // started, and every runtime finding it produces would be fiction.
        if let Some(spec) = &lifecycle {
            self.lifecycle_probe(&a, &discovery, spec);
        }
        // The engine asks a plugin that declares `readiness` before it spawns workers,
        // and the suite has already started the service, so this mirrors `scan.rs`.
        self.readiness(&a, &handshake);
        let collect_coverage = handshake.capabilities.contains(&Capability::Coverage);
        let driven = self.driven_targets(&discovery);
        self.driven = driven.len();
        self.drive(&a, &driven, collect_coverage);
        self.coverage(&a, collect_coverage);
        self.minimize(&a, &handshake);
        self.reproducer(&a, &handshake, discovery.targets.first());
        self.worker_connection(&discovery, &driven);
        self.raw_framing();
        // The template pass runs last: `deadline-sleep` may legitimately end connection A, and
        // every protocol rule must already have been answered on it.
        if let Some(manifest) = self.check.template.clone() {
            self.template_pass(&a, &handshake, &discovery, &manifest);
            // The template verdicts are the plugin's own words about its fixture. A project
            // that ships no second judge leaves those words unopposed, and says so.
            if manifest.validator.is_empty() {
                self.note("fixture-validator", "the conformance project ships no fixture validator, so these template verdicts rest on the plugin's own answers");
            }
        }
        // The fixture-side verdicts were produced before the plugin started; the two lists
        // must agree, and a disagreement is the run's own violation, not an authoring note.
        if let Some(fixture) = self.check.validator.clone() {
            self.validator = Some(fixture.clone());
            if let Some(protocol) = &self.template {
                self.violations.extend(compare_validator(&fixture, protocol));
            }
        }
        // Teardown runs after every drive: the service must outlive the last invocation, and
        // a teardown that fails is the package's bug, reported as its own violation.
        if let Some(handle) = service.take() {
            if let Err(detail) = handle.stop() {
                self.fail("lifecycle-service", detail);
            }
        }
        self.report()
    }

    /// `spawn`, `protocol-version`, `handshake-schema`.
    fn handshake(&mut self, client: &PluginClient) -> Option<Handshake> {
        let raw = match self.raw(client, Method::Handshake, serde_json::Value::Null, HANDSHAKE_TIMEOUT) {
            Ok(raw) => raw,
            Err(reason) => {
                self.fail("spawn", reason.detail());
                self.alive(client, "handshake");
                self.skip(RULES.len() - 2);
                return None;
            }
        };
        let handshake = match serde_json::from_value::<Handshake>(raw.clone()) {
            Ok(handshake) => handshake,
            Err(error) => {
                match self.schemas.handshake.mismatch("handshake", &raw) {
                    Some(detail) => self.fail("handshake-schema", detail),
                    None => self.fail(
                        "handshake-schema",
                        format!("handshake is not the shape docs/schema/handshake.json describes: {error}"),
                    ),
                }
                self.skip(RULES.len() - 3);
                return None;
            }
        };
        if let Some(detail) = self.schemas.handshake.mismatch("handshake", &raw) {
            self.fail("handshake-schema", detail);
        }
        self.plugin = format!("{} {}", handshake.name, handshake.version);
        self.protocol_version = handshake.protocol_version.clone();
        if handshake.protocol_version != PROTOCOL_VERSION {
            self.fail(
                "protocol-version",
                format!("handshake reports protocolVersion \"{}\", this build speaks \"{PROTOCOL_VERSION}\"", handshake.protocol_version),
            );
        }
        self.alive(client, "handshake");
        Some(handshake)
    }

    /// `discover-schema`, `target-ids`, `skipped-complete`, `discover-repeatable`, `discover-ms`.
    fn discover(&mut self, client: &PluginClient) -> Option<Discovery> {
        let request = self.check.discovery_request();
        let asked = Instant::now();
        let raw = match self.raw(client, Method::Discover, to_params(&request), DISCOVERY_TIMEOUT) {
            Ok(raw) => raw,
            Err(reason) => {
                self.fail("discover-schema", format!("discover answered no discovery document: {}", reason.detail()));
                self.alive(client, "discover");
                self.skip(RULES.len() - 4);
                return None;
            }
        };
        let took = asked.elapsed();
        let discovery = match serde_json::from_value::<Discovery>(raw.clone()) {
            Ok(discovery) => discovery,
            Err(error) => {
                match self.schemas.discovery.mismatch("discovery", &raw) {
                    Some(detail) => self.fail("discover-schema", detail),
                    None => self.fail(
                        "discover-schema",
                        format!("discovery is not the shape docs/schema/discovery.json describes: {error}"),
                    ),
                }
                self.skip(RULES.len() - 4);
                return None;
            }
        };
        if let Some(detail) = self.schemas.discovery.mismatch("discovery", &raw) {
            self.fail("discover-schema", detail);
        }
        self.targets = discovery.targets.len();
        self.skipped = discovery.skipped.len();
        self.check_ids(&discovery);
        self.check_skipped_are_reported(&discovery);
        self.discover_again(client, &request, &discovery);

        // Discovery is the one cost a harness pays once per plugin process, and the engine replays
        // this same request into every process it starts. The projection is what the operator
        // reads as `setup … ms` in `engine.log` (`scan.rs:1050`).
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        self.note(
            "discover-ms",
            format!(
                "discover took {} ms, so a {workers}-worker scan waits about {:.1} s before its first invocation and pays it again on every worker restart",
                took.as_millis(),
                took.as_millis() as f64 * (workers as f64 + 1.0) / 1000.0,
            ),
        );
        self.alive(client, "discover");
        Some(discovery)
    }

    /// `discover-repeatable`: the engine asks the same connection twice when a scan is scoped, and
    /// a plugin that answers differently the second time has an inventory it cannot rely on.
    fn discover_again(&mut self, client: &PluginClient, request: &DiscoverReq, first_run: &Discovery) {
        let again = self.raw(client, Method::Discover, to_params(request), DISCOVERY_TIMEOUT);
        let second = match again {
            Ok(raw) => match serde_json::from_value::<Discovery>(raw) {
                Ok(second) => second,
                Err(error) => {
                    self.fail("discover-repeatable", format!("the second discover answered something unreadable: {error}"));
                    return;
                }
            },
            Err(reason) => {
                self.fail("discover-repeatable", format!("the second discover did not answer: {}", reason.detail()));
                return;
            }
        };
        let first: BTreeSet<&str> = first_run.targets.iter().map(|t| t.id.as_str()).collect();
        let now: BTreeSet<&str> = second.targets.iter().map(|t| t.id.as_str()).collect();
        if first != now {
            self.fail(
                "discover-repeatable",
                format!("discover answered a different target id set on the second call: {} ids vs {} ids", first.len(), now.len()),
            );
        }
    }

    fn check_ids(&mut self, discovery: &Discovery) {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for target in &discovery.targets {
            if target.id.is_empty() {
                self.fail("target-ids", "discover reported a target with an empty id");
                continue;
            }
            if !seen.insert(target.id.as_str()) {
                self.fail("target-ids", format!("discover reported target id \"{}\" twice", target.id));
            }
        }
    }

    /// `materializationCounts` is summed from `targets` and `skipped` together
    /// (`scan.rs:1115-1121`), so a non-constructible symbol in neither list makes the inventory
    /// arithmetic in `scan.json` lie.
    fn check_skipped_are_reported(&mut self, discovery: &Discovery) {
        let reported: BTreeSet<&str> = discovery.skipped.iter().map(|s| s.target_id.as_str()).collect();
        for target in &discovery.targets {
            if target.materialization == Materialization::Constructible || reported.contains(target.id.as_str()) {
                continue;
            }
            self.fail(
                "skipped-complete",
                format!(
                    "\"{}\" is materialization {} and appears in neither discovery.skipped nor as constructible",
                    target.name,
                    wire_materialization(target.materialization),
                ),
            );
        }
    }

    fn driven_targets(&self, discovery: &Discovery) -> Vec<TargetDescriptor> {
        let mut targets = discovery.targets.clone();
        targets.sort_by(|a, b| a.id.cmp(&b.id));
        targets.truncate(self.check.drive_targets);
        targets
    }

    /// `prepare-answers`, `prepare-schema`, `invoke-answers`, `invoke-schema`, `outcome-duration`,
    /// `coverage-declared`, `prepare-ms`, `self-cut`, `repeat-drift`.
    fn drive(&mut self, client: &PluginClient, driven: &[TargetDescriptor], collect_coverage: bool) {
        let mut rng = Rng::seeded(self.check.seed);
        let mut slowest: Option<(u128, &str)> = None;
        let mut drifted: Vec<&str> = Vec::new();

        for target in driven {
            let values = generate_arguments(&target.parameters, &mut rng);
            let prepared = self.prepare(client, target, &values);
            if slowest.is_none_or(|(ms, _)| self.last_prepare_ms > ms) {
                slowest = Some((self.last_prepare_ms, target.name.as_str()));
            }
            if !prepared {
                continue;
            }
            let kinds = self.invoke_twice(client, target, &values, collect_coverage);
            if kinds.len() == 2 && kinds[0] != kinds[1] {
                drifted.push(target.name.as_str());
            }
        }

        if let Some((ms, name)) = slowest {
            self.note(
                "prepare-ms",
                format!("the slowest prepare was \"{name}\" at {ms} ms over {} target(s); a fresh plugin process pays this once per target", driven.len()),
            );
        }
        if self.invocations > 0 {
            let inside = self.inside_deadline;
            self.note(
                "self-cut",
                format!(
                    "{inside} of {} driven answers arrived inside the {} ms deadline; only a target slower than that shows whether the harness cuts its own call or the engine kills it",
                    self.invocations, self.check.deadline_ms,
                ),
            );
        }
        if !drifted.is_empty() {
            self.note("repeat-drift", format!("the same values answered a different kind on the second invoke for: {}", drifted.join(", ")));
        }
        self.alive(client, "prepare and invoke");
    }

    /// One `prepare`, recording `prepare-answers` and `prepare-schema`. Returns whether the target
    /// can be invoked at all: `{"ok":false,"reason":…}` is the contract's answer for a target the
    /// plugin cannot build, so it ends this target's drive and fails nothing.
    fn prepare(&mut self, client: &PluginClient, target: &TargetDescriptor, values: &[ValueTree]) -> bool {
        let request = PrepareReq { target_id: target.id.clone(), values: values.to_vec() };
        let asked = Instant::now();
        self.last_prepare_ms = 0;
        let prepared = self.raw(client, Method::Prepare, to_params(&request), PREPARE_TIMEOUT);
        self.last_prepare_ms = asked.elapsed().as_millis();
        let mut ok = false;
        match prepared {
            Err(reason) => {
                self.fail(
                    "prepare-answers",
                    format!(
                        "\"{}\" prepare answered an RPC error; report {{\"ok\":false,\"reason\":…}} instead of failing the method: {}",
                        target.name,
                        reason.detail(),
                    ),
                );
            }
            Ok(raw) => {
                if let Some(detail) = self.schemas.prepare_resp.mismatch("prepare response", &raw) {
                    self.fail("prepare-schema", detail);
                }
                match serde_json::from_value::<PrepareResp>(raw) {
                    Ok(PrepareResp::Ok { .. }) => ok = true,
                    Ok(PrepareResp::Skip { .. }) => {}
                    Err(error) => self.fail(
                        "prepare-schema",
                        format!("prepare response is not the shape docs/schema/prepare-resp.json describes: {error}"),
                    ),
                }
            }
        }
        ok
    }

    /// Two `invoke` calls with the same values on the same connection: the first answers the
    /// contract rules, the second is what `repeat-drift` compares against, because module state a
    /// plugin leaked into the first call only shows up on the second.
    fn invoke_twice(&mut self, client: &PluginClient, target: &TargetDescriptor, values: &[ValueTree], collect_coverage: bool) -> Vec<&'static str> {
        let mut kinds: Vec<&'static str> = Vec::new();
        for index in 0..2 {
            let request = InvokeReq {
                invocation_id: index,
                target_id: target.id.clone(),
                values: values.to_vec(),
                deadline_ms: self.check.deadline_ms,
                collect_coverage,
            };
            let timeout = Duration::from_millis(self.check.deadline_ms) + KILL_GRACE;
            match self.raw(client, Method::Invoke, to_params(&request), timeout) {
                Err(reason) => {
                    self.fail(
                        "invoke-answers",
                        format!(
                            "\"{}\" invoke answered an RPC error; a target fault is an Outcome, not an RPC error: {}",
                            target.name,
                            reason.detail(),
                        ),
                    );
                    break;
                }
                Ok(raw) => {
                    self.check_outcome(target, &raw, collect_coverage);
                    let Ok(outcome) = serde_json::from_value::<Outcome>(raw) else { continue };
                    self.invocations += 1;
                    kinds.push(outcome.kind());
                    if outcome.duration_ms() < self.check.deadline_ms as f64 {
                        self.inside_deadline += 1;
                    }
                    if let Some(edges) = outcome.coverage() {
                        for edge in edges {
                            self.digests.entry(*edge).or_insert_with(|| target.name.clone());
                        }
                    }
                }
            }
        }
        kinds
    }

    /// `invoke-schema`, `outcome-duration`, `coverage-declared`.
    fn check_outcome(&mut self, target: &TargetDescriptor, raw: &serde_json::Value, collect_coverage: bool) {
        if let Some(detail) = self.schemas.outcome.mismatch("outcome", raw) {
            self.fail("invoke-schema", detail);
        }
        let kind = raw.get("kind").and_then(|kind| kind.as_str()).unwrap_or("?");
        match raw.get("durationMs") {
            Some(serde_json::Value::Number(ms)) if ms.as_f64().is_some_and(|v| v >= 0.0) => {}
            Some(value) => self.fail(
                "outcome-duration",
                format!("\"{}\" answered {kind} with durationMs {value}; the deadline oracle reads that number", target.name),
            ),
            None => self.fail(
                "outcome-duration",
                format!("\"{}\" answered {kind} with no durationMs; the deadline oracle reads that number", target.name),
            ),
        }
        // `collectCoverage` is only true when the handshake declared `coverage`, so a missing
        // digest here is a capability the plugin claimed and did not honor. The engine then falls
        // back to the outcome tuple as the corpus's novelty signal (`scan.rs:learn`).
        if collect_coverage && kind == "ok" && raw.get("coverage").is_none() {
            self.fail(
                "coverage-declared",
                format!("the handshake declares \"coverage\" but \"{}\" answered ok with no \"coverage\" field", target.name),
            );
        }
    }

    /// `coverage-map-offered`, `coverage-edges-known`, `coverage-map-schema`.
    fn coverage(&mut self, client: &PluginClient, collect_coverage: bool) {
        if !collect_coverage {
            self.skip(3);
            return;
        }
        let raw = match client.request::<serde_json::Value>(Method::CoverageMap, None, COVERAGE_TIMEOUT) {
            Ok(raw) => raw,
            Err(reason) => {
                self.fail(
                    "coverage-map-offered",
                    format!("the handshake declares \"coverage\" but coverageMap did not answer: {}", reason.detail()),
                );
                self.skip(2);
                return;
            }
        };
        if let Some(detail) = self.schemas.coverage_map.mismatch("coverage map", &raw) {
            self.fail("coverage-map-schema", detail);
        }
        let map = match serde_json::from_value::<CoverageMap>(raw) {
            Ok(map) => map,
            Err(error) => {
                self.fail("coverage-map-schema", format!("coverageMap answered something unreadable: {error}"));
                self.skip(1);
                return;
            }
        };
        if map.entries.is_empty() {
            self.fail(
                "coverage-map-offered",
                format!("the handshake declares \"coverage\" but coverageMap answered no entries after {} invocations", self.invocations),
            );
        }
        // The map is the denominator of `coverageRatio` and the table `coverage.json` is written
        // from, so an edge a digest named and the map cannot names covered code no reader can
        // locate.
        let named: BTreeSet<EdgeId> = map.entries.iter().map(|e| e.edge).collect();
        if let Some((edge, name)) = self.digests.iter().find(|(edge, _)| !named.contains(edge)) {
            self.fail(
                "coverage-edges-known",
                format!("invoke reported edge {edge} for \"{name}\", which coverageMap does not name"),
            );
        }
        self.alive(client, "coverageMap");
    }

    /// `minimize-schema`, and nothing else. The engine never sends `minimize` today — minimization
    /// is engine-side delta debugging over repeated `invoke` — so a plugin that fails this rule is
    /// incompatible with a caller that does not exist yet, which is why it is a violation and not
    /// a note: the handshake already told the engine the method works.
    fn minimize(&mut self, client: &PluginClient, handshake: &Handshake) {
        if !handshake.capabilities.contains(&Capability::Minimize) {
            self.skip(1);
            return;
        }
        let request = MinimizeReq {
            target_id: "afz:t:0001".to_string(),
            values: vec![ValueTree::number(1.0), ValueTree::string("autofuzz")],
            fault: FaultSignature { class: "TypeError".to_string(), message: "check-harness".to_string(), frames: Vec::new() },
            budget_ms: self.check.deadline_ms,
        };
        let timeout = Duration::from_millis(self.check.deadline_ms) + MINIMIZE_SLACK;
        match self.raw(client, Method::Minimize, to_params(&request), timeout) {
            Ok(raw) => {
                if let Some(detail) = self.schemas.minimize_resp.mismatch("minimize response", &raw) {
                    self.fail("minimize-schema", detail);
                }
            }
            Err(reason) => self.fail(
                "minimize-schema",
                format!("the handshake declares \"minimize\" but minimize did not answer: {}", reason.detail()),
            ),
        }
        self.alive(client, "minimize");
    }

    /// `reproducer-schema`, and nothing else. The engine sends `reproducer` after it has
    /// minimized a finding and stores the script verbatim, so a declared capability that
    /// answers unreadably or not at all leaves every finding of this plugin without the
    /// artifact a triager runs. Unlike `minimize`, which can shrink any values, rendering
    /// needs a real target: the probe names a discovered one, and a plugin that rendered
    /// honestly could not name a file for a target it never saw.
    fn reproducer(&mut self, client: &PluginClient, handshake: &Handshake, target: Option<&TargetDescriptor>) {
        if !handshake.capabilities.contains(&Capability::Reproducer) {
            self.skip(1);
            return;
        }
        let Some(target) = target else {
            self.skip(1);
            return;
        };
        let request = ReproducerReq {
            target_id: target.id.clone(),
            values: vec![ValueTree::number(1.0), ValueTree::string("autofuzz")],
        };
        let timeout = Duration::from_millis(self.check.deadline_ms) + MINIMIZE_SLACK;
        match self.raw(client, Method::Reproducer, to_params(&request), timeout) {
            Ok(raw) => {
                if let Some(detail) = self.schemas.reproducer_resp.mismatch("reproducer response", &raw) {
                    self.fail("reproducer-schema", detail);
                }
            }
            Err(reason) => self.fail(
                "reproducer-schema",
                format!("the handshake declares \"reproducer\" but reproducer did not answer: {}", reason.detail()),
            ),
        }
        self.alive(client, "reproducer");
    }

    /// `readiness`, and nothing else. A plugin that declares the capability promises the
    /// engine can ask whether the environment its targets live in is up before workers
    /// spawn. The suite starts the service itself before spawning the plugin, so a
    /// not-ready answer here is the plugin misreporting the fixture it just declared.
    fn readiness(&mut self, client: &PluginClient, handshake: &Handshake) {
        if !handshake.capabilities.contains(&Capability::Readiness) {
            self.skip(1);
            return;
        }
        match client.request::<serde_json::Value>(Method::Ready, None, READY_TIMEOUT) {
            Ok(raw) => {
                if let Some(detail) = self.schemas.ready_resp.mismatch("ready response", &raw) {
                    self.fail("readiness", detail);
                } else if raw.get("ready").and_then(|v| v.as_bool()) == Some(false) {
                    let detail = raw.get("detail").and_then(|v| v.as_str()).unwrap_or("the harness gave no reason");
                    self.fail(
                        "readiness",
                        format!("the suite started the service and the plugin still answered not ready: {detail}"),
                    );
                }
            }
            Err(reason) => self.fail(
                "readiness",
                format!("the handshake declares \"readiness\" but ready did not answer: {}", reason.detail()),
            ),
        }
        self.alive(client, "ready");
    }

    /// Connection B is the shape the engine actually prepares on: `supervise.rs` spawns a worker,
    /// sends it `discover`, and never sends it `handshake`. A plugin that resolves a `targetId`
    /// only against the `discover` that ran on that same connection is correct here and nowhere
    /// else, which is why this connection exists.
    fn worker_connection(&mut self, discovery: &Discovery, driven: &[TargetDescriptor]) {
        let request = self.check.discovery_request();
        let client = match PluginClient::spawn(&self.check.launch) {
            Ok(client) => client,
            Err(reason) => {
                self.fail("connection-ready", format!("a second plugin process could not be started: {}", reason.detail()));
                return;
            }
        };
        let raw = match self.raw(&client, Method::Discover, to_params(&request), DISCOVERY_TIMEOUT) {
            Ok(raw) => raw,
            Err(reason) => {
                self.fail("discover-ids-stable", format!("a second plugin process never answered discover: {}", reason.detail()));
                self.alive(&client, "discover on the second process");
                return;
            }
        };
        match serde_json::from_value::<Discovery>(raw) {
            Ok(second) => {
                let first: BTreeSet<&str> = discovery.targets.iter().map(|t| t.id.as_str()).collect();
                let now: BTreeSet<&str> = second.targets.iter().map(|t| t.id.as_str()).collect();
                if first != now {
                    self.fail("discover-ids-stable", "a second plugin process discovered a different target id set");
                }
            }
            Err(error) => self.fail("discover-ids-stable", format!("a second plugin process answered discover unreadably: {error}")),
        }
        self.alive(&client, "discover on the second process");

        let Some(target) = driven.first() else { return };
        let mut rng = Rng::seeded(self.check.seed);
        let values = generate_arguments(&target.parameters, &mut rng);
        let prepare = PrepareReq { target_id: target.id.clone(), values: values.clone() };
        if let Err(reason) = client.prepare(&prepare, PREPARE_TIMEOUT) {
            self.fail(
                "connection-ready",
                format!("a connection that only received discover could not prepare: {}", reason.detail()),
            );
            self.alive(&client, "prepare on the second process");
            return;
        }
        let invoke = InvokeReq {
            invocation_id: 0,
            target_id: target.id.clone(),
            values,
            deadline_ms: self.check.deadline_ms,
            collect_coverage: false,
        };
        let timeout = Duration::from_millis(self.check.deadline_ms) + KILL_GRACE;
        if let Err(reason) = client.invoke(&invoke, timeout) {
            self.fail(
                "connection-ready",
                format!("a connection that only received discover could not invoke: {}", reason.detail()),
            );
        }
        self.alive(&client, "invoke on the second process");
    }

    /// Connection C is the framing probe. `PluginClient::request` takes a typed `Method` and so
    /// cannot send an unknown method name, which is the one request the engine's own client will
    /// never make and the one a plugin most often gets wrong.
    fn raw_framing(&mut self) {
        let mut connection = match Raw::open(&self.check.launch) {
            Ok(connection) => connection,
            Err(detail) => {
                self.fail("unknown-method-id", format!("the framing probe could not start the plugin: {detail}"));
                self.skip(1);
                return;
            }
        };

        let unknown = format!("{{\"jsonrpc\":\"2.0\",\"id\":{PROBE_ID},\"method\":\"afz/does-not-exist\"}}\n");
        match connection.ask(&unknown, PROBE_ID) {
            None => self.fail(
                "unknown-method-id",
                "an unknown method must be answered with code -32601 and the request id; got no answer at all".to_string(),
            ),
            Some(answer) => {
                let id = answer.get("id").and_then(|id| id.as_u64());
                let code = answer.get("error").and_then(|error| error.get("code")).and_then(|code| code.as_i64());
                if id != Some(PROBE_ID) || code != Some(METHOD_NOT_FOUND) {
                    let shape = match (id, code) {
                        (Some(id), Some(code)) => format!("id {id} with code {code}"),
                        (Some(id), None) => format!("id {id} with no error object"),
                        _ => answer.to_string(),
                    };
                    self.fail(
                        "unknown-method-id",
                        format!("an unknown method must be answered with code -32601 and the request id; got {shape}"),
                    );
                }
            }
        }

        // A plugin that exits on a request it cannot parse, or that writes a blank line, ends the
        // channel for the engine: `read_line` treats an empty line as EOF (`rpc.rs:186-188`).
        connection.write_raw("{not json\n");
        // The bad request itself must be answered with a parse-error frame. The engine drops
        // unparseable lines on both directions (`rpc.rs` `spawn_reader`), so silence here is
        // indistinguishable from a hung plugin in the engine log.
        match connection.next_frame() {
            None => self.fail(
                "framing-intact",
                "the plugin said nothing after an unparseable request; answer it with a -32700 parse-error frame so a refused request is distinguishable from a hung plugin",
            ),
            Some(frame) => {
                let code = frame.get("error").and_then(|error| error.get("code")).and_then(|code| code.as_i64());
                if code != Some(PARSE_ERROR_CODE) {
                    self.fail(
                        "framing-intact",
                        format!("an unparseable request must be answered with code {PARSE_ERROR_CODE}; got {}", compact(&frame)),
                    );
                }
            }
        }
        if connection.ask("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"handshake\"}\n", 1)
            .is_none()
        {
            self.fail(
                "framing-intact",
                "the connection stopped answering after a bad request; a blank line on stdout ends the channel for the engine",
            );
        }
    }

    /// The nonce echo: drive the manifest's probe target once and require the nonce back.
    /// A missing probe symbol is a manifest lie; a probe that answers without the nonce is a
    /// plugin that never reached the service.
    fn lifecycle_probe(&mut self, client: &PluginClient, discovery: &Discovery, spec: &template::Lifecycle) {
        let Some(target) = discovery.targets.iter().find(|target| target.name == spec.probe) else {
            self.fail("lifecycle-nonce", format!("the manifest names \"{}\" as the nonce probe and discovery did not list it", spec.probe));
            return;
        };
        let outcome = self.drive_template_call(client, &target.id, &[], false).map_err(|reason| reason.detail()).and_then(outcome_of);
        let echoed = match &outcome {
            Ok(Outcome::Ok(ok)) => ok.value.as_ref().is_some_and(|value| serde_json::to_string(value).is_ok_and(|text| text.contains(&spec.nonce))),
            _ => false,
        };
        if !echoed {
            let answer = match &outcome {
                Ok(outcome) => format!("answered {} without the nonce", outcome.kind()),
                Err(detail) => format!("could not be driven: {detail}"),
            };
            self.fail("lifecycle-nonce", format!("the probe \"{}\" {answer}; the plugin is not talking to the service this suite started", target.name));
        }
    }

    /// Template mode: judge each property of the embedded template against the plugin's own
    /// rendered fixture. The emitter only named symbols; every verdict here comes from what the
    /// plugin answered over the protocol, so a fixture that lies about what it realizes fails.
    fn template_pass(&mut self, client: &PluginClient, handshake: &Handshake, discovery: &Discovery, manifest: &template::Manifest) {
        let mut results = Vec::new();
        for spec in template::template_v1().properties {
            // `load_manifest` enforces completeness; a hand-built `Manifest` might not. A
            // missing entry is a reported failure, never a panic: the suite reports on plugins.
            let entry = match manifest.properties.get(&spec.id) {
                Some(entry) => entry,
                None => {
                    let detail = format!("the manifest does not account for \"{}\"", spec.id);
                    self.fail(template_rule(&spec.id), detail.clone());
                    results.push(template::PropertyResult { id: spec.id, verdict: template::Verdict::Fail, detail });
                    continue;
                }
            };
            let (verdict, detail) = if let Some(reason) = &entry.not_applicable {
                (template::Verdict::NotApplicable, reason.clone())
            } else {
                match self.judge_property(client, handshake, discovery, &spec.id, entry) {
                    Ok(detail) => (template::Verdict::Pass, detail),
                    Err(detail) => {
                        self.fail(template_rule(&spec.id), detail.clone());
                        (template::Verdict::Fail, detail)
                    }
                }
            };
            results.push(template::PropertyResult { id: spec.id, verdict, detail });
        }
        self.template = Some(template::TemplateReport { version: manifest.template_version, properties: results });
    }

    /// One property's verdict, in the template's order. `Err` is the message the report shows.
    fn judge_property(&mut self, client: &PluginClient, handshake: &Handshake, discovery: &Discovery, id: &str, entry: &template::ManifestProperty) -> Result<String, String> {
        let symbol = entry.symbol.clone().unwrap_or_default();
        let named = || format!("the manifest names \"{symbol}\" for {id}, and discovery did not list it as a fuzzable target");
        let input = entry.input.clone().unwrap_or_default();
        match id {
            "crash-unguarded" => {
                let target = self.named_target(discovery, &symbol).ok_or_else(named)?;
                let outcome = self.drive_template_call(client, &target.id, &input, false).map_err(|reason| reason.detail()).and_then(outcome_of)?;
                let Outcome::Failed(failed) = &outcome else {
                    return Err(format!("\"{symbol}\" answered {} where the template expects failed: the crash fixture did not crash", outcome.kind()));
                };
                let Some(origin) = target.origin.file.as_deref() else {
                    return Err(format!("discovery gave \"{symbol}\" no origin file, so the report cannot check that the frames name the fixture (trap 11)"));
                };
                if !failed.fault.frames.iter().any(|frame| frame.file == origin) {
                    return Err(format!("no fault frame of \"{symbol}\" names the fixture file {origin}: the fault came from outside the target's own code (trap 11)"));
                }
                Ok(format!("failed, and a frame names {origin}"))
            }
            "guard-refusal" => {
                let target = self.named_target(discovery, &symbol).ok_or_else(named)?;
                let outcome = self.drive_template_call(client, &target.id, &input, false).map_err(|reason| reason.detail()).and_then(outcome_of)?;
                let Outcome::Rejected(rejected) = &outcome else {
                    return Err(format!("\"{symbol}\" answered {} where the template expects rejected: the guard did not refuse the out-of-range input", outcome.kind()));
                };
                if rejected.via != "guard-site" && rejected.via != "reject-class" {
                    return Err(format!("\"{symbol}\" refused via \"{}\": the template accepts only guard-site or reject-class", rejected.via));
                }
                Ok(format!("rejected via {}", rejected.via))
            }
            "private-symbol" => self.check_skipped_reason(discovery, &symbol, SkipReason::NotPublic, "a private symbol must never be listed as fuzzable"),
            "unrepresentable-symbol" => self.check_skipped_reason(discovery, &symbol, SkipReason::UnsupportedSignature, "the unrepresentable signature must be reported, not fuzzed blind"),
            "unconstructible-receiver" => {
                self.check_skipped_reason(discovery, &symbol, SkipReason::UnconstructibleParameter, "the unconstructible receiver must be reported, not fuzzed blind")?;
                let skipped = discovery.skipped.iter().find(|s| s.name == symbol).expect("check_skipped_reason found it");
                let want = entry.blocked_by_type.clone().unwrap_or_default();
                let Some(blocked) = skipped.blocked_by.as_ref() else {
                    return Err(format!("\"{symbol}\" was skipped as unconstructible-parameter with no blockedBy: the task board needs the blocking type's name"));
                };
                if blocked.type_ != want {
                    return Err(format!("\"{symbol}\" is blocked by type \"{}\", but the manifest names \"{want}\"", blocked.type_));
                }
                Ok(format!("skipped unconstructible-parameter, blocked by {want}"))
            }
            "method-receiver" => {
                let target = self.named_target(discovery, &symbol).ok_or_else(named)?;
                if self.prepare(client, target, &input) {
                    Ok(format!("prepare built the receiver for \"{symbol}\""))
                } else {
                    Err(format!("prepare skipped \"{symbol}\": the manifest declares its receiver constructible"))
                }
            }
            "coverage-digest-live" => {
                if !handshake.capabilities.contains(&Capability::Coverage) {
                    return Err("the manifest realizes coverage-digest-live, but the handshake does not declare \"coverage\"".to_string());
                }
                let target = self.named_target(discovery, &symbol).ok_or_else(named)?;
                let Some(inputs) = entry.inputs.as_ref() else {
                    return Err(format!("the manifest realizes coverage-digest-live without two inputs for \"{symbol}\""));
                };
                let first = self.drive_template_call(client, &target.id, &inputs[0], true).map_err(|reason| reason.detail()).and_then(outcome_of)?;
                let second = self.drive_template_call(client, &target.id, &inputs[1], true).map_err(|reason| reason.detail()).and_then(outcome_of)?;
                let digest = |outcome: &Outcome| outcome.coverage().filter(|edges| !edges.is_empty()).map(|edges| BTreeSet::from_iter(edges.iter().copied()));
                let (Some(a), Some(b)) = (digest(&first), digest(&second)) else {
                    return Err(format!("\"{symbol}\" answered with an empty or missing coverage digest; collectCoverage was true and the handshake declares coverage (traps 1, 14)"));
                };
                if a == b {
                    return Err(format!("both inputs produced the same {}-edge digest for \"{symbol}\": a probe armed once, or reset before the call, cannot tell two inputs apart (traps 3, 14)", a.len()));
                }
                Ok(format!("the two inputs produced different digests ({} and {} edges)", a.len(), b.len()))
            }
            "stdout-purity" => {
                let target = self.named_target(discovery, &symbol).ok_or_else(named)?;
                self.drive_template_call(client, &target.id, &input, false).map_err(|reason| format!("\"{symbol}\" could not be driven after its stdout write: {}", reason.detail()))?;
                match self.raw(client, Method::Handshake, serde_json::Value::Null, HANDSHAKE_TIMEOUT) {
                    Err(reason) => Err(format!("the connection stopped answering after \"{symbol}\" wrote to stdout: {} (trap 9)", reason.detail())),
                    Ok(raw) => match self.schemas.handshake.mismatch("handshake", &raw) {
                        Some(detail) => Err(format!("the handshake after \"{symbol}\" wrote to stdout is malformed, so target output leaked into the protocol channel: {detail} (trap 9)")),
                        None => Ok("the protocol channel survived the target writing to stdout".to_string()),
                    },
                }
            }
            "reproducer-script" => {
                if !handshake.capabilities.contains(&Capability::Reproducer) {
                    return Err("the manifest realizes reproducer-script, but the handshake does not declare \"reproducer\"".to_string());
                }
                let target = self.named_target(discovery, &symbol).ok_or_else(named)?;
                let request = ReproducerReq { target_id: target.id.clone(), values: input };
                let raw = self.raw(client, Method::Reproducer, to_params(&request), Duration::from_millis(self.check.deadline_ms) + MINIMIZE_SLACK).map_err(|reason| format!("reproducer did not answer: {}", reason.detail()))?;
                let extension = raw.get("extension").and_then(|value| value.as_str()).unwrap_or("");
                let script = raw.get("script").and_then(|value| value.as_str()).unwrap_or("");
                if script.trim().is_empty() {
                    return Err("the reproducer answered an empty script: the finding would arrive with no way to replay it".to_string());
                }
                if !valid_extension(extension) {
                    return Err(format!("the reproducer named extension \"{extension}\": the engine writes it under repros/, so it must be short and alphanumeric (scan.rs:valid_extension)"));
                }
                let Some(origin) = target.origin.file.as_deref() else {
                    return Err(format!("discovery gave \"{symbol}\" no origin file, so the script cannot be checked for naming the fixture"));
                };
                let name = std::path::Path::new(origin).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| origin.to_string());
                if !script.contains(&name) {
                    return Err(format!("the {} script never names the fixture file {name}: it cannot reproduce the fault it claims to replay", extension));
                }
                Ok(format!("a {extension} script that names {name}"))
            }
            "deadline-sleep" => {
                let target = self.named_target(discovery, &symbol).ok_or_else(named)?;
                match self.drive_template_call(client, &target.id, &input, false) {
                    // The engine's kill is the contract's accepted second half: the supervisor
                    // survives it, and the main pass already checked `process-alive` before the
                    // template ran. A self-cut keeps the connection, which is the RECOMMENDED cut.
                    Err(reason) => Ok(format!("the plugin died mid-call at the deadline ({}) and the transport survived; a self-cut with via deadline-cut would keep the connection", reason.detail())),
                    Ok(raw) => {
                        let outcome = outcome_of(raw)?;
                        if outcome.kind() == "failed" && outcome.via() == Some("deadline-cut") {
                            Ok("the harness cut its own call at the deadline".to_string())
                        } else {
                            Err(format!("\"{symbol}\" answered {} inside the {} ms deadline: the sleep fixture is shorter than the deadline, so it never tested the cut", outcome.kind(), self.check.deadline_ms))
                        }
                    }
                }
            }
            other => unreachable!("load_manifest accepts only template v1 property ids, and {other} is not one"),
        }
    }

    /// The discovered target the manifest names, by discovery name.
    fn named_target<'d>(&self, discovery: &'d Discovery, symbol: &str) -> Option<&'d TargetDescriptor> {
        discovery.targets.iter().find(|target| target.name == symbol)
    }

    /// A skipped entry carrying exactly the reason the template expects, or the message that
    /// says what discovery answered instead - including the silent-drop case, where the symbol
    /// appears in neither list (trap 4).
    fn check_skipped_reason(&self, discovery: &Discovery, symbol: &str, want: SkipReason, why: &str) -> Result<String, String> {
        if discovery.targets.iter().any(|target| target.name == symbol) {
            return Err(format!("\"{symbol}\" was discovered as a fuzzable target; {why}"));
        }
        match discovery.skipped.iter().find(|skipped| skipped.name == symbol) {
            Some(skipped) if skipped.reason == want => Ok(format!("skipped {}", want.as_str())),
            Some(skipped) => Err(format!("\"{symbol}\" was skipped for \"{}\", not \"{}\": {why}", skipped.reason.as_str(), want.as_str())),
            None => Err(format!("\"{symbol}\" appears in neither targets nor skipped: discovery dropped it silently (trap 4)")),
        }
    }

    /// One `invoke` for the template pass. These calls deliberately do not feed the rule
    /// counters: the protocol rules were answered by the main pass on the generated values.
    fn drive_template_call(&self, client: &PluginClient, target_id: &str, values: &[ValueTree], collect_coverage: bool) -> Call<serde_json::Value> {
        let request = InvokeReq {
            invocation_id: 0,
            target_id: target_id.to_string(),
            values: values.to_vec(),
            deadline_ms: self.check.deadline_ms,
            collect_coverage,
        };
        self.raw(client, Method::Invoke, to_params(&request), Duration::from_millis(self.check.deadline_ms) + KILL_GRACE)
    }
}

/// Render one unexpected frame briefly for a violation message, without splitting a
/// multi-byte character.
fn compact(frame: &serde_json::Value) -> String {
    let text = frame.to_string();
    if text.chars().count() > 120 {
        let mut head: String = text.chars().take(120).collect();
        head.push('…');
        head
    } else {
        text
    }
}

/// Parse one driven document as an Outcome; the `String` is the message the template report shows.
fn outcome_of(raw: serde_json::Value) -> Result<Outcome, String> {
    serde_json::from_value::<Outcome>(raw).map_err(|error| format!("the plugin answered an unreadable outcome: {error}"))
}

/// The extension policy `scan.rs:valid_extension` applies before writing `repros/`: short and
/// alphanumeric, so the artifact cannot name a path outside the directory.
fn valid_extension(extension: &str) -> bool {
    !extension.is_empty() && extension.len() <= 8 && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The template ids as `'static` rule names. `load_manifest` refuses any id outside the
/// embedded template, so the lookup cannot miss.
fn template_rule(id: &str) -> &'static str {
    const IDS: &[&str] = &[
        "crash-unguarded",
        "guard-refusal",
        "private-symbol",
        "unrepresentable-symbol",
        "unconstructible-receiver",
        "method-receiver",
        "coverage-digest-live",
        "stdout-purity",
        "reproducer-script",
        "deadline-sleep",
    ];
    IDS.iter().copied().find(|known| *known == id).unwrap_or("template-manifest")
}

/// Serialize one request's params. Every request type here is a struct, so this never fails.
fn to_params(request: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(request).expect("request params are serializable")
}

fn wire_materialization(materialization: Materialization) -> String {
    serde_json::to_value(materialization).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "?".to_string())
}

/// One plugin process driven with hand-built lines. `PluginClient` cannot send a method name it
/// does not know, so the two framing rules need their own pipe.
struct Raw {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<Result<String, String>>,
}

impl Raw {
    fn open(launch: &Launch) -> Result<Raw, String> {
        use std::process::{Command, Stdio};
        let Some(program) = launch.argv.first() else {
            return Err("empty harness.command".to_string());
        };
        let mut cmd = Command::new(program);
        cmd.args(launch.argv.iter().skip(1))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Null rather than a pipe: nothing drains this one, and a plugin that writes a lot to
            // stderr would block on a full pipe the probe never reads.
            .stderr(Stdio::null())
            .envs(&launch.env);
        if let Some(cwd) = &launch.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", launch.display()))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("afz-check-framing".to_string())
            .spawn(move || {
                let mut reader = buffered(stdout);
                let mut buf = String::new();
                loop {
                    match read_line(&mut reader, &mut buf) {
                        Ok(Some(line)) => {
                            if tx.send(Ok(line)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => return,
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                            return;
                        }
                    }
                }
            })
            .expect("spawn the framing reader");
        Ok(Raw { child, stdin, lines: rx })
    }

    fn write_raw(&mut self, line: &str) {
        let _ = self.stdin.write_all(line.as_bytes());
        let _ = self.stdin.flush();
    }

    /// The next frame the plugin writes, unfiltered: `None` on silence, EOF, or read error;
    /// a line that is not JSON comes back as a string value so the caller can report it.
    fn next_frame(&mut self) -> Option<serde_json::Value> {
        let until = Instant::now() + HANDSHAKE_TIMEOUT;
        let Ok(line) = self.lines.recv_timeout(until.saturating_duration_since(Instant::now())) else { return None };
        let Ok(line) = line else { return None };
        Some(serde_json::from_str::<serde_json::Value>(&line).unwrap_or(serde_json::Value::String(line)))
    }

    /// Send one line and return the first parsed answer whose `id` matches, skipping anything else
    /// the plugin sent first: a parse-error frame for the previous probe is legitimate traffic.
    fn ask(&mut self, line: &str, id: u64) -> Option<serde_json::Value> {
        self.write_raw(line);
        let until = Instant::now() + HANDSHAKE_TIMEOUT;
        while Instant::now() < until {
            let remaining = until.saturating_duration_since(Instant::now());
            let Ok(Ok(line)) = self.lines.recv_timeout(remaining) else { return None };
            if let Ok(answer) = serde_json::from_str::<serde_json::Value>(&line) {
                if answer.get("id").and_then(|id| id.as_u64()) == Some(id) {
                    return Some(answer);
                }
            }
        }
        None
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
