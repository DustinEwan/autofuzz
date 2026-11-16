//! `afz scan`: run one coverage-guided scan and print what it found.
//!
//! The command resolves the scan project, applies the flags the user would change between runs,
//! and hands the rest to [`afz_engine::scan`]. Everything it prints comes from the scan result it
//! just wrote, so the terminal and `scan.jsonl` cannot disagree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use afz_engine::config::{self, ConfigError, LoadedConfig, Strategy};
use afz_engine::report::scanresult::StoppedReason;
use afz_engine::scan::{self, Options, Progress, Seed, MAX_SEEDS};

/// Run the command. `path` is the directory to scan; without it the scan project's own root.
#[allow(clippy::too_many_arguments)]
pub fn run(
    path: Option<String>,
    config_path: Option<PathBuf>,
    budget: &[String],
    seed: Option<String>,
    resume: Option<PathBuf>,
    seeds: Option<PathBuf>,
    strategy: Option<String>,
    git: Option<String>,
    workers: Option<u32>,
    quiet: bool,
) -> Result<(), String> {
    // Absolute before it reaches the plugin: the plugin's working directory is the scan
    // project's, not the shell's, and a relative path would resolve against the wrong place.
    let root = std::path::absolute(
        path.as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().expect("a working directory")),
    )
    .map_err(|e| format!("cannot resolve the scan path: {e}"))?;
    let loaded = load_config(config_path.as_deref(), &root)?;
    let scan_root = match &path {
        Some(_) => root.clone(),
        None => loaded.discovery_scope().map_err(|e| e.to_string())?.root,
    };
    let mut options = Options::new(loaded.clone(), scan_root.clone());
    options.out_dir = loaded.root.join(&loaded.project.output.dir);
    options.seed = parse_seed(seed.as_deref())?;
    if let Some(name) = &strategy {
        options.strategy = match name.as_str() {
            "guided" => Strategy::Guided,
            "random" => Strategy::Random,
            other => {
                return Err(format!(
                    "unknown strategy \"{other}\". Use guided or random."
                ))
            }
        };
    }
    apply_budget(&mut options, budget)?;
    if let Some(count) = workers {
        options.workers = count.max(1) as usize;
    }
    if let Some(dir) = &resume {
        if !dir.is_dir() {
            return Err(format!("--resume {} is not a directory.", dir.display()));
        }
        options.resume = Some(dir.clone());
    }
    // Read before the banner, so a file the user has to fix is reported before the run announces
    // the budget it will not spend.
    if let Some(file) = &seeds {
        options.seeds = read_seeds(file)?;
    }

    // Parsed before the banner and before preflight, so a bad selector is reported as the first
    // line and a scope outside a repository is refused by the same check the engine runs.
    if let Some(text) = git.as_deref() {
        options.scope = Some(afz_engine::git::parse(text).map_err(|e| e.0)?);
    }

    // Before the banner: a scan the engine will refuse should report the reason as its first line,
    // not after it has announced 12 workers and a budget it will never spend.
    scan::preflight(&options).map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    install_interrupt(&stop);
    // The flag `Options::new` installed is a different `Arc` (crates/afz-engine/src/scan.rs),
    // so without this assignment Ctrl-C would print the graceful-stop message and the engine
    // would never see it.
    options.stop = Arc::clone(&stop);

    if !quiet {
        println!("Scanning {}", scan_root.display());
        println!(
            "  {} workers, {}, seed 0x{:08x}, budget {} invocations / {} s",
            options.workers,
            match options.strategy {
                Strategy::Guided => "guided",
                Strategy::Random => "random",
            },
            options.seed,
            options.invocations,
            options.wall_clock.as_secs_f64(),
        );
        if !options.seeds.is_empty() {
            println!("  {} seeded inputs", options.seeds.len());
        }
    }
    let mut on_progress = |sample: &Progress| {
        if !quiet {
            let noun = if sample.findings == 1 {
                "finding"
            } else {
                "findings"
            };
            println!(
                "  {} invocations, coverage {}/{} edges ({:.1}%), {} {noun}",
                sample.invocations,
                sample.coverage_edges,
                sample.coverage_total,
                sample.coverage_ratio * 100.0,
                sample.findings
            );
        }
    };
    let report = scan::run(&options, &mut on_progress).map_err(|e| e.to_string())?;

    print_summary(&report);
    Ok(())
}

