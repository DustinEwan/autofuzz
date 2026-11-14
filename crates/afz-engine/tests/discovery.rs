//! The two sides of the plugin boundary, checked against each other on a real response.
//!
//! `docs/schema/discovery.json` is generated from the Rust types, and the plugin is written in
//! TypeScript against the same document. This test runs the built plugin over a test target whose
//! entry points need a class the generator cannot build, and parses the response with the
//! engine's own `Discovery` type: a field that changed shape on either side fails to deserialize
//! here rather than in the middle of a scan.
//!
//! This is a cross-repository acceptance test: it needs the `js-ts` plugin installed
//! (`afz install-harness https://github.com/DustinEwan/autofuzz-harness-js-ts.git`) and runs
//! only under `cargo test -- --ignored`; the recorded argv is the program the user installed,
//! so no runtime override applies here. The always-on cross-language pin is the js-ts
//! repository's own contract test against `docs/schema/`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use afz_engine::rpc::client::{Launch, PluginClient};
use afz_protocol::{DiscoverReq, SkipReason};

fn target_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/targets/blocked")
}

fn launch() -> Launch {
    afz_engine::harnesses::launch("js-ts")
        .expect("install the js-ts harness first: afz install-harness https://github.com/DustinEwan/autofuzz-harness-js-ts.git")
        .with_cwd(target_root().display().to_string())
}

#[test]
#[ignore = "cross-repository acceptance test: install the js-ts harness, then cargo test -- --ignored"]
fn a_blocked_symbol_arrives_in_the_shape_the_engine_reads() {
    let request = DiscoverReq {
        root: target_root().display().to_string(),
        include: Some(vec!["**/*.ts".to_string()]),
        exclude: None,
        config_path: None,
    };
    let client = PluginClient::spawn(&launch()).expect("spawn the js-ts plugin");
    let discovery = client.discover(&request, Duration::from_secs(120)).expect("discover the blocked test target");

    let fuzzable: Vec<&str> = discovery.targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(fuzzable, ["formatAmount"], "only the entry point with no blocked parameter is fuzzable");

    let mut skipped: Vec<(String, String)> = discovery
        .skipped
        .iter()
        .map(|s| (s.name.clone(), serde_json::to_value(s.reason).expect("a skip reason is a string").as_str().unwrap_or_default().to_string()))
        .collect();
    skipped.sort_unstable();
    assert_eq!(
        skipped,
        [
            ("Db.query".to_string(), "unconstructible-parameter".to_string()),
            ("balanceOf".to_string(), "unconstructible-parameter".to_string()),
            ("postEntry".to_string(), "unconstructible-parameter".to_string()),
        ],
        "every symbol that needs the unbuildable class is reported, none silently dropped"
    );

    // No symbol is in both lists: `fuzzable + skipped == discovered` is the arithmetic the scan
    // result publishes, and a symbol counted twice would make it read as more coverage than the
    // scan reached.
    let fuzzed_ids: Vec<&str> = discovery.targets.iter().map(|t| t.id.as_str()).collect();
    for entry in &discovery.skipped {
        assert!(!fuzzed_ids.contains(&entry.target_id.as_str()), "{} is both fuzzable and skipped", entry.target_id);
    }

    let by_name = |name: &str| {
        discovery
            .skipped
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("discovery reported no skipped {name}"))
    };

    let balance = by_name("balanceOf");
    assert_eq!(balance.reason, SkipReason::UnconstructibleParameter);
    let blocker = balance.blocked_by.as_ref().expect("a blocked parameter names its blocker");
    assert_eq!(blocker.type_, "Db");
    assert_eq!(blocker.path, "arguments[0]");
    let module = blocker.module.as_deref().expect("the blocker names the file that declares Db");
    assert!(module.ends_with("targets/blocked/src/db.ts"), "module {module} should name the declaring file");
    assert_eq!(balance.unlocks_targets, Some(1), "postEntry shares the same missing class");
    assert_eq!(by_name("postEntry").unlocks_targets, Some(1));

    // A method on the unbuildable class is blocked by its receiver, which no file names.
    let query = by_name("Db.query");
    let receiver = query.blocked_by.as_ref().expect("the method names its receiver as the blocker");
    assert_eq!(receiver.path, "receiver");
    assert_eq!(receiver.type_, "Db");
    assert!(receiver.module.is_none(), "a receiver blocker has no declaring module to name");
}
