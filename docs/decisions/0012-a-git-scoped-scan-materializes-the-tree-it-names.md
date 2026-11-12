# Decision 12 - a git-scoped scan materializes the tree its selector names

Status: accepted. 2026-11-11.
Amends: `docs/architecture.md` "Scan result", which gains the `git` object; `README.md`, which gains `--git`.

## Context

`afz scan` fuzzed the whole tree it was pointed at. The request was a way to scope a scan to what a
git selector identifies - a commit, a tag, a stash, the staged set, or the unstaged set - so a pull
request can spend its budget on the change instead of on the package.

The engine touched git in exactly one place. `git_revision` (`crates/afz-engine/src/scan.rs`) ran
`git rev-parse --short HEAD` in the scan root and stored the answer in `target.revision`. HEAD only:
the index, the working tree, tags, and stashes were invisible, and nothing in `scan.json` said
whether the 12 targets it listed were 12 of 12 or 12 of 900.

A selector names two different things at once, and separating them is the whole problem. It names a
set of *files*, and it names a *state of the tree*. Filtering the working tree by file answers the
first and ignores the second: `--git v1.2.0` would fuzz whatever the user happens to have checked out
and report the result under `v1.2.0`'s name, with `target.revision` saying `v1.2.0` because that is
what was typed.

## Decision

1. One flag, `afz scan --git <selector>`, with six selector forms (`crates/afz-engine/src/git.rs`):
   `staged`, `unstaged`, `stash`, `stash@{N}`, any revision git resolves to a commit, and a span -
   `A..B`, whose end is the state that gets scanned and whose start is what the change set is
   measured against, or `A...B`, which measures from `git merge-base A B` instead. An omitted side of
   a span is `HEAD`, as it is everywhere else in git. `staged` and `unstaged` are reserved words; a
   branch with either name is reached as `--git refs/heads/staged`.
2. A selector that names anything other than the working tree is materialized as a `git worktree`
   detached at that state, and the scan runs there. The worktree is dropped when the scan ends,
   through `Drop`, so an early return leaves no worktree registration behind. `unstaged` needs no
   tree: the working tree already is the state it names.
3. The change set is computed with `git diff --name-only --no-renames --relative` and matched, after
   joining with the scan root, against `origin.file` (falling back to `origin.module`) of every
   target the plugin reported. The filter runs in the engine after `discover`, so no harness changed
   and no protocol field was added. `--no-renames` is required: rename detection reports an add as an
   `old -> new` pair whose old path matches nothing in the scanned tree.
4. `staged` turns the index into a commit object with `git commit-tree`, because an index is not a
   check-outable tree. The object is unreachable and is garbage-collected normally. Its identity is
   supplied by the engine (`autofuzz <autofuzz@localhost>`), because `commit-tree` refuses to run
   where no git identity is configured, which is the state of every container and CI image.
5. A revision with no parent is diffed against git's empty tree
   (`4b825dc642cb6eb9a060e54bf8d69288fbee4904`), so a root commit scopes to every file it adds
   instead of failing on a missing parent.
6. The scan project file is copied into the materialized tree and reloaded from there
   (`scoped_options`, `crates/afz-engine/src/scan.rs`). That one swap makes `launch.cwd`, `AFZ_ROOT`,
   `AFZ_CONFIG`, `configHash`, and the plugin's tsconfig-beside-config lookup all resolve inside the
   tree being scanned, while `output.dir` stays the invoked project's so results never land in the
   throwaway tree.
7. The worktree lives inside the scan project's own output directory, under `git-scope/`.
8. `scan.json` gains a `git` object - selector, base, tree, `changedFiles`, and the worktree path -
   and `report.md` prints `scoped to \`<selector>\` against <base> (<n> changed file(s))` in its first
   lines. `inventory` then counts the scope, not the package.
9. A scope that leaves no fuzzable target is its own error, `ScanError::EmptyScope`, because the
   advice that belongs with an empty whole-tree discovery - check your `include` - is the wrong
   advice for a scope that simply does not touch any entry point.

