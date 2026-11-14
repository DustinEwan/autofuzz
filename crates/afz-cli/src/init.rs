//! `afz init`: the guided way to write the two files a scan needs.
//!
//! `afz init project` writes an `autofuzz.json`. `afz init harness` writes an `afz-harness.json`
//! and the protocol schemas a plugin validates against, either inside a clone of the official
//! starter repository or in an empty directory. Each wizard asks its questions in the order a
//! person can answer them, and every answer has a flag, so one command serves a terminal, a
//! script, and CI: a flag skips its prompt, and a question a script cannot answer is refused by
//! naming the flags rather than by hanging on a read.
//!
//! Asking and writing are separate steps throughout - `collect_*` asks, `write_*` writes - so the
//! tests drive the writers with flag-shaped answers and never need a terminal. Prompts go to
//! stderr, which is the terminal dialoguer owns, so stdout carries only the paths written and the
//! next command to run.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use afz_engine::config::{BudgetConfig, HarnessBlock, OutputConfig, ScanProject, Strategy};
use afz_engine::git::git;
use afz_engine::harnesses::{self, Conformance, HarnessManifest};
use afz_protocol::PROTOCOL_VERSION;
use serde::Serialize;

use crate::export;

/// The harness repository `afz init harness --from starter` clones when `--source` says nothing
/// else: a working `sh` plugin, conformance emitter, and test suite to edit into your own.
const STARTER_REPOSITORY: &str = "https://github.com/DustinEwan/autofuzz-harness-starter.git";

/// The renderers this engine writes. The first two are the default, and `ScanProject::validate`
/// refuses any name outside this list, so the menu offers exactly what a scan project may hold.
const RENDERERS: [&str; 3] = ["json", "markdown", "sarif"];
const DEFAULT_RENDERERS: [bool; 3] = [true, true, false];
const STRATEGIES: [&str; 2] = ["guided", "random"];
const DEFAULT_INVOCATIONS: u64 = 100_000;
const DEFAULT_WALL_CLOCK_SECONDS: u64 = 900;
const DEFAULT_DEADLINE_MS: u64 = 1500;

#[derive(clap::Subcommand)]
pub enum Target {
    /// Write a starter scan project: autofuzz.json.
    Project(ProjectArgs),
    /// Write a starter harness repository: afz-harness.json plus the schemas it validates against.
    Harness(HarnessArgs),
}

#[derive(clap::Args, Default)]
pub struct ProjectArgs {
    /// Directory to write autofuzz.json into. Defaults to the working directory.
    path: Option<PathBuf>,
    /// Project name. Defaults to the directory's name.
    #[arg(long)]
    name: Option<String>,
    /// Harness that drives the scan: an installed name, or one you type.
    #[arg(long)]
    harness: Option<String>,
    /// Plugin argv for a harness nobody installed, space-separated with no quoting, for example
    /// "node server.js".
    #[arg(long)]
    command: Option<String>,
    /// Discovery include globs, repeated or comma-separated.
    #[arg(long, value_delimiter = ',')]
    include: Vec<String>,
    /// Discovery exclude globs, repeated or comma-separated.
    #[arg(long, value_delimiter = ',')]
    exclude: Vec<String>,
    /// Directory a scan writes into. Defaults to `.autofuzz`.
    #[arg(long)]
    output_dir: Option<String>,
    /// Output renderers, comma-separated: json, markdown, sarif. Defaults to json,markdown.
    #[arg(long)]
    renderers: Option<String>,
    /// budget.invocations. Defaults to 100000.
    #[arg(long)]
    invocations: Option<u64>,
    /// budget.wallClockMs, asked in seconds. Defaults to 900.
    #[arg(long)]
    wall_clock_seconds: Option<u64>,
    /// `guided` steers with coverage, `random` does not. Defaults to guided.
    #[arg(long)]
    strategy: Option<String>,
}

