//! Startup, identity, capabilities, and shutdown.
//!
//! The server is tools-only: no resources, prompts, logging, roots, or sampling. Roots,
//! Sampling, and Logging are Deprecated as of protocol `2026-07-28` (SEP-2577), the scan
//! project's own `harness.root` already says where to look, and `rmcp` answers `ping` and
//! `server/discover` itself.

use std::time::{Duration, Instant};

use rmcp::{tool_handler, ServerHandler, ServiceExt};

use crate::jobs::{self, JobState, Shared};

/// The handler behind all six tool calls: the job registry every scan shares. Every tool
/// names the directory it acts on, so the handler holds no path configuration.
pub struct Afz {
    pub(crate) shared: Shared,
}

impl Afz {
    pub fn new(shared: Shared) -> Afz {
        Afz { shared }
    }
}

// The macro generates `get_info` with the tools capability and takes the version from
// `CARGO_PKG_VERSION`, so the identity below is the only one the server ever reports.
#[tool_handler(
    name = "afz",
    instructions = "autofuzz fuzzes the interfaces a harness plugin discovers. afz_probe answers whether a directory is configured and what would be fuzzed. afz_scan starts a scan and answers with a jobId; await_scan blocks until a scan ends, so run it as a background task to be woken when one finishes; afz_scan_status reports progress without waiting; afz_scan_cancel ends a running scan early and still writes its result; and afz_findings reads the findings of a finished scan. A scan writes into the scan project's own output directory; every finding names the input and reproducer files it came from."
)]
impl ServerHandler for Afz {}

/// Serve MCP on stdio until stdin closes, then leave no scan running behind.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Clients disagree wildly on the working directory they give a stdio server - the
    // terminal ones pass the shell's directory, some editors pass `/` or the home directory -
    // and a relative `path` resolves against it. Name it, so a scan that finds nothing in an
    // unexpected place is diagnosable from the client's log.
    match std::env::current_dir() {
        Ok(cwd) => eprintln!("afz-mcp: a relative `path` resolves against {}", cwd.display()),
        Err(e) => eprintln!("afz-mcp: cannot resolve the working directory: {e}"),
    }
    let shared = Shared::default();
    let service = Afz::new(shared.clone()).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    stop_and_reap(&shared).await;
    Ok(())
}

/// Set `stop` on every running scan and give the engine up to five seconds to write its
/// result and reap its plugin processes, so nothing outlives the process.
async fn stop_and_reap(shared: &Shared) {
    let stopped = jobs::stop_everything(shared);
    if stopped == 0 {
        return;
    }
    eprintln!("afz-mcp: shutting down, stopping {stopped} scan(s)");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let running = jobs::lock(shared).jobs.values().any(|job| job.state == JobState::Running);
        if !running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
