use std::path::PathBuf;

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
