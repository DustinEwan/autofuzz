//! The CLI's flag vocabulary and the MCP tools' argument vocabulary are one design, and they
//! drift silently: a flag added to `afz` and not to the tool's parameter struct is invisible to
//! an agent, and the reverse is an argument no human can pass. This test reads both vocabularies
//! from the artifacts that publish them - the real binary's `--help` and the parameter structs'
//! JSON Schemas - and fails on any name one side has and the other lacks, unless a table below
//! says why the difference is intended.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use schemars::schema_for;
use serde_json::Value;

/// CLI command → the tool that answers it, and the MCP param struct whose properties are
/// compared with the command's flags. An empty type name means the tool takes no arguments.
const PAIRINGS: &[(&str, &str, &str)] = &[
    ("probe-plugin", "afz_probe", "ProbeParams"),
    ("scan", "afz_scan", "ScanParams"),
    ("validate-harness", "afz_validate_harness", "ValidateParams"),
    ("harnesses", "afz_harnesses", ""),
];

/// Commands a tool must not answer, with why.
const CLI_ONLY: &[(&str, &str)] = &[
    (
        "install-harness",
        "clones a git repository and runs its install steps; a setup step a human approves",
    ),
    (
        "export-schema",
        "writes a schema set into a directory, which the caller's own file tools do",
    ),
    (
        "init",
        "prompts with dialoguer, which a stdio server has no terminal to prompt on",
    ),
    ("help", "clap's own subcommand"),
];

/// CLI flags with no MCP property, with why.
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "probe-plugin",
        "command",
        "the server resolves the plugin from the scan project; no tool names a raw argv",
    ),
    (
        "probe-plugin",
        "json",
        "a tool result is always structured JSON",
    ),
    (
        "validate-harness",
        "json",
        "a tool result is always structured JSON",
    ),
    (
        "validate-harness",
        "out",
        "the suite-program role is invoked by the pluginrpc mechanism, which is the only reader of the directory it writes",
    ),
    (
        "harnesses",
        "json",
        "a tool result is always structured JSON",
    ),
    (
        "scan",
        "quiet",
        "progress printing has no meaning on a stdio JSON-RPC channel",
    ),
    ("all", "help", "clap adds --help to every command"),
];

/// Same meaning, different name. Applied to the CLI side before comparing.
const RENAMES: &[(&str, &str, &str)] = &[("validate-harness", "deadline", "deadlineMs")];

/// MCP properties no CLI flag spells, with why.
const MCP_ONLY_PROPS: &[(&str, &str, &str)] = &[
    (
        "ProbeParams",
        "targetLimit",
        "caps the reply so a harness with thousands of targets cannot overflow the caller's context",
    ),
    (
        "ProbeParams",
        "config",
        "the tool names the scan project file itself; `afz probe-plugin` takes only --path and walks up from it",
    ),
    (
        "ScanParams",
        "path",
        "the CLI names the scan directory as a positional argument",
    ),
    (
        "ValidateParams",
        "harness",
        "the CLI names the harness as a positional argument",
    ),
];

/// Run the real binary from the working directory and return its combined stdout and stderr.
fn afz(args: &[&str]) -> String {
    run_in(Path::new("."), args)
}

fn run_in(dir: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_afz"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the afz binary runs");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// The lines clap printed under `{heading}:`, trimmed, stopping at the first blank line or
/// column-0 line.
///
/// Entry lines share one indent and a wrapped description is indented deeper, so keeping only the
/// first entry line's indent drops the continuations without dropping an entry.
fn section(text: &str, heading: &str) -> Vec<String> {
    let wanted = format!("{heading}:");
    let start = text
        .lines()
        .position(|line| line == wanted)
        .unwrap_or_else(|| panic!("no `{wanted}` section in:\n{text}"));
    let mut entries: Vec<(usize, String)> = Vec::new();
    for line in text.lines().skip(start + 1) {
        if line.trim().is_empty() || !line.starts_with(' ') {
            break;
        }
        entries.push((line.len() - line.trim_start_matches(' ').len(), line.into()));
    }
    let Some(indent) = entries.first().map(|(indent, _)| *indent) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|(entry_indent, _)| *entry_indent == indent)
        .map(|(_, line)| line.trim().to_string())
        .collect()
}

