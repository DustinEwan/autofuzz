// Shared over `tests/mcp_wire.rs` (`mod client;`). Standalone, this file compiles to an
// empty test crate, so the helpers look unused there; allow that rather than duplicate them.
#![allow(dead_code)]

//! A hand-written MCP client over the child's stdio: one JSON value per line.
//!
//! No SDK types here on purpose. The tests must assert the wire a real harness sees, not
//! whatever the Rust SDK happens to accept.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use serde_json::Value;

/// Start the built server with `root` as its working directory, which is what a terminal
/// harness does. The server takes no configuration: every tool call names the directory it
/// acts on, so reaching a different tree means calling the tool again, not restarting it.
pub fn spawn(root: &Path) -> Child {
    let exe = env!("CARGO_BIN_EXE_afz-mcp");
    Command::new(exe)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the server starts")
}

/// Send one request and read lines until the response carrying `id` arrives. Notifications
/// and other ids are skipped, so a server that talks while we wait cannot desync the reader.
pub fn ask(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>, request: &Value) -> Value {
    let mut line = serde_json::to_string(request).expect("a request serializes");
    line.push('\n');
    stdin.write_all(line.as_bytes()).expect("the request is written");
    stdin.flush().expect("the request is flushed");
    let wanted = request.get("id").cloned();
    loop {
        let mut text = String::new();
        let read = stdout.read_line(&mut text).expect("a line arrives");
        assert_ne!(read, 0, "the server closed stdout before answering {request}");
        let Ok(message) = serde_json::from_str::<Value>(text.trim()) else { continue };
        if wanted.is_some() && message.get("id") == wanted.as_ref() {
            return message;
        }
    }
}

pub fn request(id: i64, method: &str, params: Value) -> Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// The legacy `initialize` handshake every installed harness performs, then the
/// `initialized` notification. Returns nothing; panics if the server does not negotiate.
pub fn handshake(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>) {
    let initialize = request(
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "afz-test", "version": "0"}
        }),
    );
    let answer = ask(stdin, stdout, &initialize);
    let version = answer["result"]["protocolVersion"].as_str().expect("a negotiated protocolVersion");
    assert!(!version.is_empty(), "protocolVersion is {answer}");
    let initialized = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let line = format!("{}\n", serde_json::to_string(&initialized).expect("a notification serializes"));
    stdin.write_all(line.as_bytes()).expect("the notification is written");
    stdin.flush().expect("the notification is flushed");
}

/// One `tools/call`, returning its `result` object. `id` is the caller's, so the test can
/// keep the wire order straight.
pub fn call(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>, id: i64, name: &str, arguments: Value) -> Value {
    let answer = ask(stdin, stdout, &request(id, "tools/call", serde_json::json!({"name": name, "arguments": arguments})));
    answer.get("result").cloned().unwrap_or_else(|| panic!("tools/call {name} answered {answer}"))
}

