//! Caller-supplied seeds and the per-target rows that report them.
//!
//! A seed is measured, not trusted: `Seed` carries no edge list, so the digest recorded for it is
//! the one the plugin answered when the input actually ran. The three cases below are the whole
//! contract - a seed that earns an edge is retained, a seed naming a target discovery never
//! reported is dropped with a log line, and a seed that teaches nothing stays out of the corpus -
//! and the third is also where `execution.targets` gets its zeroed row for a target that never
//! taught the search anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use afz_engine::config::Strategy;
use afz_engine::scan::{self, Options, Seed};

const FIRST: &str = "afz:t:0001";
const SECOND: &str = "afz:t:0002";

/// The fake plugin is a binary of the `xtask` package, so `CARGO_BIN_EXE_…` does not cover it from
/// here.
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
            panic!(
                "build the fake plugin: `cargo build --workspace` (no afz-fakeplugin above {})",
                exe.display()
            );
        };
        dir = parent;
    }
}

fn target(id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "targetClass": "design-time",
        "origin": { "file": "src/lib.js", "line": 1, "module": "src/lib.js" },
        "name": name,
        "parameters": [{
            "name": "n",
            "type": { "kind": "scalar", "scalar": "number" },
            "required": true,
        }],
        "effects": {},
        "materialization": "constructible",
    })
}

/// Two targets, so a test can name one and leave the other uninvoked.
fn inventory() -> serde_json::Value {
    serde_json::json!({
        "targets": [target(FIRST, "entry1"), target(SECOND, "entry2")],
        "skipped": [],
    })
}

/// The script these scans run: the two-target inventory plus the invocation directives.
fn script(directives: serde_json::Value) -> String {
    serde_json::json!({ "discovery": inventory(), "directives": directives }).to_string()
}

