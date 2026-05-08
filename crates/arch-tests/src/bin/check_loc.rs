//! check-loc — flag Rust files whose code LOC exceeds a threshold.
//!
//! "Code LOC" excludes:
//!   - blank lines (whitespace-only)
//!   - single-line comments starting with //
//!   - block-comment delimiters /* and */
//!
//! Usage:
//!   check-loc [--threshold N] [--test-threshold N] [--dir DIR]
//!
//! Options:
//!   --threshold N        Max code LOC for production files (default: 1000)
//!   --test-threshold N   Max code LOC for test files (default: same as --threshold)
//!   --dir DIR            Directory to scan (default: crates/ relative to cwd)
//!
//! Exit codes:
//!   0  All files within limits
//!   1  One or more files exceed the limit
//!   2  Usage error

use std::{
    fs,
    path::{Path, PathBuf},
    process,
};

fn count_code_loc_str(content: &str) -> (usize, usize) {
    let mut in_block = false;
    let mut prod_lines = 0usize;
    let mut test_lines = 0usize;
    let mut in_test_mod = false;
    let mut cfg_test_seen = false;
    let mut brace_depth = 0i32;

    for raw in content.lines() {
        let stripped = raw.trim();

        if in_block {
            if let Some(end) = stripped.find("*/") {
                in_block = false;
                let after_close = stripped[end + 2..].trim();
                if !after_close.is_empty() && !after_close.starts_with("//") {
                    if in_test_mod {
                        test_lines += 1;
                    } else {
                        prod_lines += 1;
                    }
                    for ch in after_close.chars() {
                        match ch {
                            '{' => brace_depth += 1,
                            '}' => {
                                brace_depth -= 1;
                                if in_test_mod && brace_depth == 0 {
                                    in_test_mod = false;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            continue;
        }

        if stripped.is_empty() {
            continue;
        }

        if stripped.starts_with("//") {
            continue;
        }

        if stripped == "#[cfg(test)]" && !in_test_mod {
            cfg_test_seen = true;
            test_lines += 1;
            continue;
        }

        let is_code_line;
        if let Some(block_start) = stripped.find("/*") {
            let before = stripped[..block_start].trim();
            let after = &stripped[block_start + 2..];
            if after.contains("*/") {
                is_code_line = if before.is_empty() {
                    let close = after.find("*/").unwrap();
                    let after_close = after[close + 2..].trim();
                    !after_close.is_empty() && !after_close.starts_with("//")
                } else {
                    true
                };
            } else {
                in_block = true;
                is_code_line = !before.is_empty();
            }
        } else {
            is_code_line = true;
        }

        if !is_code_line {
            continue;
        }

        if cfg_test_seen && stripped.contains('{') {
            cfg_test_seen = false;
            in_test_mod = true;
        }

        if in_test_mod {
            test_lines += 1;
            for ch in stripped.chars() {
                match ch {
                    '{' => brace_depth += 1,
                    '}' => {
                        brace_depth -= 1;
                        if brace_depth == 0 {
                            in_test_mod = false;
                        }
                    }
                    _ => {}
                }
            }
        } else {
            prod_lines += 1;
            if cfg_test_seen {
                for ch in stripped.chars() {
                    match ch {
                        '{' => brace_depth += 1,
                        '}' => brace_depth -= 1,
                        _ => {}
                    }
                }
            }
        }
    }

    (prod_lines, test_lines)
}

fn count_code_loc(path: &Path) -> (usize, usize) {
    fs::read_to_string(path).map_or((0, 0), |content| count_code_loc_str(&content))
}

fn is_test_file(rel: &str) -> bool {
    rel.contains("/tests/") || rel.ends_with("_test.rs") || {
        // bare filename starts with test_
        let fname = rel.rsplit('/').next().unwrap_or(rel);
        fname.starts_with("test_")
    }
}

fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_recursive(dir, &mut files);
    files.sort();
    files
}

fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn usage() -> ! {
    eprintln!("Usage: check-loc [--threshold N] [--test-threshold N] [--dir DIR]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --threshold N        Max code LOC for production files (default: 1000)");
    eprintln!("  --test-threshold N   Max code LOC for test files (default: same as --threshold)");
    eprintln!("  --dir DIR            Directory to scan (default: crates/)");
    process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut threshold: usize = 1000;
    let mut test_threshold: Option<usize> = None;
    let mut scan_dir: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold" => {
                i += 1;
                threshold = args.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                    eprintln!("--threshold requires a numeric argument");
                    usage()
                });
            }
            "--test-threshold" => {
                i += 1;
                test_threshold =
                    Some(args.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                        eprintln!("--test-threshold requires a numeric argument");
                        usage()
                    }));
            }
            "--dir" => {
                i += 1;
                scan_dir = Some(PathBuf::from(args.get(i).unwrap_or_else(|| {
                    eprintln!("--dir requires a path argument");
                    usage()
                })));
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("Unknown option: {other}");
                usage();
            }
        }
        i += 1;
    }

    let test_threshold = test_threshold.unwrap_or(threshold);
    let scan_dir = scan_dir.unwrap_or_else(|| cwd.join("crates"));

    let files = collect_rs_files(&scan_dir);

    let mut violations: Vec<String> = Vec::new();

    for file in &files {
        let rel = file
            .strip_prefix(&cwd)
            .unwrap_or(file.as_path())
            .to_string_lossy()
            .to_string();

        let (prod_loc, test_loc) = count_code_loc(file);

        if is_test_file(&rel) {
            let total = prod_loc + test_loc;
            if total > test_threshold {
                violations.push(format!("  {total} / {test_threshold}  [test]  {rel}"));
            }
        } else {
            if prod_loc > threshold {
                violations.push(format!("  {prod_loc} / {threshold}  [src]  {rel}"));
            }
            if test_loc > test_threshold {
                violations.push(format!(
                    "  {test_loc} / {test_threshold}  [inline-test]  {rel}"
                ));
            }
        }
    }

    if violations.is_empty() {
        println!("LOC check passed (threshold: src={threshold}, test={test_threshold}).");
    } else {
        println!(
            "LOC check FAILED — {} file(s) exceed the limit (threshold: src={}, test={}):",
            violations.len(),
            threshold,
            test_threshold
        );
        for v in &violations {
            println!("{v}");
        }
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::count_code_loc_str;
    use super::is_test_file;

    // Helper: trim leading indentation from a here-doc string so we can
    // write nicely-indented test content inside a test function.
    fn dedent(s: &str) -> String {
        // Find the minimum indentation of non-empty lines.
        let min_indent = s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        s.lines()
            .map(|l| {
                if l.len() >= min_indent {
                    &l[min_indent..]
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn only_blank_lines_returns_zero() {
        let content = "\n   \n\t\n\n";
        assert_eq!(count_code_loc_str(content), (0, 0));
    }

    #[test]
    fn only_line_comments_returns_zero() {
        let content = dedent(
            "
            // This is a comment
            // So is this
            //! doc comment
        ",
        );
        assert_eq!(count_code_loc_str(&content), (0, 0));
    }

    #[test]
    fn only_real_code_lines_counted() {
        let content = dedent(
            "
            fn foo() {
                let x = 1;
                let y = 2;
            }
        ",
        );
        // 4 non-blank lines: fn, let x, let y, }
        assert_eq!(count_code_loc_str(&content), (4, 0));
    }

    #[test]
    fn mixed_code_blanks_comments() {
        let content = dedent(
            "
            // top comment

            fn bar() {
                // inner comment
                let z = 3; // trailing comment — still code
            }

        ",
        );
        // Code lines: `fn bar() {`, `let z = 3;`, `}`  → 3
        assert_eq!(count_code_loc_str(&content), (3, 0));
    }

    #[test]
    fn block_comment_spanning_multiple_lines_not_counted() {
        let content = dedent(
            "
            /*
             * This is a block comment.
             * It spans several lines.
             */
            fn baz() {}
        ",
        );
        // Only `fn baz() {}` is code.
        assert_eq!(count_code_loc_str(&content), (1, 0));
    }

    #[test]
    fn block_comment_open_line_with_code_before_counts_once() {
        let content = dedent(
            "
            let a = 1; /* start of block
             * still in block
             */
            let b = 2;
        ",
        );
        // `let a = 1;` (code before /*) + `let b = 2;` → 2
        assert_eq!(count_code_loc_str(&content), (2, 0));
    }

    #[test]
    fn pure_inline_block_comment_not_counted() {
        let content = "/* just a comment */\n";
        assert_eq!(count_code_loc_str(content), (0, 0));
    }

    #[test]
    fn inline_block_comment_mixed_with_code_counts_once() {
        let content = "let x = /* comment */ 42;\n";
        assert_eq!(count_code_loc_str(content), (1, 0));
    }

    #[test]
    fn code_after_closing_block_comment_counted() {
        let content = dedent(
            "
            /*
             * block
             */ let x = 1;
        ",
        );
        // The line with */ and trailing code should count as 1.
        assert_eq!(count_code_loc_str(&content), (1, 0));
    }

    #[test]
    fn nested_block_comment_delimiter_does_not_reopen() {
        let content = dedent(
            "
            /* outer /* still outer
             */
            real_code();
        ",
        );
        assert_eq!(count_code_loc_str(&content), (1, 0));
    }

    #[test]
    fn multiple_block_comments_in_one_file() {
        let content = dedent(
            "
            /* comment one */
            let a = 1;
            /* comment two */
            let b = 2;
        ",
        );
        // Two pure inline block comments (not counted) + two code lines → 2
        assert_eq!(count_code_loc_str(&content), (2, 0));
    }

    #[test]
    fn cfg_test_mod_excluded_from_prod_loc() {
        let content = dedent(
            "
            fn foo() {
                let x = 1;
            }
            #[cfg(test)]
            mod tests {
                use super::*;
                #[test]
                fn test_foo() {
                    assert_eq!(1, 1);
                }
            }
        ",
        );
        let (prod, _test) = count_code_loc_str(&content);
        // prod code: `fn foo() {`, `let x = 1;`, `}` = 3 lines
        assert_eq!(prod, 3);
    }

    #[test]
    fn cfg_test_mod_counted_in_test_loc() {
        let content = dedent(
            "
            fn foo() {}
            #[cfg(test)]
            mod tests {
                #[test]
                fn bar() {}
            }
        ",
        );
        let (_prod, test) = count_code_loc_str(&content);
        // test lines: `#[cfg(test)]`, `mod tests {`, `#[test]`, `fn bar() {}`, `}` = 5
        assert_eq!(test, 5);
    }

    #[test]
    fn prod_code_counted_normally_when_no_test_mod() {
        let content = dedent(
            "
            fn foo() {
                let x = 1;
            }
        ",
        );
        let (prod, test) = count_code_loc_str(&content);
        assert_eq!(prod, 3);
        assert_eq!(test, 0);
    }

    // ── is_test_file ─────────────────────────────────────────────────────────

    #[test]
    fn is_test_file_tests_dir_in_path() {
        assert!(is_test_file("crates/foo/tests/integration.rs"));
    }

    #[test]
    fn is_test_file_ends_with_test_rs_suffix() {
        assert!(is_test_file("crates/foo/src/server_test.rs"));
    }

    #[test]
    fn is_test_file_basename_starts_with_test_prefix() {
        assert!(is_test_file("crates/foo/src/test_helpers.rs"));
    }

    #[test]
    fn is_test_file_bare_name_starts_with_test() {
        assert!(is_test_file("test_utils.rs"));
    }

    #[test]
    fn is_test_file_normal_src_file_is_false() {
        assert!(!is_test_file("crates/foo/src/lib.rs"));
    }

    #[test]
    fn is_test_file_normal_bare_name_is_false() {
        assert!(!is_test_file("utils.rs"));
    }

    #[test]
    fn is_test_file_nested_tests_dir_in_path() {
        assert!(is_test_file("crates/bar/tests/mod.rs"));
    }
}
