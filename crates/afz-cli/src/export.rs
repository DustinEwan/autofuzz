//! `afz export-schema`: write the protocol schemas compiled into this binary, so a harness
//! author can `cue import` them or point an editor's `$schema` at them without an engine
//! checkout. The exported text is the text `afz validate-harness` validates against, because
//! both read the same embedded table (`afz_conformance::SCHEMA_SETS`): an export and a
//! validation cannot disagree, and the version is pinned by the binary you ran, not by
//! whichever branch of the engine repository you cloned.

use std::path::PathBuf;

use afz_conformance::{embedded_schemas, schema_versions};
use afz_protocol::PROTOCOL_VERSION;

/// `afz export-schema [version] [--out DIR]`. The version defaults to the newest revision
/// this engine speaks; an unknown revision is refused by naming the ones it carries.
pub fn run(version: Option<String>, out: Option<PathBuf>) -> Result<(), String> {
    let version = version.unwrap_or_else(|| PROTOCOL_VERSION.to_string());
    let set = embedded_schemas(&version).ok_or_else(|| {
        format!(
            "this engine carries no schema set for protocol \"{version}\"; it carries {}",
            schema_versions().join(", ")
        )
    })?;
    let dir = out.unwrap_or_else(|| PathBuf::from("schema"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for (file, text) in set {
        let path = dir.join(file);
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("afz-export-{tag}-{}", std::process::id()))
    }

    #[test]
    fn the_default_version_writes_the_set_the_engine_validates_with() {
        let dir = temp_dir("default");
        let _ = std::fs::remove_dir_all(&dir);
        run(None, Some(dir.clone())).expect("the newest revision always has a set");
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .expect("export dir")
            .map(|e| e.unwrap().path().file_name().unwrap().to_string_lossy().to_string())
            .collect();
        files.sort();
        assert_eq!(files.len(), 22, "every committed schema exports, got {files:?}");
        assert!(files.contains(&"handshake.json".to_string()));
        let handshake: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("handshake.json")).unwrap()).unwrap();
        assert_eq!(handshake["$schema"].as_str(), Some("https://json-schema.org/draft/2020-12/schema"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unsupported_version_is_refused_naming_the_supported_ones() {
        let dir = temp_dir("unknown");
        let err = run(Some("afz/9".to_string()), Some(dir.clone())).expect_err("no afz/9 set exists");
        assert!(err.contains("afz/9"), "the message names the asked-for revision: {err}");
        assert!(err.contains(afz_protocol::PROTOCOL_VERSION), "the message names what the engine carries: {err}");
        assert!(!dir.exists(), "a refused export writes nothing");
    }
}
