//! Emits the committed JSON Schema in `docs/schema/` from the same types the engine compiles
//! against. `cargo run -p afz-xtask -- schema` is the single entry point, so the output path
//! lives in one place.

use std::path::Path;

use schemars::schema_for;

/// Write every protocol schema into `dir`. Returns the file names written.
pub fn write_schemas(dir: &Path) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut written = Vec::new();

    emit(dir, &mut written, "outcome", &schema_for!(afz_protocol::Outcome))?;
    emit(dir, &mut written, "type-node", &schema_for!(afz_protocol::TypeNode))?;
    emit(dir, &mut written, "value-tree", &schema_for!(afz_protocol::ValueTree))?;
    emit(dir, &mut written, "target-descriptor", &schema_for!(afz_protocol::TargetDescriptor))?;
    emit(dir, &mut written, "skipped-target", &schema_for!(afz_protocol::SkippedTarget))?;
    emit(dir, &mut written, "handshake", &schema_for!(afz_protocol::Handshake))?;
    emit(dir, &mut written, "discovery", &schema_for!(afz_protocol::Discovery))?;
    emit(dir, &mut written, "coverage-map", &schema_for!(afz_protocol::CoverageMap))?;
    emit(dir, &mut written, "request", &schema_for!(afz_protocol::Request))?;
    emit(dir, &mut written, "response", &schema_for!(afz_protocol::Response))?;
    emit(dir, &mut written, "invoke-req", &schema_for!(afz_protocol::InvokeReq))?;
    emit(dir, &mut written, "prepare-req", &schema_for!(afz_protocol::PrepareReq))?;
    emit(dir, &mut written, "prepare-resp", &schema_for!(afz_protocol::PrepareResp))?;
    emit(dir, &mut written, "minimize-req", &schema_for!(afz_protocol::MinimizeReq))?;
    emit(dir, &mut written, "minimize-resp", &schema_for!(afz_protocol::MinimizeResp))?;
    emit(dir, &mut written, "reproducer-req", &schema_for!(afz_protocol::ReproducerReq))?;
    emit(dir, &mut written, "reproducer-resp", &schema_for!(afz_protocol::ReproducerResp))?;
    emit(dir, &mut written, "discover-req", &schema_for!(afz_protocol::DiscoverReq))?;
    emit(dir, &mut written, "materialized-call", &schema_for!(afz_protocol::MaterializedCall))?;
    emit(dir, &mut written, "call-input", &schema_for!(afz_protocol::CallInput))?;
    // The scan result is the engine's document, not a plugin message, but it is published the
    // same way: one file in `docs/schema/`, generated from the type that writes it.
    emit(
        dir,
        &mut written,
        "scan-result",
        &schema_for!(afz_engine::report::scanresult::ScanDocument),
    )?;
    Ok(written)
}

fn emit<S: serde::Serialize>(
    dir: &Path,
    written: &mut Vec<String>,
    name: &str,
    schema: &S,
) -> Result<(), String> {
    let path = dir.join(format!("{name}.json"));
    let text = serde_json::to_string_pretty(schema).map_err(|e| format!("serialize {name}: {e}"))?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("write {}: {e}", path.display()))?;
    written.push(path.display().to_string());
    Ok(())
}
