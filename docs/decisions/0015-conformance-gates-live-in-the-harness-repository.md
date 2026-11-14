# Decision 15 — Conformance gates live in the harness repository

Status: accepted, 2026-11-13.
Context: `docs/harnesses.md`, `CONTRIBUTING.md`, `.github/workflows/ci.yml`.
Amends: `0014`, whose Consequences named the engine's `rust` CI job as the install that gates the shipped harnesses and as the source of the template gates' records.

## Context

Decision 14 left the engine's CI installing every published harness from its published URL: the
install step was itself the wire-rules gate, and the template gates read the install records that
step wrote. That inverted ownership. The engine's build began to depend on the availability of
five other repositories and on the state of their default branches; a harness change that broke
nothing in the engine still reddened the engine's CI, and an engine change that no harness used
reddened nothing the harness author could see. Conformance is a property of a harness, and the
pull request that can break it lives in the harness's repository.

## Decision

- Each harness repository's own CI is the gate. It installs its own checkout —
  `afz install-harness "$GITHUB_WORKSPACE"`, which drives the 24 wire rules before it records
  anything — and asserts the report, pass counts and justified absences, against its own golden.
  All five repositories already did this; this decision makes it the only place the gate exists.
- The engine's CI installs no published harness. It installs the repository's own fake plugin,
  generated as a one-file harness repository, and that step exercises the engine's side of the
  install contract: clone, manifest parse, spawn, protocol-version check, name match, record.
- The three engine tests that drive the real `js-ts` plugin over stdio become `#[ignore]`d
  cross-repository acceptance tests, run locally with `cargo test -- --ignored` after an
  `afz install-harness`.

## Consequences

- The engine's CI goes red only for the engine's own code; the test job fetches no other
  repository.
- A harness that stops conforming fails its own repository's CI and no longer reddens the
  engine's build. The engine learns of a harness regression where a user installs that harness:
  `install-harness` validates before it records, so the failure surfaces at the install, not in
  a scan. That is the cost of the ownership change, and it is the honest one — the person who
  can fix the harness is the person whose gate went red.
- The cross-language pin that `crates/afz-engine/tests/discovery.rs` ran against the real plugin
  is no longer a CI gate in the engine; the js-ts repository's contract test against
  `docs/schema/` is the always-on pin for that vocabulary.
- The generated fixture's manifest must name the plugin the handshake reports, because the
  install rejects a name mismatch: the same command a user runs pins that rule.

## Reconsidered

- **Keep installing one published harness in the engine's CI so the stdio tests run there.**
  Rejected: it keeps the coupling the decision removes, with one URL instead of five, and the
  engine's tests would still depend on another repository's default branch.
- **Rewrite the three stdio tests against the fake plugin.** Rejected: their value is that a
  real plugin in a real language produces the bytes the engine parses; a scripted fake proves
  only that the engine reads its own fixture.
