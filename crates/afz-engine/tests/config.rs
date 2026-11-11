//! The scan project is the unit of configuration, so its validation rules are user-facing
//! contracts: an unusable configuration must fail loudly with the exact message the design
//! names, never silently fall back.

use std::path::{Path, PathBuf};

use afz_engine::config::{self, Block, ScanProject};

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("afz-config-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join(config::CONFIG_FILE_NAME);
    std::fs::write(&path, text).expect("write config");
    path
}

fn message(err: &dyn std::fmt::Display) -> String {
    err.to_string()
}

#[test]
fn a_codebase_block_loads_with_design_defaults() {
    let dir = tmp("codebase");
    let path = write(
        &dir,
        r#"{
          "project": "billing-service",
          "codebase": { "harness": "js-ts", "root": ".", "include": ["src/**/*.ts"], "exclude": ["**/*.test.ts"],
                        "fixtures": { "module": "./autofuzz.fixtures.ts" } },
          "oracles": { "deadline": { "ms": 2000 } },
          "budget": { "invocations": 200000, "wallClockMs": 900000, "workers": 8 },
          "output": { "dir": ".autofuzz", "renderers": ["json", "markdown"] }
        }"#,
    );
    let loaded = config::load(&path).expect("loads");
    assert_eq!(loaded.project.project, "billing-service");
    assert_eq!(loaded.root, dir.canonicalize().unwrap_or(dir.clone()));
    assert_eq!(loaded.project.budget.invocations, 200000);
    assert_eq!(loaded.project.budget.workers_or_default(), 8);
    assert_eq!(loaded.project.oracles.deadline.ms, Some(2000));

    let scope = loaded.discovery_scope().expect("scope");
    assert_eq!(scope.root, loaded.root);
    assert_eq!(scope.include.as_deref(), Some(&["src/**/*.ts".to_string()][..]));
    assert_eq!(
        scope.exclude.as_deref(),
        Some(&["**/*.test.ts".to_string(), "**/.autofuzz/**".to_string()][..]),
        "the scan's own output directory must not be discovery input"
    );

    let hash = loaded.config_hash().expect("hash");
    assert!(hash.starts_with("sha256:") && hash.len() == 71, "hash was {hash}");
    assert_eq!(hash, loaded.config_hash().expect("stable"), "hash changed between reads");

    let launch = loaded.launch_for(Block::Codebase).expect("launch");
    assert_eq!(launch.argv, vec!["node".to_string(), "packages/harness-js/dist/server.js".to_string()]);
    assert_eq!(launch.cwd.as_deref(), Some(loaded.root.display().to_string().as_str()));
    assert_eq!(launch.env.get("AFZ_ROOT").map(String::as_str), Some(loaded.root.display().to_string().as_str()));
    assert_eq!(loaded.harness_name(Block::Codebase), "js-ts");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_explicit_command_overrides_the_builtin_launch() {
    let dir = tmp("command");
    let path = write(
        &dir,
        r#"{ "project": "p", "codebase": { "harness": "js-ts",
             "command": ["node", "--import", "tsx", "server.mjs"],
             "env": { "AFZ_TRACE": "1" } } }"#,
    );
    let loaded = config::load(&path).expect("loads");
    let launch = loaded.launch_for(Block::Codebase).expect("launch");
    assert_eq!(launch.argv, vec!["node".to_string(), "--import".to_string(), "tsx".to_string(), "server.mjs".to_string()]);
    assert_eq!(launch.env.get("AFZ_TRACE").map(String::as_str), Some("1"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_unknown_harness_without_a_command_is_rejected() {
    let dir = tmp("unknown-harness");
    let path = write(&dir, r#"{ "project": "p", "codebase": { "harness": "ocaml" } }"#);
    let err = message(&config::load(&path).unwrap_err());
    assert_eq!(err, "harness \"ocaml\" has no built-in command; set `command` to the plugin argv");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn at_least_one_block_is_required() {
    let dir = tmp("no-blocks");
    let path = write(&dir, r#"{ "project": "p" }"#);
    assert_eq!(
        message(&config::load(&path).unwrap_err()),
        "at least one of `codebase` or `runtime` must be present"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_runtime_block_validates_but_names_its_host_requirement() {
    let dir = tmp("runtime");
    let path = write(
        &dir,
        r#"{ "project": "p",
             "runtime": { "baseUrl": "http://127.0.0.1:3000", "openapi": "./openapi.json",
                          "allowMutating": ["POST /v1/tokens"], "destructive": ["DELETE /v1/accounts/1"],
                          "maxConcurrent": 1, "requestsPerSecond": 20 } }"#,
    );
    let loaded = config::load(&path).expect("the block parses and validates");
    assert_eq!(loaded.harness_name(Block::Runtime), "http");
    let launch = loaded.launch_for(Block::Runtime).expect_err("http has no built-in command yet");
    assert!(message(&launch).contains("http"), "{launch}");

    let path = write(&dir, r#"{ "project": "p", "runtime": { "openapi": "./openapi.json" } }"#);
    assert_eq!(
        message(&config::load(&path).unwrap_err()),
        "`runtime.baseUrl` is required: requests to a host the user did not name are refused"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_unimplemented_oracle_is_a_configuration_error() {
    // The design names this message exactly: naming an oracle the engine does not run must
    // never be a silent no-op.
    for (name, body) in [("differential", r#"{ "against": "v1.4.0", "severity": "high" }"#), ("property", r#"{ "severity": "high" }"#)] {
        let dir = tmp("oracle");
        let path = write(
            &dir,
            &format!(r#"{{ "project": "p", "codebase": {{ "harness": "js-ts" }}, "oracles": {{ "{name}": {body} }} }}"#),
        );
        assert_eq!(
            message(&config::load(&path).unwrap_err()),
            format!("oracle \"{name}\" is not implemented in this version")
        );
        std::fs::remove_dir_all(dir).ok();
    }
}

#[test]
fn defaults_match_the_design() {
    let dir = tmp("defaults");
    let path = write(&dir, r#"{ "project": "p", "codebase": { "harness": "js-ts" } }"#);
    let loaded = config::load(&path).expect("loads");
    let o = &loaded.project.oracles;
    assert_eq!(o.failed.severity, config::Severity::High);
    assert_eq!(o.rejected.severity, config::Severity::Off, "rejected is off by default");
    assert_eq!(o.deadline.severity, config::Severity::Medium);
    assert_eq!(o.deadline.ms, Some(2000));
    assert_eq!(o.memory_growth.severity, config::Severity::Medium);
    assert_eq!(o.memory_growth.mb, Some(256));
    assert_eq!(o.process_exit.severity, config::Severity::High);
    assert_eq!(loaded.project.budget.max_corpus, 2048);
    assert_eq!(loaded.project.budget.strategy, config::Strategy::Guided);
    assert_eq!(loaded.project.output.dir, ".autofuzz");
    assert_eq!(loaded.project.budget.workers_or_default(), num_cpus::get() as u32, "the default is one worker per core, with no cap");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn overrides_are_checked_and_severity_off_disables() {
    let dir = tmp("overrides");
    let path = write(
        &dir,
        r#"{ "project": "p", "codebase": { "harness": "js-ts" },
             "oracles": { "rejected": { "severity": "high" },
                          "overrides": [ { "target": "afz:t:44be", "via": "result-err", "treatAs": "failed" } ] } }"#,
    );
    let loaded = config::load(&path).expect("loads");
    assert_eq!(loaded.project.oracles.rejected.severity, config::Severity::High);
    assert!(loaded.project.oracles.rejected.severity.enabled());
    assert!(!config::Severity::Off.enabled());
    assert_eq!(loaded.project.oracles.overrides.len(), 1);
    assert_eq!(loaded.project.oracles.overrides[0].treat_as, "failed");

    let path = write(
        &dir,
        r#"{ "project": "p", "codebase": { "harness": "js-ts" },
             "oracles": { "overrides": [ { "via": "result-err", "treatAs": "panic" } ] } }"#,
    );
    assert_eq!(
        message(&config::load(&path).unwrap_err()),
        "`oracles.overrides[].treatAs` must be \"ok\", \"rejected\", or \"failed\", got \"panic\""
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_zero_budget_is_rejected_rather_than_running_forever() {
    for (field, json) in [
        ("invocations", r#"{ "invocations": 0 }"#),
        ("wallClockMs", r#"{ "wallClockMs": 0 }"#),
        ("maxCorpus", r#"{ "maxCorpus": 0 }"#),
    ] {
        let dir = tmp("budget");
        let path = write(&dir, &format!(r#"{{ "project": "p", "codebase": {{ "harness": "js-ts" }}, "budget": {json} }}"#));
        assert_eq!(message(&config::load(&path).unwrap_err()), format!("`budget.{field}` must be greater than 0"));
        std::fs::remove_dir_all(dir).ok();
    }
}

#[test]
fn an_unknown_top_level_key_is_rejected() {
    let dir = tmp("unknown-key");
    let path = write(&dir, r#"{ "project": "p", "codebase": { "harness": "js-ts" }, "target": {} }"#);
    let err = message(&config::load(&path).unwrap_err());
    assert!(err.contains("unknown field `target`"), "err was {err}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn discovery_walks_up_to_find_the_project_file() {
    let dir = tmp("discover");
    let path = write(&dir, r#"{ "project": "p", "codebase": { "harness": "js-ts" } }"#);
    let nested = dir.join("src").join("deep");
    std::fs::create_dir_all(&nested).expect("nested dir");

    let found = config::discover(&nested).expect("found upward");
    assert_eq!(found.canonicalize().unwrap(), path.canonicalize().unwrap());
    // A file argument is used directly.
    assert_eq!(config::discover(&path).expect("file argument").canonicalize().unwrap(), path.canonicalize().unwrap());

    let orphan = tmp("orphan");
    let missing = config::discover(&orphan).unwrap_err();
    assert!(message(&missing).contains("no autofuzz.json found from"), "err was {missing}");
    assert!(message(&missing).contains("--config"), "err was {missing}");
    std::fs::remove_dir_all(dir).ok();
    std::fs::remove_dir_all(orphan).ok();
}

#[test]
fn a_missing_file_says_which_file_it_tried() {
    let err = config::load(Path::new("/nonexistent/autofuzz.json")).unwrap_err();
    assert!(message(&err).contains("/nonexistent/autofuzz.json"), "err was {err}");
}

#[test]
fn malformed_json_names_the_file_and_the_parser() {
    let dir = tmp("malformed");
    let path = write(&dir, r#"{ "project": "p", "codebase": { "harness": "js-ts", } }"#);
    let err = message(&config::load(&path).unwrap_err());
    assert!(err.contains("autofuzz.json"), "err was {err}");
    assert!(err.contains("trailing comma"), "err was {err}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn validate_is_callable_on_an_in_memory_project() {
    let mut project = ScanProject::default();
    assert_eq!(
        message(&project.validate().unwrap_err()),
        "at least one of `codebase` or `runtime` must be present"
    );
    project.project = "p".into();
    project.output.renderers = vec!["sarif".into(), "docx".into()];
    project.codebase = Some(config::CodebaseBlock { harness: "js-ts".into(), ..Default::default() });
    assert_eq!(message(&project.validate().unwrap_err()), "renderer \"docx\" is not implemented in this version");
}

/// A scan writes generated reproducers into the tree it scans, so its own output directory is
/// never discovery input. A project scanning `.` had been fuzzing the `.ts` reproducers its
/// previous run wrote under `.autofuzz/`.
#[test]
fn the_scan_output_directory_is_never_discovered() {
    let dir = tmp("output-excluded");
    let path = write(
        &dir,
        r#"{ "project": "p", "codebase": { "harness": "js-ts" },
             "output": { "dir": "results", "renderers": ["json"] } }"#,
    );
    let loaded = config::load(&path).expect("loads");
    let scope = loaded.discovery_scope().expect("scope");
    assert_eq!(scope.exclude.as_deref(), Some(&["**/results/**".to_string()][..]));
}
