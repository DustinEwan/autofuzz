//! The template contract itself: the embedded v1 is well-formed and complete, and the manifest
//! loader accepts every realization form and refuses the dishonest ones. These tests run without
//! a plugin; the semantic pass over a rendered fixture is gated by the harness tests in
//! `crates/xtask/tests/conformance.rs`.

use std::fs;

use afz_conformance::template::{Verdict, load_manifest, template_v1};

/// The property ids template v1 publishes, in file order. Adding, removing, or renaming one
/// here means editing a published version, which the immutability policy forbids: ship v2.
const V1_IDS: &[&str] = &[
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

#[test]
fn template_v1_is_complete_and_wire_level() {
    let template = template_v1();
    assert_eq!(template.version, 1);
    let ids: Vec<&str> = template.properties.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, V1_IDS, "template v1 is immutable: this list is the published contract");
    for spec in &template.properties {
        assert!(!spec.expectation.is_empty(), "{} carries no expectation", spec.id);
        assert!(!spec.emitter_notes.is_empty(), "{} carries no emitter notes", spec.id);
        // The template states outcomes, not code shape: every expectation names a wire token.
        let wire_named = ["failed", "rejected", "skipped", "ok", "unconstructible-parameter", "coverage", "handshake", "prepare", "reproducer"]
            .iter()
            .any(|token| spec.expectation.contains(token));
        assert!(wire_named, "{} must state a wire-level expectation, not a code shape", spec.id);
    }
}

/// A minimal realization for every property, as a hand-written manifest would carry it. The
/// pricing fixture's manifest (written by the python emitter) has this same shape.
fn full_manifest() -> serde_json::Value {
    let value = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap();
    let int = |n: i64| value(&format!("{{\"node\":\"number\",\"value\":{n},\"provenance\":\"generated\"}}"));
    serde_json::json!({
        "templateVersion": 1,
        "properties": {
            "crash-unguarded": {"symbol": "tiered_price", "input": [int(0)]},
            "guard-refusal": {"symbol": "apply_discount", "input": [int(-5)]},
            "private-symbol": {"symbol": "_cache_key"},
            "unrepresentable-symbol": {"symbol": "untyped"},
            "unconstructible-receiver": {"symbol": "post", "blockedByType": "Ledger"},
            "method-receiver": {"symbol": "Invoice.add_line", "input": [int(1)]},
            "coverage-digest-live": {"symbol": "tiered_price", "inputs": [[int(1)], [int(99)]]},
            "stdout-purity": {"symbol": "noisy"},
            "reproducer-script": {"symbol": "tiered_price", "input": [int(0)]},
            "deadline-sleep": {"symbol": "spin", "input": [int(1)]}
        }
    })
}