/// A scratch scan project whose plugin is scripted through the environment. The directory name is
/// unique per test so two runs cannot share an output directory.
fn project(name: &str, script: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("afz-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).expect("scratch directory");
    let config = serde_json::json!({
        "project": name,
        "harness": {
            "name": "afz-fakeplugin",
            "command": [fakeplugin_path().display().to_string()],
            "env": { "AFZ_FAKE_SCRIPT": script },
        },
        "budget": { "invocations": 5, "wallClockMs": 20_000, "workers": 1 },
        "output": { "dir": ".autofuzz", "renderers": ["json"] },
    });
    std::fs::write(root.join("autofuzz.json"), config.to_string()).expect("scan project");
    root
}

/// The options every case shares: one worker, so invocation 1 is the seed's invocation in the
/// plugin's own per-process counter, and an output directory inside the scratch project.
fn options_for(root: &Path, invocations: u64, seeds: Vec<Seed>) -> Options {
    let loaded =
        afz_engine::config::load(&root.join("autofuzz.json")).expect("the scan project validates");
    let mut options = Options::new(loaded, root.to_path_buf());
    options.out_dir = root.join(".autofuzz");
    options.invocations = invocations;
    options.workers = 1;
    options.strategy = Strategy::Random;
    options.wall_clock = Duration::from_secs(20);
    options.seeds = seeds;
    options
}

fn engine_log(report: &scan::ScanReport) -> String {
    std::fs::read_to_string(report.dir.join("engine.log")).expect("the engine writes its log")
}

/// The seed is the scan's first invocation, and the directive gives it an edge its own digest
/// would not have produced, so the corpus entry it holds is the caller's input rather than
/// something the generator drew.
#[test]
fn a_seed_that_earns_an_edge_enters_the_corpus() {
    let root = project(
        "seeds-earned",
        &script(serde_json::json!([{"at": 1, "coverage": [42]}])),
    );
    let options = options_for(
        &root,
        5,
        vec![Seed {
            target_id: FIRST.to_string(),
            values: vec![],
        }],
    );

    let report = scan::run(&options, &mut |_| {}).expect("the scan completes");
    let log = engine_log(&report);
    std::fs::remove_dir_all(&root).ok();

    let execution = &report.document.execution;
    assert_eq!(execution.seeded, 1, "one seed was run: {execution:?}");
    assert!(
        execution.corpus_size >= 1,
        "the seed earned an edge, so it is retained: {execution:?}"
    );
    assert!(
        log.contains("seeded 1 input(s): 1 retained"),
        "the log counts what it ran: {log}"
    );
    let row = execution
        .targets
        .iter()
        .find(|stat| stat.target_id == FIRST)
        .expect("a row for the seeded target");
    assert!(
        row.invocations >= 1,
        "the seed's invocation is charged to its target: {row:?}"
    );
    // `totalMs` and `maxMs` are deliberately not asserted: the fake plugin answers in well under a
    // millisecond, so a `> 0.0` assertion would depend on the clock's granularity.
    assert!(
        row.novel_edges >= 1,
        "the edge the seed earned is credited to it: {row:?}"
    );
}

/// One mistyped id must not cost the caller the run: the seed is skipped, the scan goes ahead, and
/// the engine log says which seed went nowhere.
#[test]
fn a_seed_naming_an_unknown_target_is_dropped_not_run() {
    let root = project("seeds-unknown", &script(serde_json::json!([])));
    let options = options_for(
        &root,
        5,
        vec![Seed {
            target_id: "afz:t:nope".to_string(),
            values: vec![],
        }],
    );

    let report = scan::run(&options, &mut |_| {}).expect("the scan completes");
    let log = engine_log(&report);
    std::fs::remove_dir_all(&root).ok();

    let execution = &report.document.execution;
    assert_eq!(
        execution.seeded, 0,
        "a seed naming no known target never ran: {execution:?}"
    );
    assert_eq!(
        execution.invocations, 5,
        "the budget the seed did not spend still ran: {execution:?}"
    );
    assert!(
        log.contains("names unknown target afz:t:nope"),
        "the dropped seed is named in the log: {log}"
    );
    assert!(
        log.contains("0 retained, 1 named an unknown target"),
        "the summary counts the drop: {log}"
    );
}

/// `Corpus::record` keeps an input only when it adds an edge the corpus has not produced, and that
/// rule governs a hand-written seed exactly as it governs a generated one: an input that reaches
/// nothing new is not kept just because the caller wrote it.
#[test]
fn a_seed_that_teaches_nothing_is_not_retained() {
    // The directive answers the seed's invocation with an empty digest. Without it the fake would
    // attach its default one-edge digest, which a first invocation would find novel.
    let root = project(
        "seeds-untaught",
        &script(serde_json::json!([{"at": 1, "coverage": []}])),
    );
    let options = options_for(
        &root,
        1,
        vec![Seed {
            target_id: FIRST.to_string(),
            values: vec![],
        }],
    );

    let report = scan::run(&options, &mut |_| {}).expect("the scan completes");
    let log = engine_log(&report);
    std::fs::remove_dir_all(&root).ok();

    let execution = &report.document.execution;
    assert_eq!(execution.seeded, 1, "the seed ran: {execution:?}");
    assert_eq!(
        execution.corpus_size, 0,
        "an input with no new edge is not retained: {execution:?}"
    );
    assert!(
        log.contains("seeded 1 input(s): 0 retained"),
        "the log says the seed taught nothing: {log}"
    );
    let row = execution
        .targets
        .iter()
        .find(|stat| stat.target_id == FIRST)
        .expect("a row for the seeded target");
    assert_eq!(row.novel_edges, 0, "{row:?}");
    let untouched = execution
        .targets
        .iter()
        .find(|stat| stat.target_id == SECOND)
        .expect("a row for the target the scan never invoked");
    assert_eq!(
        untouched.invocations, 0,
        "the seed consumed the one-invocation budget: {untouched:?}"
    );
}
