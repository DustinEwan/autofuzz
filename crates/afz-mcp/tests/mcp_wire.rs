//! The eight tools over the wire a real harness sees: spawn the built binary, speak
//! newline-delimited JSON-RPC, assert the JSON. No MCP client library, so nothing but the
//! published protocol sits between the assertion and the server.

mod client;
mod fixture;

use std::io::BufReader;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout};

use serde_json::{json, Value};

/// One server process, handshake done, ids counting up.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Session {
    fn start(root: &Path) -> Session {
        Session::start_with_env(root, &[])
    }

    /// A server whose environment the test controls, for the tools that read it: `AFZ_HOME`
    /// moves the harness registry into a scratch directory.
    fn start_with_env(root: &Path, env: &[(&str, &str)]) -> Session {
        let mut child = client::spawn_with_env(root, env);
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut session = Session {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 1,
        };
        client::handshake(session.stdin.as_mut().expect("stdin"), &mut session.stdout);
        session
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.next_id += 1;
        client::call(
            self.stdin.as_mut().expect("stdin"),
            &mut self.stdout,
            self.next_id,
            name,
            arguments,
        )
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        client::ask(
            self.stdin.as_mut().expect("stdin"),
            &mut self.stdout,
            &client::request(self.next_id, method, params),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing stdin is the portable shutdown signal; the server exits on EOF.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

fn error_text(result: &Value) -> &str {
    result["content"][0]["text"]
        .as_str()
        .expect("an error text block")
}

#[test]
fn tools_list_reports_the_afz_tools() {
    let root = fixture::scratch("tools-list");
    let mut session = Session::start(&root);
    let answer = session.request("tools/list", json!({}));
    let tools = answer["result"]["tools"]
        .as_array()
        .expect("a tools array")
        .clone();
    let mut names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("a tool name"))
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "afz_findings",
            "afz_harnesses",
            "afz_probe",
            "afz_scan",
            "afz_scan_cancel",
            "afz_scan_status",
            "afz_validate_harness",
            "await_scan"
        ]
    );
    for tool in &tools {
        assert_eq!(
            tool["inputSchema"]["type"], "object",
            "inputSchema of {}",
            tool["name"]
        );
    }
    let probe = tools
        .iter()
        .find(|tool| tool["name"] == "afz_probe")
        .expect("afz_probe is listed");
    assert_eq!(probe["annotations"]["readOnlyHint"], json!(true), "{probe}");
    let harnesses = tools
        .iter()
        .find(|tool| tool["name"] == "afz_harnesses")
        .expect("afz_harnesses is listed");
    assert_eq!(
        harnesses["annotations"]["readOnlyHint"],
        json!(true),
        "{harnesses}"
    );
    assert!(
        harnesses["inputSchema"]["properties"]
            .as_object()
            .is_none_or(|properties| properties.is_empty()),
        "afz_harnesses takes no arguments: {harnesses}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn probe_reports_the_plugin_and_the_inventory() {
    let root = fixture::project("probe", &fixture::scripted(&["src/a.ts", "src/b.ts"]));
    let mut session = Session::start(&root);
    let result = session.call("afz_probe", json!({"path": root.display().to_string()}));
    assert_ne!(result["isError"], json!(true), "{result}");
    let content = &result["structuredContent"];
    assert_eq!(content["plugin"]["name"], "afz-fakeplugin", "{content}");
    assert_eq!(content["inventory"]["fuzzable"], 2, "{content}");
    assert_eq!(content["inventory"]["discovered"], 2, "{content}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_scan_runs_and_its_findings_come_back() {
    let root = fixture::project(
        "scan-findings",
        &fixture::faulted(&["src/a.ts", "src/b.ts"]),
    );
    let mut session = Session::start(&root);
    // Starting answers at once; await_scan is the blocking step, so it is the call a harness
    // backgrounds to be woken when the scan ends.
    let started = session.call("afz_scan", json!({"budget": {"invocations": 64}}));
    assert_ne!(started["isError"], json!(true), "{started}");
    let job_id = started["structuredContent"]["jobId"].clone();
    assert_eq!(
        started["structuredContent"]["state"], "running",
        "{}",
        started["structuredContent"]
    );
    let result = session.call("await_scan", json!({"jobId": job_id}));
    assert_ne!(result["isError"], json!(true), "{result}");
    let content = result["structuredContent"].clone();
    assert_eq!(content["state"], "completed", "{content}");
    eprintln!("tools/call afz_scan result: {content}");
    // The engine's scan id: eight Crockford-base32 characters of the start time, then two of
    // randomness (`crates/afz-engine/src/report/scanresult.rs`, `scan_id`).
    let scan_id = content["scanId"].as_str().expect("a scanId").to_string();
    assert_eq!(scan_id.len(), 10, "scanId {scan_id}");
    let dir = content["dir"].as_str().expect("a dir").to_string();
    assert!(
        Path::new(&dir).join("scan.jsonl").is_file(),
        "the scan result is written under {dir}"
    );

    let findings = session.call("afz_findings", json!({"jobId": content["jobId"]}));
    eprintln!(
        "tools/call afz_findings result: {}",
        findings["structuredContent"]
    );
    assert_ne!(findings["isError"], json!(true), "{findings}");
    let found = findings["structuredContent"]["findings"]
        .as_array()
        .expect("a findings array")
        .clone();
    assert_eq!(found.len(), 1, "{findings}");
    assert_eq!(found[0]["oracle"], "failed", "{}", found[0]);
    assert_eq!(found[0]["severity"], "high", "{}", found[0]);
    assert_eq!(found[0]["fault"]["class"], "TypeError", "{}", found[0]);
    let artifact = found[0]["input"]["artifact"]
        .as_str()
        .expect("an artifact path");
    assert!(
        Path::new(&dir).join(artifact).is_file(),
        "the finding's input exists at {dir}/{artifact}"
    );

    // The severity floor: a `high` finding survives `high` and is dropped by `critical`.
    let high = session.call(
        "afz_findings",
        json!({"jobId": content["jobId"], "severity": "high"}),
    );
    assert_eq!(
        high["structuredContent"]["findings"]
            .as_array()
            .expect("a findings array")
            .len(),
        1,
        "{high}"
    );
    let critical = session.call(
        "afz_findings",
        json!({"jobId": content["jobId"], "severity": "critical"}),
    );
    assert_eq!(
        critical["structuredContent"]["findings"]
            .as_array()
            .expect("a findings array")
            .len(),
        0,
        "{critical}"
    );

    // `markdown: true` returns the report.md text the scan's renderer wrote.
    let reported = session.call(
        "afz_findings",
        json!({"jobId": content["jobId"], "markdown": true}),
    );
    let markdown = reported["structuredContent"]["markdown"]
        .as_str()
        .expect("the markdown report");
    assert!(
        markdown.contains("autofuzz scan"),
        "markdown report is present"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_directory_outside_the_launch_directory_is_probed() {
    // The convention every other agent tool follows: the target is a call argument. A harness
    // reaches a different tree by calling the tool again, not by reconfiguring the server.
    let here = fixture::project("launch-dir", &fixture::scripted(&["src/a.ts"]));
    let elsewhere = fixture::project("elsewhere", &fixture::scripted(&["src/b.ts", "src/c.ts"]));
    let mut session = Session::start(&here);
    let result = session.call(
        "afz_probe",
        json!({"path": elsewhere.display().to_string()}),
    );
    eprintln!(
        "tools/call afz_probe on a second directory: {}",
        result["structuredContent"]
    );
    assert_ne!(result["isError"], json!(true), "{result}");
    let content = &result["structuredContent"];
    assert_eq!(content["plugin"]["name"], "afz-fakeplugin", "{content}");
    assert_eq!(content["inventory"]["fuzzable"], 2, "{content}");
    assert_eq!(
        content["root"],
        elsewhere.display().to_string(),
        "{content}"
    );
    std::fs::remove_dir_all(&here).ok();
    std::fs::remove_dir_all(&elsewhere).ok();
}

#[test]
fn a_second_scan_is_refused_and_a_running_one_stops() {
    let root = fixture::project("cancel", &fixture::faulted(&["src/a.ts", "src/b.ts"]));
    let mut session = Session::start(&root);
    let started = session.call(
        "afz_scan",
        json!({"budget": {"invocations": 100_000, "wall": "10m"}}),
    );
    assert_ne!(started["isError"], json!(true), "{started}");
    let job_id = started["structuredContent"]["jobId"]
        .as_str()
        .expect("a jobId")
        .to_string();

    // A status call answers while the scan runs, because the wait is a separate call: checking
    // progress never costs a block.
    let peek = session.call("afz_scan_status", json!({"jobId": job_id}));
    assert_ne!(peek["isError"], json!(true), "{peek}");
    assert_eq!(
        peek["structuredContent"]["state"], "running",
        "{}",
        peek["structuredContent"]
    );

    // The timeout is the bounded form of the same wait: it answers with the state as it is, and
    // the scan keeps running. A timeout the server ignored would hang this call for the wall clock.
    let timed_out = session.call(
        "await_scan",
        json!({"jobId": job_id, "timeoutSeconds": 1.0}),
    );
    assert_ne!(timed_out["isError"], json!(true), "{timed_out}");
    assert_eq!(
        timed_out["structuredContent"]["state"], "running",
        "{}",
        timed_out["structuredContent"]
    );

    let second = session.call("afz_scan", json!({}));
    assert_eq!(second["isError"], json!(true), "{second}");
    assert!(
        error_text(&second).contains("a scan is already running"),
        "{}",
        error_text(&second)
    );

    let cancelled = session.call("afz_scan_cancel", json!({}));
    assert_ne!(cancelled["isError"], json!(true), "{cancelled}");
    assert_eq!(
        cancelled["structuredContent"]["jobId"], job_id,
        "{cancelled}"
    );
    assert_eq!(
        cancelled["structuredContent"]["cancelRequested"],
        json!(true),
        "{cancelled}"
    );
    assert_eq!(
        cancelled["structuredContent"]["message"],
        "stopping after the current batch; await_scan reports the result",
        "{cancelled}"
    );

    let status = session.call("await_scan", json!({}));
    assert_ne!(status["isError"], json!(true), "{status}");
    let content = &status["structuredContent"];
    assert_eq!(content["state"], "cancelled", "{content}");
    assert_eq!(content["stoppedReason"], "interrupted", "{content}");
    let dir = content["dir"].as_str().expect("a dir").to_string();
    let document = std::fs::read_to_string(Path::new(&dir).join("scan.jsonl"))
        .expect("the written scan result");
    assert!(
        document.contains("\"stoppedReason\":\"interrupted\""),
        "the document says interrupted under {dir}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn afz_harnesses_lists_the_installed_records() {
    // `harnesses::root()` honors `AFZ_HOME`, so the registry under test is a scratch directory:
    // the tool is testable without a git checkout or an install.
    let root = fixture::scratch("harnesses");
    let home = fixture::scratch("harnesses-home");
    fixture::write(
        &home.join("harnesses/demo/harness.json"),
        r#"{"name":"demo","url":"file:///tmp/demo","commit":"0000000000000000000000000000000000000000","root":"/tmp/demo","argv":["true"]}"#,
    );
    let home_text = home.display().to_string();
    let mut session = Session::start_with_env(&root, &[("AFZ_HOME", &home_text)]);
    let result = session.call("afz_harnesses", json!({}));
    assert_ne!(result["isError"], json!(true), "{result}");
    let listed = result["structuredContent"]
        .as_array()
        .expect("an array of records")
        .clone();
    assert_eq!(listed.len(), 1, "{result}");
    assert_eq!(listed[0]["name"], "demo", "{result}");
    assert_eq!(listed[0]["argv"][0], "true", "{result}");
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&home).ok();
}

/// An unknown harness is the sentence the caller acts on, not a protocol error: the same one
/// `afz validate-harness` prints for the same mistake. The success path is the one
/// `crates/afz-conformance/tests/*` and `afz-xtask check-harness` already drive through the same
/// `harness::drive` this tool calls, and the fake plugin this repository ships carries no
/// conformance emitter, so a passing validation cannot be driven from here.
#[test]
fn validate_reports_the_missing_harness_as_a_sentence() {
    let root = fixture::scratch("validate-missing");
    let mut session = Session::start(&root);
    let result = session.call(
        "afz_validate_harness",
        json!({"harness": "no-such-harness"}),
    );
    assert_eq!(result["isError"], json!(true), "{result}");
    assert!(
        error_text(&result).contains("afz-harness.json"),
        "{}",
        error_text(&result)
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_seeded_scan_reports_its_seed() {
    let root = fixture::project("scan-seeded", &fixture::scripted(&["src/a.ts"]));
    let mut session = Session::start(&root);
    let started = session.call(
        "afz_scan",
        json!({
            "budget": {"invocations": 4},
            "seeds": [{"targetId": "afz:t:0000", "values": []}],
        }),
    );
    assert_ne!(started["isError"], json!(true), "{started}");
    let job_id = started["structuredContent"]["jobId"]
        .as_str()
        .expect("a jobId")
        .to_string();
    let result = session.call("await_scan", json!({"jobId": job_id}));
    assert_ne!(result["isError"], json!(true), "{result}");
    assert_eq!(
        result["structuredContent"]["state"], "completed",
        "{}",
        result["structuredContent"]
    );
    let findings = session.call("afz_findings", json!({"jobId": job_id}));
    assert_ne!(findings["isError"], json!(true), "{findings}");
    let content = &findings["structuredContent"];
    assert_eq!(content["execution"]["seeded"], 1, "{content}");
    let targets = content["targets"]
        .as_array()
        .expect("a targets array")
        .clone();
    assert_eq!(targets.len(), 1, "{content}");
    assert_eq!(targets[0]["targetId"], "afz:t:0000", "{content}");
    assert!(
        targets[0]["invocations"]
            .as_u64()
            .expect("an invocation count")
            >= 1,
        "the seed's invocation is charged to its target: {content}"
    );
    std::fs::remove_dir_all(&root).ok();
}
