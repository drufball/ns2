use std::path::{Path, PathBuf};

// ── Public types ──────────────────────────────────────────────────────────────


/// A single git worktree entry, parsed from `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq)]
pub struct WorktreeEntry {
    pub branch: String,
    pub path: PathBuf,
}

// ── ensure_worktree ───────────────────────────────────────────────────────────

/// Ensure a git worktree exists at `worktree_path` for `branch`.
///
/// Algorithm:
/// 1. If `worktree_path` is already a directory → reuse (no error).
/// 2. Detect the remote default branch via `git rev-parse --abbrev-ref origin/HEAD`
///    (falls back to `"origin/main"` if unset).
/// 3. Try `git worktree add <path> -b <branch> <default_branch>`.
/// 4. If that fails (branch already exists in git) → retry with
///    `git worktree add <path> <branch>`.
///
/// Returns the worktree path on success, or logs a warning and returns `None` on failure.
pub fn ensure_worktree(
    git_root: &Path,
    worktree_path: &Path,
    branch: &str,
) -> Option<PathBuf> {
    // Already exists → reuse.
    if worktree_path.is_dir() {
        return Some(worktree_path.to_path_buf());
    }

    // Create parent directories so `git worktree add` can place the worktree there.
    if let Some(parent) = worktree_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "failed to create worktree parent dir {}: {e}",
                parent.display()
            );
            return None;
        }
    }

    // Detect the remote default branch (e.g. origin/main or origin/master).
    let default_branch = detect_remote_default_branch(git_root);

    // First attempt: create a new branch tracking the remote default branch.
    let output = std::process::Command::new("git")
        .current_dir(git_root)
        .args([
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            "-b",
            branch,
            &default_branch,
        ])
        .output();

    match output {
        Ok(ref o) if o.status.success() => return Some(worktree_path.to_path_buf()),
        Ok(ref o) => {
            tracing::warn!(
                "git worktree add -b {} {} failed: {}",
                branch,
                default_branch,
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(ref e) => {
            tracing::warn!("failed to run git worktree add for branch '{}': {e}", branch);
        }
    }

    // Second attempt: branch already exists in git — check it out into the worktree.
    let output = std::process::Command::new("git")
        .current_dir(git_root)
        .args([
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            branch,
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => Some(worktree_path.to_path_buf()),
        Ok(o) => {
            tracing::warn!(
                "git worktree add failed for branch '{}': {}",
                branch,
                String::from_utf8_lossy(&o.stderr).trim()
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "failed to run git worktree add for branch '{}': {e}",
                branch
            );
            None
        }
    }
}

// ── detect_remote_default_branch ─────────────────────────────────────────────

/// Detect the remote default branch by asking git what `origin/HEAD` resolves to.
///
/// Returns the full remote ref (e.g. `"origin/master"` or `"origin/main"`).
/// Falls back to `"origin/main"` when `origin/HEAD` is not set or the git command fails.
pub(crate) fn detect_remote_default_branch(git_root: &Path) -> String {
    std::process::Command::new("git")
        .current_dir(git_root)
        .args(["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "origin/main".to_string())
}

// ── list_worktrees ────────────────────────────────────────────────────────────

/// Parse `git worktree list --porcelain` output into a list of `WorktreeEntry` values,
/// keeping only entries whose path is under `worktree_base`.
///
/// Returns an empty `Vec` when git is not available or the output cannot be parsed.
pub fn list_worktrees(git_root: &Path, worktree_base: &Path) -> Vec<WorktreeEntry> {
    let output = match std::process::Command::new("git")
        .current_dir(git_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return vec![],
    };

    let text = match String::from_utf8(output) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    parse_worktree_porcelain(&text, worktree_base)
}

/// Parse the porcelain output of `git worktree list --porcelain`.
///
/// Each worktree block looks like:
/// ```text
/// worktree /abs/path/to/worktree
/// HEAD <sha>
/// branch refs/heads/<branch>
///
/// ```
/// Blocks are separated by blank lines.
pub fn parse_worktree_porcelain(text: &str, worktree_base: &Path) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in text.lines() {
        if line.is_empty() {
            // End of a block — emit entry if we have both fields and the path is under base.
            if let (Some(path), Some(branch)) = (current_path.take(), current_branch.take()) {
                if path.starts_with(worktree_base) {
                    entries.push(WorktreeEntry { branch, path });
                }
            }
            // Reset for next block even if we didn't emit.
            current_path = None;
            current_branch = None;
        } else if let Some(p) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(b.to_string());
        }
    }

    // Flush the last block (file may not end with a blank line).
    if let (Some(path), Some(branch)) = (current_path, current_branch) {
        if path.starts_with(worktree_base) {
            entries.push(WorktreeEntry { branch, path });
        }
    }

    entries
}

// ── delete_worktree ───────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum DeleteWorktreeError {
    #[error("no worktree found for branch {0}")]
    NotFound(String),
    #[error("branch {0} has unmerged commits. Use --force to delete anyway.")]
    UnmergedCommits(String),
    #[error("git command failed: {0}")]
    GitFailed(String),
}

/// Delete a worktree for `branch`.
///
/// Steps:
/// 1. Compute `worktree_path = worktree_base / branch`.
/// 2. If path does not exist → `DeleteWorktreeError::NotFound`.
/// 3. Check whether branch is merged to main using
///    `git merge-base --is-ancestor <branch> main`.
///    If it has unmerged commits and `force` is false → `DeleteWorktreeError::UnmergedCommits`.
/// 4. Run `git worktree remove --force <path>`.
/// 5. Run `git branch -D <branch>`.
pub fn delete_worktree(
    git_root: &Path,
    worktree_base: &Path,
    branch: &str,
    force: bool,
) -> Result<PathBuf, DeleteWorktreeError> {
    let worktree_path = worktree_base.join(branch);

    if !worktree_path.exists() {
        return Err(DeleteWorktreeError::NotFound(branch.to_string()));
    }

    // Check merge status unless --force was passed.
    if !force {
        let merged = is_branch_merged_to_main(git_root, branch);
        if !merged {
            return Err(DeleteWorktreeError::UnmergedCommits(branch.to_string()));
        }
    }

    // Remove the worktree directory.
    let remove_status = std::process::Command::new("git")
        .current_dir(git_root)
        .args(["worktree", "remove", "--force", &worktree_path.to_string_lossy()])
        .status();

    match remove_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            return Err(DeleteWorktreeError::GitFailed(format!(
                "git worktree remove exited with code {:?}",
                s.code()
            )));
        }
        Err(e) => {
            return Err(DeleteWorktreeError::GitFailed(format!(
                "failed to run git worktree remove: {e}"
            )));
        }
    }

    // Delete the local branch.
    let branch_status = std::process::Command::new("git")
        .current_dir(git_root)
        .args(["branch", "-D", branch])
        .status();

    match branch_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            return Err(DeleteWorktreeError::GitFailed(format!(
                "git branch -D exited with code {:?}",
                s.code()
            )));
        }
        Err(e) => {
            return Err(DeleteWorktreeError::GitFailed(format!(
                "failed to run git branch -D: {e}"
            )));
        }
    }

    Ok(worktree_path)
}

