use std::path::PathBuf;
use crate::render::{format_sync_error, format_sync_warning};

/// The result of verifying a batch of spec paths.
pub struct VerifyResult {
    /// Lines to print to stdout (one per successfully verified path).
    pub(crate) stdout_lines: Vec<String>,
    /// Lines to print to stderr (one per failure).
    pub(crate) stderr_lines: Vec<String>,
    /// Whether any path failed.
    pub(crate) any_failed: bool,
}

/// Core logic for `ns2 spec verify <paths...>`.
///
/// For each path: resolve it relative to `git_root`, attempt to load + write the spec,
/// record success/failure.  Does NOT call `process::exit` — returns a [`VerifyResult`]
/// so callers (main and tests) can assert on the outcome.
pub fn verify_spec_paths(git_root: &std::path::Path, paths: &[String]) -> VerifyResult {
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut any_failed = false;

    for path in paths {
        let resolved = if PathBuf::from(path).is_absolute() {
            PathBuf::from(path)
        } else {
            git_root.join(path)
        };

        let Some(mut def) = specs::load_spec(&resolved) else {
            stderr_lines.push(format!("Error: could not load spec at {path}"));
            any_failed = true;
            continue;
        };

        def.verified = Some(chrono::Utc::now());

        if let Err(e) = specs::write_spec(&resolved, &def) {
            stderr_lines.push(format!("Error writing spec file {path}: {e}"));
            any_failed = true;
            continue;
        }

        stdout_lines.push(format!("Verified {path}"));
    }

    VerifyResult { stdout_lines, stderr_lines, any_failed }
}

pub fn run_new(path: String, targets: Vec<String>, severity: &str) {
    if targets.is_empty() {
        eprintln!("Error: at least one --target is required");
        std::process::exit(1);
    }
    let severity = match severity {
        "warning" => specs::Severity::Warning,
        "error" => specs::Severity::Error,
        _ => {
            eprintln!("Error: --severity must be 'error' or 'warning'");
            std::process::exit(1);
        }
    };
    let git_root = workspace::git_root_sync().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    let resolved = if PathBuf::from(&path).is_absolute() {
        PathBuf::from(&path)
    } else {
        git_root.join(&path)
    };
    let path_display = path;
    let path = resolved;
    if path.exists() {
        eprintln!("Error: spec already exists at {}", path.display());
        std::process::exit(1);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("Error creating directories: {e}");
                std::process::exit(1);
            }
        }
    }
    let def = specs::SpecDef { targets, verified: None, severity, body: String::new() };
    if let Err(e) = specs::write_spec(&path, &def) {
        eprintln!("Error writing spec file: {e}");
        std::process::exit(1);
    }
    println!("Created spec at {path_display}");
}