#[derive(clap::Args, Default)]
pub struct HarnessArgs {
    /// Directory to create and fill. Defaults to the working directory.
    path: Option<PathBuf>,
    /// Scaffold source: `starter` clones the official starter repository, `blank` writes only the
    /// manifest and schemas.
    #[arg(long, value_enum)]
    from: Option<Scaffold>,
    /// Git URL or local path for the starter scaffold. Defaults to the official starter repository.
    #[arg(long)]
    source: Option<String>,
    /// Harness name. Defaults to the directory's name.
    #[arg(long)]
    name: Option<String>,
    /// Protocol revision whose schemas are exported, for example afz/1. Defaults to the newest
    /// this engine speaks.
    #[arg(long)]
    protocol: Option<String>,
    /// Plugin argv, space-separated with no quoting, for example "node plugin/server.mjs".
    #[arg(long)]
    command: Option<String>,
    /// Dependency-fetch argv, space-separated, run only by afz install-harness. Empty to skip.
    #[arg(long)]
    install: Option<String>,
    /// Build argv, space-separated, run by afz install-harness and afz validate-harness. Empty to
    /// skip.
    #[arg(long)]
    build: Option<String>,
    /// Conformance emitter argv, space-separated. An empty value means no conformance block.
    #[arg(long)]
    emitter: Option<String>,
    /// conformance.deadlineMs. Defaults to 1500.
    #[arg(long)]
    deadline_ms: Option<u64>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Scaffold {
    Starter,
    Blank,
}

pub fn run(target: Target) -> Result<(), String> {
    match target {
        Target::Project(args) => write_project(&collect_project(args)?)?,
        Target::Harness(args) => {
            let answers = collect_harness(args)?;
            match answers.from {
                Scaffold::Starter => write_starter(&answers)?,
                Scaffold::Blank => write_harness(&answers)?,
            }
        }
    };
    Ok(())
}

/// One wizard's prompts, so the terminal rule lives in one place.
///
/// A question with a documented default answers itself when there is no terminal, because that
/// default is what a script means by saying nothing. A question with no default - the project
/// name, which harness drives the scan, which scaffold to start from - is refused with a message
/// naming the flag that stands in for it, because guessing those changes what lands on disk.
struct Prompter {
    /// The command the user typed, and the flags that answer this flow's required questions.
    command: &'static str,
    flags: &'static str,
}

impl Prompter {
    fn terminal(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    /// The refusal a non-terminal run gets, and the message every aborted prompt gets: an
    /// interrupted prompt and an unanswerable one want the same remedy.
    fn stuck(&self) -> String {
        format!("afz init needs a terminal to ask its questions; pass {} (see `{} --help`)", self.flags, self.command)
    }

    /// A line of text. With no default, an empty answer is refused in place; without a terminal to
    /// answer, there is nothing to fall back on, so this is where a script stops.
    fn ask(&self, label: &str, default: Option<&str>) -> Result<String, String> {
        let mut input = dialoguer::Input::<String>::new().with_prompt(label);
        input = match default {
            Some(text) if !self.terminal() => return Ok(text.to_string()),
            Some(text) => input.default(text.to_string()),
            None if !self.terminal() => return Err(self.stuck()),
            None => input.allow_empty(false),
        };
        let answer = input.interact_text().map_err(|_| self.stuck())?;
        Ok(answer.trim().to_string())
    }

    /// A line of text whose empty answer is itself an answer: the key is left out of the file.
    /// Without a terminal, skipping is what a silent flag means, so this never blocks.
    fn ask_optional(&self, label: &str) -> Result<String, String> {
        if !self.terminal() {
            return Ok(String::new());
        }
        let answer = dialoguer::Input::<String>::new()
            .with_prompt(label)
            .allow_empty(true)
            .interact_text()
            .map_err(|_| self.stuck())?;
        Ok(answer.trim().to_string())
    }

    /// A whole number. The validator re-asks in place, so a typo is corrected where it was made
    /// rather than ending the wizard.
    fn ask_number(&self, label: &str, default: u64) -> Result<u64, String> {
        if !self.terminal() {
            return Ok(default);
        }
        let answer = dialoguer::Input::<String>::new()
            .with_prompt(label)
            .default(default.to_string())
            .validate_with(|text: &String| -> Result<(), String> {
                if text.trim().parse::<u64>().is_ok() {
                    Ok(())
                } else {
                    Err(format!("\"{text}\" is not a whole number"))
                }
            })
            .interact_text()
            .map_err(|_| self.stuck())?;
        Ok(answer.trim().parse::<u64>().expect("the validator accepted only whole numbers"))
    }

    /// A menu whose answer changes what lands on disk, so a script has to give it.
    fn choose(&self, label: &str, options: &[&str]) -> Result<usize, String> {
        if !self.terminal() {
            return Err(self.stuck());
        }
        self.pick(label, options, 0)
    }

    /// A menu whose first row is the documented default, which is what a script gets unasked.
    fn choose_with(&self, label: &str, options: &[&str], default: usize) -> Result<usize, String> {
        if !self.terminal() {
            return Ok(default);
        }
        self.pick(label, options, default)
    }

    fn pick(&self, label: &str, options: &[&str], default: usize) -> Result<usize, String> {
        dialoguer::Select::new()
            .with_prompt(label)
            .items(options)
            .default(default)
            .interact()
            .map_err(|_| self.stuck())
    }

    /// A multi-choice menu; the checked rows are the documented default.
    fn choose_many(&self, label: &str, options: &[&str], defaults: &[bool]) -> Result<Vec<usize>, String> {
        if !self.terminal() {
            return Ok(defaults.iter().enumerate().filter(|(_, on)| **on).map(|(row, _)| row).collect());
        }
        dialoguer::MultiSelect::new()
            .with_prompt(label)
            .items(options)
            .defaults(defaults)
            .interact()
            .map_err(|_| self.stuck())
    }
}

/// The scan project the wizards write, before it is a file.
#[derive(Debug)]
struct ProjectAnswers {
    path: PathBuf,
    name: String,
    harness: String,
    /// The plugin argv, present only for a harness `afz install-harness` did not record.
    command: Option<Vec<String>>,
    include: Vec<String>,
    exclude: Vec<String>,
    output_dir: String,
    renderers: Vec<String>,
    invocations: u64,
    wall_clock_ms: u64,
    strategy: String,
}

fn collect_project(args: ProjectArgs) -> Result<ProjectAnswers, String> {
    let prompts = Prompter {
        command: "afz init project",
        flags: "--name, --harness, and --command",
    };
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let name = match args.name {
        Some(name) => name.trim().to_string(),
        None => prompts.ask("Project name", path.file_name().and_then(|name| name.to_str()))?,
    };
    let (harness, command) = ask_harness(&prompts, args.harness, args.command)?;
    let include = match !args.include.is_empty() {
        true => args.include,
        false => split_list(&prompts.ask_optional("Include globs, comma-separated (empty for the harness's own default)")?),
    };
    let exclude = match !args.exclude.is_empty() {
        true => args.exclude,
        false => split_list(&prompts.ask_optional("Exclude globs, comma-separated (empty for none)")?),
    };
    let output_dir = match args.output_dir {
        Some(dir) => dir,
        None => prompts.ask("Output directory", Some(".autofuzz"))?,
    };
    let renderers = match args.renderers {
        Some(list) => split_list(&list),
        None => prompts
            .choose_many("Which renderers write the scan result?", &RENDERERS, &DEFAULT_RENDERERS)?
            .into_iter()
            .map(|row| RENDERERS[row].to_string())
            .collect(),
    };
    let invocations = match args.invocations {
        Some(n) => n,
        None => prompts.ask_number("Invocations", DEFAULT_INVOCATIONS)?,
    };
    let wall_clock_seconds = match args.wall_clock_seconds {
        Some(seconds) => seconds,
        None => prompts.ask_number("Wall clock, seconds", DEFAULT_WALL_CLOCK_SECONDS)?,
    };
    let wall_clock_ms =
        wall_clock_seconds.checked_mul(1_000).ok_or("`budget.wallClockMs` would overflow: name a smaller wall clock")?;
    let strategy = match args.strategy {
        Some(strategy) => strategy,
        None => STRATEGIES[prompts.choose_with("Which strategy steers the scan?", &STRATEGIES, 0)?].to_string(),
    };
    Ok(ProjectAnswers {
        path,
        name,
        harness,
        command,
        include,
        exclude,
        output_dir,
        renderers,
        invocations,
        wall_clock_ms,
        strategy,
    })
}

/// The harness a scan drives: a name, plus the argv to start it when the registry has no record
/// for that name. An installed name is recorded alone, so the launch stays the one
/// `afz install-harness` validated.
fn ask_harness(prompts: &Prompter, named: Option<String>, command: Option<String>) -> Result<(String, Option<Vec<String>>), String> {
    let installed: Vec<String> = harnesses::all().into_iter().map(|harness| harness.name).collect();
    let typed = "Other (type a name)";
    let name = match named {
        Some(name) => name.trim().to_string(),
        // A registry with nothing in it has no menu to offer, so the name is typed.
        None if installed.is_empty() => prompts.ask("Which harness drives the scan?", None)?,
        None => {
            let mut options: Vec<&str> = installed.iter().map(String::as_str).collect();
            options.push(typed);
            let picked = prompts.choose("Which harness drives the scan?", &options)?;
            if options[picked] == typed {
                prompts.ask("Harness name", None)?
            } else {
                installed[picked].clone()
            }
        }
    };
    if let Some(text) = command {
        return Ok((name, Some(argv_of(&text))));
    }
    if harnesses::lookup(&name).is_some() {
        return Ok((name, None));
    }
    let text = prompts.ask("Plugin command (program and arguments)", None)?;
    let argv = argv_of(&text);
    if argv.is_empty() {
        return Err(command_required(&name));
    }
    Ok((name, Some(argv)))
}

/// The engine's own wording, so the wizard refuses an unlaunchable harness the way a scan does.
fn command_required(harness: &str) -> String {
    format!(
        "`command` is required for harness \"{harness}\": the engine does not guess a runtime; install one with `afz install-harness <git-url>`"
    )
}

/// The scan project as a file: the engine's own block types, declared in the key order of the
/// engine's own `autofuzz.json`, which is the order a person reads a new project file in.
/// `oracles` is `{}` because the engine's defaults are the recommended starting point, and a
/// starter file that spells every default out hides the keys that are worth a decision.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectFile<'a> {
    project: &'a str,
    harness: HarnessBlock,
    oracles: serde_json::Value,
    budget: BudgetConfig,
    output: OutputConfig,
}

/// Write `<path>/autofuzz.json`. The document is read back as a [`ScanProject`] and validated
/// before anything is written, so `afz init project` cannot leave a file `afz scan` rejects.
fn write_project(answers: &ProjectAnswers) -> Result<PathBuf, String> {
    let dir = absolute(&answers.path)?;
    let path = dir.join("autofuzz.json");
    if path.exists() {
        return Err(format!("`autofuzz.json` already exists in {}: edit it, or delete it to start over", dir.display()));
    }
    let document = ProjectFile {
        project: &answers.name,
        harness: HarnessBlock {
            name: answers.harness.clone(),
            include: list_or_none(&answers.include),
            exclude: list_or_none(&answers.exclude),
            command: answers.command.clone(),
            ..Default::default()
        },
        oracles: serde_json::json!({}),
        budget: BudgetConfig {
            invocations: answers.invocations,
            wall_clock_ms: answers.wall_clock_ms,
            workers: 0,
            max_corpus: 2048,
            strategy: strategy_of(&answers.strategy)?,
        },
        output: OutputConfig { dir: answers.output_dir.clone(), renderers: answers.renderers.clone() },
    };
    let written: ScanProject = serde_json::from_value(serde_json::to_value(&document).map_err(|e| format!("cannot serialize the scan project: {e}"))?)
        .map_err(|e| format!("the scan project afz init built does not parse: {e}"))?;
    written.validate().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(&document).map_err(|e| format!("cannot serialize the scan project: {e}"))?;
    std::fs::write(&path, format!("{text}\n")).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("{}", path.display());
    println!("next: afz scan {}", dir.display());
    Ok(path)
}

fn strategy_of(text: &str) -> Result<Strategy, String> {
    serde_json::from_value(serde_json::Value::String(text.to_string()))
        .map_err(|_| format!("`budget.strategy` is \"guided\" or \"random\", not \"{text}\""))
}

/// The harness repository the wizards write, before it is a directory.
#[derive(Debug)]
struct HarnessAnswers {
    path: PathBuf,
    from: Scaffold,
    /// Clone source, asked only in starter mode; `None` means the official repository.
    source: Option<String>,
    name: String,
    protocol: String,
    /// Blank mode only: starter mode ships a working plugin, emitter, install, and build.
    command: Vec<String>,
    install: Vec<Vec<String>>,
    build: Vec<Vec<String>>,
    conformance: Option<Conformance>,
}

fn collect_harness(args: HarnessArgs) -> Result<HarnessAnswers, String> {
    let prompts = Prompter {
        command: "afz init harness",
        flags: "--name, --from, and --command",
    };
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let from = match args.from {
        Some(from) => from,
        None => match prompts.choose(
            "Start from the official starter scaffold, or begin blank?",
            &[
                "starter - a working sh plugin, emitter, and tests to edit into your own",
                "blank - manifest and exported schemas only",
            ],
        )? {
            0 => Scaffold::Starter,
            _ => Scaffold::Blank,
        },
    };
    let name = match args.name {
        Some(name) => name.trim().to_string(),
        None => prompts.ask("Harness name", path.file_name().and_then(|name| name.to_str()))?,
    };
    check_name(&name)?;
    let revisions = revisions();
    let options: Vec<&str> = revisions.iter().map(String::as_str).collect();
    let protocol = match args.protocol {
        Some(protocol) => protocol,
        None => revisions[prompts.choose_with("Which protocol revision will the plugin speak?", &options, 0)?].clone(),
    };
    if let Scaffold::Starter = from {
        return Ok(HarnessAnswers {
            path,
            from,
            source: args.source,
            name,
            protocol,
            command: Vec::new(),
            install: Vec::new(),
            build: Vec::new(),
            conformance: None,
        });
    }
    let command = match args.command {
        Some(text) => argv_of(&text),
        None => argv_of(&prompts.ask("Plugin command (program and arguments)", None)?),
    };
    if command.is_empty() {
        return Err("`command` is required: a harness manifest names the plugin `afz validate-harness` starts".to_string());
    }
    let install = one_argv(args.install, &prompts, "Dependency install command (afz install-harness runs it; empty to skip)")?;
    let build = one_argv(args.build, &prompts, "Build command (afz install-harness and afz validate-harness run it; empty to skip)")?;
    let emitter = match args.emitter {
        Some(text) => argv_of(&text),
        None => argv_of(&prompts.ask_optional("Conformance emitter command (empty if the harness ships none)")?),
    };
    let conformance = if emitter.is_empty() {
        None
    } else {
        let deadline_ms = match args.deadline_ms {
            Some(ms) => ms,
            None => prompts.ask_number("Conformance deadline, milliseconds", DEFAULT_DEADLINE_MS)?,
        };
        Some(Conformance { emitter, deadline_ms: Some(deadline_ms) })
    };
    Ok(HarnessAnswers { path, from, source: args.source, name, protocol, command, install, build, conformance })
}

/// The protocol revisions this build carries, newest first: `PROTOCOL_VERSION` is the newest one
/// the engine speaks, and the table may hold older ones it still validates.
fn revisions() -> Vec<String> {
    let mut revisions = vec![PROTOCOL_VERSION.to_string()];
    revisions.extend(
        afz_conformance::schema_versions()
            .into_iter()
            .filter(|version| *version != PROTOCOL_VERSION)
            .map(str::to_string),
    );
    revisions
}

/// The name rules `harnesses::lookup` will apply when a scan resolves this harness: the registry
/// stores one directory per name, so a separator or a `..` in the name would reach outside it.
fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a harness name must not be empty".to_string());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(format!(
            "harness name \"{name}\" must not contain `/`, `\\`, or `..`: afz install-harness stores each harness in a directory of that name"
        ));
    }
    Ok(())
}