/// Write a manifest to a private temp file and load it the way the CLIs do. The counter keeps
/// concurrent tests from sharing a directory.
fn through_loader(document: &serde_json::Value) -> Result<afz_conformance::template::Manifest, String> {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("afz-manifest-test-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("afz-conformance-manifest.json");
    fs::write(&file, serde_json::to_vec_pretty(document).unwrap()).unwrap();
    let loaded = load_manifest(&file);
    let _ = fs::remove_dir_all(&dir);
    loaded
}

#[test]
fn a_complete_manifest_loads() {
    let manifest = through_loader(&full_manifest()).expect("the hand-written manifest loads");
    assert_eq!(manifest.template_version, 1);
    assert_eq!(manifest.properties.len(), V1_IDS.len());
    assert!(manifest.lifecycle.is_none(), "an in-process harness carries no lifecycle");
}

#[test]
fn a_not_applicable_entry_needs_no_realization() {
    let mut document = full_manifest();
    document["properties"]["coverage-digest-live"] = serde_json::json!({"notApplicable": "the runtime cannot observe per-call edges"});
    let manifest = through_loader(&document).expect("a justified absence loads");
    let entry = &manifest.properties["coverage-digest-live"];
    assert!(entry.symbol.is_none());
    assert_eq!(entry.not_applicable.as_deref(), Some("the runtime cannot observe per-call edges"));
}

#[test]
fn a_wrong_template_version_is_refused() {
    let mut document = full_manifest();
    document["templateVersion"] = serde_json::json!(2);
    let error = through_loader(&document).unwrap_err();
    assert!(error.contains("templateVersion 2"), "{error}");
}

#[test]
fn a_missing_property_is_refused() {
    let mut document = full_manifest();
    document["properties"].as_object_mut().unwrap().remove("stdout-purity");
    let error = through_loader(&document).unwrap_err();
    assert!(error.contains("does not account for template property \"stdout-purity\""), "{error}");
}

#[test]
fn an_unknown_property_is_refused() {
    let mut document = full_manifest();
    document["properties"]["timing-attack-resistant"] = serde_json::json!({"symbol": "hmac_verify"});
    let error = through_loader(&document).unwrap_err();
    assert!(error.contains("immutable"), "{error}");
}

#[test]
fn a_realization_without_its_inputs_is_refused() {
    let mut document = full_manifest();
    document["properties"]["crash-unguarded"] = serde_json::json!({"symbol": "tiered_price"});
    let error = through_loader(&document).unwrap_err();
    assert!(error.contains("missing input"), "{error}");
}

#[test]
fn verdicts_count_separately() {
    let report = afz_conformance::template::TemplateReport {
        version: 1,
        properties: V1_IDS
            .iter()
            .map(|id| afz_conformance::template::PropertyResult {
                id: (*id).to_string(),
                verdict: if *id == "stdout-purity" { Verdict::NotApplicable } else { Verdict::Pass },
                detail: String::new(),
            })
            .collect(),
    };
    assert_eq!(report.passed(), V1_IDS.len() - 1);
    assert_eq!(report.failed(), 0);
    assert_eq!(report.not_applicable(), 1);
}

/// Build a `TemplateReport` from `(id, verdict)` pairs, the shape the driver parses out of
/// the project's own fixture validator.
fn report(pairs: &[(&str, &str)]) -> afz_conformance::template::TemplateReport {
    afz_conformance::template::TemplateReport {
        version: 1,
        properties: pairs
            .iter()
            .map(|(id, verdict)| afz_conformance::template::PropertyResult {
                id: (*id).to_string(),
                verdict: serde_json::from_str(&format!("\"{verdict}\"")).expect("a template verdict spelling"),
                detail: format!("{id}: {verdict}"),
            })
            .collect(),
    }
}

fn fixture_all_pass() -> afz_conformance::template::TemplateReport {
    report(&afz_conformance::template::FIXTURE_SHAPED.iter().map(|id| (*id, "pass")).collect::<Vec<_>>())
}

fn protocol_all_pass() -> afz_conformance::template::TemplateReport {
    report(&V1_IDS.iter().map(|id| (*id, "pass")).collect::<Vec<_>>())
}

#[test]
fn agreeing_verdicts_compare_clean() {
    assert!(afz_conformance::compare_validator(&fixture_all_pass(), &protocol_all_pass()).is_empty());
}

#[test]
fn a_version_mismatch_is_named_before_anything_else() {
    let mut fixture = fixture_all_pass();
    fixture.version = 2;
    let violations = afz_conformance::compare_validator(&fixture, &protocol_all_pass());
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert_eq!(violations[0].detail, "the fixture validator targets template v2, the suite targets v1");
}

#[test]
fn a_process_property_in_the_validator_is_refused() {
    let mut pairs: Vec<(&str, &str)> = afz_conformance::template::FIXTURE_SHAPED.iter().map(|id| (*id, "pass")).collect();
    pairs.push(("deadline-sleep", "pass"));
    let violations = afz_conformance::compare_validator(&report(&pairs), &protocol_all_pass());
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].detail.contains("deadline-sleep") && violations[0].detail.contains("process property"),
        "{}",
        violations[0].detail
    );
}

#[test]
fn an_omitted_fixture_property_is_a_violation() {
    let pairs: Vec<(&str, &str)> = afz_conformance::template::FIXTURE_SHAPED
        .iter()
        .filter(|id| **id != "coverage-digest-live")
        .map(|id| (*id, "pass"))
        .collect();
    let violations = afz_conformance::compare_validator(&report(&pairs), &protocol_all_pass());
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert_eq!(violations[0].detail, "the fixture validator does not report coverage-digest-live");
}

#[test]
fn a_doubly_reported_property_is_a_violation() {
    let mut pairs: Vec<(&str, &str)> = afz_conformance::template::FIXTURE_SHAPED.iter().map(|id| (*id, "pass")).collect();
    pairs.push(("crash-unguarded", "pass"));
    let violations = afz_conformance::compare_validator(&report(&pairs), &protocol_all_pass());
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert_eq!(violations[0].detail, "the fixture validator reports crash-unguarded twice");
}

#[test]
fn a_quiet_validator_does_not_upgrade_a_plugin_claim() {
    // The failure Step 4b exists for: the plugin says the crash fixture crashed and the
    // validator declines to check. `not-applicable` against `pass` is a disagreement.
    let mut pairs: Vec<(&str, &str)> = afz_conformance::template::FIXTURE_SHAPED.iter().map(|id| (*id, "pass")).collect();
    pairs[0] = ("crash-unguarded", "not-applicable");
    let violations = afz_conformance::compare_validator(&report(&pairs), &protocol_all_pass());
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert_eq!(violations[0].rule, "fixture-validator");
    assert_eq!(
        violations[0].detail,
        "the fixture validator says not-applicable for crash-unguarded and the plugin says pass: crash-unguarded: not-applicable"
    );
}
