//! Step 9 acceptance: the scan result document, its schema, and the markdown renderer.
//!
//! The result is `scan.jsonl`: durable lines as they happen, then the `finished` line that
//! carries the document. The document validates against the committed
//! `docs/schema/afz-1/scan-result.json`, and every artifact it names exists. The renderer is
//! given the result's path and must produce
//! a section per finding while the files it points at are unreadable, which is the only way to
//! show it reads nothing but the document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use afz_engine::config::Severity;
use afz_engine::report::render_markdown;
use afz_engine::report::scanresult::{self, *};
use afz_protocol::{
    Blocker, Capability, Materialization, SkipReason, SkippedTarget, TargetClass, ValueNode,
    ValueTree,
};

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("afz-scanresult-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .expect("workspace root")
}

fn committed_schema_path() -> PathBuf {
    repo_root()
        .join("docs/schema")
        .join(afz_protocol::schema_dir(afz_protocol::PROTOCOL_VERSION))
        .join("scan-result.json")
}

fn order_input() -> Vec<ValueTree> {
    vec![ValueTree::new(ValueNode::Object {
        fields: vec![
            (
                "lines".into(),
                ValueTree::new(ValueNode::Array {
                    items: (0..14)
                        .map(|i| {
                            ValueTree::new(ValueNode::Object {
                                fields: vec![
                                    (
                                        "sku".into(),
                                        ValueTree::new(ValueNode::string(format!("SKU-{i:04}"))),
                                    ),
                                    ("qty".into(), ValueTree::number(f64::from(i))),
                                ],
                            })
                        })
                        .collect(),
                }),
            ),
            ("region".into(), ValueTree::string("eu")),
        ],
    })]
}

fn minimized_input() -> Vec<ValueTree> {
    vec![ValueTree::new(ValueNode::Object {
        fields: vec![(
            "lines".into(),
            ValueTree::new(ValueNode::Array { items: vec![] }),
        )],
    })]
}

const FAILED_ID: &str = "afz:f:7a41c2e0";
const DEADLINE_ID: &str = "afz:f:11b0aa99";

