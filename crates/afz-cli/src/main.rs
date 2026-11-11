//! The `afz` command.

mod probe;

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
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::ProbePlugin { command, path, json } => probe::run(command, path, json),
    };
    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("afz: {e}");
            std::process::exit(1);
        }
    }
}
