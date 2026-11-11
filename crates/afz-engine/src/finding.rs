//! Finding identity, and the record of what a project has already reported.
//!
//! Identity is a hash over the target, the fault class, and the top fault frame — never over the
//! input. That is what makes the same bug the same finding across scans, which a triage queue and
//! a PR bot both need: two inputs that reach the same line are one finding, and a scan of a tree
//! it has already looked at marks the finding `seenBefore` instead of listing it twice.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use afz_protocol::FaultSignature;

/// Prefix of every identity, so a triager can tell one at a glance.
pub const IDENTITY_PREFIX: &str = "afz:i:";
/// Prefix of every finding id. The id names the finding's artifacts on disk.
pub const FINDING_PREFIX: &str = "afz:f:";
/// The identity is the first 16 hex characters of the digest, which is 64 bits: enough that two
/// different faults in one project do not collide, short enough to read in a log line.
const IDENTITY_HEX: usize = 8;

/// `afz:i:<16 hex>` over target id, fault class, and the top frame's file and line.
///
/// The fields are joined with NUL rather than concatenated, so `["ab", "c"]` and `["a", "bc"]`
/// cannot produce one digest. A fault with no frames hashes empty strings for file and line: it
/// still deduplicates against itself, which is the most that can be asked of a fault with no
/// location.
pub fn identity(target_id: &str, fault: &FaultSignature) -> String {
    let top = fault.top_frame();
    let line = top.map(|f| f.line.to_string()).unwrap_or_default();
    let joined = format!(
        "{target_id}\0{}\0{}\0{}",
        fault.class,
        top.map(|f| f.file.as_str()).unwrap_or(""),
        line
    );
    format!("{IDENTITY_PREFIX}{}", &digest_hex(&joined)[..IDENTITY_HEX * 2])
}

/// The id a finding is written under, derived from its identity so the same bug keeps the same id
/// in every scan. `afz-f-<hex>` is the same string with the separator the file names use.
pub fn finding_id(identity: &str) -> String {
    format!("{FINDING_PREFIX}{}", &digest_hex(identity)[..8])
}

fn digest_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The identities a project has already reported, in one small file beside the scan results.
///
/// The engine reads it at the start of a scan and writes it back at the end, so a scan that
/// crashes mid-way reports the same `seenBefore` flags as the run before it rather than
/// re-announcing every finding.
#[derive(Debug, Clone)]
pub struct Seen {
    path: PathBuf,
    identities: BTreeSet<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SeenFile {
    schema_version: String,
    identities: Vec<String>,
}

impl Seen {
    /// Read `path`, or start empty when it does not exist yet. A file that exists but does not
    /// parse is an error rather than an empty history: silently forgetting the history would
    /// re-announce every finding.
    pub fn open(path: &Path) -> io::Result<Seen> {
        if !path.exists() {
            return Ok(Seen { path: path.to_path_buf(), identities: BTreeSet::new() });
        }
        let text = fs::read_to_string(path)?;
        let file: SeenFile = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))?;
        Ok(Seen { path: path.to_path_buf(), identities: file.identities.into_iter().collect() })
    }

    pub fn contains(&self, identity: &str) -> bool {
        self.identities.contains(identity)
    }

    /// Record an identity. `true` when it was not there, which is the same answer `contains`
    /// would have given a moment earlier and the one the caller needs to set `seenBefore`.
    pub fn record(&mut self, identity: &str) -> bool {
        self.identities.insert(identity.to_string())
    }

    pub fn len(&self) -> usize {
        self.identities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }

    /// Write the file atomically enough for a tool that reads it at the start of the next scan:
    /// a temporary file in the same directory, then a rename.
    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = SeenFile {
            schema_version: "afz-seen/1".to_string(),
            identities: self.identities.iter().cloned().collect(),
        };
        let temp = self.path.with_extension("json.tmp");
        let mut handle = fs::File::create(&temp)?;
        serde_json::to_writer_pretty(&mut handle, &file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        handle.flush()?;
        drop(handle);
        fs::rename(&temp, &self.path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
