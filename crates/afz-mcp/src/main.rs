//! The `afz-mcp` binary: parse the command line, then serve over [`afz_mcp::server::run`].

use clap::Parser;

/// The server takes no configuration on purpose. Every tool call names the directory it acts
/// on, so a harness reaches a different tree by calling a tool again with a different `path`,
/// exactly as it does with every other tool it has - never by restarting the server.
#[derive(Parser)]
#[command(name = "afz-mcp", version, about = "MCP server for autofuzz: probe, scan, and read findings.")]
struct Args {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse for `--help` and `--version`; there is nothing else to configure.
    Args::parse();
    afz_mcp::server::run().await
}