## Consequences

- A scoped scan reads the *currently installed* dependencies, not the ones the named commit pinned.
  The worktree is placed inside the output directory precisely so that a `node_modules` lookup walks
  up from the scanned tree to the project root; a worktree in `/tmp` would resolve nothing and every
  import would fail. A scan of a commit from before a dependency bump therefore fuzzes new library
  code with old call sites, and `git.tree` is what records the mismatch.
- Discovery still walks the whole tree. The filter runs after `discover`, so the cost of a scoped
  scan includes one whole-tree discovery: 8.07 s measured on this repository's own scan project
  (249 targets, Node start-up included) against a 100 000-invocation budget. A scoped run of the
  two-file fixture cost 8.9 s end to end, which is that same discovery cost rather than anything the
  filter added.
- `target.root` is a path inside the output directory for a materialized scope, and
  `target.revision` is the short sha of the state that was scanned - for `staged`, the throwaway
  commit's sha, which is honest: that object is what was checked out.
- A tag names one commit, so `--git v1.2.0` scopes the commit that tag carries and nothing earlier;
  the span is what expresses a release. Measured on a repository whose first commit (`v1`) carried
  `src/alpha.ts`, whose second added `src/beta.ts`, and whose third (`v2`) added `src/gamma.ts`:
  `--git v2` fuzzed one target with `base` at the second commit, and `--git v1..v2` fuzzed two with
  `base` at `v1`. Two dots compare two trees, so work that only the start side has counts as changed
  as well; three dots measure from the merge base and leave the other branch's work out. That
  difference is the reason both spellings are accepted rather than one.
- `--resume` across a tree boundary is allowed and logged. Coverage ids are hashed from the file url,
  so a corpus carried from another tree reports every edge as new; the run is valid, its ratio is not
  comparable, and `engine.log` says so.
- A wasm scan scopes by the `.wasm` file, because that is what a wasm target's `origin.file` names
  (`crates/afz-wasm/src/main.rs`). A commit that changed only `.rs` sources reports
  `git scope "…" touches no fuzzable target`. That is the intended answer: the engine does not rebuild
  the module it is about to scan.
- A changed file fuzzes its own exports, not its callers. A change that breaks an unchanged importer
  is outside a scoped scan, and omitting `--git` remains the way to ask the whole-tree question.
- The engine's own output is excluded from the `unstaged` change set. A project that does not
  gitignore `.autofuzz` would otherwise read the previous scan's artifacts as part of this change,
  which is what an early version of the filter did on a repository with no ignore rule.

## Reconsidered

- **Filtering the working tree without materializing it.** Rejected: it is the whole-tree scan with a
  file mask, and it would report findings at line numbers the named commit does not contain. The
  file list and the state are both part of what a selector means, and only a checkout carries the
  second.
- **Passing the changed paths to the plugin as `DiscoverReq.include`.** Rejected for now: it needs a
  protocol field and a schema change for `discover-req.json`, a second globber in the wasm harness,
  and it still cannot select a *version*, so the worktree would remain necessary. It stays the way to
  cut discovery cost if 8 s ever dominates a pull-request gate: `packages/harness-js/src/discover.ts`
  already feeds `req.include` through `ts.parseJsonConfigFileContent`, which accepts exact file paths
  as well as globs.
- **A `--git-files-only` flag that filters the current tree.** Rejected: the same confusion the
  decision exists to remove, offered as a flag. A user who wants the file filter against the working
  tree has `--git unstaged` plus a clean tree.
- **Symlinking or copying only the changed files into a scratch directory.** Rejected: an import of
  an unchanged module fails, and the repair - a symlink farm over the whole tree - reimplements
  `git worktree add` with more ways to get a stale file wrong.
- **Recording the scope in a free-form field of `scan.json`.** Rejected: `ScanDocument` has no such
  field, and a scope is structured data - the base, the tree, and the count are what a triager
  compares between two runs.
