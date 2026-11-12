//! The conformance template and the manifest a rendered project ships.
//!
//! `template/v1.json` is the contract: ten semantic properties stated as wire-level
//! expectations. A published version is immutable - new rules add version 2, they never edit
//! version 1 - so a validation result stays reproducible after the tool ships. The suite embeds
//! v1, and `afz validate-harness` refuses a manifest whose `templateVersion` it does not embed.
//!
//! The harness package's emitter renders a fixture that realizes every property and writes
//! `afz-conformance-manifest.json` naming the symbols and inputs. The emitter never decides
//! pass/fail: the suite re-reads the rendered project through the protocol and judges. A
//! property the language genuinely cannot express is declared `notApplicable` with a reason,
//! and the report shows that line rather than a silent pass.

use std::collections::BTreeMap;
use std::path::Path;

use afz_protocol::ValueTree;
use serde::{Deserialize, Serialize};

/// The embedded template v1. Immutable: this bytes-hash is the version authors pin against.
const TEMPLATE_V1: &str = include_str!("../template/v1.json");

/// One property of the template: what the suite will check and what the emitter must realize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateProperty {
    pub id: String,
    /// A capability the plugin must declare for this property to be drivable, e.g. `coverage`.
    #[serde(default)]
    pub requires: Option<String>,
    /// The wire-level expectation, in prose, for the report and for authors.
    pub expectation: String,
    /// What a conforming fixture looks like, for the emitter's author.
    pub emitter_notes: String,
}

/// One published template version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub version: u32,
    pub properties: Vec<TemplateProperty>,
}

/// The embedded template v1.
pub fn template_v1() -> Template {
    serde_json::from_str(TEMPLATE_V1).expect("the embedded template/v1.json parses")
}

/// The manifest a rendered project ships as `afz-conformance-manifest.json`: the same property
/// ids as the template, each realized by a named symbol (plus its inputs) or declared
/// not-applicable with a reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub template_version: u32,
    pub properties: BTreeMap<String, ManifestProperty>,
    /// Present only for a runtime harness (guide section 12): how to start, prove, and stop the
    /// service the harness talks to. `None` means the harness is in-process and lifecycle
    /// properties are not applicable.
    #[serde(default)]
    pub lifecycle: Option<Lifecycle>,
}

/// One manifest entry: a realization, or a justified absence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestProperty {
    /// The fixture symbol that realizes the property, by discovery name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The single input list to drive, where the property drives one call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ValueTree>>,
    /// The two input lists, for `coverage-digest-live`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<[Vec<ValueTree>; 2]>,
    /// For `unconstructible-receiver`: the type the skipped entry's `blockedBy.type` must name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_type: Option<String>,
    /// Set instead of a realization: the language cannot express this property, and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_applicable: Option<String>,
}

/// How the suite starts, proves, and stops a runtime harness's service. The suite allocates the
/// port and substitutes it for the manifest's `{{port}}` placeholder in every argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lifecycle {
    pub launch: Vec<String>,
    /// An HTTP URL on the allocated port; ready when it answers 2xx/3xx/4xx.
    pub health: String,
    pub teardown: Vec<String>,
    /// The value the probe target must echo back over the harness's transport.
    pub nonce: String,
    /// Discovery name of the target the suite drives once after discovery; its `ok` value
    /// must contain `nonce`, proving the plugin reached the service this suite started.
    pub probe: String,
    /// How long the suite waits for `health` to answer. Absent means the suite default of
    /// 120 s; a test stub shortens it, a slow JVM runtime may lengthen it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_timeout_secs: Option<u64>,
}

/// How one template property ended. `NotApplicable` carries the manifest's reason; it is never
/// counted as a pass and never counted as a violation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    Fail,
    NotApplicable,
}

/// One property's line of the template report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyResult {
    pub id: String,
    pub verdict: Verdict,
    pub detail: String,
}

/// The template section of a report: present only when the run was template-driven.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateReport {
    pub version: u32,
    pub properties: Vec<PropertyResult>,
}

impl TemplateReport {
    pub fn passed(&self) -> usize {
        self.properties.iter().filter(|p| p.verdict == Verdict::Pass).count()
    }
    pub fn failed(&self) -> usize {
        self.properties.iter().filter(|p| p.verdict == Verdict::Fail).count()
    }
    pub fn not_applicable(&self) -> usize {
        self.properties.iter().filter(|p| p.verdict == Verdict::NotApplicable).count()
    }
}

/// Which manifest fields a realization must carry, per property id. A property not listed here
/// (or a manifest id not in the template) is a manifest error, never a silent skip.
fn required_fields(id: &str) -> &'static [&'static str] {
    match id {
        "crash-unguarded" | "guard-refusal" | "method-receiver" | "reproducer-script" | "deadline-sleep" => &["symbol", "input"],
        "coverage-digest-live" => &["symbol", "inputs"],
        "unconstructible-receiver" => &["symbol", "blockedByType"],
        "private-symbol" | "unrepresentable-symbol" | "stdout-purity" => &["symbol"],
        _ => &[],
    }
}

/// Read and validate a manifest against the embedded template: right version, every property
/// accounted for, every realization carrying the fields its property drives with. Errors are
/// usage errors - the CLI reports them before spawning the plugin.
pub fn load_manifest(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| format!("{} is not a conformance manifest: {e}", path.display()))?;
    let template = template_v1();
    if manifest.template_version != template.version {
        return Err(format!(
            "{} declares templateVersion {}, this tool embeds version {}",
            path.display(),
            manifest.template_version,
            template.version,
        ));
    }
    for spec in &template.properties {
        let Some(entry) = manifest.properties.get(&spec.id) else {
            return Err(format!("{} does not account for template property \"{}\"; realize it or declare it notApplicable with a reason", path.display(), spec.id));
        };
        if entry.not_applicable.is_some() {
            continue;
        }
        let missing: Vec<&str> = required_fields(&spec.id)
            .iter()
            .copied()
            .filter(|field| match *field {
                "symbol" => entry.symbol.is_none(),
                "input" => entry.input.is_none(),
                "inputs" => entry.inputs.is_none(),
                "blockedByType" => entry.blocked_by_type.is_none(),
                _ => false,
            })
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{} realizes \"{}\" without the fields it drives with: missing {}",
                path.display(),
                spec.id,
                missing.join(", "),
            ));
        }
    }
    if let Some(extra) = manifest.properties.keys().find(|id| !template.properties.iter().any(|p| &p.id == *id)) {
        return Err(format!("{} names \"{extra}\", which is not a property of template v1; template versions are immutable", path.display()));
    }
    Ok(manifest)
}