/// One optional command line, split into argv: `install` and `build` are lists of commands, and
/// the wizard asks for one of each. An empty answer contributes no command.
fn one_argv(given: Option<String>, prompts: &Prompter, label: &str) -> Result<Vec<Vec<String>>, String> {
    let text = match given {
        Some(text) => text,
        None => prompts.ask_optional(label)?,
    };
    let argv = argv_of(&text);
    Ok(if argv.is_empty() { Vec::new() } else { vec![argv] })
}

/// Write a blank harness repository: the manifest, then the schemas of the chosen revision.
fn write_harness(answers: &HarnessAnswers) -> Result<PathBuf, String> {
    export::schema_set(&answers.protocol)?;
    if answers.command.is_empty() {
        return Err("`command` is required: a harness manifest names the plugin `afz validate-harness` starts".to_string());
    }
    let dir = absolute(&answers.path)?;
    if dir.join("afz-harness.json").exists() {
        return Err(format!("`afz-harness.json` already exists in {}: edit it, or delete it to start over", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let manifest = HarnessManifest {
        name: answers.name.clone(),
        install: answers.install.clone(),
        build: answers.build.clone(),
        command: answers.command.clone(),
        conformance: answers.conformance.clone(),
    };
    let path = write_manifest(&dir, &manifest)?;
    export::run(Some(answers.protocol.clone()), Some(dir.join("schema")))?;
    println!("{}", path.display());
    println!("next: write the plugin and the emitter, then run afz validate-harness {}", dir.display());
    Ok(path)
}

/// Clone the starter scaffold, rename its manifest, and export the schemas beside it.
///
/// The clone needs the directory to be absent or empty, because git refuses a non-empty target and
/// a half-written scaffold is worse than none. Writing a manifest into a directory that already has
/// files is a different job - repairing an existing plugin checkout - and only blank mode allows it.
fn write_starter(answers: &HarnessAnswers) -> Result<PathBuf, String> {
    export::schema_set(&answers.protocol)?;
    let dir = absolute(&answers.path)?;
    if dir.exists() && !empty_dir(&dir)? {
        return Err(format!(
            "{} is not empty: afz init harness --from starter writes into a new or empty directory",
            dir.display()
        ));
    }
    let source = local_source(answers.source.as_deref().unwrap_or(STARTER_REPOSITORY));
    let parent = dir.parent().ok_or_else(|| format!("{} has no parent directory to clone into", dir.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    git(parent, &["clone", &source, &dir.display().to_string()], &[]).map_err(|e| format!("clone {source} failed: {e}"))?;
    // The scaffold ships code, not history: the author starts their own repository.
    std::fs::remove_dir_all(dir.join(".git")).map_err(|e| format!("cannot remove {}: {e}", dir.join(".git").display()))?;
    rename_manifest(&dir, &answers.name)?;
    export::run(Some(answers.protocol.clone()), Some(dir.join("schema")))?;
    println!("{}", dir.join("afz-harness.json").display());
    println!("next: edit plugin/ and conformance/, then run afz validate-harness {}", dir.display());
    Ok(dir.join("afz-harness.json"))
}

/// Set the `name` of a cloned manifest and nothing else: the starter's `command`, `install`,
/// `build`, and `conformance` work as shipped. Round-tripping through [`HarnessManifest`] keeps
/// the file the shape `afz validate-harness` reads.
fn rename_manifest(dir: &Path, name: &str) -> Result<HarnessManifest, String> {
    let mut manifest = harnesses::read_manifest(dir)?;
    manifest.name = name.to_string();
    write_manifest(dir, &manifest)?;
    Ok(manifest)
}

fn write_manifest(dir: &Path, manifest: &HarnessManifest) -> Result<PathBuf, String> {
    let path = dir.join("afz-harness.json");
    let text = serde_json::to_string_pretty(manifest).map_err(|e| format!("cannot serialize the harness manifest: {e}"))?;
    std::fs::write(&path, format!("{text}\n")).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn empty_dir(dir: &Path) -> Result<bool, String> {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))
}

/// Absolutize a clone source that names a local path, exactly as `afz install-harness` does: git
/// resolves a relative source against the directory it was run with `-C`, not the shell's, so an
/// offline clone of a local checkout needs the absolute name.
fn local_source(source: &str) -> String {
    match std::path::absolute(source) {
        Ok(absolute) if Path::new(source).exists() => absolute.display().to_string(),
        _ => source.to_string(),
    }
}

/// The directory as an absolute path, so every message and every printed path says where the file
/// landed regardless of where `afz` was run.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}

/// A command line as argv. The manifest holds argv, and the flags and prompts hold one string, so
/// this is the whole quoting story: spaces separate arguments and nothing is quoted.
fn argv_of(text: &str) -> Vec<String> {
    text.split_ascii_whitespace().map(str::to_string).collect()
}

/// A comma-separated answer as a list, with the blanks a trailing comma leaves removed.
fn split_list(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|item| !item.is_empty()).map(str::to_string).collect()
}

/// An empty glob list is no scope limit at all, so the key is left out rather than written as
/// `[]`, which a reader would have to learn means the same thing.
fn list_or_none(list: &[String]) -> Option<Vec<String>> {
    (!list.is_empty()).then(|| list.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("afz-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn project_answers(path: PathBuf) -> ProjectAnswers {
        ProjectAnswers {
            path,
            name: "demo".to_string(),
            harness: "js-ts".to_string(),
            command: Some(argv_of("node server.js")),
            include: Vec::new(),
            exclude: Vec::new(),
            output_dir: ".autofuzz".to_string(),
            renderers: vec!["json".to_string(), "markdown".to_string()],
            invocations: DEFAULT_INVOCATIONS,
            wall_clock_ms: DEFAULT_WALL_CLOCK_SECONDS * 1_000,
            strategy: "guided".to_string(),
        }
    }

    fn harness_answers(path: PathBuf) -> HarnessAnswers {
        HarnessAnswers {
            path,
            from: Scaffold::Blank,
            source: None,
            name: "demo-h".to_string(),
            protocol: PROTOCOL_VERSION.to_string(),
            command: argv_of("sh plugin/server.sh"),
            install: Vec::new(),
            build: Vec::new(),
            conformance: Some(Conformance { emitter: argv_of("sh conformance/emit.sh"), deadline_ms: Some(DEFAULT_DEADLINE_MS) }),
        }
    }

    #[test]
    fn a_written_scan_project_is_a_file_the_engine_loads() {
        let dir = temp_dir("project");
        let path = write_project(&project_answers(dir.clone())).expect("a complete answer set writes the file");
        let text = std::fs::read_to_string(&path).expect("the file is there");
        assert!(text.contains("\"wallClockMs\": 900000"), "camelCase keys, seconds turned into ms:\n{text}");
        assert!(text.contains("\"project\": \"demo\""), "the name is the one asked for:\n{text}");
        let keys: Vec<&str> = ["project", "harness", "oracles", "budget", "output"]
            .iter()
            .filter(|key| text.contains(&format!("\"{key}\"")))
            .copied()
            .collect();
        assert_eq!(keys, vec!["project", "harness", "oracles", "budget", "output"], "the file reads in the engine's own key order:\n{text}");
        let loaded = afz_engine::config::load(&path).expect("afz scan can load what afz init wrote");
        assert_eq!(loaded.root, std::path::absolute(&dir).expect("the directory resolves"), "the project root is the directory init was pointed at");
        let harness = loaded.project.harness.expect("a scan project names a harness");
        assert_eq!(harness.name, "js-ts");
        assert_eq!(harness.command, Some(argv_of("node server.js")));
        assert_eq!(harness.include, None, "an empty glob answer omits the key");
        assert_eq!(loaded.project.output.renderers, vec!["json".to_string(), "markdown".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_random_strategy_answer_round_trips_to_the_engine_strategy() {
        let dir = temp_dir("strategy");
        let mut answers = project_answers(dir.clone());
        answers.strategy = "random".to_string();
        answers.renderers = vec![RENDERERS[2].to_string()];
        answers.include = split_list("**/*.ts, src/**");
        let path = write_project(&answers).expect("random is a strategy the engine knows");
        let loaded = afz_engine::config::load(&path).expect("the file loads");
        assert_eq!(loaded.project.budget.strategy, Strategy::Random);
        assert_eq!(loaded.project.harness.expect("harness block").include, Some(split_list("**/*.ts, src/**")));
        let bogus = write_project(&{
            let mut answers = project_answers(temp_dir("strategy-bogus"));
            answers.strategy = "coverage".to_string();
            answers
        })
        .expect_err("a strategy the engine does not have is refused before anything is written");
        assert!(bogus.contains("coverage"), "the message names the answer it rejected: {bogus}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_existing_scan_project_is_left_exactly_as_it_was() {
        let dir = temp_dir("overwrite");
        std::fs::create_dir_all(&dir).expect("the directory is creatable");
        let existing = dir.join("autofuzz.json");
        std::fs::write(&existing, "{ the author's own bytes }").expect("the file is writable");
        let err = write_project(&project_answers(dir.clone())).expect_err("init never overwrites a scan project");
        assert!(err.contains("already exists"), "the refusal says what to do instead: {err}");
        assert_eq!(std::fs::read_to_string(&existing).expect("still there"), "{ the author's own bytes }");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_blank_harness_is_a_manifest_and_the_schemas_it_validates_against() {
        let dir = temp_dir("blank");
        let manifest_path = write_harness(&harness_answers(dir.clone())).expect("a complete answer set writes the repository");
        let manifest = harnesses::read_manifest(&dir).expect("afz validate-harness can read the manifest");
        assert_eq!(manifest.name, "demo-h");
        assert_eq!(manifest.command, argv_of("sh plugin/server.sh"));
        assert_eq!(
            manifest.conformance,
            Some(Conformance { emitter: argv_of("sh conformance/emit.sh"), deadline_ms: Some(DEFAULT_DEADLINE_MS) })
        );
        assert_eq!(manifest.install, Vec::<Vec<String>>::new(), "a skipped install answer writes []");
        let schemas = std::fs::read_dir(dir.join("schema")).expect("the schemas are exported beside the manifest");
        assert_eq!(schemas.count(), 22, "the whole embedded set for the chosen revision");
        assert!(dir.join("schema/handshake.json").exists());
        assert!(manifest_path.ends_with("afz-harness.json"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_revision_names_itself_and_writes_no_manifest() {
        let dir = temp_dir("revision");
        let mut answers = harness_answers(dir.clone());
        answers.protocol = "afz/9".to_string();
        let err = write_harness(&answers).expect_err("this engine carries no afz/9 schema set");
        assert!(err.contains("afz/9"), "the message names the revision that was asked for: {err}");
        assert!(!dir.join("afz-harness.json").exists(), "a refused revision writes no manifest");
        let mut starter = harness_answers(temp_dir("revision-starter"));
        starter.from = Scaffold::Starter;
        starter.protocol = "afz/9".to_string();
        assert!(write_starter(&starter).expect_err("starter mode checks the revision before it clones").contains("afz/9"));
    }

    #[test]
    fn renaming_a_cloned_manifest_changes_only_the_name() {
        let dir = temp_dir("starter");
        std::fs::create_dir_all(&dir).expect("the directory is creatable");
        std::fs::write(
            dir.join("afz-harness.json"),
            r#"{
  "name": "starter",
  "command": ["sh", "plugin/server.sh"],
  "conformance": { "emitter": ["sh", "conformance/emit.sh"], "deadlineMs": 1500 }
}"#,
        )
        .expect("the manifest is writable");
        let manifest = rename_manifest(&dir, "my-h").expect("the starter manifest renames");
        assert_eq!(manifest.name, "my-h");
        let read = harnesses::read_manifest(&dir).expect("the renamed manifest still parses");
        assert_eq!(read.command, argv_of("sh plugin/server.sh"), "the shipped launch is untouched");
        assert_eq!(read.conformance, manifest.conformance, "the shipped conformance block is untouched");
        assert!(read.install.is_empty() && read.build.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_question_with_no_default_stops_a_run_that_has_no_terminal() {
        // A test process is given no terminal, so this is the scripted path.
        let err = collect_project(ProjectArgs::default()).expect_err("the project name has no default for `.`");
        assert!(err.contains("needs a terminal"), "the refusal says why: {err}");
        assert!(err.contains("--name"), "the refusal names the flags that answer it: {err}");
        let err = collect_harness(HarnessArgs::default()).expect_err("the scaffold choice has no default");
        assert!(err.contains("--from"), "the refusal names this flow's flags: {err}");
    }

    #[test]
    fn flags_answer_every_question_a_terminal_would_be_asked() {
        let dir = temp_dir("flags");
        let answers = collect_project(ProjectArgs {
            path: Some(dir.clone()),
            name: Some("demo".to_string()),
            harness: Some("js-ts".to_string()),
            command: Some("node server.js".to_string()),
            include: split_list("**/*.ts"),
            renderers: Some("json,markdown".to_string()),
            ..Default::default()
        })
        .expect("with the required answers given, nothing is asked");
        assert_eq!(answers.harness, "js-ts");
        assert_eq!(answers.command, Some(argv_of("node server.js")));
        assert_eq!(answers.include, vec!["**/*.ts".to_string()]);
        assert_eq!(answers.exclude, Vec::<String>::new(), "an unanswered optional answer is no key");
        assert_eq!(answers.output_dir, ".autofuzz", "the defaults a terminal would have shown are the defaults a script gets");
        assert_eq!(answers.wall_clock_ms, 900_000);
        assert_eq!(answers.strategy, "guided");
        let harness = collect_harness(HarnessArgs {
            path: Some(dir.clone()),
            from: Some(Scaffold::Blank),
            name: Some("demo-h".to_string()),
            command: Some("sh plugin/server.sh".to_string()),
            emitter: Some("sh conformance/emit.sh".to_string()),
            ..Default::default()
        })
        .expect("blank mode needs no terminal either");
        assert_eq!(harness.protocol, PROTOCOL_VERSION, "the newest revision is the documented default");
        assert_eq!(harness.conformance.expect("an emitter answer makes the block").deadline_ms, Some(DEFAULT_DEADLINE_MS));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_harness_name_that_cannot_be_a_directory_is_refused() {
        let dir = temp_dir("name");
        for name in ["", "a/b", "a\\b", "a..b"] {
            let err = collect_harness(HarnessArgs {
                path: Some(dir.clone()),
                from: Some(Scaffold::Blank),
                name: Some(name.to_string()),
                command: Some("sh plugin/server.sh".to_string()),
                ..Default::default()
            })
            .expect_err(&format!("{name:?} is not a name the registry can store"));
            assert!(err.contains(name) || err.contains("must not"), "the message names the answer it rejected: {err}");
        }
    }
}
