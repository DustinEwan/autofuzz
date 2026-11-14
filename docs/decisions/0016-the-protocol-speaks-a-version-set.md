# Decision 16 — The protocol speaks a version set, and versions deprecate on a published schedule

Status: accepted, 2026-11-13.
Context: `docs/harness-authoring.md`, `docs/cli.md`, `crates/afz-protocol/src/targets.rs`.
Amends: `0001`, whose handshake carried `protocolVersion` as the single accepted value; `0014`, whose committed-schema bullet located the schemas at `docs/schema/<file>.json`.

## Context

The engine carried one protocol revision: a `PROTOCOL_VERSION` constant, an equality check at
every meeting point, and refusal for anything else. That works only while one revision exists.
The intent is to maintain several revisions at once and deprecate old ones on a schedule, so an
author who shipped a plugin against `afz/1` keeps a working plugin while the ecosystem moves.
That intent was nowhere written down, which made the single constant look like the policy rather
than the starting state. This record is the policy.

## Decision

- `SUPPORTED_VERSIONS` is the truth the handshake is checked against: membership, not equality
  (`probe::check_protocol`, the MCP probe). A refusal names the set the engine speaks.
- `DEPRECATED_VERSIONS` lists revisions that are still accepted and scheduled to be refused.
  While a revision is listed, every meeting point - `probe-plugin`, `install-harness`,
  `validate-harness`, the MCP probe - prints a warning naming it. The default support policy:
  the newest two revisions are supported; a revision is listed as deprecated at least one
  engine release before it is removed from `SUPPORTED_VERSIONS`. The warning is the
  operator's lead time, not a surprise.
- `docs/schema/` holds one directory per revision: `docs/schema/afz-1/`. The current
  revision's directory regenerates from the Rust types (the drift gate keeps it honest);
  once a newer revision exists, an older directory is frozen - a change to the contract adds
  a revision directory, it never edits an old one. This is the same rule the conformance
  template already follows ("new properties add version 2, they never edit version 1").
- `afz export-schema [version] [--out DIR]` writes the schema set compiled into the binary,
  defaulting to the newest revision the engine speaks. The binary is the distribution
  mechanism: an export is byte-identical to what that `afz` validates against, because
  `afz_conformance::SCHEMA_SETS` is the single table both read. An author who wants CUE,
  IDE `$schema` validation, or a local copy for a code generator needs no engine checkout;
  `afz --version` is what pins the version of the export.
- `scan.json` records the negotiated revision at `target.harness.protocolVersion`, so an old
  artifact declares which contract wrote its bytes.
- Unbuilt until a second revision exists: negotiation. With one member in the set, membership
  is equality in practice; when `afz/2` lands, the handshake agrees on the newest revision both
  sides speak, and the conformance validator selects its schema set by that agreement.

## Consequences

- Shipping `afz/2` means: a new `docs/schema/afz-2/`, a second entry in `SCHEMA_SETS`,
  parallel serde types only where the wire shape actually breaks, and the handshake
  negotiation. Additive changes stay inside the current revision; the frozen-directory rule
  starts binding the moment a successor exists.
- A harness repository's contract test reads `$AFZ_DOCS/schema/<revision>/`; the versioned
  path is part of the pin, so an engine that drops a revision fails that repository's CI
  before it fails a user's install.
- `afz export-schema` makes the deprecation schedule observable: `afz export-schema afz/1`
  keeps working from any engine that still carries the set, and the refusal message names
  the revisions that engine speaks.

## Reconsidered

- **`#[serde(default)]` growth inside one version instead of a version set.** Rejected: it
  silently reclassifies "never tested" as "declared absent", and a plugin cannot tell an
  old engine from a new one. The version set makes the difference explicit at the handshake.
- **Publishing `docs/schema/` as a versioned release asset instead of an `afz` subcommand.**
  Rejected: the release pipeline is a second copy of the truth. The binary that validates is
  the artifact that exports, so the two cannot disagree.
