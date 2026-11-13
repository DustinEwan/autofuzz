//! `--git <selector>`: the change set a scan is limited to, and the tree it runs against.
//!
//! The four tests below cover the two halves of the feature separately. `resolve` is checked
//! against real git objects with no plugin in sight; the scan tests script the fake plugin's
//! inventory, so the filter is observed without Node.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use afz_engine::config::Strategy;
use afz_engine::git::{self, Selector};
use afz_engine::scan::{self, Options};

/// The fake plugin is a binary of the `xtask` package, so `CARGO_BIN_EXE_…` does not cover it from
/// here.
fn fakeplugin_path() -> PathBuf {
    // cargo places a test executable under `<target>/<profile>/deps/` or under
    // `<target>/<profile>/build/<package>/<hash>/out/` depending on its version: walk every
    // ancestor for the plugin instead of assuming the depth.
    let exe = std::env::current_exe().expect("test executable");
    let mut dir = exe.parent().expect("test executable in a directory");
    loop {
        let candidate = dir.join("afz-fakeplugin");
        if candidate.is_file() {
            return candidate;
        }
        let Some(parent) = dir.parent() else {
            panic!("build the fake plugin: `cargo build --workspace` (no afz-fakeplugin above {})", exe.display());
        };
        dir = parent;
    }
}

/// A scratch directory, removed by the caller's last statement, as the other engine tests do.
fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("afz-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("src")).expect("scratch directory");
    root
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent directory");
    }
    std::fs::write(path, text).expect("write a source file");
}

/// git with an identity, because the machine running the tests may have none configured.
fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "afz")
        .env("GIT_AUTHOR_EMAIL", "afz@test")
        .env("GIT_COMMITTER_NAME", "afz")
        .env("GIT_COMMITTER_EMAIL", "afz@test")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository with two committed sources, a tag on the root commit, a stash holding an edit to
/// `src/a.ts`, a staged new file, and an unstaged edit to `src/b.ts`.
fn repository(name: &str) -> (PathBuf, String) {
    let root = scratch(name);
    write(&root.join("src/a.ts"), "export const a = 1\n");
    write(&root.join("src/b.ts"), "export const b = 1\n");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let first = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["tag", "v1"]);
    write(&root.join("src/a.ts"), "export const a = 2\n");
    git(&root, &["stash", "-q"]);
    write(&root.join("src/c.ts"), "export const c = 3\n");
    git(&root, &["add", "src/c.ts"]);
    write(&root.join("src/b.ts"), "export const b = 2\n");
    (root, first)
}