/// The names clap lists under `Commands:`.
fn commands(text: &str) -> BTreeSet<String> {
    section(text, "Commands")
        .into_iter()
        .map(|line| {
            line.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every long flag clap lists under `Options:`, in the camelCase the tool arguments use.
///
/// A flag with a short form prints as `-q, --quiet`, so the token sought is the first one that
/// begins with `--`, not the first token of the line.
fn long_flags(text: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    for line in section(text, "Options") {
        let Some(token) = line
            .split_whitespace()
            .find(|token| token.starts_with("--"))
        else {
            continue;
        };
        let name = token
            .trim_start_matches('-')
            .split(['=', '|'])
            .next()
            .unwrap_or_default();
        flags.insert(camel(name));
    }
    flags
}

/// kebab-case to camelCase, the convention every MCP tool argument uses.
fn camel(name: &str) -> String {
    let mut text = String::new();
    let mut upper = false;
    for letter in name.chars() {
        if letter == '-' {
            upper = true;
        } else if upper {
            text.extend(letter.to_uppercase());
            upper = false;
        } else {
            text.push(letter);
        }
    }
    text
}

/// The argument names one tool's parameter struct declares, read from its JSON Schema. The names
/// are the table's strings, so a row of `PAIRINGS` says which struct backs which command.
fn tool_properties(type_name: &str) -> BTreeSet<String> {
    if type_name.is_empty() {
        return BTreeSet::new();
    }
    let schema = match type_name {
        "ProbeParams" => schema_for!(afz_mcp::tools::ProbeParams),
        "ScanParams" => schema_for!(afz_mcp::tools::ScanParams),
        "ScanBudget" => schema_for!(afz_mcp::tools::ScanBudget),
        "ValidateParams" => schema_for!(afz_mcp::tools::ValidateParams),
        other => panic!("no MCP parameter struct named {other} is listed by this test"),
    };
    let Value::Object(map) = serde_json::to_value(&schema).expect("a schema serializes") else {
        panic!("the schema of {type_name} is not an object");
    };
    match map.get("properties") {
        Some(Value::Object(properties)) => properties.keys().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

fn exempt(command: &str, flag: &str) -> bool {
    EXEMPT.iter().any(|(row_command, row_flag, _)| {
        *row_flag == flag && (*row_command == command || *row_command == "all")
    })
}

fn tool_only(type_name: &str, property: &str) -> bool {
    MCP_ONLY_PROPS
        .iter()
        .any(|(row_type, row_property, _)| *row_type == type_name && *row_property == property)
}

/// The CLI name of a flag as the tool would spell it.
fn renamed(command: &str, flag: &str) -> String {
    RENAMES
        .iter()
        .find(|(row_command, row_flag, _)| *row_command == command && *row_flag == flag)
        .map(|(_, _, tool_name)| (*tool_name).to_string())
        .unwrap_or_else(|| flag.to_string())
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("afz-cli-parity-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).expect("scratch directory");
    root
}

/// A new subcommand must be paired with a tool or listed as CLI-only with a reason, and a removed
/// one must leave both tables.
#[test]
fn every_cli_command_is_answered_by_an_mcp_tool() {
    let on_cli = commands(&afz(&["--help"]));
    let table: BTreeSet<String> = PAIRINGS
        .iter()
        .map(|(command, tool, _)| format!("{command} (answered by {tool})"))
        .chain(
            CLI_ONLY
                .iter()
                .map(|(command, reason)| format!("{command} (CLI-only: {reason})")),
        )
        .collect();
    let named: BTreeSet<String> = table
        .iter()
        .map(|row| {
            row.split(' ')
                .next()
                .expect("a leading command name")
                .to_string()
        })
        .collect();
    assert_eq!(
        on_cli,
        named,
        "the CLI's commands and this test's tables disagree.\n  \
         on the CLI, unexplained by the tables: {:?}\n  \
         in the tables, absent from the CLI: {:?}\n  \
         the tables say:\n    {}",
        on_cli.difference(&named).collect::<Vec<_>>(),
        named.difference(&on_cli).collect::<Vec<_>>(),
        table
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n    "),
    );
}

/// Every flag a paired command prints must be an argument of its tool, and every argument of the
/// tool must be a flag of that command, except where a table gives the reason.
#[test]
fn mcp_tool_arguments_cover_the_cli_flags() {
    let mut drift: Vec<String> = Vec::new();
    for (command, tool, type_name) in PAIRINGS {
        let flags = long_flags(&afz(&[command, "--help"]));
        // The renames apply to the CLI side, so the tool's names are compared against the flags
        // spelled the way the tool would spell them.
        let spelled: BTreeSet<String> = flags.iter().map(|flag| renamed(command, flag)).collect();
        let properties = tool_properties(type_name);
        for flag in &flags {
            if exempt(command, flag) {
                continue;
            }
            let named = renamed(command, flag);
            if !properties.contains(&named) {
                drift.push(format!(
                    "`afz {command} --{flag}` is spelled by no argument of {tool}, whose arguments are {properties:?}"
                ));
            }
        }
        for property in &properties {
            if tool_only(type_name, property) {
                continue;
            }
            if !spelled.contains(property) {
                drift.push(format!(
                    "{tool} argument `{property}` is spelled by no flag of `afz {command}`, whose flags are {spelled:?}"
                ));
            }
        }
        // A tool with no arguments is a decision too: every flag of its command has to be exempt.
        if type_name.is_empty() {
            let unexplained: Vec<&String> =
                flags.iter().filter(|flag| !exempt(command, flag)).collect();
            if !unexplained.is_empty() {
                drift.push(format!(
                    "{tool} takes no arguments, so every flag of `afz {command}` must be exempt; these are not: {unexplained:?}"
                ));
            }
        }
    }
    assert!(
        drift.is_empty(),
        "the CLI's flags and the MCP tools' arguments disagree:\n  {}",
        drift.join("\n  ")
    );
}

/// `--budget` keys are a third vocabulary, inside one flag's value. The CLI names them in an error
/// sentence and the tool names them in `ScanBudget`, so reading the sentence rather than a table is
/// what catches a key added on one side alone.
#[test]
fn budget_keys_are_named_by_both_sides() {
    let root = scratch("budget-keys");
    std::fs::write(
        root.join(afz_engine::config::CONFIG_FILE_NAME),
        serde_json::json!({"harness": {"name": "x", "command": ["true"]}}).to_string(),
    )
    .expect("a minimal scan project");
    // `apply_budget` runs before `scan::preflight`, so the command refuses the key without ever
    // starting a plugin.
    let text = run_in(&root, &["scan", "--budget", "bogus=1", "."]);
    std::fs::remove_dir_all(&root).ok();
    let line = text
        .lines()
        .find(|line| line.contains("--budget key \"bogus\" is unknown."))
        .unwrap_or_else(|| panic!("the command did not refuse the key as expected:\n{text}"));
    let tail = line
        .split_once("Use ")
        .map(|(_, keys)| keys)
        .unwrap_or_else(|| panic!("the sentence names no key list: {line}"))
        .trim_end_matches('.')
        // The sentence reads "Use invocations or wall.", so the conjunction is not a key.
        .replace(" or ", " ");
    let keys: BTreeSet<String> = tail.split_whitespace().map(str::to_string).collect();
    let properties = tool_properties("ScanBudget");
    assert_eq!(
        keys, properties,
        "`--budget` keys and `ScanBudget`'s properties are different vocabularies; the CLI named {keys:?} and the schema {properties:?}"
    );
}