pub fn run_sync(path: Option<String>, error_on_warnings: bool) {
    let git_root = workspace::git_root_sync().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    if let Some(p) = path {
        let resolved = if PathBuf::from(&p).is_absolute() {
            PathBuf::from(&p)
        } else {
            git_root.join(&p)
        };
        if resolved.is_dir() {
            let all_specs = specs::list_specs(&resolved);
            let mut has_errors = false;
            for (spec_path, def) in &all_specs {
                let stale = specs::stale_files(&git_root, spec_path, def);
                if !stale.is_empty() {
                    let display_path = spec_path
                        .strip_prefix(&git_root)
                        .unwrap_or(spec_path)
                        .display()
                        .to_string();
                    let is_error =
                        def.severity == specs::Severity::Error || error_on_warnings;
                    if is_error {
                        eprint!("{}", format_sync_error(&display_path, &stale));
                        has_errors = true;
                    } else {
                        eprint!("{}", format_sync_warning(&display_path, &stale));
                    }
                }
            }
            if has_errors {
                std::process::exit(1);
            }
        } else {
            let def = specs::load_spec(&resolved).unwrap_or_else(|| {
                eprintln!("Error: could not load spec at {p}");
                std::process::exit(1);
            });
            let stale = specs::stale_files(&git_root, &resolved, &def);
            if !stale.is_empty() {
                let is_error =
                    def.severity == specs::Severity::Error || error_on_warnings;
                if is_error {
                    eprint!("{}", format_sync_error(&p, &stale));
                    std::process::exit(1);
                } else {
                    eprint!("{}", format_sync_warning(&p, &stale));
                }
            }
        }
    } else {
        let all_specs = specs::list_specs(&git_root);
        let mut has_errors = false;
        for (spec_path, def) in &all_specs {
            let stale = specs::stale_files(&git_root, spec_path, def);
            if !stale.is_empty() {
                let display_path = spec_path
                    .strip_prefix(&git_root)
                    .unwrap_or(spec_path)
                    .display()
                    .to_string();
                let is_error =
                    def.severity == specs::Severity::Error || error_on_warnings;
                if is_error {
                    eprint!("{}", format_sync_error(&display_path, &stale));
                    has_errors = true;
                } else {
                    eprint!("{}", format_sync_warning(&display_path, &stale));
                }
            }
        }
        if has_errors {
            std::process::exit(1);
        }
    }
}

pub fn run_verify(paths: &[String]) {
    let git_root = workspace::git_root_sync().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    let result = verify_spec_paths(&git_root, paths);
    for line in &result.stdout_lines {
        println!("{line}");
    }
    for line in &result.stderr_lines {
        eprintln!("{line}");
    }
    if result.any_failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_valid_spec(dir: &std::path::Path, name: &str, target: &str) {
        let path = dir.join(name);
        let content = format!("---\ntargets:\n  - {target}\n---\n");
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn verify_spec_paths_empty_input() {
        let result = verify_spec_paths(std::path::Path::new("/tmp"), &[]);
        assert!(result.stdout_lines.is_empty());
        assert!(result.stderr_lines.is_empty());
        assert!(!result.any_failed);
    }

    #[test]
    fn verify_spec_paths_valid_spec_absolute() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_spec(dir.path(), "test.spec.md", "crates/foo/src/lib.rs");
        let abs_path = dir.path().join("test.spec.md").to_str().unwrap().to_string();
        let result = verify_spec_paths(dir.path(), &[abs_path]);
        assert!(!result.any_failed, "expected success, got: {:?}", result.stderr_lines);
        assert_eq!(result.stdout_lines.len(), 1);
        assert!(result.stderr_lines.is_empty());
    }

    #[test]
    fn verify_spec_paths_relative_path_resolved_against_root() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_spec(dir.path(), "my.spec.md", "src/lib.rs");
        let result = verify_spec_paths(dir.path(), &["my.spec.md".to_string()]);
        assert!(!result.any_failed, "expected success, got: {:?}", result.stderr_lines);
        assert_eq!(result.stdout_lines.len(), 1);
    }

    #[test]
    fn verify_spec_paths_nonexistent_path_fails() {
        let dir = tempfile::tempdir().unwrap();
        let missing = "/nonexistent/path/does-not-exist.spec.md".to_string();
        let result = verify_spec_paths(dir.path(), &[missing]);
        assert!(result.any_failed);
        assert_eq!(result.stderr_lines.len(), 1);
        assert!(result.stdout_lines.is_empty());
    }

    #[test]
    fn verify_spec_paths_multiple_paths_some_failing() {
        let dir = tempfile::tempdir().unwrap();
        write_valid_spec(dir.path(), "ok.spec.md", "src/main.rs");
        let result = verify_spec_paths(dir.path(), &[
            "ok.spec.md".to_string(),
            "missing.spec.md".to_string(),
        ]);
        assert!(result.any_failed);
        assert_eq!(result.stdout_lines.len(), 1);
        assert_eq!(result.stderr_lines.len(), 1);
    }
}
