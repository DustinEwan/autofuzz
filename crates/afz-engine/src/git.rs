//! Scoping a scan to a git change set.
//!
//! `--git <selector>` names a *state*: the index, the working tree, a stash, or a revision. The
//! selector decides two things, and they are computed here rather than in the scan loop: the
//! change set, which is the file list the scan is limited to, and the tree the scan runs against.
//! A selector that names something other than the working tree gets a `git worktree` of that
//! state, because a scan of a commit that reads the working tree reports findings that commit does
//! not contain.
//!
//! Every call here is a blocking `git -C <dir> …` with no timeout. git does local filesystem work
//! on these paths, not user code, which is the same rule [`crate::scan`]'s `git_revision` follows.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Git's empty tree: no files at all. A revision with no parent is diffed against this, so a root
/// commit scopes to every file it adds instead of failing on a missing parent.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// The message of the throwaway commit `staged` makes so the index becomes a check-outable tree.
const STAGED_COMMIT: &str = "autofuzz: staged scope";
/// The identity of that commit. It is unreachable and only exists to name a tree, and naming an
/// identity here is what lets `--git staged` work on a machine with no git identity configured.
const COMMIT_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "autofuzz"),
    ("GIT_AUTHOR_EMAIL", "autofuzz@localhost"),
    ("GIT_COMMITTER_NAME", "autofuzz"),
    ("GIT_COMMITTER_EMAIL", "autofuzz@localhost"),
];

/// What the user named after `--git`.
///
/// `staged` and `unstaged` are reserved words: a branch, tag, or file with either name is reached
/// as `--git refs/heads/staged`, not as `--git staged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// The index against `HEAD`.
    Staged,
    /// The working tree against the index, plus the untracked files the ignore rules allow.
    Unstaged,
    /// A stash entry; `None` is `stash@{0}`.
    Stash(Option<u32>),
    /// Any revision git resolves to a commit: a sha, a branch, a tag, or `HEAD~3`.
    Rev(String),
    /// A span: `A..B`, or `A...B` to measure the change set from the merge base. The end is the
    /// tree that gets scanned. An omitted side is `HEAD`.
    Range { from: String, to: String, three_dot: bool },
}

/// A resolved selector: the two states the change set was computed between, the changed paths,
/// and the tree the scan runs against when it differs from the working tree.
#[derive(Debug)]
pub struct Resolved {
    /// Exactly what the user typed.
    pub selector: String,
    /// 40-hex sha, or `index` for `unstaged`.
    pub base: String,
    /// 40-hex sha, or `worktree` for `unstaged`.
    pub tree: String,
    /// Scan-root-relative, posix, no leading `./`.
    pub changed: BTreeSet<String>,
    /// The directory the scan runs against.
    pub root: PathBuf,
    /// `Some` only when a tree was materialized. Dropping it removes the worktree, so this field
    /// is what keeps the tree alive for the length of the scan.
    pub worktree: Option<Worktree>,
}

/// A `git worktree` the engine created and removes on `Drop`.
#[derive(Debug)]
pub struct Worktree {
    pub path: PathBuf,
    repo: PathBuf,
}

impl Drop for Worktree {
    fn drop(&mut self) {
        // Both results are ignored on purpose: a scan that returns early, or through a panic,
        // still has to leave no worktree registration behind, and there is no caller left to
        // report a failure to.
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A git failure the user has to see. The message is git's own, trimmed, and it reaches the
/// terminal as `afz: <message>`.
#[derive(Debug)]
pub struct GitError(pub String);

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GitError {}

/// Read the text the user typed. `staged`, `unstaged`, `stash`, and `stash@{N}` are the reserved
/// spellings; `A..B` and `A...B` are a span; everything else is a revision, including `HEAD`,
/// `v1.2.0`, and `main~2`.
pub fn parse(text: &str) -> Result<Selector, GitError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(GitError(
            "--git needs a selector: staged, unstaged, stash[@{n}], a revision, or a span such as A..B".to_string(),
        ));
    }
    Ok(match text {
        "staged" => Selector::Staged,
        "unstaged" => Selector::Unstaged,
        "stash" => Selector::Stash(None),
        other => match stash_reference(other) {
            Some(slot) => Selector::Stash(slot),
            None => match range_sides(other) {
                Some((from, to, three_dot)) => Selector::Range { from, to, three_dot },
                None => Selector::Rev(other.to_string()),
            },
        },
    })
}

/// `A..B` and `A...B` name a span, and an omitted side means `HEAD` as it does everywhere else in
/// git. Splitting on the dots is safe because a git ref name cannot contain `..`.
fn range_sides(text: &str) -> Option<(String, String, bool)> {
    let (from, to, three_dot) = match text.split_once("...") {
        Some((from, to)) => (from, to, true),
        None => match text.split_once("..") {
            Some((from, to)) => (from, to, false),
            None => return None,
        },
    };
    let side = |text: &str| {
        let text = text.trim();
        if text.is_empty() { "HEAD".to_string() } else { text.to_string() }
    };
    Some((side(from), side(to), three_dot))
}

