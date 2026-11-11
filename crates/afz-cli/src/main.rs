//! The `afz` command.

mod probe;
mod scan;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "afz",
    version,
    about = "autofuzz: type-directed fuzzing across languages",
    long_about = "autofuzz discovers interfaces through a harness plugin, generates structured inputs, \
                  runs a feedback-guided loop, and writes one machine-readable scan result."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a harness plugin, shake hands, and print the inventory it discovers.
    ProbePlugin {
        /// Plugin argv, e.g. `--command node packages/harness-js/dist/server.js`.
        #[arg(long, num_args = 1..)]
        command: Vec<String>,
        /// Scan project to discover against. Defaults to the current directory.
        #[arg(long)]
        path: Option<String>,
        /// Print raw JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Run a coverage-guided scan and write the scan result.
    Scan {
        /// Directory to scan. Defaults to the scan project's own root.
        path: Option<String>,
        /// Scan project file. Defaults to the nearest autofuzz.json above the scan root.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Budget limits, repeated or comma-separated: `invocations=50000`, `wall=90s`.
        #[arg(long, value_delimiter = ',')]
        budget: Vec<String>,
        /// Seed for every random choice the scan makes, so a rerun replays.
        #[arg(long)]
        seed: Option<String>,
        /// A previous scan directory: its corpus is reloaded and its findings suppressed.
        #[arg(long)]
        resume: Option<PathBuf>,
        /// `guided` steers with coverage; `random` generates without the corpus.
        #[arg(long)]
        strategy: Option<String>,
        /// Plugin processes to run at once. Overrides `budget.workers`.
        #[arg(long)]
        workers: Option<u32>,
        /// Print only the summary, not the coverage samples.
        #[arg(long, short)]
        quiet: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::ProbePlugin { command, path, json } => probe::run(command, path, json),
        Command::Scan { path, config, budget, seed, resume, strategy, workers, quiet } => {
            scan::run(path, config, &budget, seed, resume, strategy, workers, quiet)
        }
    };
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("afz: {e}");
            std::process::exit(1);
        }
    }
}
