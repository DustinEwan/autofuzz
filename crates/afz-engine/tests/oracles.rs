//! The oracle decision table, on outcomes built by hand.
//!
//! `oracle_targets.rs` proves the plugin produces these outcomes; this file proves what the
//! engine makes of them. Together they are Step 7: an outcome is a finding, a silence, or one of
//! exactly four oracles, and the `via` the plugin reported decides which.

use std::time::Duration;

use afz_engine::config::{OracleConfig, OracleOverride, OraclesConfig, Severity};
use afz_engine::oracles::{Finding, ENVIRONMENT_VIA, HARNESS_VIA, Oracle, Oracles, OVERRIDE_CLASS};
use afz_protocol::{FailedOutcome, FaultSignature, OkOutcome, Outcome, RejectedOutcome};

const TARGET: &str = "afz:t:guard";

fn ok(duration_ms: f64, memory_bytes: Option<u64>) -> Outcome {
    Outcome::Ok(OkOutcome { duration_ms, memory_bytes, coverage: None, value: None })
}

fn rejected(via: &str, detail: &str) -> Outcome {
    Outcome::Rejected(RejectedOutcome {
        duration_ms: 1.0,
        coverage: None,
        via: via.to_string(),
        detail: detail.to_string(),
    })
}

fn failed(via: &str, class: &str, message: &str) -> Outcome {
    Outcome::Failed(FailedOutcome {
        duration_ms: 5.0,
        coverage: None,
        via: via.to_string(),
        fault: FaultSignature {
            class: class.to_string(),
            message: message.to_string(),
            frames: Vec::new(),
        },
    })
}

fn evaluate(outcome: &Outcome, config: &OraclesConfig) -> Vec<Finding> {
    Oracles::new(config).evaluate(TARGET, 1, outcome)
}

fn oracles_of(findings: &[Finding]) -> Vec<&'static str> {
    findings.iter().map(|f| f.oracle.as_str()).collect()
}

#[test]
fn defaults_are_the_severities_part_i_names() {
    let engine = Oracles::new(&OraclesConfig::default());
    assert_eq!(engine.config_for(Oracle::Failed).severity, Severity::High);
    assert_eq!(engine.config_for(Oracle::Rejected).severity, Severity::Off);
    assert_eq!(engine.config_for(Oracle::Deadline).severity, Severity::Medium);
    assert_eq!(engine.config_for(Oracle::Deadline).ms, Some(2000));
    assert_eq!(engine.config_for(Oracle::MemoryGrowth).severity, Severity::Medium);
    assert_eq!(engine.config_for(Oracle::MemoryGrowth).mb, Some(256));
    assert_eq!(engine.config_for(Oracle::ProcessExit).severity, Severity::High);
    assert_eq!(engine.invocation_deadline(), Some(Duration::from_millis(2000)));
}

#[test]
fn a_guard_site_rejection_is_not_a_finding() {
    let outcome = rejected("guard-site", "RangeError: rate must be positive");
    assert!(evaluate(&outcome, &OraclesConfig::default()).is_empty());

    // The audit switch: turning `rejected` on lists it, at the severity the user chose.
    let mut config = OraclesConfig::default();
    config.rejected = OracleConfig { severity: Severity::Low, ms: None, mb: None };
    let findings = evaluate(&outcome, &config);
    assert_eq!(oracles_of(&findings), vec!["rejected"]);
    assert_eq!(findings[0].severity, Severity::Low);
    assert_eq!(findings[0].summary, "RangeError: rate must be positive");
}

#[test]
fn an_unguarded_type_error_is_a_failed_finding() {
    let outcome = failed(
        "engine-error-outside-declared-channel",
        "TypeError",
        "Cannot read properties of undefined (reading 'map')",
    );
    let findings = evaluate(&outcome, &OraclesConfig::default());
    assert_eq!(oracles_of(&findings), vec!["failed"]);
    assert_eq!(findings[0].severity, Severity::High);
    assert_eq!(findings[0].via, "engine-error-outside-declared-channel");
    assert_eq!(findings[0].fault.as_ref().expect("fault").class, "TypeError");
    assert_eq!(findings[0].target_id, TARGET);
    assert_eq!(findings[0].invocation_id, 1);
}

#[test]
fn an_unhandled_rejection_is_a_finding_too() {
    let outcome = failed("unhandled-rejection", "Error", "db connection lost");
    assert_eq!(oracles_of(&evaluate(&outcome, &OraclesConfig::default())), vec!["failed"]);
}

#[test]
fn a_missed_deadline_is_reported_once_and_not_as_a_failure() {
    let outcome = failed("deadline", "Deadline", "plugin did not answer before the deadline");
    let findings = evaluate(&outcome, &OraclesConfig::default());
    assert_eq!(oracles_of(&findings), vec!["deadline"]);
    assert_eq!(findings[0].severity, Severity::Medium);
    assert!(findings[0].summary.contains("deadline"), "{}", findings[0].summary);
}

#[test]
fn a_slow_ok_outcome_trips_the_deadline_oracle() {
    let findings = evaluate(&ok(2500.0, None), &OraclesConfig::default());
    assert_eq!(oracles_of(&findings), vec!["deadline"]);
    assert!(findings[0].summary.contains("2500"), "{}", findings[0].summary);
}