#[test]
fn each_selector_resolves_its_own_base_and_tree() {
    let (root, first) = repository("git-selectors");
    let out_dir = root.join(".autofuzz");
    let empty_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    let staged = git_resolve(&root, &out_dir, &Selector::Staged);
    assert_eq!(staged.base, first, "staged is measured against HEAD");
    assert!(staged.tree.len() == 40 && staged.tree != first, "the index became a commit object: {}", staged.tree);
    assert_eq!(staged.changed, ["src/c.ts".to_string()].into_iter().collect());
    assert!(staged.root.join("src/c.ts").is_file(), "the staged tree holds the added file");
    assert!(staged.worktree.is_some(), "staged is a materialized tree");

    let unstaged = git_resolve(&root, &out_dir, &Selector::Unstaged);
    assert_eq!(unstaged.base, "index");
    assert_eq!(unstaged.tree, "worktree");
    assert_eq!(unstaged.changed, ["src/b.ts".to_string()].into_iter().collect());
    assert_eq!(unstaged.root, root, "unstaged scans the working tree where it stands");
    assert!(unstaged.worktree.is_none(), "unstaged materializes nothing");

    let stash = git_resolve(&root, &out_dir, &Selector::Stash(None));
    let stash_sha = git(&root, &["rev-parse", "refs/stash@{0}"]);
    assert_eq!(stash.base, first, "a stash is measured against the commit it was made on");
    assert_eq!(stash.tree, stash_sha);
    assert_eq!(stash.changed, ["src/a.ts".to_string()].into_iter().collect());
    assert_eq!(
        std::fs::read_to_string(stash.root.join("src/a.ts")).expect("the stashed file"),
        "export const a = 2\n",
        "the materialized stash tree carries the stashed edit"
    );

    // The tag names the root commit, which has no parent: the empty tree is the base, so the scope
    // is every file that commit added rather than an error about a missing parent.
    let tagged = git_resolve(&root, &out_dir, &Selector::Rev("v1".to_string()));
    assert_eq!(tagged.base, empty_tree);
    assert_eq!(tagged.tree, first);
    assert_eq!(
        tagged.changed,
        ["src/a.ts".to_string(), "src/b.ts".to_string()].into_iter().collect(),
        "a root commit scopes to every file it adds"
    );

    let by_sha = git_resolve(&root, &out_dir, &Selector::Rev(first.clone()));
    assert_eq!((by_sha.base.as_str(), by_sha.tree.as_str()), (empty_tree, first.as_str()), "a sha resolves like the tag on it");

    let missing = git::resolve(&root, &out_dir, &Selector::Rev("no-such-revision".to_string()))
        .expect_err("a revision that does not exist is refused");
    assert!(missing.to_string().starts_with("git scope \"no-such-revision\" does not resolve: "), "{missing}");

    // Dropping each `Resolved` is what removes the worktree the scan ran in.
    let last = staged.root.clone();
    drop(staged);
    drop(unstaged);
    drop(stash);
    drop(tagged);
    drop(by_sha);
    assert!(!last.exists(), "the materialized tree is gone once its handle is dropped");
    std::fs::remove_dir_all(&root).ok();
}

/// `resolve` with the engine's own error type spelled out for the assertions above.
fn git_resolve(root: &Path, out_dir: &Path, selector: &Selector) -> git::Resolved {
    git::resolve(root, out_dir, selector).unwrap_or_else(|e| panic!("resolve {selector:?}: {e}"))
}

/// A scan project whose plugin is the fake one, with the discovery it answers scripted inline.
fn project(name: &str, script: &str) -> PathBuf {
    let root = scratch(name);
    let config = serde_json::json!({
        "project": name,
        "codebase": {
            "harness": "afz-fakeplugin",
            "command": [fakeplugin_path().display().to_string()],
            "env": { "AFZ_FAKE_SCRIPT": script },
        },
        "budget": { "invocations": 4, "wallClockMs": 20_000, "workers": 1 },
        "output": { "dir": ".autofuzz", "renderers": ["json"] },
    });
    std::fs::write(root.join("autofuzz.json"), config.to_string()).expect("scan project");
    root
}

fn scan(root: &Path, selector: Selector) -> Result<scan::ScanReport, scan::ScanError> {
    let loaded = afz_engine::config::load(&root.join("autofuzz.json")).expect("the scan project validates");
    let mut options = Options::new(loaded.clone(), root.to_path_buf());
    options.out_dir = root.join(".autofuzz");
    options.invocations = 4;
    options.workers = 1;
    options.seed = 0x5e3a_91c4;
    options.strategy = Strategy::Random;
    options.wall_clock = Duration::from_secs(20);
    options.scope = Some(selector);
    scan::run(&options, &mut |_| {})
}

/// A `discover` answer with one constructible target per listed source file.
fn scripted(files: &[&str]) -> String {
    let targets: Vec<serde_json::Value> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            serde_json::json!({
                "id": format!("afz:t:{index:04}"),
                "targetClass": "design-time",
                "origin": { "file": file, "line": 1, "module": file },
                "name": format!("entry{index}"),
                "parameters": [{
                    "name": "n",
                    "type": { "kind": "scalar", "scalar": "number" },
                    "required": true,
                }],
                "effects": {},
                "materialization": "constructible",
            })
        })
        .collect();
    serde_json::json!({ "discovery": { "targets": targets, "skipped": [] } }).to_string()
}

