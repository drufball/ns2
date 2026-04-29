use std::path::PathBuf;
use crate::render::{format_sync_error, format_sync_warning};

/// The result of verifying a batch of spec paths.
pub(crate) struct VerifyResult {
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
pub(crate) fn verify_spec_paths(git_root: &std::path::Path, paths: &[String]) -> VerifyResult {
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();
    let mut any_failed = false;

    for path in paths {
        let resolved = if PathBuf::from(path).is_absolute() {
            PathBuf::from(path)
        } else {
            git_root.join(path)
        };

        let mut def = match specs::load_spec(&resolved) {
            Some(d) => d,
            None => {
                stderr_lines.push(format!("Error: could not load spec at {path}"));
                any_failed = true;
                continue;
            }
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

pub(crate) fn run_new(path: String, targets: Vec<String>, severity: String) {
    if targets.is_empty() {
        eprintln!("Error: at least one --target is required");
        std::process::exit(1);
    }
    let severity = match severity.as_str() {
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
    let path_display = path.clone();
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

pub(crate) fn run_sync(path: Option<String>, error_on_warnings: bool) {
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

pub(crate) fn run_verify(paths: Vec<String>) {
    let git_root = workspace::git_root_sync().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    let result = verify_spec_paths(&git_root, &paths);
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
