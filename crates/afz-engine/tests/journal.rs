//! Crash durability: what a scan leaves behind, and what `--resume` rebuilds from it.
//!
//! The scenario the journal exists for is a machine that dies between the first invocation and
//! finalization: no signal handler runs, so only what was fsynced survives. The test below runs a
//! real scan against the scripted fake plugin, drops the finished line from its `scan.jsonl`
//! to model exactly that death, and resumes from what remains: the corpus and the finding
//! identities must come back
//! from the journal's lines alone.

use std::path::{Path, PathBuf};
use std::time::Duration;

use afz_engine::config::Strategy;
use afz_engine::report::scanresult::{self, JournalLine, JOURNAL_FILE, SCAN_FILE};
use afz_engine::scan::{self, Options};

const FIRST: &str = "afz:t:0001";

/// The fake plugin is a binary of the `xtask` package, so `CARGO_BIN_EXE_…` does not cover it
/// from here; walk every ancestor for it, as the seed tests do.
fn fakeplugin_path() -> PathBuf {
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

fn inventory() -> serde_json::Value {
    serde_json::json!({
        "targets": [{
            "id": FIRST,
            "targetClass": "design-time",
            "origin": { "file": "src/lib.js", "line": 1, "module": "src/lib.js" },
            "name": "entry1",
            "parameters": [{
                "name": "n",
                "type": { "kind": "scalar", "scalar": "number" },
                "required": true,
            }],
            "effects": {},
            "materialization": "constructible",
        }],
        "skipped": [],
    })
}

/// Two invocations earn two distinct edges, so the corpus holds two entries the journal must
/// carry; a third invocation teaches nothing and stays out.
fn script() -> String {
    serde_json::json!({
        "discovery": inventory(),
        "directives": [
            { "at": 1, "coverage": [42] },
            { "at": 2, "coverage": [43] },
            { "at": 3, "coverage": [42] },
        ],
    })
    .to_string()
}

fn project(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("afz-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).expect("scratch directory");
    let config = serde_json::json!({
        "project": name,
        "harness": {
            "name": "afz-fakeplugin",
            "command": [fakeplugin_path().display().to_string()],
            "env": { "AFZ_FAKE_SCRIPT": script() },
        },
        "budget": { "invocations": 3, "wallClockMs": 20_000, "workers": 1 },
        "output": { "dir": ".autofuzz", "renderers": ["json"] },
    });
    std::fs::write(root.join("autofuzz.json"), config.to_string()).expect("scan project");
    root
}

fn options_for(root: &Path) -> Options {
    let loaded =
        afz_engine::config::load(&root.join("autofuzz.json")).expect("the scan project validates");
    let mut options = Options::new(loaded, root.to_path_buf());
    options.out_dir = root.join(".autofuzz");
    options.invocations = 3;
    options.workers = 1;
    options.strategy = Strategy::Random;
    options.wall_clock = Duration::from_secs(20);
    options
}

#[test]
fn a_journal_survives_the_death_of_its_scan_and_resumes_from_it() {
    let root = project("journal-crash");
    let report = scan::run(&options_for(&root), &mut |_| {}).expect("the first scan completes");

    // The journal is the result: the finished line rides the same file as the durable records,
    // last, and it is what makes the directory a finished run.
    let journal = scanresult::read_journal(&report.dir).expect("the finished scan has a journal");
    let corpus_lines = journal
        .lines
        .iter()
        .filter(|line| matches!(line, JournalLine::Corpus(_)))
        .count();
    assert!(
        corpus_lines >= 2,
        "each retained entry is journalled as it is retained, got {corpus_lines}"
    );
    assert!(
        matches!(journal.lines.last(), Some(JournalLine::Finished(_))),
        "the finished line lands last"
    );

    // Model the power loss: the journal keeps every fsynced line and loses the finished record
    // that finalization appended. Without it the directory is an unfinished run.
    let journal_path = report.dir.join(JOURNAL_FILE);
    let journal_text = std::fs::read_to_string(&journal_path).expect("the journal");
    let kept: Vec<&str> = journal_text
        .lines()
        .filter(|line| !line.contains("\"kind\":\"finished\""))
        .collect();
    std::fs::write(&journal_path, format!("{}\n", kept.join("\n"))).expect("drop the finished line");
    assert!(
        scanresult::read(&report.dir).is_err(),
        "a journal without a finished line must not read as a result"
    );

    let mut resume = options_for(&root);
    resume.resume = Some(report.dir.clone());
    let resumed = scan::run(&resume, &mut |_| {}).expect("the resume runs from the journal");
    let log = std::fs::read_to_string(resumed.dir.join("engine.log")).expect("the engine log");
    assert!(
        log.contains("from journal"),
        "the resume must say it rebuilt from the journal: {log}"
    );
    assert!(
        log.contains("2 corpus entr(ies) reloaded"),
        "both journalled entries must come back: {log}"
    );
    assert!(
        scanresult::read(&resumed.dir).is_ok(),
        "the resumed scan lands its own finished record"
    );
    assert!(
        !resumed.dir.join(SCAN_FILE).is_file(),
        "the journal is the result: no scan.json is written"
    );
    std::fs::remove_dir_all(&root).ok();
}