#[test]
fn turning_the_deadline_oracle_off_removes_the_engine_deadline() {
    let mut config = OraclesConfig::default();
    config.deadline = OracleConfig { severity: Severity::Off, ms: Some(2000), mb: None };
    let engine = Oracles::new(&config);
    // The supervisor reads this as "let the call run", so a slow target is not a finding either.
    assert_eq!(engine.invocation_deadline(), None);
    assert!(engine.evaluate(TARGET, 1, &ok(60_000.0, None)).is_empty());
}

#[test]
fn memory_growth_is_measured_against_the_configured_megabytes() {
    let mib = 1024 * 1024;
    let findings = evaluate(&ok(50.0, Some(300 * mib)), &OraclesConfig::default());
    assert_eq!(oracles_of(&findings), vec!["memoryGrowth"]);
    assert_eq!(findings[0].severity, Severity::Medium);
    assert!(findings[0].summary.contains("300 MB"), "{}", findings[0].summary);

    assert!(evaluate(&ok(50.0, Some(100 * mib)), &OraclesConfig::default()).is_empty());
    // A plugin that reports no number never trips the oracle; the engine does not guess one.
    assert!(evaluate(&ok(50.0, None), &OraclesConfig::default()).is_empty());
}

#[test]
fn the_environment_channels_belong_to_process_exit() {
    for via in ENVIRONMENT_VIA {
        let outcome = failed(via, "ProcessExit", "plugin exited with code 1");
        let findings = evaluate(&outcome, &OraclesConfig::default());
        assert_eq!(oracles_of(&findings), vec!["processExit"], "via {via}");
        assert_eq!(findings[0].severity, Severity::High);
    }
}

#[test]
fn harness_channels_are_run_errors_not_findings() {
    for via in HARNESS_VIA {
        let outcome = failed(via, "MaterializationBlocked", "opaque node with no fixture");
        assert!(evaluate(&outcome, &OraclesConfig::default()).is_empty(), "via {via}");
    }
}

#[test]
fn an_override_can_make_a_declared_channel_a_failure() {
    let mut config = OraclesConfig::default();
    config.overrides = vec![OracleOverride {
        target: None,
        via: "guard-site".to_string(),
        treat_as: "failed".to_string(),
    }];
    let findings = evaluate(&rejected("guard-site", "RangeError: bad rate"), &config);
    assert_eq!(oracles_of(&findings), vec!["failed"]);
    let fault = findings[0].fault.as_ref().expect("synthesised fault");
    assert_eq!(fault.class, OVERRIDE_CLASS);
    assert!(fault.message.contains("guard-site"), "{}", fault.message);
}

#[test]
fn an_override_named_for_one_target_leaves_the_others_alone() {
    let mut config = OraclesConfig::default();
    config.overrides = vec![OracleOverride {
        target: Some("afz:t:other".to_string()),
        via: "guard-site".to_string(),
        treat_as: "failed".to_string(),
    }];
    let engine = Oracles::new(&config);
    assert!(engine.evaluate(TARGET, 1, &rejected("guard-site", "RangeError")).is_empty());
    assert_eq!(engine.evaluate("afz:t:other", 1, &rejected("guard-site", "RangeError")).len(), 1);
}

#[test]
fn an_override_can_silence_a_channel_entirely() {
    let mut config = OraclesConfig::default();
    config.overrides = vec![OracleOverride {
        target: None,
        via: "engine-error-outside-declared-channel".to_string(),
        treat_as: "ok".to_string(),
    }];
    let outcome = failed("engine-error-outside-declared-channel", "TypeError", "nope");
    assert!(evaluate(&outcome, &config).is_empty());
}

#[test]
fn an_override_to_rejected_needs_the_rejected_oracle_enabled() {
    let mut config = OraclesConfig::default();
    config.overrides = vec![OracleOverride {
        target: None,
        via: "unhandled-rejection".to_string(),
        treat_as: "rejected".to_string(),
    }];
    let outcome = failed("unhandled-rejection", "Error", "db connection lost");
    assert!(evaluate(&outcome, &config).is_empty());

    config.rejected = OracleConfig { severity: Severity::Medium, ms: None, mb: None };
    let findings = evaluate(&outcome, &config);
    assert_eq!(oracles_of(&findings), vec!["rejected"]);
    assert!(findings[0].summary.contains("Error: db connection lost"), "{}", findings[0].summary);
}

#[test]
fn the_first_matching_override_wins() {
    let mut config = OraclesConfig::default();
    config.rejected = OracleConfig { severity: Severity::Low, ms: None, mb: None };
    config.overrides = vec![
        OracleOverride { target: None, via: "guard-site".to_string(), treat_as: "ok".to_string() },
        OracleOverride {
            target: None,
            via: "guard-site".to_string(),
            treat_as: "failed".to_string(),
        },
    ];
    assert!(evaluate(&rejected("guard-site", "RangeError"), &config).is_empty());
}

#[test]
fn an_ok_outcome_cannot_be_overridden_because_it_names_no_channel() {
    let mut config = OraclesConfig::default();
    config.overrides = vec![OracleOverride {
        target: None,
        via: "guard-site".to_string(),
        treat_as: "failed".to_string(),
    }];
    assert!(evaluate(&ok(1.0, None), &config).is_empty());
}
