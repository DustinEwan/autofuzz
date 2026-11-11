//! `afz probe-plugin`: start a plugin, handshake, discover, and print what it reported.
//!
//! This is the transport proof. It exercises every request type the engine will ever send,
//! with no target language involved.

use std::path::{Path, PathBuf};

use afz_engine::config::{self, LoadedConfig};
use afz_engine::rpc::client::{Launch, PluginClient};
use afz_protocol::*;

/// Run the command. `argv` empty means the repository's fake plugin.
pub fn run(argv: Vec<String>, path: Option<String>, json: bool) -> Result<(), String> {
    let launch = launch_for(argv)?;
    let root = PathBuf::from(path.as_deref().unwrap_or("."));
    let (root, include, exclude, config_path) = scope_for(&root)?;

    let client = PluginClient::spawn(&launch).map_err(|r| r.detail())?;
    let handshake = client.handshake().map_err(|r| r.detail())?;
    check_protocol(&handshake)?;

    let discovery = client
        .discover(
            &DiscoverReq {
                root: root.display().to_string(),
                include,
                exclude,
                config_path: config_path.map(|p| p.display().to_string()),
            },
            std::time::Duration::from_secs(120),
        )
        .map_err(|r| r.detail())?;
    let coverage = client.coverage_map(std::time::Duration::from_secs(30)).ok();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "handshake": handshake,
                "discovery": discovery,
                "coverageMap": coverage,
            }))
            .map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    println!("plugin      {} {} ({})", handshake.name, handshake.version, launch.display());
    println!("protocol    {}", handshake.protocol_version);
    println!("class       {}", wire_target_class(handshake.target_class));
    println!("languages   {}", handshake.languages.join(", "));
    println!(
        "capability  {}",
        if handshake.capabilities.is_empty() {
            "none".to_string()
        } else {
            handshake
                .capabilities
                .iter()
                .map(|c| serde_json::to_value(c).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!(
        "discovery   {}",
        if handshake.discover_requires_execution { "executes target code" } else { "static" }
    );
    println!();
    println!("targets     {}", discovery.targets.len());
    for t in &discovery.targets {
        let params = t
            .parameters
            .iter()
            .map(|p| format!("{}: {}", p.name, wire_kind(p.ty.kind())))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {:<12} {:<24} ({params}) {}",
            t.id,
            t.name,
            serde_json::to_value(t.materialization).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
        );
    }
    println!();
    println!("skipped     {}", discovery.skipped.len());
    for s in &discovery.skipped {
        let blocker = s
            .blocked_by
            .as_ref()
            .map(|b| match &b.module {
                Some(m) => format!(" blockedBy {} at {} ({})", b.type_, b.path, m),
                None => format!(" blockedBy {} at {}", b.type_, b.path),
            })
            .unwrap_or_default();
        let unlocks = s.unlocks_targets.map(|n| format!(" unlocksTargets {n}")).unwrap_or_default();
        let detail = s.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default();
        println!(
            "  {:<12} {:<24} {}{blocker}{unlocks}{detail}",
            s.target_id,
            s.name,
            serde_json::to_value(s.reason).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
        );
    }
    match &coverage {
        Some(map) => println!("\ncoverage    {} edges mapped", map.entries.len()),
        None => println!("\ncoverage    coverageMap not offered"),
    }
    let fuzzable = discovery.targets.iter().filter(|t| t.is_fuzzable()).count();
    println!(
        "\n{} discovered, {} fuzzable, {} skipped",
        discovery.targets.len(),
        fuzzable,
        discovery.skipped.len()
    );
    Ok(())
}

/// Reject a plugin that speaks a different protocol revision, rather than misreading it.
fn check_protocol(handshake: &Handshake) -> Result<(), String> {
    if handshake.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "plugin speaks protocol {}, this engine speaks {PROTOCOL_VERSION}",
            handshake.protocol_version
        ));
    }
    Ok(())
}

/// Resolve the plugin argv. With no `--command`, use the repository's fake plugin, which is
/// what `cargo run -p afz-cli -- probe-plugin` is for.
fn launch_for(argv: Vec<String>) -> Result<Launch, String> {
    if !argv.is_empty() {
        return Ok(Launch::new(argv));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    for candidate in [dir.join("afz-fakeplugin"), dir.join("debug").join("afz-fakeplugin"), dir.join("release").join("afz-fakeplugin")] {
        if candidate.is_file() {
            return Ok(Launch::new([candidate.display().to_string()]));
        }
    }
    Err("no --command given and afz-fakeplugin is not beside this binary; build it with `cargo build --workspace`".to_string())
}

/// Discovery scope: the scan project's block if there is one, otherwise the given directory.
fn scope_for(root: &Path) -> Result<(PathBuf, Option<Vec<String>>, Option<Vec<String>>, Option<PathBuf>), String> {
    let candidate = root.join(config::CONFIG_FILE_NAME);
    if candidate.is_file() {
        let loaded: LoadedConfig = config::load(&candidate).map_err(|e| e.to_string())?;
        let (scope_root, include, exclude) = loaded.discovery_scope().map_err(|e| e.to_string())?;
        return Ok((scope_root, include, exclude, Some(loaded.path)));
    }
    Ok((root.to_path_buf(), None, None, None))
}

fn wire_target_class(class: TargetClass) -> &'static str {
    match class {
        TargetClass::DesignTime => "design-time",
        TargetClass::Runtime => "runtime",
    }
}

fn wire_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Scalar => "scalar",
        Kind::StringFormat => "stringFormat",
        Kind::Literal => "literal",
        Kind::Collection => "collection",
        Kind::Object => "object",
        Kind::Tuple => "tuple",
        Kind::Composite => "composite",
        Kind::Buffer => "buffer",
        Kind::Promise => "promise",
        Kind::Function => "function",
        Kind::Stream => "stream",
        Kind::Opaque => "opaque",
        Kind::Ref => "ref",
    }
}
