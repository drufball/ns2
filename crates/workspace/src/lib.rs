use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_root_returns_path_with_dot_git() {
        let root = git_root().expect("should be inside a git repo");
        assert!(
            root.join(".git").exists(),
            "git_root() should return a directory containing .git, got: {}",
            root.display()
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
        let root = git_root().expect("should be inside a git repo");
        // CLAUDE.md is always committed in this repo
        let result = git_last_commit_for_file(&root, Path::new("CLAUDE.md"));
        assert!(result.is_some());
        let hash = result.unwrap();
        assert_eq!(hash.len(), 40, "commit hash should be 40 hex chars, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn git_is_ancestor_or_equal_same_commit_is_true() {
        let root = git_root().expect("should be inside a git repo");
        let hash = git_last_commit_for_file(&root, Path::new("CLAUDE.md"))
            .expect("CLAUDE.md should have commits");
        assert!(git_is_ancestor_or_equal(&root, &hash, &hash));
    }

    #[test]
    fn git_is_ancestor_or_equal_older_ancestor_is_true() {
        let root = git_root().expect("should be inside a git repo");
        // HEAD~1 is an ancestor of HEAD
        let head = std::process::Command::new("git")
            .current_dir(&root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let head_hash = String::from_utf8(head.stdout).unwrap().trim().to_string();
        let parent = std::process::Command::new("git")
            .current_dir(&root)
            .args(["rev-parse", "HEAD~1"])
            .output()
            .unwrap();
        let parent_hash = String::from_utf8(parent.stdout).unwrap().trim().to_string();
        assert!(git_is_ancestor_or_equal(&root, &parent_hash, &head_hash));
        assert!(!git_is_ancestor_or_equal(&root, &head_hash, &parent_hash));
    }
}
