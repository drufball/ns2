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

fn count_code_loc_str(content: &str) -> usize {
    let mut in_block = false;
    let mut code_lines = 0usize;

    for raw in content.lines() {
        let stripped = raw.trim();

        if in_block {
            if let Some(end) = stripped.find("*/") {
                in_block = false;
                // Check whether there is non-whitespace code after the closing */
                let after_close = stripped[end + 2..].trim();
                if !after_close.is_empty() && !after_close.starts_with("//") {
                    code_lines += 1;
                }
            }
            continue;
        }

        if stripped.is_empty() {
            continue; // blank line
        }

        if stripped.starts_with("//") {
            continue; // full-line comment
        }

        if let Some(block_start) = stripped.find("/*") {
            let before = stripped[..block_start].trim();
            let after = &stripped[block_start + 2..];
            if after.contains("*/") {
                // Inline block comment (/* ... */ on one line).
                // The line counts as code only if there is real content
                // outside the comment (before or after it).
                if !before.is_empty() {
                    code_lines += 1;
                } else {
                    // There might be content after the closing */
                    let close = after.find("*/").unwrap();
                    let after_close = after[close + 2..].trim();
                    if !after_close.is_empty() && !after_close.starts_with("//") {
                        code_lines += 1;
                    }
                    // else: line is purely a block comment — do not count
                }
            } else {
                in_block = true;
                // The part before /* on this line counts as code if non-empty
                if !before.is_empty() {
                    code_lines += 1;
                }
            }
            continue;
        }

        code_lines += 1;
    }

    code_lines
}

fn count_code_loc(path: &Path) -> usize {
    match fs::read_to_string(path) {
        Ok(content) => count_code_loc_str(&content),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::count_code_loc_str;

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
            .map(|l| if l.len() >= min_indent { &l[min_indent..] } else { l })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn only_blank_lines_returns_zero() {
        let content = "\n   \n\t\n\n";
        assert_eq!(count_code_loc_str(content), 0);
    }

    #[test]
    fn only_line_comments_returns_zero() {
        let content = dedent("
            // This is a comment
            // So is this
            //! doc comment
        ");
        assert_eq!(count_code_loc_str(&content), 0);
    }

    #[test]
    fn only_real_code_lines_counted() {
        let content = dedent("
            fn foo() {
                let x = 1;
                let y = 2;
            }
        ");
        // 4 non-blank lines: fn, let x, let y, }
        assert_eq!(count_code_loc_str(&content), 4);
    }

    #[test]
    fn mixed_code_blanks_comments() {
        let content = dedent("
            // top comment

            fn bar() {
                // inner comment
                let z = 3; // trailing comment — still code
            }

        ");
        // Code lines: `fn bar() {`, `let z = 3;`, `}`  → 3
        assert_eq!(count_code_loc_str(&content), 3);
    }

    #[test]
    fn block_comment_spanning_multiple_lines_not_counted() {
        let content = dedent("
            /*
             * This is a block comment.
             * It spans several lines.
             */
            fn baz() {}
        ");
        // Only `fn baz() {}` is code.
        assert_eq!(count_code_loc_str(&content), 1);
    }

    #[test]
    fn block_comment_open_line_with_code_before_counts_once() {
        let content = dedent("
            let a = 1; /* start of block
             * still in block
             */
            let b = 2;
        ");
        // `let a = 1;` (code before /*) + `let b = 2;` → 2
        assert_eq!(count_code_loc_str(&content), 2);
    }

    #[test]
    fn pure_inline_block_comment_not_counted() {
        // A line that is *only* a /* ... */ comment should not count.
        let content = "/* just a comment */\n";
        assert_eq!(count_code_loc_str(content), 0);
    }

    #[test]
    fn inline_block_comment_mixed_with_code_counts_once() {
        // Code before the inline block comment — line counts as 1.
        let content = "let x = /* comment */ 42;\n";
        assert_eq!(count_code_loc_str(content), 1);
    }

    #[test]
    fn code_after_closing_block_comment_counted() {
        // The closing */ is followed by real code on the same line.
        let content = dedent("
            /*
             * block
             */ let x = 1;
        ");
        // The line with */ and trailing code should count as 1.
        assert_eq!(count_code_loc_str(&content), 1);
    }

    #[test]
    fn nested_block_comment_delimiter_does_not_reopen() {
        // Rust doesn't support nested /* */ but we should handle /* inside block gracefully.
        let content = dedent("
            /* outer /* still outer
             */
            real_code();
        ");
        assert_eq!(count_code_loc_str(&content), 1);
    }

    #[test]
    fn multiple_block_comments_in_one_file() {
        let content = dedent("
            /* comment one */
            let a = 1;
            /* comment two */
            let b = 2;
        ");
        // Two pure inline block comments (not counted) + two code lines → 2
        assert_eq!(count_code_loc_str(&content), 2);
    }
}

fn is_test_file(rel: &str) -> bool {
    rel.contains("/tests/")
        || rel.ends_with("_test.rs")
        || {
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
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
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
                test_threshold = Some(args.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| {
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
                eprintln!("Unknown option: {}", other);
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

        let (limit, kind) = if is_test_file(&rel) {
            (test_threshold, "test")
        } else {
            (threshold, "src")
        };

        let loc = count_code_loc(file);
        if loc > limit {
            violations.push(format!("  {} / {}  [{}]  {}", loc, limit, kind, rel));
        }
    }

    if !violations.is_empty() {
        println!(
            "LOC check FAILED — {} file(s) exceed the limit (threshold: src={}, test={}):",
            violations.len(),
            threshold,
            test_threshold
        );
        for v in &violations {
            println!("{}", v);
        }
        process::exit(1);
    } else {
        println!(
            "LOC check passed (threshold: src={}, test={}).",
            threshold, test_threshold
        );
    }
}
