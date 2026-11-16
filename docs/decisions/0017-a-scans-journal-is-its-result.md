# Decision 17 — A scan's journal is its result: `scan.jsonl` replaces `scan.json`

Status: accepted, 2026-11-15.
Context: `docs/cli.md`, `crates/afz-engine/src/report/scanresult.rs`, `crates/afz-engine/src/scan.rs`.
Amends: `0016`, whose `docs/schema/` rule leaves the scan-result schema inside the current revision because the finished record's shape did not change.

## Context

A scan wrote one file, `scan.json`, and wrote it last. Everything the run learned before
finalization lived only in the engine's memory: a `kill -9`, an OOM kill, or a power loss at
invocation 280,000 of a 1,000,000-invocation budget lost the corpus, the findings, and the
coverage facts of the whole run, because no signal handler runs for those deaths. The result file
was also the only record of the run's existence: a directory without `scan.json` was
indistinguishable from a directory where nothing had happened.

The fix considered first was a sidecar: keep `scan.json` as the result and append a durability
journal beside it. That design keeps the same fact in two files that can disagree, and it keeps
the property that made the failure: the file a reader trusts is still the one written last, so
the journal must be complete enough to rebuild the result — at which point it *is* the result,
and `scan.json` is a copy of its last line. One file, append-only, says the same thing with no
second truth to keep in sync.

## Decision

- `scan.jsonl` is the scan result. One JSON record per line, tagged by `kind`:
  `started` (the run's identity and budget), `corpus` (one retained input), `finding` (one
  reported finding with its input inline), `progress` (one coverage sample), and `finished`
  (the complete result document). Every line is flushed and fsynced before the call returns;
  lines are rare — one per corpus retention, finding, and sample — so the cost is invisible
  next to the batch it follows.
- The `finished` line is what makes the journal a result. It carries the whole document — the
  same shape that validates against `schema/afz-1/scan-result.json` — and lands last, after the
  artifact files, so a reader that finds it never sees a scan directory whose artifacts are
  still being written. A journal without it is an unfinished run: `read` refuses it rather than
  rendering a half-scan as a scan, and `--resume` restarts from the lines that landed.
- The scan directory is claimed before the first invocation (`reserve_dir`), so the journal has
  somewhere to live from the first line and the finished record lands in the directory the
  journal started in. A directory that cannot be claimed fails the scan at open: it is a scan
  with nowhere to land.
- `scan.json` is never written again. It is still read — `--resume`, the renderers, and
  `afz_findings` fall back to it — so scan directories written by older engines stay
  inspectable and resumable. The fallback triggers only when no `scan.jsonl` exists.
- The document shape grew additively inside `afz/scan-1`: `findingsSuppressed`,
  `corpusBytes`, and per-target `deadlineCuts` are optional or defaulted, so old documents
  parse and new documents say what a suppressed finding count and a corpus byte count are.

## Consequences

- A hard kill costs the invocations in flight, not the run: the resumed scan reloads every
  fsynced corpus entry and suppresses every finding identity already reported, and the engine
  log names the journal as the source it rebuilt from.
- A finished scan directory carries `scan.jsonl` and no `scan.json`. Scripts that opened
  `scan.json` directly must open the journal's last line or use `afz render`/`afz_findings`,
  which read the directory.
- The journal is durable state, so a torn last line is an expected shape after a hard kill:
  the reader skips exactly one incomplete trailing line and reports it; a corrupt line in the
  middle is an error, because every line that got through was fsynced.
- The finished line duplicates the finding and corpus records that earlier lines carry. That
  is deliberate: a completed scan should be readable from the last line alone, without folding
  the stream, and the duplication is bounded by the document's own size.

## Reconsidered

- **`scan.json` as the result with a journal beside it.** Rejected: two files holding the same
  facts, with the trusted one still written last. The journal would have had to grow until it
  was the result anyway.
- **Folding the document from durable lines instead of a `finished` line.** Rejected: the
  aggregates — inventory, per-target stats, samples, the git scope — are not per-invocation
  facts, so a fold could not rebuild them, and every completed read would pay for assembling a
  document that finalization could simply state.
- **A new schema revision for the changed fields.** Rejected: every new field is optional or
  defaulted and the file format change is outside the document itself. `afz/scan-1` still
  describes the finished record exactly; a revision would announce a break that is not there.
