//! The runtime-harness lifecycle: the suite starts the package's service, proves it is
//! ready, and stops it again, so a REST-backed harness is validated with the same command as
//! an in-process one and every failure names the package's own argv.
//!
//! The suite allocates the port (bind `127.0.0.1:0`, read it, release it) and substitutes it
//! for `{{port}}` in every lifecycle argv, so parallel runs never collide and the package
//! never picks a port the suite does not know. The allocation is racy by design - the suite
//! hands out a number, not a reservation - which is acceptable because the health poll fails
//! loudly and fast when another process wins the race.
//!
//! The nonce closes the loop: the manifest carries a random value, the fixture's service
//! echoes it, and the harness's probe target must return it. That is what proves the plugin
//! under test talks to the service the suite started, rather than to a stub that answers
//! discovery from a table.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::template::Lifecycle;

/// How long the service may take to answer its health URL when the manifest does not say.
/// A cold JVM runtime is the reason this is minutes rather than seconds; the poll interval
/// keeps the fast case fast.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(120);
const HEALTH_POLL: Duration = Duration::from_millis(200);
/// One health request may not block the suite: the service is up or this attempt failed.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The port the suite hands to the package: bind port 0, read the assigned number, release
/// the listener. See the module comment for why the race is accepted.
pub fn allocate_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("could not allocate a loopback port: {e}"))?;
    let port = listener.local_addr().map_err(|e| format!("allocated port is unreadable: {e}"))?.port();
    drop(listener);
    Ok(port)
}

/// Replace `{{port}}` in every argv token, so `["java", "-jar", "app.jar", "--port={{port}}"]`
/// and `["node", "server.js", "{{port}}"]` both work.
pub fn substitute(argv: &[String], port: u16) -> Vec<String> {
    argv.iter().map(|argument| argument.replace("{{port}}", &port.to_string())).collect()
}

/// `http://host:port/path` with the path defaulting to `/`. TLS and unix sockets are out of
/// scope for a loopback health URL; the error says so rather than guessing.
fn split_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| format!("{url}: the health URL must be http:// on loopback"))?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], rest[at..].to_string()),
        None => (rest, "/".to_string()),
    };
    let colon = authority.rfind(':').ok_or_else(|| format!("{url}: the health URL must carry a port"))?;
    let host = authority[..colon].to_string();
    let port = authority[colon + 1..].parse().map_err(|_| format!("{url}: the health URL port is not a number"))?;
    Ok((host, port, path))
}

/// One loopback HTTP GET with a fixed budget. Any status line answers: 2xx/3xx/4xx all prove
/// the service accepted the connection and speaks HTTP, and a 4xx health path is a package
/// bug the harness author will see in the response, not a suite failure.
pub fn health_ready(url: &str) -> Result<(), String> {
    let (host, port, path) = split_http_url(url)?;
    let address = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("{url}: cannot resolve: {e}"))?
        .next()
        .ok_or_else(|| format!("{url}: no address"))?;
    let mut stream = TcpStream::connect_timeout(&address, HEALTH_PROBE_TIMEOUT).map_err(|e| format!("{url}: {e}"))?;
    stream.set_read_timeout(Some(HEALTH_PROBE_TIMEOUT)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(HEALTH_PROBE_TIMEOUT)).map_err(|e| e.to_string())?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n").map_err(|e| format!("{url}: {e}"))?;
    let mut head = [0u8; 64];
    let read = stream.read(&mut head).map_err(|e| format!("{url}: {e}"))?;
    let text = String::from_utf8_lossy(&head[..read]);
    let status: i32 = text
        .strip_prefix("HTTP/")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("{url}: answered without an HTTP status line: {:?}", text.trim()))?;
    match status {
        200..=499 => Ok(()),
        other => Err(format!("{url}: answered {other}; the service is not ready")),
    }
}

/// Poll until the health URL answers or the budget is spent. The error names the URL and the
/// last observation, which is what distinguishes "the package never started" from "it started
/// and returned 503".
pub fn wait_for_health(url: &str, budget: Duration) -> Result<(), String> {
    let until = Instant::now() + budget;
    let mut last = String::from("no attempt yet");
    while Instant::now() < until {
        match health_ready(url) {
            Ok(()) => return Ok(()),
            Err(detail) => last = detail,
        }
        std::thread::sleep(HEALTH_POLL);
    }
    Err(format!("{url}: never answered within {} s; last: {last}", budget.as_secs()))
}

/// The running service. Teardown is explicit (`Service::stop`), and dropping without stopping
/// kills the process: a suite that panicked must not leave a listener behind.
#[derive(Debug)]
pub struct Service {
    child: Child,
    lifecycle: Lifecycle,
    port: u16,
}

impl Service {
    /// Start the package's service and wait for its health URL. Every error names the
    /// package's own launch argv, because the failure is the package's, not the suite's.
    pub fn start(lifecycle: &Lifecycle) -> Result<Service, String> {
        let port = allocate_port()?;
        let argv = substitute(&lifecycle.launch, port);
        let program = argv.first().ok_or("lifecycle.launch is empty")?;
        let child = Command::new(program)
            .args(argv.iter().skip(1))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("the package's service could not start: {} failed: {e}", argv.join(" ")))?;
        let service = Service { child, lifecycle: lifecycle.clone(), port };
        let health = substitute(std::slice::from_ref(&lifecycle.health), port).remove(0);
        let budget = lifecycle.health_timeout_secs.map(Duration::from_secs).unwrap_or(HEALTH_TIMEOUT);
        wait_for_health(&health, budget).map_err(|detail| format!("the package's service never became ready: {detail} (launched: {})", argv.join(" ")))?;
        Ok(service)
    }

    /// Run the package's teardown, then make sure the process is gone either way.
    pub fn stop(mut self) -> Result<(), String> {
        let argv = substitute(&self.lifecycle.teardown, self.port);
        let mut result = Ok(());
        if let Some(program) = argv.first() {
            match Command::new(program).args(argv.iter().skip(1)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status() {
                Ok(status) if !status.success() => result = Err(format!("the package's teardown {} exited {status}", argv.join(" "))),
                Err(e) => result = Err(format!("the package's teardown {} could not run: {e}", argv.join(" "))),
                _ => {}
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        result
    }

    /// The port the suite allocated, for diagnostics and tests.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
