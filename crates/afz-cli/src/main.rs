//! The `afz` command.

mod export;
mod install;
mod probe;
mod scan;
mod validate;

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
        /// Plugin argv, e.g. `--command node /home/me/harness-js/dist/server.js`.
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
        /// Scope the scan to a git change set: `staged`, `unstaged`, `stash`, `stash@{0}`, any revision, or a span such as `v1..HEAD`
        #[arg(long, value_name = "SELECTOR")]
        git: Option<String>,
        /// Plugin processes to run at once. Overrides `budget.workers`.
        #[arg(long)]
        workers: Option<u32>,
        /// Print only the summary, not the coverage samples.
        #[arg(long, short)]
        quiet: bool,
    },
    /// Validate a harness plugin against the contract and the conformance template.
    ValidateHarness {
        /// An installed harness name, or a harness directory. Defaults to the working directory,
        /// so `afz validate-harness` inside a harness checkout validates that checkout.
        harness: Option<String>,
        /// How many discovered targets `prepare` and `invoke` run on. Defaults to 8.
        #[arg(long)]
        targets: Option<usize>,
        /// `deadlineMs` for every driven invocation. Defaults to the harness manifest's
        /// `conformance.deadlineMs`, then to 1000.
        #[arg(long)]
        deadline: Option<u64>,
        /// Print the machine-readable report instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Install the harness plugin a git repository provides, and validate it.
    InstallHarness {
        /// Git URL or local path of a harness repository carrying `afz-harness.json`.
        url: String,
        /// Branch, tag, or commit to install. Defaults to the repository's default branch.
        #[arg(long = "ref")]
        reference: Option<String>,
        /// Record the plugin without driving the conformance suite.
        #[arg(long)]
        no_validate: bool,
        /// Print the conformance report as JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// List the installed harness plugins and the command each one resolves to.
    Harnesses {
        #[arg(long)]
        json: bool,
    },
    /// Write the protocol schemas this engine validates against, so a harness author can
    /// `cue import` them or point an editor at them without an engine checkout.
    ExportSchema {
        /// The protocol revision to export. Defaults to the newest this engine speaks.
        version: Option<String>,
        /// Directory to write into. Defaults to `schema` under the working directory.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::ProbePlugin { command, path, json } => probe::run(command, path, json),
        Command::Scan { path, config, budget, seed, resume, strategy, git, workers, quiet } => {
            scan::run(path, config, &budget, seed, resume, strategy, git, workers, quiet)
        }
        Command::ValidateHarness { harness, targets, deadline, json } => validate::run(harness.as_deref(), targets, deadline, json),
        Command::InstallHarness { url, reference, no_validate, json } => install::run(&url, reference.as_deref(), no_validate, json),
        Command::Harnesses { json } => install::list(json),
        Command::ExportSchema { version, out } => export::run(version, out),
    };
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("afz: {e}");
            std::process::exit(1);
        }
    }
}