fn document() -> ScanDocument {
    ScanDocument {
        schema_version: ScanSchemaVersion::AfzScan1,
        scan_id: "01J8ZQ3M7K".to_string(),
        started_at: "2026-11-09T09:14:02Z".to_string(),
        finished_at: "2026-11-09T10:02:41Z".to_string(),
        engine: EngineInfo {
            version: "0.1.0".to_string(),
            seed: "0x5e3a91c4".to_string(),
            config_hash: "sha256:3f9ac0de33f5a4a9".to_string(),
        },
        target: TargetInfo {
            root: "/home/jdoe/billing-service".to_string(),
            revision: Some("git:8f2c1ab".to_string()),
            harness: HarnessInfo {
                protocol_version: "afz/1".to_string(),
                name: "js-ts".to_string(),
                version: "0.3.1".to_string(),
                target_class: TargetClass::DesignTime,
                capabilities: vec![
                    Capability::Coverage,
                    Capability::Stateful,
                    Capability::Minimize,
                ],
            },
        },
        git: None,
        inventory: Inventory {
            discovered: 412,
            fuzzable: 287,
            skipped: vec![SkippedTarget {
                target_id: "afz:t:9c11f0a4b7d2".to_string(),
                name: "BillingService.sync".to_string(),
                reason: SkipReason::UnconstructibleParameter,
                blocked_by: Some(Blocker {
                    type_: "Db".to_string(),
                    module: Some("src/db.ts".to_string()),
                    path: "receiver.db".to_string(),
                }),
                unlocks_targets: Some(14),
                materialization: Some(Materialization::FixtureRequired),
                detail: Some("fixture-required: Db@src/db.ts".to_string()),
            }],
            materialization_counts: BTreeMap::from([
                (Materialization::Constructible, 287),
                (Materialization::FixtureRequired, 61),
                (Materialization::Opaque, 64),
            ]),
        },
        execution: Execution {
            invocations: 184320,
            corpus_size: 1204,
            corpus_bytes: Some(58_301),
            seeded: 1,
            coverage_edges: 8312,
            coverage_ratio: 0.41,
            deadline_exceeded: 3,
            deadline_cuts: 7,
            stopped_reason: StoppedReason::Budget,
            restarts: 1,
            unattempted: 96,
            disabled_targets: vec!["afz:t:44de0b12c9aa".to_string()],
            samples: vec![
                ProgressSample {
                    invocations: 5000,
                    coverage_edges: 1140,
                    coverage_ratio: 0.056,
                },
                ProgressSample {
                    invocations: 10000,
                    coverage_edges: 2011,
                    coverage_ratio: 0.099,
                },
            ],
            targets: vec![
                TargetStat {
                    target_id: FAILED_ID.to_string(),
                    invocations: 120_004,
                    findings: 1,
                    total_ms: 4_812.5,
                    max_ms: 41.0,
                    novel_edges: 612,
                    deadline_cuts: 5,
                    disabled: false,
                },
                TargetStat {
                    target_id: DEADLINE_ID.to_string(),
                    invocations: 64_316,
                    findings: 1,
                    total_ms: 61_102.0,
                    max_ms: 1_000.0,
                    novel_edges: 1_399,
                    deadline_cuts: 2,
                    disabled: true,
                },
            ],
        },
        findings: vec![
            FindingRecord {
                finding_id: FAILED_ID.to_string(),
                identity: "afz:i:1f7c8a9b4d3e2c1f".to_string(),
                target_id: "afz:t:44de0b12c9aa".to_string(),
                oracle: "failed".to_string(),
                severity: Severity::High,
                via: "thrown".to_string(),
                fault: ScanFault {
                    class: "TypeError".to_string(),
                    message: "Cannot read properties of undefined (reading 'sku')".to_string(),
                    frames: vec![ScanFrame {
                        file: "src/config.ts".to_string(),
                        line: 41,
                        column: Some(19),
                        fn_name: Some("loadConfig".to_string()),
                    }],
                },
                input: InputArtifact {
                    encoding: InputEncoding::ValueTreeJson,
                    artifact: scanresult::input_artifact_path(FAILED_ID),
                },
                minimized_input: Some(MinimizedArtifact {
                    artifact: scanresult::minimized_artifact_path(FAILED_ID),
                    bytes_before: 1840,
                    bytes_after: 116,
                }),
                reproducer: Some(Artifact {
                    artifact: scanresult::reproducer_artifact_path(FAILED_ID, "ts"),
                }),
                first_seen_invocation: 10412,
                reproduced: true,
                seen_before: true,
            },
            FindingRecord {
                finding_id: DEADLINE_ID.to_string(),
                identity: "afz:i:aa0b9c8d7e6f5041".to_string(),
                target_id: "afz:t:77c1de30f00b".to_string(),
                oracle: "deadline".to_string(),
                severity: Severity::Medium,
                via: "deadline".to_string(),
                fault: ScanFault {
                    class: "deadline".to_string(),
                    message: "exceeded 2000 ms".to_string(),
                    frames: vec![],
                },
                input: InputArtifact {
                    encoding: InputEncoding::ValueTreeJson,
                    artifact: scanresult::input_artifact_path(DEADLINE_ID),
                },
                minimized_input: None,
                reproducer: None,
                first_seen_invocation: 158204,
                reproduced: false,
                seen_before: false,
            },
        ],
        findings_suppressed: Some(2),
        artifacts: Artifacts {
            corpus: "corpus/".to_string(),
            coverage: Some("coverage.json".to_string()),
            log: Some("engine.log".to_string()),
        },
    }
}