// ── Merge-status helper ───────────────────────────────────────────────────────

/// Returns `true` if `branch` is an ancestor of (or equal to) the repo's default branch
/// (detected via `origin/HEAD`, falling back to `main`).
///
/// Uses `git merge-base --is-ancestor <branch> <default-branch>`.
fn is_branch_merged_to_main(git_root: &Path, branch: &str) -> bool {
    // Detect the local name of the default branch (strip "origin/" prefix).
    let remote_default = detect_remote_default_branch(git_root);
    let local_default = remote_default
        .strip_prefix("origin/")
        .unwrap_or(&remote_default)
        .to_string();

    std::process::Command::new("git")
        .current_dir(git_root)
        .args(["merge-base", "--is-ancestor", branch, &local_default])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── detect_remote_default_branch ─────────────────────────────────────────

    /// Build a repo whose remote default branch is `master` (not `main`) and verify
    /// that `detect_remote_default_branch` returns `"origin/master"`.
    #[test]
    fn detect_remote_default_branch_returns_origin_master_when_remote_uses_master() {
        let (local_dir, _origin_dir) = setup_git_repo_with_remote_branch("master");
        let result = detect_remote_default_branch(local_dir.path());
        assert_eq!(
            result, "origin/master",
            "should detect origin/master when remote HEAD points to master"
        );
    }

    /// When `origin/HEAD` is not set at all, the function must fall back to `"origin/main"`.
    #[test]
    fn detect_remote_default_branch_falls_back_to_origin_main_when_head_unset() {
        let origin_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "--bare"])
            .current_dir(origin_dir.path())
            .status()
            .expect("git init --bare");

        let local_dir = tempfile::TempDir::new().unwrap();
        // Clone but then explicitly unset origin/HEAD so rev-parse fails.
        std::process::Command::new("git")
            .args(["clone", &origin_dir.path().to_string_lossy(), "."])
            .current_dir(local_dir.path())
            .status()
            .expect("git clone");

        // Ensure origin/HEAD does not exist (bare repo with no commits has no HEAD to resolve).
        // We forcibly delete it if cloning set one.
        let _ = std::process::Command::new("git")
            .args(["remote", "set-head", "origin", "--delete"])
            .current_dir(local_dir.path())
            .status();

        let result = detect_remote_default_branch(local_dir.path());
        assert_eq!(
            result, "origin/main",
            "should fall back to origin/main when origin/HEAD is not set"
        );
    }

    /// `ensure_worktree` must succeed even when the remote only has `origin/master`
    /// (not `origin/main`).  The function should auto-detect the default branch and
    /// use it as the start point instead of hardcoding `origin/main`.
    #[test]
    fn ensure_worktree_succeeds_when_remote_default_is_master() {
        let (local_dir, _origin_dir) = setup_git_repo_with_remote_branch("master");
        let wt_base = tempfile::TempDir::new().unwrap();
        let branch = "feat/from-master";
        let worktree_path = wt_base.path().join(branch);

        let result = ensure_worktree(local_dir.path(), &worktree_path, branch);

        assert!(
            result.is_some(),
            "ensure_worktree must succeed when remote default branch is master, not main"
        );
        assert!(
            worktree_path.is_dir(),
            "worktree directory must be created"
        );
    }

    // ── parse_worktree_porcelain ──────────────────────────────────────────────

    #[test]
    fn parse_worktree_porcelain_empty_input_returns_empty() {
        let base = PathBuf::from("/worktrees");
        let entries = parse_worktree_porcelain("", &base);
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_worktree_porcelain_single_block_under_base_is_included() {
        let base = PathBuf::from("/worktrees");
        let text = "worktree /worktrees/feat/my-branch\nHEAD abc123\nbranch refs/heads/feat/my-branch\n\n";
        let entries = parse_worktree_porcelain(text, &base);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch, "feat/my-branch");
        assert_eq!(entries[0].path, PathBuf::from("/worktrees/feat/my-branch"));
    }

    #[test]
    fn parse_worktree_porcelain_entry_outside_base_is_excluded() {
        let base = PathBuf::from("/worktrees");
        let text = "worktree /other/path\nHEAD abc123\nbranch refs/heads/main\n\n";
        let entries = parse_worktree_porcelain(text, &base);
        assert!(entries.is_empty(), "main worktree outside base should be filtered out");
    }

    #[test]
    fn parse_worktree_porcelain_multiple_blocks_filters_correctly() {
        let base = PathBuf::from("/worktrees");
        let text = concat!(
            "worktree /main/repo\n",
            "HEAD aaa\n",
            "branch refs/heads/main\n",
            "\n",
            "worktree /worktrees/feat/a\n",
            "HEAD bbb\n",
            "branch refs/heads/feat/a\n",
            "\n",
            "worktree /worktrees/feat/b\n",
            "HEAD ccc\n",
            "branch refs/heads/feat/b\n",
            "\n",
        );
        let entries = parse_worktree_porcelain(text, &base);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.branch == "feat/a"));
        assert!(entries.iter().any(|e| e.branch == "feat/b"));
        assert!(!entries.iter().any(|e| e.branch == "main"));
    }

    #[test]
    fn parse_worktree_porcelain_detached_head_no_branch_line_excluded() {
        let base = PathBuf::from("/worktrees");
        // Detached HEAD entries have no "branch" line.
        let text = "worktree /worktrees/detached\nHEAD abc123\ndetached\n\n";
        let entries = parse_worktree_porcelain(text, &base);
        assert!(
            entries.is_empty(),
            "detached HEAD entry has no branch — must be excluded"
        );
    }

    #[test]
    fn parse_worktree_porcelain_last_block_without_trailing_newline() {
        let base = PathBuf::from("/worktrees");
        // The last block might not have a trailing blank line.
        let text = "worktree /worktrees/feat/x\nHEAD abc\nbranch refs/heads/feat/x";
        let entries = parse_worktree_porcelain(text, &base);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch, "feat/x");
    }

    // ── ensure_worktree ───────────────────────────────────────────────────────

    #[test]
    fn ensure_worktree_existing_dir_is_reused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let worktree_path = tmp.path().join("existing-wt");
        std::fs::create_dir_all(&worktree_path).unwrap();

        // git_root doesn't matter because the path already exists
        let result = ensure_worktree(tmp.path(), &worktree_path, "my-branch");
        assert_eq!(result, Some(worktree_path));
    }

    #[test]
    fn ensure_worktree_creates_worktree_in_real_git_repo() {
        let (local_dir, wt_base) = setup_git_repo_with_remote();
        let branch = "feature/my-feature";
        let worktree_path = wt_base.path().join(branch);

        let result = ensure_worktree(local_dir.path(), &worktree_path, branch);

        assert!(
            result.is_some(),
            "ensure_worktree should succeed for new branch in real git repo"
        );
        assert!(
            worktree_path.is_dir(),
            "worktree directory should be created at expected path"
        );
    }

    #[test]
    fn ensure_worktree_reuse_existing_worktree() {
        let (local_dir, wt_base) = setup_git_repo_with_remote();
        let branch = "feat/reuse-test";
        let worktree_path = wt_base.path().join(branch);

        let result1 = ensure_worktree(local_dir.path(), &worktree_path, branch);
        assert!(result1.is_some(), "first ensure_worktree should succeed");

        let result2 = ensure_worktree(local_dir.path(), &worktree_path, branch);
        assert!(result2.is_some(), "second ensure_worktree should succeed (reuse)");
        assert!(worktree_path.is_dir(), "worktree dir should still exist");
    }

    #[test]
    fn ensure_worktree_existing_local_branch_checkout() {
        let (local_dir, wt_base) = setup_git_repo_with_remote();
        let branch = "existing-branch";

        // Pre-create the branch locally.
        std::process::Command::new("git")
            .args(["branch", branch])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        let worktree_path = wt_base.path().join(branch);
        let result = ensure_worktree(local_dir.path(), &worktree_path, branch);
        assert!(
            result.is_some(),
            "ensure_worktree should succeed for existing local branch via fallback"
        );
        assert!(worktree_path.is_dir(), "worktree dir should exist");
    }

    // ── delete_worktree ───────────────────────────────────────────────────────

    #[test]
    fn delete_worktree_missing_path_returns_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        let result = delete_worktree(tmp.path(), &base, "no-such-branch", false);
        assert!(
            matches!(result, Err(DeleteWorktreeError::NotFound(_))),
            "expected NotFound, got: {result:?}"
        );
    }

    #[test]
    fn delete_worktree_unmerged_without_force_returns_error() {
        let (local_dir, wt_base) = setup_git_repo_with_remote();
        let branch = "feat/unmerged";
        let worktree_path = wt_base.path().join(branch);

        // Create the worktree with a new commit not on main.
        ensure_worktree(local_dir.path(), &worktree_path, branch).unwrap();

        // Add a commit to the worktree branch so it has unmerged commits.
        std::fs::write(worktree_path.join("new_file.txt"), "content").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&worktree_path)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "unmerged commit"])
            .current_dir(&worktree_path)
            .status()
            .unwrap();

        let result = delete_worktree(local_dir.path(), wt_base.path(), branch, false);
        assert!(
            matches!(result, Err(DeleteWorktreeError::UnmergedCommits(_))),
            "expected UnmergedCommits, got: {result:?}"
        );
    }

    #[test]
    fn delete_worktree_unmerged_with_force_succeeds() {
        let (local_dir, wt_base) = setup_git_repo_with_remote();
        let branch = "feat/force-delete";
        let worktree_path = wt_base.path().join(branch);

        ensure_worktree(local_dir.path(), &worktree_path, branch).unwrap();

        // Add an unmerged commit.
        std::fs::write(worktree_path.join("extra.txt"), "data").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&worktree_path)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "extra"])
            .current_dir(&worktree_path)
            .status()
            .unwrap();

        let result = delete_worktree(local_dir.path(), wt_base.path(), branch, true);
        assert!(
            result.is_ok(),
            "force delete of unmerged worktree should succeed, got: {result:?}"
        );
        assert!(!worktree_path.exists(), "worktree directory should be gone");
    }

    #[test]
    fn delete_worktree_merged_branch_succeeds_without_force() {
        let (local_dir, wt_base) = setup_git_repo_with_remote();
        let branch = "feat/merged-branch";
        let worktree_path = wt_base.path().join(branch);

        // Create the worktree (branches off origin/main, so it is already merged into main).
        ensure_worktree(local_dir.path(), &worktree_path, branch).unwrap();

        // The branch has no unique commits → it IS an ancestor of main → merged.
        let result = delete_worktree(local_dir.path(), wt_base.path(), branch, false);
        assert!(
            result.is_ok(),
            "deleting a merged worktree without --force should succeed, got: {result:?}"
        );
        assert!(!worktree_path.exists(), "worktree directory should be gone");
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Set up a temporary bare origin + a cloned local repo with an initial commit on `main`.
    /// Returns `(local_dir, wt_base)` as `TempDir` handles.
    pub(crate) fn setup_git_repo_with_remote() -> (tempfile::TempDir, tempfile::TempDir) {
        let origin_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(origin_dir.path())
            .status()
            .expect("git init --bare");

        let local_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["clone", &origin_dir.path().to_string_lossy(), "."])
            .current_dir(local_dir.path())
            .status()
            .expect("git clone");

        for (k, v) in &[("user.email", "test@test.com"), ("user.name", "Test")] {
            std::process::Command::new("git")
                .args(["config", k, v])
                .current_dir(local_dir.path())
                .status()
                .unwrap();
        }

        std::fs::write(local_dir.path().join("README.md"), "init").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(local_dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(local_dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        let wt_base = tempfile::TempDir::new().unwrap();
        (local_dir, wt_base)
    }

    /// Set up a bare origin + cloned local repo whose default branch is `branch_name`
    /// (e.g. `"master"` or `"main"`).  `origin/HEAD` is set so that
    /// `git rev-parse --abbrev-ref origin/HEAD` works.
    ///
    /// Returns `(local_dir, origin_dir)`.
    fn setup_git_repo_with_remote_branch(
        branch_name: &str,
    ) -> (tempfile::TempDir, tempfile::TempDir) {
        let origin_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init", "--bare", "--initial-branch", branch_name])
            .current_dir(origin_dir.path())
            .status()
            .expect("git init --bare");

        let local_dir = tempfile::TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["clone", &origin_dir.path().to_string_lossy(), "."])
            .current_dir(local_dir.path())
            .status()
            .expect("git clone");

        for (k, v) in &[("user.email", "test@test.com"), ("user.name", "Test")] {
            std::process::Command::new("git")
                .args(["config", k, v])
                .current_dir(local_dir.path())
                .status()
                .unwrap();
        }

        std::fs::write(local_dir.path().join("README.md"), "init").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(local_dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(local_dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "origin", branch_name])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        // Ensure origin/HEAD is set so `rev-parse --abbrev-ref origin/HEAD` works.
        std::process::Command::new("git")
            .args(["remote", "set-head", "origin", "--auto"])
            .current_dir(local_dir.path())
            .status()
            .unwrap();

        (local_dir, origin_dir)
    }
}
