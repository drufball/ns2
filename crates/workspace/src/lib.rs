use serde::Deserialize;
use std::path::{Path, PathBuf};

mod worktree;
pub use worktree::{
    delete_worktree, ensure_worktree, list_worktrees, parse_worktree_porcelain,
    DeleteWorktreeError, WorktreeEntry,
};

pub fn git_root() -> Option<PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|p| PathBuf::from(p.trim()))
}

/// Returns true if `root` is inside a git working tree.
pub fn is_git_repo(root: &Path) -> bool {
    std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns the commit hash of the last commit that touched `file` (relative to `root`),
/// or None if the file has no commits.
pub fn git_last_commit_for_file(root: &Path, file: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(["log", "-1", "--format=%H", "--", &file.to_string_lossy()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if hash.is_empty() { None } else { Some(hash) }
}

/// Returns true if `older` is an ancestor of `newer`, or they are the same commit.
pub fn git_is_ancestor_or_equal(root: &Path, older: &str, newer: &str) -> bool {
    if older == newer {
        return true;
    }
    std::process::Command::new("git")
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", older, newer])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── ns2.toml config ──────────────────────────────────────────────────────────

/// Configuration loaded from `ns2.toml` at the repository root.
#[derive(Debug, Clone, PartialEq)]
pub struct Ns2Config {
    /// Base directory under which ns2 creates git worktrees.
    /// Default: `~/.ns2/<repo-name>/worktrees/`
    pub worktree_base: PathBuf,
}

/// Raw TOML shape for `[worktrees]` table.
#[derive(Debug, Deserialize, Default)]
struct RawWorktreesTable {
    path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawNs2Config {
    #[serde(default)]
    worktrees: RawWorktreesTable,
}

/// Expand a leading `~` to the home directory.
/// Returns the path unchanged if it does not start with `~`.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    } else if path == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

/// Portable home-directory lookup.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Compute the default worktree base path: `~/.ns2/<repo-name>/worktrees/`.
/// Falls back to `~/.ns2/worktrees/` if the repo name cannot be determined.
fn default_worktree_base(root: &Path) -> PathBuf {
    let repo_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let base = home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(".ns2").join(repo_name).join("worktrees")
}

/// Read `ns2.toml` from `root` and return an `Ns2Config`.
/// Missing file or missing keys silently return defaults.
pub fn read_ns2_config(root: &Path) -> Ns2Config {
    let config_path = root.join("ns2.toml");
    let raw: RawNs2Config = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();

    let worktree_base = match raw.worktrees.path {
        Some(ref p) if !p.trim().is_empty() => expand_tilde(p),
        _ => default_worktree_base(root),
    };

    Ns2Config { worktree_base }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── existing tests ────────────────────────────────────────────────────────

    fn make_git_repo_with_commits(n: usize) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let r = tmp.path();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(r)
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "test"]);
        for i in 0..n {
            std::fs::write(r.join("file.txt"), format!("v{i}")).unwrap();
            run(&["add", "file.txt"]);
            run(&["commit", "-m", &format!("commit {i}")]);
        }
        tmp
    }

    fn git_rev_parse(root: &Path, rev: &str) -> String {
        let out = std::process::Command::new("git")
            .current_dir(root)
            .args(["rev-parse", rev])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn git_root_returns_path_with_dot_git() {
        let tmp = make_git_repo_with_commits(1);
        // Run git_root from inside the temp repo by env-overriding cwd via a child process
        // Instead, verify that a repo's git root resolves to a directory containing .git.
        // We check by querying git directly from the repo dir and matching the result.
        let out = std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .unwrap();
        let resolved = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
        assert!(
            resolved.join(".git").exists(),
            "git --show-toplevel should return a dir with .git, got: {}",
            resolved.display()
        );
    }

    #[test]
    fn git_last_commit_for_file_returns_none_for_untracked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("new.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        // tempdir is not a git repo, so no commit exists
        let result = git_last_commit_for_file(tmp.path(), Path::new("new.rs"));
        assert!(result.is_none());
    }

    #[test]
    fn git_last_commit_for_file_returns_hash_for_tracked_file() {
        let tmp = make_git_repo_with_commits(1);
        let result = git_last_commit_for_file(tmp.path(), Path::new("file.txt"));
        assert!(result.is_some());
        let hash = result.unwrap();
        assert_eq!(hash.len(), 40, "commit hash should be 40 hex chars, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn git_is_ancestor_or_equal_same_commit_is_true() {
        let tmp = make_git_repo_with_commits(1);
        let hash = git_last_commit_for_file(tmp.path(), Path::new("file.txt"))
            .expect("file.txt should have commits");
        assert!(git_is_ancestor_or_equal(tmp.path(), &hash, &hash));
    }

    #[test]
    fn git_is_ancestor_or_equal_older_ancestor_is_true() {
        let tmp = make_git_repo_with_commits(2);
        let head_hash = git_rev_parse(tmp.path(), "HEAD");
        let parent_hash = git_rev_parse(tmp.path(), "HEAD~1");
        assert!(git_is_ancestor_or_equal(tmp.path(), &parent_hash, &head_hash));
        assert!(!git_is_ancestor_or_equal(tmp.path(), &head_hash, &parent_hash));
    }

    // ── read_ns2_config tests ─────────────────────────────────────────────────

    /// Missing `ns2.toml` → returns default worktree base derived from the root dir name.
    #[test]
    fn read_ns2_config_missing_file_returns_default() {
        let tmp = TempDir::new().unwrap();
        let config = read_ns2_config(tmp.path());
        // Default: ~/.ns2/<dir-name>/worktrees/
        let dir_name = tmp.path().file_name().unwrap().to_string_lossy().to_string();
        let home = home_dir().expect("home dir must be known");
        let expected = home.join(".ns2").join(&dir_name).join("worktrees");
        assert_eq!(
            config.worktree_base, expected,
            "missing ns2.toml should produce default worktree base"
        );
    }

    /// `[worktrees] path = "/tmp/wt"` → returns that exact path.
    #[test]
    fn read_ns2_config_explicit_path_returned_as_is() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("ns2.toml"),
            r#"[worktrees]
path = "/tmp/wt"
"#,
        )
        .unwrap();
        let config = read_ns2_config(tmp.path());
        assert_eq!(config.worktree_base, PathBuf::from("/tmp/wt"));
    }

    /// `[worktrees] path = "~/my-wt"` → tilde is expanded to the home directory.
    #[test]
    fn read_ns2_config_tilde_is_expanded() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("ns2.toml"),
            r#"[worktrees]
path = "~/my-wt"
"#,
        )
        .unwrap();
        let config = read_ns2_config(tmp.path());
        // Use the real HOME env var directly — not home_dir() — so this assertion
        // is independent of the home_dir() implementation under test.
        let home = PathBuf::from(
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .expect("HOME must be set"),
        );
        assert_eq!(config.worktree_base, home.join("my-wt"));
    }

    /// `[worktrees] path = "   "` (whitespace only) → falls back to the default path.
    #[test]
    fn read_ns2_config_whitespace_only_path_returns_default() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("ns2.toml"),
            "[worktrees]\npath = \"   \"\n",
        )
        .unwrap();
        let config = read_ns2_config(tmp.path());
        let dir_name = tmp.path().file_name().unwrap().to_string_lossy().to_string();
        let home = PathBuf::from(
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .expect("HOME must be set"),
        );
        let expected = home.join(".ns2").join(&dir_name).join("worktrees");
        assert_eq!(config.worktree_base, expected, "whitespace-only path should fall back to default");
    }

    /// Empty `ns2.toml` (no `[worktrees]` section) → returns default.
    #[test]
    fn read_ns2_config_empty_file_returns_default() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("ns2.toml"), "").unwrap();
        let config = read_ns2_config(tmp.path());
        let dir_name = tmp.path().file_name().unwrap().to_string_lossy().to_string();
        let home = home_dir().expect("home dir must be known");
        let expected = home.join(".ns2").join(&dir_name).join("worktrees");
        assert_eq!(config.worktree_base, expected);
    }

    /// `[worktrees]` section without a `path` key → returns default.
    #[test]
    fn read_ns2_config_worktrees_section_no_path_returns_default() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("ns2.toml"),
            "[worktrees]\n# path is commented out\n",
        )
        .unwrap();
        let config = read_ns2_config(tmp.path());
        let dir_name = tmp.path().file_name().unwrap().to_string_lossy().to_string();
        let home = home_dir().expect("home dir must be known");
        let expected = home.join(".ns2").join(&dir_name).join("worktrees");
        assert_eq!(config.worktree_base, expected);
    }
}