fn files() -> ScanFiles<'static> {
    ScanFiles {
        findings: vec![
            FindingFiles {
                finding_id: FAILED_ID.to_string(),
                input: order_input(),
                minimized: Some(MinimizedInput {
                    values: minimized_input(),
                    bytes_before: 1840,
                    bytes_after: 116,
                }),
                reproducer: Some(ReproFile {
                    artifact: scanresult::reproducer_artifact_path(FAILED_ID, "ts"),
                    content: "import { loadConfig } from \"../src/config\";\nloadConfig({ lines: [] });\n".to_string(),
                }),
            },
            FindingFiles {
                finding_id: DEADLINE_ID.to_string(),
                input: vec![ValueTree::string("slow")],
                minimized: None,
                reproducer: None,
            },
        ],
        corpus: vec![
            CorpusFile {
                name: "00000001".to_string(),
                target_id: FAILED_ID.to_string(),
                values: order_input(),
                edges: vec![1, 2],
            },
            CorpusFile {
                name: "00000002".to_string(),
                target_id: DEADLINE_ID.to_string(),
                values: vec![ValueTree::number(7.0)],
                edges: vec![],
            },
        ],
        coverage: Some("{\"edges\":8312}"),
        log: Some("invocation 1 ok\n"),
    }
}

fn write_once(name: &str) -> (PathBuf, PathBuf) {
    let root = workdir(name);
    std::fs::create_dir_all(&root).expect("create output dir");
    let dir = scanresult::write(&root, &document(), &files()).expect("write scan");
    (root, dir)
}

/// The result document as JSON: the `finished` line of the directory's journal, which is the
/// result now.
fn finished_value(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join(JOURNAL_FILE)).expect("scan.jsonl");
    let last = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("a finished line");
    serde_json::from_str(last).expect("the finished line is JSON")
}

fn validate(scan_dir: &Path) -> Vec<String> {
    let committed =
        std::fs::read_to_string(committed_schema_path()).expect("committed scan-result schema");
    let schema: serde_json::Value = serde_json::from_str(&committed).expect("schema parses");
    let validator =
        jsonschema::validator_for(&schema).expect("the committed schema is a valid JSON Schema");
    let instance = finished_value(scan_dir);
    validator
        .iter_errors(&instance)
        .map(|e| e.to_string())
        .collect()
}

#[test]
fn written_document_validates_against_the_committed_schema() {
    let (_root, dir) = write_once("validate");
    let errors = validate(&dir);
    assert!(
        errors.is_empty(),
        "the finished line does not validate against {}: {errors:#?}",
        committed_schema_path().display()
    );
    assert!(
        !dir.join(SCAN_FILE).is_file(),
        "the journal is the result: a finished scan writes no scan.json"
    );
    std::fs::remove_dir_all(&_root).ok();
}

/// The `git` object a `--git HEAD` scan of one changed file writes.
fn scoped() -> GitScope {
    GitScope {
        selector: "HEAD".to_string(),
        base: "1f4a9c0d2b7e4a6f8c01d3e5a7b9c0d1e2f3a4b5".to_string(),
        tree: "8f2c1ab4d6e8f0a2b3c5d7e9f1a2b3c4d5e6f7a8".to_string(),
        changed_files: 1,
        worktree: Some("/home/jdoe/billing-service/.autofuzz/git-scope/41-2".to_string()),
    }
}