/// Find and validate the scan project: `--config`, or the nearest `autofuzz.json` above the scan
/// root, which is what makes `afz scan path/to/package` work inside a repository that declares one.
fn load_config(explicit: Option<&Path>, scan_root: &Path) -> Result<LoadedConfig, String> {
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None => config::discover(scan_root).map_err(|e| match e {
            ConfigError::Message(message) => format!(
                "{message}\ncreate {} in the directory you are scanning, or pass --config <path>",
                config::CONFIG_FILE_NAME
            ),
            other => other.to_string(),
        })?,
    };
    config::load(&path).map_err(|e| e.to_string())
}

/// `--seed 0x5e3a91c4`, or a decimal, or nothing: no seed means 0, which is a seed like any other
/// and keeps a run without the flag reproducible.
fn parse_seed(text: Option<&str>) -> Result<u64, String> {
    let Some(text) = text else { return Ok(0) };
    let trimmed = text.trim();
    let (radix, digits) = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(rest) => (16, rest),
        None => (10, trimmed),
    };
    u64::from_str_radix(digits, radix).map_err(|_| {
        format!("--seed {text} is not a number. Use a decimal number or 0x-prefixed hex.")
    })
}

/// The caller's seeds: a JSON array of `{"targetId":…,"values":[…]}` inputs the engine drives
/// before the guided loop starts.
///
/// A previous scan's `corpus/*.json` files are not seed files: they carry the `edges` their input
/// earned, which [`Seed`] refuses, because the engine measures an input's digest rather than
/// trusting a recorded one. `--resume` is how a scan continues from a corpus.
fn read_seeds(file: &Path) -> Result<Vec<Seed>, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("--seeds {} could not be read: {e}", file.display()))?;
    let seeds: Vec<Seed> = serde_json::from_str(&text).map_err(|e| {
        format!(
            "--seeds {} is not an array of {{\"targetId\":…,\"values\":[…]}}: {e}",
            file.display()
        )
    })?;
    if seeds.len() > MAX_SEEDS {
        return Err(format!(
            "--seeds accepts at most {MAX_SEEDS} inputs, {} were in {}.",
            seeds.len(),
            file.display()
        ));
    }
    Ok(seeds)
}

/// Apply `--budget invocations=50000` and `--budget wall=90s`, repeated or comma-separated.
fn apply_budget(options: &mut Options, entries: &[String]) -> Result<(), String> {
    for entry in entries.iter().flat_map(|e| e.split(',')) {
        let (key, value) = entry.split_once('=').ok_or_else(|| {
            format!("--budget {entry} is not key=value. Keys are invocations and wall.")
        })?;
        match key.trim() {
            "invocations" => {
                options.invocations = value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| format!("--budget invocations={value} is not a whole number."))?;
            }
            "wall" => options.wall_clock = parse_duration(value.trim())?,
            other => {
                return Err(format!(
                    "--budget key \"{other}\" is unknown. Use invocations or wall."
                ))
            }
        }
    }
    if options.invocations == 0 {
        return Err("--budget invocations=0 would run no calls.".to_string());
    }
    if options.wall_clock.is_zero() {
        return Err("--budget wall=0 would run no calls.".to_string());
    }
    Ok(())
}

