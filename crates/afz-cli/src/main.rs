//! The `afz` command.

mod export;
mod init;
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
    about = "A universal fuzzer for code, services, and APIs",
    long_about = "autofuzz discovers the functions, methods, and routes that your code or service exposes, \
                  calls them with generated inputs to find bugs, and writes a report. The engine contains no \
                  language-specific code; each harness plugin connects it to one language, runtime, or API."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a harness plugin and print the targets it discovers.
    ProbePlugin {
        /// Plugin command, for example `--command node /home/me/harness-js/dist/server.js`.
        #[arg(long, num_args = 1..)]
        command: Vec<String>,
        /// Scan project to discover against. Defaults to the current directory.
        #[arg(long)]
        path: Option<String>,
        /// Print the raw JSON answer instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Run a scan and write the scan result.
    Scan {
        /// Directory to scan. Defaults to the scan project's discovery root.
        path: Option<String>,
        /// Scan project file. Defaults to the nearest autofuzz.json above the scan root.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Budget limits, repeated or comma-separated: `invocations=50000`, `wall=90s`.
        #[arg(long, value_delimiter = ',')]
        budget: Vec<String>,
        /// Seed for every random choice. The same seed and tree replay the same calls.
        #[arg(long)]
        seed: Option<String>,
        /// Earlier scan directory whose corpus this scan starts from.
        #[arg(long)]
        resume: Option<PathBuf>,
        /// JSON file holding `[{"targetId":…,"values":[…]}]` inputs to run before the guided loop.
        #[arg(long)]
        seeds: Option<PathBuf>,
        /// `guided` selects inputs with coverage; `random` ignores the corpus.
        #[arg(long)]
        strategy: Option<String>,
        /// Scan one git change set: `staged`, `unstaged`, `stash`, a revision, or a span such as `v1..HEAD`.
        #[arg(long, value_name = "SELECTOR")]
        git: Option<String>,
        /// Plugin processes to run at once. Overrides `budget.workers`.
        #[arg(long)]
        workers: Option<u32>,
        /// Print only the scan summary, not the progress samples.
        #[arg(long, short)]
        quiet: bool,
    },
    /// Check a harness plugin against the plugin contract and conformance template.
    ValidateHarness {
        /// An installed harness name, or a directory with afz-harness.json. Defaults to the
        /// working directory, so the command validates a checkout when run inside one.
        harness: Option<String>,
        /// Number of discovered targets that `prepare` and `invoke` run on. Defaults to 8.
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
    /// Install a harness plugin from a git repository, and validate it.
    InstallHarness {
        /// Git URL or local path of a harness repository with an afz-harness.json.
        url: String,
        /// Branch, tag, or commit to install. Defaults to the repository's default branch.
        #[arg(long = "ref")]
        reference: Option<String>,
        /// Record the plugin without running the conformance suite.
        #[arg(long)]
        no_validate: bool,
        /// Print the conformance report as JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// List installed harness plugins and the command each one resolves to.
    Harnesses {
        /// Print the records as JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Write the protocol JSON Schemas that this build carries.
    ExportSchema {
        /// Protocol revision to export. Defaults to the newest revision this build speaks.
        version: Option<String>,
        /// Directory to write into. Defaults to `schema` under the working directory.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write a scan project or a harness repository from answers to its questions.
    Init {
        #[command(subcommand)]
        target: init::Target,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::ProbePlugin {
            command,
            path,
            json,
        } => probe::run(command, path, json),
        Command::Scan {
            path,
            config,
            budget,
            seed,
            resume,
            seeds,
            strategy,
            git,
            workers,
            quiet,
        } => scan::run(
            path, config, &budget, seed, resume, seeds, strategy, git, workers, quiet,
        ),
        Command::ValidateHarness {
            harness,
            targets,
            deadline,
            json,
        } => validate::run(harness.as_deref(), targets, deadline, json),
        Command::InstallHarness {
            url,
            reference,
            no_validate,
            json,
        } => install::run(&url, reference.as_deref(), no_validate, json),
        Command::Harnesses { json } => install::list(json),
        Command::ExportSchema { version, out } => export::run(version, out),
        Command::Init { target } => init::run(target),
    };
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("afz: {e}");
            std::process::exit(1);
        }
    }
}