#[test]
fn a_scoped_document_validates_and_keeps_its_git_object() {
    let mut document = document();
    document.git = Some(scoped());
    let root = workdir("scoped");
    std::fs::create_dir_all(&root).expect("create output dir");
    let dir = scanresult::write(&root, &document, &files()).expect("write a scoped scan");
    let errors = validate(&dir);
    assert!(
        errors.is_empty(),
        "a scoped scan's finished line does not validate: {errors:#?}"
    );
    let read_back = scanresult::read(&dir).expect("read back a scoped scan");
    assert_eq!(
        read_back.git, document.git,
        "the change set a scan covered has to survive the write"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_validator_rejects_a_wrong_schema_version() {
    // The check above only means something if a wrong document actually fails it.
    let (root, dir) = write_once("negative");
    let journal = dir.join(JOURNAL_FILE);
    let lines: Vec<String> = std::fs::read_to_string(&journal)
        .expect("scan.jsonl")
        .lines()
        .map(str::to_string)
        .collect();
    let (head, last) = lines.split_at(lines.len() - 1);
    let mut value: serde_json::Value =
        serde_json::from_str(&last[0]).expect("the finished line is JSON");
    value["schemaVersion"] = serde_json::json!("afz-scan/2");
    let mut rewritten: Vec<String> = head.to_vec();
    rewritten.push(value.to_string());
    std::fs::write(&journal, rewritten.join("\n") + "\n").expect("rewrite the finished line");
    let errors = validate(&dir);
    assert!(
        !errors.is_empty(),
        "a document with schemaVersion afz-scan/2 passed validation"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn committed_schema_is_generated_from_the_document_type() {
    let committed =
        std::fs::read_to_string(committed_schema_path()).expect("committed scan-result schema");
    assert_eq!(
        committed,
        format!("{}\n", scanresult::schema_json()),
        "docs/schema/afz-1/scan-result.json is stale; run `cargo run -p afz-xtask -- schema`"
    );
}

#[test]
fn every_artifact_the_document_names_exists_and_round_trips() {
    let (root, dir) = write_once("artifacts");
    let written = scanresult::read(&dir).expect("read back");
    assert_eq!(
        written,
        document(),
        "the document changed across a write and read"
    );

    let failed = dir.join(input_artifact_path(FAILED_ID));
    let text = std::fs::read_to_string(&failed).expect("input artifact");
    let parsed: Vec<ValueTree> =
        serde_json::from_str(&text).expect("input artifact is a value tree array");
    assert_eq!(parsed, order_input());

    let minimized: Vec<ValueTree> = serde_json::from_str(
        &std::fs::read_to_string(dir.join(minimized_artifact_path(FAILED_ID))).expect("minimized"),
    )
    .expect("minimized artifact is a value tree array");
    assert_eq!(minimized, minimized_input());
    assert!(dir
        .join(reproducer_artifact_path(FAILED_ID, "ts"))
        .is_file());

    // The finding with no minimized input and no reproducer must not leave stray files behind.
    assert!(!dir.join(minimized_artifact_path(DEADLINE_ID)).exists());
    assert!(!dir
        .join(reproducer_artifact_path(DEADLINE_ID, "ts"))
        .exists());

    assert_eq!(
        std::fs::read_to_string(dir.join("coverage.json")).expect("coverage"),
        "{\"edges\":8312}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("engine.log")).expect("log"),
        "invocation 1 ok\n"
    );
    let resumed = scanresult::read_corpus(&dir).expect("read the corpus back for a resume");
    let names: Vec<&str> = resumed.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["00000001", "00000002"],
        "a resume reads every entry, in name order"
    );
    assert_eq!(resumed[1].values, vec![ValueTree::number(7.0)]);
    assert_eq!(
        resumed[0].target_id, FAILED_ID,
        "an entry names the target it belongs to"
    );
    assert_eq!(resumed[0].edges, vec![1, 2], "and the edges it earned");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_second_scan_with_the_same_id_gets_its_own_directory() {
    let root = workdir("collision");
    std::fs::create_dir_all(&root).expect("create output dir");
    let first = scanresult::write(&root, &document(), &files()).expect("first scan");
    let second = scanresult::write(&root, &document(), &files()).expect("second scan");
    assert_eq!(
        first.file_name().map(|n| n.to_string_lossy().to_string()),
        Some("01J8ZQ3M7K".to_string())
    );
    assert_eq!(
        second.file_name().map(|n| n.to_string_lossy().to_string()),
        Some("01J8ZQ3M7K-2".to_string())
    );
    assert_ne!(first, second, "two scans must not overwrite each other");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scan_id_sorts_by_time_and_stays_ten_characters() {
    let early = scanresult::scan_id(
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_000),
    );
    let late = scanresult::scan_id(
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_001),
    );
    assert_eq!(early.len(), 10);
    assert!(
        early[..8] < late[..8],
        "the time part of {early} should precede {late}"
    );
    assert!(early.chars().all(|c| c.is_ascii_alphanumeric()));
}

#[test]
fn markdown_prints_one_section_per_finding() {
    let (root, dir) = write_once("markdown");
    let text = render_markdown::render_file(&dir.join(SCAN_FILE)).expect("render");
    let sections = text.lines().filter(|line| line.starts_with("### ")).count();
    assert_eq!(sections, 2, "one section per finding:\n{text}");

    assert!(
        text.contains("### afz:f:7a41c2e0 — failed (high)"),
        "severity belongs in the heading:\n{text}"
    );
    assert!(
        text.contains("### afz:f:11b0aa99 — deadline (medium)"),
        "the second finding needs its own section:\n{text}"
    );
    assert!(
        text.contains(&minimized_artifact_path(FAILED_ID)),
        "the minimized input belongs in the section:\n{text}"
    );
    assert!(
        text.contains(&reproducer_artifact_path(FAILED_ID, "ts")),
        "the reproducer path belongs in the section:\n{text}"
    );
    assert!(
        text.contains("1840 → 116") || text.contains("116 bytes, from 1840"),
        "byte counts:\n{text}"
    );
    assert!(
        text.contains("src/config.ts:41:19"),
        "the fault frame:\n{text}"
    );
    assert!(
        text.contains("Stopped by budget"),
        "why the run ended:\n{text}"
    );
    assert!(text.contains("41.0%"), "coverage ratio:\n{text}");
    assert!(
        text.contains("Seen before"),
        "the failed finding was reported by an earlier scan:\n{text}"
    );
    assert!(
        text.contains("`Db` in `src/db.ts`"),
        "the skipped target's blocker:\n{text}"
    );
    assert!(
        text.contains("unlocks") && text.contains("14"),
        "one missing type is one task:\n{text}"
    );
    assert!(
        !text.contains("scoped to `"),
        "a whole-tree scan claims nothing about a scope:\n{text}"
    );

    // The deadline finding has neither, and the report says so rather than printing a blank.
    let deadline_section = text
        .split("### afz:f:11b0aa99")
        .nth(1)
        .expect("second section");
    assert!(
        deadline_section.contains("Minimized input: none. No smaller input reproduced the fault."),
        "an absent minimization is stated:\n{deadline_section}"
    );
    assert!(
        deadline_section.contains("Reproducer: none emitted"),
        "an absent reproducer is stated:\n{deadline_section}"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn markdown_notes_two_findings_from_the_same_call() {
    let mut document = document();
    // One call classified by two oracles: the same target, first seen on the same invocation,
    // two fault classes. The report must say the snippets describe one event.
    document.findings[1].target_id = document.findings[0].target_id.clone();
    document.findings[1].first_seen_invocation = document.findings[0].first_seen_invocation;
    let text = render_markdown::render(&document);
    assert!(
        text.contains("- The same call also produced: `afz:f:11b0aa99`"),
        "the failed finding names its sibling:\n{text}"
    );
    assert!(
        text.contains("- The same call also produced: `afz:f:7a41c2e0`"),
        "the deadline finding names its sibling back:\n{text}"
    );
    let notes = text.lines().filter(|line| line.starts_with("- The same call also produced")).count();
    assert_eq!(notes, 2, "one note per sibling, none elsewhere:\n{text}");
}

#[test]
fn markdown_names_the_change_set_a_scan_was_scoped_to() {
    let mut document = document();
    document.git = Some(scoped());
    let text = render_markdown::render(&document);
    assert!(
        text.contains(
            "scoped to `HEAD` against 1f4a9c0d2b7e4a6f8c01d3e5a7b9c0d1e2f3a4b5 (1 changed file(s))"
        ),
        "the report has to name the change set it covers:\n{text}"
    );
}

#[test]
fn renderer_opens_nothing_but_the_document() {
    let (root, dir) = write_once("alone");
    let expected =
        render_markdown::render_file(&dir.join(SCAN_FILE)).expect("render with everything present");

    // Replace every file the document points at with bytes no reader could parse. A renderer that
    // opens any of them now fails or changes its output; this one cannot tell they exist.
    for entry in std::fs::read_dir(dir.join(CORPUS_DIR)).expect("corpus") {
        std::fs::write(
            entry.expect("corpus entry").path(),
            [0xff, 0xfe, 0x00, 0x01],
        )
        .expect("poison corpus");
    }
    for name in ["coverage.json", "engine.log"] {
        std::fs::write(dir.join(name), [0xff, 0xfe, 0x00, 0x01]).expect("poison side file");
    }
    for entry in std::fs::read_dir(dir.join(INPUTS_DIR)).expect("inputs") {
        std::fs::write(entry.expect("input entry").path(), "not json").expect("poison input");
    }
    std::fs::remove_dir_all(dir.join(REPROS_DIR)).expect("remove repros");

    let rendered = render_markdown::render_file(&dir.join(SCAN_FILE))
        .expect("render with artifacts unreadable");
    assert_eq!(
        rendered, expected,
        "the report changed when only the artifacts it points at changed"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// The journal is the scan's durability contract: every line that lands must read back exactly,
/// because a resumed scan rebuilds its corpus and finding identities from nothing else.
#[test]
fn a_journal_round_trips_every_durable_line() {
    let dir = workdir("journal-round");
    std::fs::create_dir_all(&dir).expect("journal directory");
    let lines = vec![
        JournalLine::Started(JournalStarted {
            scan_id: "M7TEST0000".into(),
            started_at: "2026-11-15T00:00:00Z".into(),
            seed: 7,
            invocations: 100,
            wall_clock_ms: 60_000,
            workers: 2,
        }),
        JournalLine::Corpus(CorpusFile {
            name: "c1".into(),
            target_id: "afz:t:0001".into(),
            values: vec![ValueTree::new(ValueNode::string("input"))],
            edges: vec![42],
        }),
        JournalLine::Progress(JournalProgress {
            invocations: 10,
            coverage_edges: 3,
            coverage_total: 9,
            findings: 0,
        }),
    ];
    {
        let mut journal = scanresult::Journal::create(&dir).expect("the journal opens");
        for line in &lines {
            journal.append(line).expect("append");
        }
    }
    let read = scanresult::read_journal(&dir).expect("the journal reads back");
    std::fs::remove_dir_all(&dir).ok();
    assert!(!read.torn_tail, "a whole journal reports no torn tail");
    assert_eq!(read.lines, lines, "every line must read back exactly");
}

/// The failure the journal exists for: the machine dies mid-append. The lines before the cut
/// were fsynced and must survive; the half-written last line is skipped and reported.
#[test]
fn a_torn_last_journal_line_is_skipped_and_reported() {
    use std::io::Write;
    let dir = workdir("journal-torn");
    std::fs::create_dir_all(&dir).expect("journal directory");
    {
        let mut journal = scanresult::Journal::create(&dir).expect("the journal opens");
        journal
            .append(&JournalLine::Corpus(CorpusFile {
                name: "c1".into(),
                target_id: "afz:t:0001".into(),
                values: Vec::new(),
                edges: Vec::new(),
            }))
            .expect("append");
    }
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join(JOURNAL_FILE))
        .expect("append handle")
        .write_all(b"{\"kind\":\"corpus\",\"targetId\":")
        .expect("torn bytes");
    let read = scanresult::read_journal(&dir).expect("a torn tail is tolerated");
    std::fs::remove_dir_all(&dir).ok();
    assert!(read.torn_tail, "the cut line is reported");
    assert_eq!(read.lines.len(), 1, "the fsynced line survives the cut");
}

/// The finished line is what makes a journal a result. A directory whose journal stops short
/// of it is an unfinished run, and reading it as a report would present a half-scan as a scan.
#[test]
fn a_journal_without_a_finished_line_is_an_unfinished_run() {
    let (root, dir) = write_once("unfinished");
    let journal = dir.join(JOURNAL_FILE);
    let text = std::fs::read_to_string(&journal).expect("scan.jsonl");
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("\"kind\":\"finished\""))
        .collect();
    std::fs::write(&journal, format!("{}\n", kept.join("\n"))).expect("drop the finished line");
    let err = scanresult::read(&dir).expect_err("an unfinished journal must not read as a result");
    assert!(err.to_string().contains("did not complete"), "{err}");
    assert!(
        !scanresult::has_result(&dir),
        "an unfinished directory holds no finished result"
    );
    std::fs::remove_dir_all(&root).ok();
}
