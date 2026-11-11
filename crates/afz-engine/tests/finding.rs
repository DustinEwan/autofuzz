//! Finding identity and the record of what a project has already reported.
//!
//! The identity rule is what makes a triage queue work: two inputs that reach the same line are
//! one finding, and a scan that has already reported it says so instead of listing it again.

use std::path::PathBuf;

use afz_engine::finding::{finding_id, identity, Seen, FINDING_PREFIX, IDENTITY_PREFIX};
use afz_protocol::{FaultFrame, FaultSignature};

const TARGET: &str = "afz:t:44be";

fn fault(class: &str, message: &str, file: &str, line: u32) -> FaultSignature {
    FaultSignature {
        class: class.to_string(),
        message: message.to_string(),
        frames: vec![FaultFrame {
            file: file.to_string(),
            line,
            column: Some(19),
            fn_name: Some("normalize".to_string()),
        }],
    }
}

fn frameless(class: &str) -> FaultSignature {
    FaultSignature { class: class.to_string(), message: "trap".to_string(), frames: Vec::new() }
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("afz-finding-{}-{}", name, std::process::id()))
}

#[test]
fn two_inputs_that_reach_the_same_line_are_one_finding() {
    let first = identity(TARGET, &fault("TypeError", "Cannot read properties of undefined (reading 'map')", "src/config.ts", 88));
    let second = identity(TARGET, &fault("TypeError", "Cannot read properties of undefined (reading 'join')", "src/config.ts", 88));
    assert_eq!(first, second, "the message is not part of the identity, or every input gets its own finding");
}

#[test]
fn identity_separates_what_a_triager_would_separate() {
    let base = identity(TARGET, &fault("TypeError", "boom", "src/config.ts", 88));
    let other_target = identity("afz:t:9c1f", &fault("TypeError", "boom", "src/config.ts", 88));
    let other_class = identity(TARGET, &fault("RangeError", "boom", "src/config.ts", 88));
    let other_file = identity(TARGET, &fault("TypeError", "boom", "src/other.ts", 88));
    let other_line = identity(TARGET, &fault("TypeError", "boom", "src/config.ts", 89));
    let all = [base, other_target, other_class, other_file, other_line];
    let unique: std::collections::BTreeSet<&String> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "each difference must produce a different identity: {all:?}");
}

#[test]
fn identity_is_the_prefix_and_sixteen_hex_characters() {
    let id = identity(TARGET, &fault("TypeError", "boom", "src/config.ts", 88));
    let hex = id.strip_prefix(IDENTITY_PREFIX).unwrap_or_else(|| panic!("identity {id} lacks the {IDENTITY_PREFIX} prefix"));
    assert_eq!(hex.len(), 16, "{id}");
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
}

#[test]
fn a_fault_with_no_frame_still_deduplicates() {
    let first = identity(TARGET, &frameless("RuntimeError"));
    assert_eq!(first, identity(TARGET, &frameless("RuntimeError")));
    assert_ne!(first, identity(TARGET, &frameless("OutOfMemory")));
}

#[test]
fn finding_id_is_derived_from_the_identity() {
    let id = identity(TARGET, &fault("TypeError", "boom", "src/config.ts", 88));
    let finding = finding_id(&id);
    let hex = finding.strip_prefix(FINDING_PREFIX).unwrap_or_else(|| panic!("{finding} lacks the {FINDING_PREFIX} prefix"));
    assert_eq!(hex.len(), 8, "{finding}");
    assert_eq!(finding, finding_id(&id), "the same identity must keep the same finding id across scans");
    assert_ne!(finding, finding_id(&identity(TARGET, &fault("TypeError", "boom", "src/config.ts", 89))));
}

#[test]
fn a_second_scan_of_the_same_tree_marks_the_identity_seen() {
    let dir = temp("second-scan");
    let path = dir.join("seen.json");
    let _ = std::fs::remove_dir_all(&dir);

    let id = identity(TARGET, &fault("TypeError", "boom", "src/config.ts", 88));

    // The first scan: nothing is known yet, so the finding is new and gets recorded.
    let mut first = Seen::open(&path).expect("a missing history is an empty one, not an error");
    assert!(first.is_empty());
    assert!(first.record(&id), "the first scan must report the finding");
    assert!(!first.record(&id), "the same identity twice in one scan is one finding");
    assert!(!first.contains("afz:i:0000000000000000"));
    first.save().expect("write the history");

    // The second scan over the same tree: the identity comes back from the file.
    let mut second = Seen::open(&path).expect("read the history the first scan wrote");
    assert_eq!(second.len(), 1);
    assert!(second.contains(&id), "the second scan must mark the finding seenBefore");
    assert!(!second.record(&id), "and must not list it as new");

    std::fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_history_that_does_not_parse_is_an_error_not_an_empty_history() {
    let dir = temp("corrupt");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("seen.json");
    std::fs::write(&path, "not json").expect("write a broken history");
    let error = Seen::open(&path).expect_err("silently forgetting the history would re-announce every finding");
    assert!(error.to_string().contains("seen.json"), "{error}");
    std::fs::remove_dir_all(&dir).expect("clean up");
}