/// `stash@{2}` and the bare `stash@{}` mean a stash entry; anything else is not one.
fn stash_reference(text: &str) -> Option<Option<u32>> {
    let inner = text.strip_prefix("stash@{")?.strip_suffix('}')?;
    if inner.is_empty() {
        return Some(None);
    }
    inner.parse::<u32>().ok().map(Some)
}

/// The text the user typed, so an error message can name the selector the way it was written.
pub fn render(selector: &Selector) -> String {
    match selector {
        Selector::Staged => "staged".to_string(),
        Selector::Unstaged => "unstaged".to_string(),
        Selector::Stash(None) => "stash".to_string(),
        Selector::Stash(Some(slot)) => format!("stash@{{{slot}}}"),
        Selector::Rev(revision) => revision.clone(),
        Selector::Range { from, to, three_dot } => {
            let dots = if *three_dot { "..." } else { ".." };
            format!("{from}{dots}{to}")
        }
    }
}

/// Whether `root` is inside a git repository. A scope needs one; a plain scan does not.
pub fn is_repo(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Work out the change set for `selector` and the tree to scan.
///
/// `invoked_root` is the directory the scan was pointed at, which is what `--relative` makes the
/// changed paths relative to. `out_dir` is the scan project's own output directory: a materialized
/// tree lives inside it, so the scanned tree still resolves the project's installed dependencies by
/// walking up from the worktree to the project root.
pub fn resolve(invoked_root: &Path, out_dir: &Path, selector: &Selector) -> Result<Resolved, GitError> {
    let named = render(selector);
    let out_dir = std::path::absolute(out_dir).map_err(|e| GitError(format!("cannot resolve the output directory: {e}")))?;
    let repo = PathBuf::from(git(invoked_root, &["rev-parse", "--show-toplevel"], &[]).map_err(|e| scoped(&named, e))?);
    let plan = plan_for(invoked_root, &out_dir, &named, selector)?;
    let worktree = match &plan.checkout {
        Some(treeish) => Some(materialize(invoked_root, &out_dir, treeish)?),
        None => None,
    };
    // The worktree holds the whole repository, so a scan pointed at a subdirectory has to keep
    // pointing at that same subdirectory inside it.
    let root = match (&worktree, invoked_root.strip_prefix(&repo)) {
        (Some(worktree), Ok(relative)) if !relative.as_os_str().is_empty() => worktree.path.join(relative),
        (Some(worktree), _) => worktree.path.clone(),
        (None, _) => invoked_root.to_path_buf(),
    };
    Ok(Resolved {
        selector: named,
        base: plan.base,
        tree: plan.tree,
        changed: plan.changed,
        root,
        worktree,
    })
}

/// The two states a change set was computed between, the paths between them, and the revision to
/// check out. `checkout: None` means the working tree is the tree the scan runs against.
struct Plan {
    base: String,
    tree: String,
    changed: BTreeSet<String>,
    checkout: Option<String>,
}

fn plan_for(invoked_root: &Path, out_dir: &Path, named: &str, selector: &Selector) -> Result<Plan, GitError> {
    match selector {
        // The index is not a tree git can check out, so it becomes a throwaway commit: an
        // unreachable object that names the tree and lets `git.tree` report a real sha.
        Selector::Staged => {
            let head = git(invoked_root, &["rev-parse", "HEAD"], &[]).ok();
            let index_tree = git(invoked_root, &["write-tree"], &[]).map_err(|e| scoped(named, e))?;
            let base = head.clone().unwrap_or(EMPTY_TREE.to_string());
            let tree = match &head {
                Some(head) => git(
                    invoked_root,
                    &["commit-tree", &index_tree, "-p", head, "-m", STAGED_COMMIT],
                    &COMMIT_IDENTITY,
                ),
                // A repository with no commit yet has nothing to parent the throwaway to, so the
                // throwaway is itself a root commit.
                None => git(invoked_root, &["commit-tree", &index_tree, "-m", STAGED_COMMIT], &COMMIT_IDENTITY),
            }
            .map_err(|e| scoped(named, e))?;
            let changed = changed_between(invoked_root, named, &["--cached", &base])?;
            Ok(Plan { base, tree: tree.clone(), changed, checkout: Some(tree) })
        }
        // The working tree is already the state `unstaged` names, so nothing is materialized and
        // the scan runs where the user pointed it.
        Selector::Unstaged => {
            let tracked = git(invoked_root, &["diff", "--name-only", "--no-renames", "--relative"], &[])
                .map_err(|e| scoped(named, e))?;
            let untracked = git(invoked_root, &["ls-files", "--others", "--exclude-standard"], &[])
                .map_err(|e| scoped(named, e))?;
            // The engine writes its own results inside the repository, and a project that does not
            // gitignore them would otherwise read the previous scan's files as this change set.
            let output = under(invoked_root, out_dir);
            let mut changed = paths(&tracked);
            changed.extend(paths(&untracked).into_iter().filter(|path| match &output {
                Some(prefix) => !path.starts_with(prefix.as_str()),
                None => true,
            }));
            Ok(Plan { base: "index".to_string(), tree: "worktree".to_string(), changed, checkout: None })
        }
        Selector::Stash(slot) => {
            let reference = match slot {
                Some(slot) => format!("refs/stash@{{{slot}}}"),
                None => "refs/stash".to_string(),
            };
            let tree = commit_of(invoked_root, named, &reference)?;
            let base = git(invoked_root, &["rev-parse", &format!("{reference}^")], &[]).map_err(|e| unresolved(named, &e.0))?;
            let changed = changed_between(invoked_root, named, &[&base, &tree])?;
            Ok(Plan { base, tree: tree.clone(), changed, checkout: Some(tree) })
        }
        Selector::Rev(revision) => {
            let tree = commit_of(invoked_root, named, revision)?;
            // A root commit has no parent: the empty tree is what makes it scope to its own files.
            let base = git(invoked_root, &["rev-parse", &format!("{revision}^")], &[]).unwrap_or(EMPTY_TREE.to_string());
            let changed = changed_between(invoked_root, named, &[&base, &tree])?;
            Ok(Plan { base, tree: tree.clone(), changed, checkout: Some(tree) })
        }
        // A span names both states outright: the end is the tree that gets scanned, and the start
        // is what the change set is measured against. `...` moves the start to the merge base,
        // which is git's own meaning for it, so `A...B` is the work `B` has that `A` does not.
        // Both sides are resolved first, so a merge-base failure can only mean no shared history.
        Selector::Range { from, to, three_dot } => {
            let tree = commit_of(invoked_root, named, to)?;
            let start = commit_of(invoked_root, named, from)?;
            let base = if *three_dot {
                git(invoked_root, &["merge-base", &start, &tree], &[]).map_err(|_| {
                    GitError(format!("git scope \"{named}\" has no merge base: {from} and {to} share no history"))
                })?
            } else {
                start
            };
            let changed = changed_between(invoked_root, named, &[&base, &tree])?;
            Ok(Plan { base, tree: tree.clone(), changed, checkout: Some(tree) })
        }
    }
}

/// The commit `reference` names, with the message a user needs when it names nothing.
fn commit_of(invoked_root: &Path, named: &str, reference: &str) -> Result<String, GitError> {
    let verify = format!("{reference}^{{commit}}");
    git(invoked_root, &["rev-parse", "--verify", &verify], &[]).map_err(|e| unresolved(named, &e.0))
}

fn unresolved(named: &str, detail: &str) -> GitError {
    GitError(format!("git scope \"{named}\" does not resolve: {detail}"))
}

fn scoped(named: &str, error: GitError) -> GitError {
    GitError(format!("git scope \"{named}\": {}", error.0))
}

/// The paths one change set touches, relative to `invoked_root` because `--relative` says so.
///
/// `--no-renames` is required, not a simplification: rename detection reports an add as a
/// `old -> new` pair, and the old path matches nothing in the scanned tree.
fn changed_between(invoked_root: &Path, named: &str, operands: &[&str]) -> Result<BTreeSet<String>, GitError> {
    let mut args = vec!["diff", "--name-only", "--no-renames", "--relative"];
    args.extend(operands);
    let text = git(invoked_root, &args, &[]).map_err(|e| scoped(named, e))?;
    Ok(paths(&text))
}

fn paths(text: &str) -> BTreeSet<String> {
    text.lines().map(str::to_string).filter(|line| !line.is_empty()).collect()
}

/// `path` expressed as a posix directory prefix under `root`, or `None` when it is not under it.
fn under(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let text = relative.to_string_lossy().replace('\\', "/");
    (!text.is_empty()).then(|| format!("{text}/"))
}

/// Check `treeish` out under `<out_dir>/git-scope/`, inside the scan project's own output
/// directory, which is above the directory the plugin walks.
///
/// `out_dir` is absolute, so the path handed to git does not depend on the process's working
/// directory, which git does not share with the `-C` directory it reads the repository in.
fn materialize(invoked_root: &Path, out_dir: &Path, treeish: &str) -> Result<Worktree, GitError> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let path = out_dir.join("git-scope").join(format!("{}-{nanos}", std::process::id()));
    let destination = path.display().to_string();
    git(invoked_root, &["worktree", "add", "--detach", &destination, treeish], &[])?;
    Ok(Worktree { path, repo: invoked_root.to_path_buf() })
}

/// One `git -C <dir> …`, with git's stderr as the error text.
pub fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, GitError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.args(args).output().map_err(|e| GitError(format!("cannot run git: {e}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if detail.is_empty() { format!("git {} exited {}", args.join(" "), output.status) } else { detail };
        return Err(GitError(detail));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
