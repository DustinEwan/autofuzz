//! Repository tasks. `afz-xtask schema` regenerates `docs/schema/`; `afz-xtask fakeplugin` runs
//! the scripted plugin on stdio, which is convenient when debugging the protocol by hand;
//! `afz-xtask check-harness` drives a harness plugin through the conformance suite.

use std::path::{Path, PathBuf};

use afz_engine::config::{self, Block, LoadedConfig};
use afz_engine::rpc::client::Launch;

const USAGE: &str = "usage: afz-xtask <schema [dir]|fakeplugin|check-harness [--path <dir>] [--command <argv…>] [--targets <n>] [--deadline <ms>] [--json]>";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() {
    let task = std::env::args().nth(1).unwrap_or_else(|| "help".to_string());
    match task.as_str() {
        "schema" => {
            let dir = std::env::args()
                .nth(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| repo_root().join("docs/schema"));
            match afz_xtask::schema::write_schemas(&dir) {
                Ok(files) => {
                    for f in files {
                        println!("wrote {f}");
                    }
                }
                Err(e) => {
                    eprintln!("afz-xtask: {e}");
                    std::process::exit(1);
                }
            }
        }
        "fakeplugin" => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            afz_xtask::fake::serve(
                afz_protocol::buffered(stdin.lock()),
                std::io::BufWriter::new(stdout.lock()),
            );
        }
        "check-harness" => {
            let argv: Vec<String> = std::env::args().skip(2).collect();
            match check_harness(&argv) {
                Ok(report) => {
                    print!("{}", afz_conformance::render(&report));
                    // Notes are advice; only a violated rule fails the command.
                    std::process::exit(i32::from(!report.violations.is_empty()));
                }
                Err(e) => {
                    eprintln!("afz-xtask: {e}");
                    std::process::exit(2);
                }
            }
        }
        other => {
            eprintln!("{USAGE}");
            if other != "help" {
                std::process::exit(2);
            }
        }
    }
}

/// One `check-harness` invocation. A usage or launch problem is the `Err` arm, which exits 2;
/// a plugin that violates a rule is an `Ok` report, which exits 1, because the run itself worked.
fn check_harness(argv: &[String]) -> Result<afz_conformance::Report, String> {
    let mut path: Option<String> = None;
    let mut command: Option<Vec<String>> = None;
    let mut targets = 8usize;
    let mut deadline = 1000u64;
    let mut json = false;
    let mut index = 0;
    while index < argv.len() {
        let flag = argv[index].as_str();
        // Everything after `--command` is the plugin's argv, so a plugin that takes flags of its
        // own does not need them separated from the checker's.
        if flag == "--command" {
            if index + 1 >= argv.len() {
                return Err("--command needs at least the program name".to_string());
            }
            command = Some(argv[index + 1..].to_vec());
            break;
        }
        let missing = || format!("{flag} needs a value");
        let take = || argv.get(index + 1).cloned().ok_or_else(missing);
        match flag {
            "--path" => path = Some(take()?),
            "--targets" => targets = take()?.parse().map_err(|e| format!("--targets: {e}"))?,
            "--deadline" => deadline = take()?.parse().map_err(|e| format!("--deadline: {e}"))?,
            "--json" => json = true,
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
        index += 2;
    }

    let scope = Scope::for_path(path.as_deref())?;
    let launch = match (&command, scope.config.as_ref()) {
        (Some(argv), _) => Launch::new(argv.clone()),
        (None, Some(loaded)) => loaded.launch_for(Block::Codebase).map_err(|e| e.to_string())?,
        (None, None) => fakeplugin_launch()?,
    };
    let mut check = afz_conformance::Check::new(launch, scope.root.display().to_string());
    check.include = scope.include.unwrap_or_default();
    check.exclude = scope.exclude.unwrap_or_default();
    check.config_path = scope.config.as_ref().map(|loaded| loaded.path.display().to_string());
    // A rendered conformance project ships `afz-conformance-manifest.json` at its root; its
    // presence turns on template mode, exactly as it does for `afz validate-harness`.
    let manifest = scope.root.join("afz-conformance-manifest.json");
    if manifest.is_file() {
        check.template = Some(afz_conformance::template::load_manifest(&manifest)?);
    }
    check.drive_targets = targets;
    check.deadline_ms = deadline;

    let report = afz_conformance::run(&check);
    if json {
        println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    }
    Ok(report)
}

/// Where to discover, and the scan project that said so, if there was one. Mirrors
/// `crates/afz-cli/src/probe.rs`, so `check-harness` and `probe-plugin` point a plugin at the
/// same tree with the same flags.
struct Scope {
    root: PathBuf,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    config: Option<LoadedConfig>,
}

impl Scope {
    /// A scan project is consulted only when `--path` names one. A bare `check-harness` then
    /// checks the repository's own reference plugin, which is what a contributor who has just
    /// read the guide types first, and what the fake-plugin tests below rely on.
    fn for_path(path: Option<&str>) -> Result<Scope, String> {
        let Some(given) = path.map(PathBuf::from) else {
            return Ok(Scope { root: PathBuf::from("."), include: None, exclude: None, config: None });
        };
        // `--path` may name the scan project's directory or the file itself, the way `afz scan`
        // accepts either.
        let file = if given.join(config::CONFIG_FILE_NAME).is_file() {
            given.join(config::CONFIG_FILE_NAME)
        } else if given.is_file() {
            given
        } else {
            return Err(format!(
                "no {} under {}; pass --command to check a plugin no scan project names",
                config::CONFIG_FILE_NAME,
                given.display(),
            ));
        };
        let loaded = config::load(&file).map_err(|e| e.to_string())?;
        let declared = loaded.discovery_scope().map_err(|e| e.to_string())?;
        Ok(Scope {
            root: declared.root,
            include: declared.include,
            exclude: declared.exclude,
            config: Some(loaded),
        })
    }
}

/// The repository's own fake plugin, which is what a bare `check-harness` checks. The same
/// three-candidate search `afz probe-plugin` makes: a test binary sits one level below these.
fn fakeplugin_launch() -> Result<Launch, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    for candidate in [dir.join("afz-fakeplugin"), dir.join("debug").join("afz-fakeplugin"), dir.join("release").join("afz-fakeplugin")] {
        if candidate.is_file() {
            return Ok(Launch::new([candidate.display().to_string()]));
        }
    }
    Err("no --command given and afz-fakeplugin is not beside this binary; build it with `cargo build --workspace`".to_string())
}
