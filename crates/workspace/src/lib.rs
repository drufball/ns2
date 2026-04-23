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

/// Returns the subset of `files` (relative to `root`) that have commits
/// after `after_iso` (ISO 8601 UTC string).
pub fn git_files_committed_after(root: &Path, after_iso: &str, files: &[PathBuf]) -> Vec<PathBuf> {
    if files.is_empty() {
        return Vec::new();
    }
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(root)
        .args(["log", "--name-only", "--format=", &format!("--after={after_iso}"), "--"]);
    for f in files {
        cmd.arg(f);
    }
    let Ok(output) = cmd.output() else { return Vec::new() };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(stdout) = String::from_utf8(output.stdout) else { return Vec::new() };
    stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect()
}

/// Returns the subset of `files` (relative to `root`) that have uncommitted
/// local modifications (staged or unstaged).
pub fn git_files_locally_modified(root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    if files.is_empty() {
        return Vec::new();
    }
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(root).args(["status", "--porcelain", "--"]);
    for f in files {
        cmd.arg(f);
    }
    let Ok(output) = cmd.output() else { return Vec::new() };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(stdout) = String::from_utf8(output.stdout) else { return Vec::new() };
    // Format: "XY filename" — first 3 chars are XY status codes + space
    stdout.lines().filter(|l| l.len() > 3).map(|l| PathBuf::from(l[3..].trim())).collect()
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
}
