# Decision 13 - an MCP server drives the engine in process

Status: accepted. 2026-11-11.
Amends: `README.md`, which gains "Drive it from a coding harness"; the workspace `rust-version`, which rises to 1.88.

## Context

`afz` reached a coding harness only through a shell: `afz probe-plugin` and `afz scan` print a
terminal summary, and `afz_engine::scan::run` is one blocking call. A harness that integrates over
the Model Context Protocol (Claude Code, Cursor, VS Code Copilot, Zed, Codex CLI) does not parse a
terminal; it calls named tools over a stdio channel and reads structured JSON back. The gap this
closes is the question an agent asks mid-task - "does this change set fuzz clean?" - answered
without a shell and with findings that name the files they came from.

Two facts fixed the shape of the answer before any code was written. First, the engine's scan is
`afz_engine::scan::run(&Options, &mut impl FnMut(&Progress))` (`crates/afz-engine/src/scan.rs`),
whose only in-flight control is `Options::stop`, an `Arc<AtomicBool>` polled between batches, and
which writes nothing to stdout (the engine's diagnostics go to `eprintln!` and to `engine.log`).
Embedding it behind a stdio channel is therefore safe: there is no stdout to interleave with the
protocol, and one atomic flag is the whole cancellation surface. Second, the current MCP revision
`2026-07-28` made `server/discover`, per-request `_meta`, a `resultType` on every result, and
`ttlMs`/`cacheScope` on list results mandatory, while every harness then installed still spoke the
legacy `initialize` handshake. A server that answers only one era fails half its users, so it must
be dual-era.

## Decision

1. A new binary, `afz-mcp` (`crates/afz-mcp`), not an `afz mcp` subcommand. It depends on
   `afz-engine` and speaks MCP on stdio; `afz-cli` keeps its dependency graph untouched and `tokio`
   stays inside `afz-mcp`. `afz-wasm` and `xtask` already declare second binaries, so the workspace
   has precedent.

2. The `rmcp` crate (3.9.0, feature `transport-io`) is the protocol layer, so the server is
   dual-era by default: the SDK answers `ping` and `server/discover` and negotiates either revision
   without hand-written version branches. The workspace `rust-version` rises from 1.85 to 1.88,
   which is `rmcp`'s own minimum; the local toolchain and CI's `dtolnay/rust-toolchain@stable` both
   build it.

3. Six tools, tools-only capabilities - no resources, prompts, logging, roots, or sampling.
   `afz_probe` (does this directory have a plugin, and what would it fuzz), `afz_scan` (start a
   scan, return a `jobId`), `afz_scan_status` (state and live progress, answered immediately),
   `await_scan` (block until a scan ends), `afz_scan_cancel` (stop a running scan, which still
   writes its result), and `afz_findings` (read a finished scan's
   findings). Each returns `structuredContent` built from the engine's own serialized types
   (`afz_protocol::{Handshake, TargetDescriptor, SkippedTarget, CoverageMap}` and
   `afz_engine::report::scanresult::{FindingRecord, GitScope}`), so the tool output is the published
   `docs/schema/scan-result.json` contract, not a second shape invented for the server.

4. A scan runs on `tokio::task::spawn_blocking` and the tool call returns a server-minted handle
   `job-<scanId>` (`crates/afz-mcp/src/jobs.rs`), because the engine names its output directory only
   when it writes it at the end. `afz_scan_cancel` sets that job's `Options::stop`; the engine reads
   it between batches and still writes `scan.json` with `stoppedReason: interrupted`. One scan runs
   at a time per server: `budget.workers` defaults to the core count (Decision 0011), so a second
   concurrent scan would only halve the first.
   Waiting is its own tool, `await_scan`, rather than a flag on the calls that start or report a
   scan: a harness backgrounds `await_scan` to be woken when a scan ends, and reads
   `afz_scan_status` for progress while it runs. It blocks with no deadline by default, because a
   mandatory cap would force the caller to reissue the wait, which is the polling the handle exists
   to avoid; the optional `timeoutSeconds` is the caller's choice, not a limit the server imposes.

5. The server takes no path configuration. Every tool call names the directory it acts on, and
   `project::resolve` (`crates/afz-mcp/src/project.rs`) makes it absolute and walks up to the
   nearest `autofuzz.json`, the same order `afz scan` uses. Reaching a different tree means calling
   a tool again with a different `path`, which is how every other tool a harness has works; the
   server has no state that pins it to one directory.

6. The CLI's Ctrl-C flag is wired into `Options::stop` (`crates/afz-cli/src/scan.rs`). The CLI has
   always advertised "finishing the current batch and writing the scan result" on interrupt, but it
   set a flag the engine never read, because `Options::new` fills `stop` with a different `Arc`. The
   graceful stop the CLI promised, and which `afz_scan_cancel` now depends on, had to be real.

## Consequences

- The MSRV claim moves to 1.88 in `Cargo.toml` and the README. Anyone building on 1.85 to 1.87 must
  update their toolchain.
- `afz-mcp` is a library plus a thin binary. The six tools are tested over the raw wire in
  `crates/afz-mcp/tests/`, which asserts the JSON a real harness reads, not SDK types. One test
  probes a second directory from the same session, which is the regression guard against
  reintroducing a launch-time directory restriction.
- The server prints the working directory it was launched with to stderr at startup. A relative
  `path` resolves against it and clients do not agree on it: terminal harnesses pass the shell's
  directory, while some editors pass `/` or the home directory.
- Cancelling a `tools/call` stops the wait, never the scan. `await_job` races the request's
  cancellation token against its poll, so a cancelled call returns instead of running to a
  deadline - there is no deadline. Only `afz_scan_cancel` stops a scan, and both tool descriptions
  say so. When the server's stdin closes, it sets `stop` on every running job and gives the engine
  five seconds to reap its plugin processes before exiting.
- The workspace gains `rmcp`, `tokio`, and their transitive dependencies to `afz-mcp` only.
  `afz-cli`, `afz-engine`, and `afz-wasm` are unchanged apart from the one-line interrupt wiring.

## Reconsidered

- **Hand-rolling the MCP server.** Rejected: `2026-07-28` makes `server/discover`, per-request
  `_meta`, `resultType`, `ttlMs`, and `cacheScope` mandatory, and a hand-rolled server answers one
  protocol era, not both. Getting the negotiation wrong is invisible until a harness fails to
  connect, and `rmcp` answers both eras by default for the cost of a dependency.
- **An `afz mcp` subcommand instead of a separate binary.** Rejected: it would pull `tokio` and the
  whole transport stack into `afz-cli`, whose current dependency graph has no async runtime, for a
  command most `afz` users never run.
- **The `io.modelcontextprotocol/tasks` extension instead of job handles.** Rejected: task support
  is advertised by four clients, and a server-minted handle polled through `afz_scan_status` works
  in all of them. The tasks methods would give a subset of clients a capability every client already
  has through the registry.
- **A launch-time `--root` allowlist.** Built, then removed. A scan project's `codebase.command`
  is arbitrary code (`LoadedConfig::launch_for`), so a `path` argument does name code to run - but
  the harness calling this server also gives the model a shell, and `afz scan <path>` runs that
  same code with nothing the server withholds. The guard also defaulted to the process working
  directory, which clients do not standardize: Cline starts stdio servers with `cwd=/`
  ([cline#9950](https://github.com/cline/cline/issues/9950)), where every absolute path satisfies a
  `starts_with` test, so the fence silently enforced nothing while reading like a fence. Where it
  did bind it was a dead end the model could not work around, because changing the target would
  mean restarting the server rather than calling the tool again. Restricting a process to one
  directory is the operating system's job, not this server's.
- **Concurrent scans in one server.** Rejected for now: two scans default to a worker pool of the
  machine's core count each, so they only contend. A harness that must scan two checkouts at once
  starts two `afz-mcp` processes; making the refusal per-root instead of global is the change to
  make if that ever becomes the wrong call.