/// A repository whose two sources are committed together, then committed again with `later`
/// applied. Returns the directory and the two commit shas.
fn two_commits(name: &str, script: &str, later: impl Fn(&Path)) -> (PathBuf, String, String) {
    let root = project(name, script);
    write(&root.join("src/a.ts"), "export const a = 1\n");
    write(&root.join("src/b.ts"), "export const b = 1\n");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "both sources"]);
    let first = git(&root, &["rev-parse", "HEAD"]);
    later(&root);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "one file changed"]);
    let second = git(&root, &["rev-parse", "HEAD"]);
    (root, first, second)
}

#[test]
fn a_rev_scope_scans_that_commit_and_keeps_only_changed_targets() {
    let (root, first, second) = two_commits("rev-scope", &scripted(&["src/a.ts", "src/b.ts"]), |root| {
        write(&root.join("src/a.ts"), "export const a = 2\n");
    });
    let short = git(&root, &["rev-parse", "--short", "HEAD"]);
    // The working tree carries an uncommitted edit to the other file. A change set computed from
    // the working tree rather than from the named commits would put `src/b.ts` in scope as well.
    write(&root.join("src/b.ts"), "export const b = 99\n");
    let report = scan(&root, Selector::Rev("HEAD".to_string())).expect("the scoped scan completes");
    let document = &report.document;

    assert_eq!(document.inventory.fuzzable, 1, "only the target in the changed file is in scope");
    assert_eq!(document.inventory.discovered, 1, "the inventory counts the scope, not the package");
    let scope = document.git.as_ref().expect("a scoped scan records its scope");
    assert_eq!(scope.selector, "HEAD");
    assert_eq!(scope.tree, second, "the scanned tree is the named commit");
    assert_eq!(scope.base, first, "the change set is the named commit against its parent");
    assert_eq!(scope.changed_files, 1);
    assert_eq!(
        document.target.revision.as_deref(),
        Some(format!("git:{short}").as_str()),
        "the revision is the commit that was scanned, not whatever HEAD moved to"
    );

    let worktree = scope.worktree.as_ref().expect("a revision scan materializes its tree");
    assert_eq!(document.target.root, *worktree, "the scan ran inside the materialized tree");
    assert!(worktree.contains("/git-scope/"), "the tree lives in the output directory: {worktree}");
    assert!(!Path::new(worktree).exists(), "the scan removed the tree it checked out");
    assert_eq!(git(&root, &["worktree", "list"]).lines().count(), 1, "no worktree registration survives the scan");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_scope_that_touches_nothing_says_so() {
    let (root, _first, _second) = two_commits("empty-scope", &scripted(&["src/a.ts", "src/b.ts"]), |root| {
        write(&root.join("README.md"), "# scoped\n");
    });
    let error = scan(&root, Selector::Rev("HEAD".to_string())).expect_err("a scope over no fuzzable target is refused");
    assert_eq!(
        error.to_string(),
        "git scope \"HEAD\" touches no fuzzable target: 1 changed file(s), 2 target(s) discovered"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn unstaged_scope_keeps_new_untracked_files_and_scans_the_working_tree() {
    let root = project("unstaged-scope", &scripted(&["src/c.ts", "src/a.ts"]));
    write(&root.join("src/a.ts"), "export const a = 1\n");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "src/a.ts"]);
    git(&root, &["commit", "-q", "-m", "one source"]);
    write(&root.join("src/c.ts"), "export const c = 1\n");

    let report = scan(&root, Selector::Unstaged).expect("the scan completes");
    let document = &report.document;
    let scope = document.git.as_ref().expect("a scoped scan records its scope");
    assert_eq!(document.inventory.fuzzable, 1, "the untracked file is in scope and the committed one is not");
    let log = std::fs::read_to_string(report.dir.join("engine.log")).expect("the scan writes its log");
    assert!(
        log.contains("1 of 2 target(s) in scope"),
        "the log says which half of the inventory the scope kept:\n{log}"
    );
    assert!(scope.worktree.is_none(), "the working tree is already the state `unstaged` names");
    assert_eq!(scope.base, "index");
    assert_eq!(scope.tree, "worktree");
    assert_eq!(document.target.root, root.display().to_string(), "the scan ran where the user pointed it");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_span_that_omits_a_side_means_head() {
    assert_eq!(
        git::parse("..v2").expect("a span with no start"),
        Selector::Range { from: "HEAD".to_string(), to: "v2".to_string(), three_dot: false }
    );
    assert_eq!(
        git::parse("v1...").expect("a three-dot span with no end"),
        Selector::Range { from: "v1".to_string(), to: "HEAD".to_string(), three_dot: true }
    );
    assert_eq!(git::render(&git::parse("HEAD~3..main").expect("a span")), "HEAD~3..main");
    assert_eq!(git::render(&git::parse("..v2").expect("a span")), "HEAD..v2", "the side that was left out is named");
    assert_eq!(git::parse("v1").expect("a revision"), Selector::Rev("v1".to_string()), "one dot pair is what makes a span");
    assert_eq!(git::parse("stash@{1}").expect("a stash"), Selector::Stash(Some(1)), "the braces of a stash are not a span");
}

#[test]
fn a_range_scope_scans_the_end_and_keeps_every_file_the_span_touched() {
    let root = project("range-scope", &scripted(&["src/a.ts", "src/b.ts", "src/c.ts"]));
    write(&root.join("src/a.ts"), "export const a = 1\n");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first source"]);
    let first = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["tag", "v1"]);
    write(&root.join("src/b.ts"), "export const b = 1\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "second source"]);
    write(&root.join("src/c.ts"), "export const c = 1\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "third source"]);
    let last = git(&root, &["rev-parse", "HEAD"]);

    let span = Selector::Range { from: "v1".to_string(), to: "HEAD".to_string(), three_dot: false };
    let report = scan(&root, span).expect("the span scan completes");
    let document = &report.document;
    let scope = document.git.as_ref().expect("a scoped scan records its scope");

    assert_eq!(document.inventory.fuzzable, 2, "the whole span is in scope, not just its last commit");
    assert_eq!(scope.selector, "v1..HEAD");
    assert_eq!(scope.base, first, "the span starts where it was told to");
    assert_eq!(scope.tree, last, "the span scans the tree at its end");
    assert_eq!(scope.changed_files, 2);
    assert!(scope.worktree.is_some(), "a span names a state, so it is materialized");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_three_dot_span_measures_from_the_merge_base() {
    let root = scratch("three-dot");
    let out_dir = root.join(".autofuzz");
    write(&root.join("src/a.ts"), "export const a = 1\n");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "common ancestor"]);
    let merge_base = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "-q", "-b", "feature"]);
    write(&root.join("src/feature.ts"), "export const f = 1\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "feature work"]);
    let tip = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "-q", "master"]);
    write(&root.join("src/main.ts"), "export const m = 1\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "main work"]);
    let main = git(&root, &["rev-parse", "HEAD"]);

    let span = |three_dot| Selector::Range { from: "master".to_string(), to: "feature".to_string(), three_dot };
    let two_dot = git_resolve(&root, &out_dir, &span(false));
    assert_eq!(two_dot.base, main, "two dots start at the revision that was named");
    assert_eq!(
        two_dot.changed,
        ["src/feature.ts".to_string(), "src/main.ts".to_string()].into_iter().collect(),
        "two dots compare two trees, so main's own work counts as changed too"
    );

    let three_dot = git_resolve(&root, &out_dir, &span(true));
    assert_eq!(three_dot.base, merge_base, "three dots start where the branches diverged");
    assert_eq!(three_dot.tree, tip, "both spans scan the same tree: the end of the span");
    assert_eq!(
        three_dot.changed,
        ["src/feature.ts".to_string()].into_iter().collect(),
        "three dots count only what the feature branch added"
    );

    let backwards = Selector::Range { from: "feature".to_string(), to: "no-such-branch".to_string(), three_dot: false };
    let error = git::resolve(&root, &out_dir, &backwards).expect_err("a span with one bad side is refused");
    assert!(error.to_string().starts_with("git scope \"feature..no-such-branch\" does not resolve: "), "{error}");
    std::fs::remove_dir_all(&root).ok();
}