/// `90s`, `1500ms`, `10m`, `1h`, or a bare number of seconds.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let invalid =
        || format!("--budget wall={text} is not a duration. Use 1500ms, 90s, 10m, or 1h.");
    let digits =
        |unit: &str| -> Option<u64> { text.strip_suffix(unit)?.trim().parse::<u64>().ok() };
    if let Some(ms) = digits("ms") {
        return Ok(Duration::from_millis(ms));
    }
    if let Some(seconds) = digits("s") {
        return Ok(Duration::from_secs(seconds));
    }
    if let Some(minutes) = digits("m") {
        return Ok(Duration::from_secs(minutes * 60));
    }
    if let Some(hours) = digits("h") {
        return Ok(Duration::from_secs(hours * 3_600));
    }
    if let Ok(seconds) = text.parse::<u64>() {
        return Ok(Duration::from_secs(seconds));
    }
    Err(invalid())
}

/// Ctrl-C sets the flag between batches; the engine then writes the scan result it has, with
/// `stoppedReason: "interrupted"`. A second Ctrl-C ends the process without a result, which is
/// what a user holding the key down is asking for.
fn install_interrupt(stop: &Arc<AtomicBool>) {
    let flag = Arc::clone(stop);
    let _ = ctrlc::set_handler(move || {
        if flag.swap(true, Ordering::Relaxed) {
            eprintln!("afz: quitting without a scan result.");
            std::process::exit(130);
        }
        eprintln!("afz: stopping after the current batch and writing the scan result. Press Ctrl-C again to quit without a result.");
    });
}

fn print_summary(report: &scan::ScanReport) {
    let document = &report.document;
    println!();
    println!(
        "Scan {} written to {}",
        document.scan_id,
        report.dir.display()
    );
    println!(
        "  {} invocations, {} of {} targets fuzzable, coverage {} edges ({:.1}%), corpus {}, stopped by {}",
        document.execution.invocations,
        document.inventory.fuzzable,
        document.inventory.discovered,
        document.execution.coverage_edges,
        document.execution.coverage_ratio * 100.0,
        document.execution.corpus_size,
        spelling(&document.execution.stopped_reason)
    );
    if document.execution.restarts > 0
        || document.execution.unattempted > 0
        || !document.execution.disabled_targets.is_empty()
    {
        println!(
            "  {} plugin restarts, {} unattempted invocations, {} disabled targets",
            document.execution.restarts,
            document.execution.unattempted,
            document.execution.disabled_targets.len()
        );
    }
    if let Some(suppressed) = document.findings_suppressed {
        if suppressed > 0 {
            println!("  {suppressed} finding(s) suppressed as already reported.");
        }
    }
    if !document.execution.disabled_targets.is_empty() {
        eprintln!(
            "warning: {} target(s) were disabled after repeated plugin process loss; their planned invocations were dropped (execution.disabledTargets)",
            document.execution.disabled_targets.len()
        );
    }
    if document.findings.is_empty() {
        println!("  No finding met the configured oracles.");
        return;
    }
    let noun = if document.findings.len() == 1 {
        "finding"
    } else {
        "findings"
    };
    println!("  {} {noun}:", document.findings.len());
    for (index, finding) in document.findings.iter().enumerate() {
        let seen = if finding.seen_before {
            " (seen before)"
        } else {
            ""
        };
        println!(
            "    {}. {} {} {} {}{}",
            index + 1,
            finding.finding_id,
            finding.oracle,
            finding.severity.as_str(),
            finding.target_id,
            seen
        );
        println!("       {}", finding.fault.message);
        println!("       input: {}", finding.input.artifact);
        println!(
            "       reproducer: {}",
            finding
                .reproducer
                .as_ref()
                .map(|a| a.artifact.as_str())
                .unwrap_or("not emitted")
        );
    }
}

fn spelling(reason: &StoppedReason) -> &'static str {
    match reason {
        StoppedReason::Budget => "budget",
        StoppedReason::Complete => "complete",
        StoppedReason::Interrupted => "interrupted",
    }
}
