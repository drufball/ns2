//! check-fanout — fan-in / fan-out / git-churn risk analysis for Rust files.
//!
//! Usage:
//!   check-fanout [--root <path>] [--top <n>] [--threshold <score>]
//!
//! Fan-out counts:
//!   1. `use <crate>::...` import statements
//!   2. Fully-qualified path references: `<crate>::Something` not preceded by `:` or word char
//!
//! Fan-in counts references across:
//!   1. `use <crate_name>::` or `use <crate_name>::<module>::` in other files
//!   2. `<crate_name>::` fully-qualified usage in other files
//!   3. `mod <module_name>;` declarations in other files
//!   4. Cargo.toml [dependencies] entries (crate-level only)
//!
//! Score = (fanin + fanout) * churn. Churn = commits in last 90 days. Default threshold 300.

use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

// ── Constants ─────────────────────────────────────────────────────────────────

const KEYWORD_EXCLUSIONS: &[&str] = &[
    // Rust keywords
    "crate", "super", "self", "use", "pub", "mod", "fn", "let", "mut", "impl",
    "trait", "struct", "enum", "type", "where", "for", "if", "else", "match",
    "return", "async", "await", "move", "ref", "in", "loop", "while", "break",
    "continue", "extern", "unsafe", "dyn", "box", "as",
    // Standard library crate name and its sub-modules that appear as path prefix segments
    "std",
    "collections", "convert", "path", "sync", "io", "fmt", "str", "ops", "mem",
    "net", "time", "error", "iter", "ffi", "env", "os", "fs", "process",
    "thread", "any", "cmp", "borrow", "marker", "clone", "hash", "num", "pin",
    "task", "future",
    // axum/tower sub-module segments
    "extract", "response", "routing", "http", "sse", "body", "stream", "wrappers",
    "sqlite", "json", "middleware",
    // tokio sub-modules
    "mpsc", "broadcast", "watch", "oneshot", "signal",
    // turbofish method names commonly used with ::<Type>
    "collect", "parse", "into", "from", "default", "new", "unwrap", "expect",
    "ok", "err", "map", "filter", "fold", "iter", "clone", "to_string", "len",
    "push", "get",
    // numeric types that can appear in turbofish
    "usize", "isize", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32",
    "i64", "i128", "f32", "f64",
];

// ── Workspace helpers ─────────────────────────────────────────────────────────

fn get_workspace_crates(root: &Path) -> HashSet<String> {
    let cargo_toml = root.join("Cargo.toml");
    let Ok(text) = fs::read_to_string(&cargo_toml) else { return HashSet::new() };
    let mut names = HashSet::new();
    let mut in_members = false;
    let member_re = Regex::new(r#""crates/([^"]+)""#).unwrap();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("members") {
            in_members = true;
        }
        if in_members {
            if let Some(cap) = member_re.captures(line) {
                let raw = cap[1].to_string();
                names.insert(raw.clone());
                names.insert(raw.replace('-', "_"));
            }
            if line.ends_with(']') {
                in_members = false;
            }
        }
    }
    names
}

fn get_crate_cargo_deps(crate_root: &Path) -> HashSet<String> {
    let cargo_toml = crate_root.join("Cargo.toml");
    let Ok(text) = fs::read_to_string(&cargo_toml) else { return HashSet::new() };
    let mut deps = HashSet::new();
    let mut in_deps = false;
    let deps_header_re = Regex::new(r"^\[(dependencies|dev-dependencies|build-dependencies)\]").unwrap();
    let dep_entry_re = Regex::new(r"^([A-Za-z_][A-Za-z0-9_-]*)\s*=").unwrap();
    for line in text.lines() {
        let stripped = line.trim();
        if deps_header_re.is_match(stripped) {
            in_deps = true;
            continue;
        }
        if stripped.starts_with('[') && in_deps {
            in_deps = false;
        }
        if in_deps {
            if let Some(cap) = dep_entry_re.captures(stripped) {
                let name = cap[1].replace('-', "_");
                deps.insert(name);
            }
        }
    }
    deps
}

// ── Fan-out ───────────────────────────────────────────────────────────────────

/// Pure-logic fanout computation on source text. Returns (count, set-of-crate-names).
///
/// This is separated from filesystem I/O so it can be tested without touching disk.
fn fanout_from_str(text: &str) -> (usize, HashSet<String>) {
    let mut crates_used: HashSet<String> = HashSet::new();
    let exclusions: HashSet<&str> = KEYWORD_EXCLUSIONS.iter().copied().collect();

    // Strategy 1: `use <ident>::` at any indentation
    let use_re = Regex::new(r"\buse\s+([A-Za-z_][A-Za-z0-9_]*)::").unwrap();
    for cap in use_re.captures_iter(text) {
        let name = &cap[1];
        if !matches!(name, "crate" | "super" | "self") && !exclusions.contains(name) {
            crates_used.insert(name.to_string());
        }
    }

    // Strategy 2: `<lowercase_or_snake_ident>::` NOT preceded by `:` or word char.
    // The regex crate doesn't support lookbehinds, so we use find_iter to get byte positions
    // and manually check the byte immediately before each match.
    let qualified_re = Regex::new(r"([a-z][a-z0-9_]*)::").unwrap();
    let text_bytes = text.as_bytes();
    for mat in qualified_re.find_iter(text) {
        let matched = mat.as_str();
        // Extract just the name part (everything before `::`)
        let name = &matched[..matched.len() - 2];
        if exclusions.contains(name) {
            continue;
        }
        // Check the byte immediately before the match
        let start = mat.start();
        if start > 0 {
            let prev = text_bytes[start - 1];
            // Skip if preceded by `:` or alphanumeric/underscore (another word char)
            if prev == b':' || prev.is_ascii_alphanumeric() || prev == b'_' {
                continue;
            }
        }
        crates_used.insert(name.to_string());
    }

    (crates_used.len(), crates_used)
}

fn compute_fanout(path: &Path) -> (usize, HashSet<String>) {
    let Ok(text) = fs::read_to_string(path) else { return (0, HashSet::new()) };
    fanout_from_str(&text)
}

// ── Module names ──────────────────────────────────────────────────────────────

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn module_names_for_file(rs_path: &Path, crate_root: &Path, crate_name: &str) -> Vec<String> {
    let src_dir = crate_root.join("src");
    let Ok(rel) = rs_path.strip_prefix(&src_dir) else { return vec![] };

    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();

    if parts == ["lib.rs"] || parts == ["main.rs"] {
        return vec![crate_name.to_string()];
    }

    let mut parts = parts;
    // Strip .rs extension from last element
    if let Some(last) = parts.last_mut() {
        if last.ends_with(".rs") {
            *last = last[..last.len() - 3].to_string();
        }
    }
    // mod.rs is referenced by its parent dir name
    if parts.last().map(std::string::String::as_str) == Some("mod") {
        parts.pop();
    }

    let module_path = std::iter::once(crate_name.to_string())
        .chain(parts.iter().cloned())
        .collect::<Vec<_>>()
        .join("::");
    let short_name = parts.last().cloned().unwrap_or_else(|| crate_name.to_string());

    vec![module_path, short_name]
}

// ── Fan-in ────────────────────────────────────────────────────────────────────

fn compute_fanin(
    target: &Path,
    all_files: &[PathBuf],
    module_names: &[String],
    crate_name: &str,
    is_crate_root: bool,
    workspace_root: &Path,
) -> usize {
    if module_names.is_empty() {
        return 0;
    }

    // Build patterns for each possible reference form
    // Note: regex crate doesn't support lookbehinds, so for `(?<![:\w])name::` we use
    // a workaround: match `(^|[^:\w])name::` with a capture to find the actual name.
    // We'll use a simpler approach: search for `name::` and check the preceding byte manually.
    let mut rs_patterns: Vec<Regex> = Vec::new();

    for name in module_names {
        // use server:: / use workspace::worktree::
        rs_patterns.push(
            Regex::new(&format!(r"\buse\s+{}(\s*::|;|\s*\{{)", regex::escape(name))).unwrap(),
        );
        // fully-qualified usage — we'll handle lookbehind manually below
        rs_patterns.push(
            Regex::new(&format!(r"{}::", regex::escape(name))).unwrap(),
        );
        // mod <leaf>;
        let leaf = name.split("::").last().unwrap_or(name);
        rs_patterns.push(
            Regex::new(&format!(r"\bmod\s+{}\s*;", regex::escape(leaf))).unwrap(),
        );
    }

    // Patterns that need lookbehind-manual-check (the `name::` qualified patterns)
    // indices 1, 4, 7, ... (every 3rd starting at 1)
    let qualified_indices: Vec<usize> = (1..rs_patterns.len()).step_by(3).collect();
    let qualified_names: Vec<String> = module_names.to_vec();

    let mut count = 0usize;
    let mut counted_paths: HashSet<String> = HashSet::new();

    'outer: for other in all_files {
        if other.as_path() == target {
            continue;
        }
        let Ok(text) = fs::read_to_string(other) else { continue };
        let text_bytes = text.as_bytes();

        for (pi, pat) in rs_patterns.iter().enumerate() {
            let is_qualified = qualified_indices.contains(&pi);

            if is_qualified {
                // Find the module name this pattern corresponds to
                // Pattern index 1 -> module_names[0], 4 -> module_names[1], etc.
                let mod_idx = (pi - 1) / 3;
                let mod_name = qualified_names.get(mod_idx).map_or("", std::string::String::as_str);

                let search_str = format!("{mod_name}::");
                // Manual scan for `mod_name::` not preceded by `:` or word char
                let mut pos = 0;
                while pos < text.len() {
                    if let Some(idx) = text[pos..].find(&search_str) {
                        let abs = pos + idx;
                        let prev_ok = if abs == 0 {
                            true
                        } else {
                            let prev = text_bytes[abs - 1];
                            prev != b':' && !prev.is_ascii_alphanumeric() && prev != b'_'
                        };
                        if prev_ok {
                            counted_paths.insert(other.to_string_lossy().to_string());
                            count += 1;
                            continue 'outer;
                        }
                        pos = abs + 1;
                    } else {
                        break;
                    }
                }
            } else if pat.is_match(&text) {
                counted_paths.insert(other.to_string_lossy().to_string());
                count += 1;
                continue 'outer;
            }
        }
    }

    // Strategy 4: Cargo.toml dependency scan for crate root files
    if is_crate_root {
        let crates_dir = workspace_root.join("crates");
        if let Ok(entries) = fs::read_dir(&crates_dir) {
            let crate_dir_name = crate_name.replace('_', "-");
            for entry in entries.flatten() {
                let other_crate_dir = entry.path();
                if !other_crate_dir.is_dir() {
                    continue;
                }
                if other_crate_dir.file_name().and_then(|n| n.to_str()) == Some(&crate_dir_name) {
                    continue; // skip self
                }
                let deps = get_crate_cargo_deps(&other_crate_dir);
                if deps.contains(crate_name) || deps.contains(&crate_name.replace('_', "-")) {
                    // Add 1 only if this crate's .rs files weren't already counted
                    let other_rs: Vec<PathBuf> = {
                        let src = other_crate_dir.join("src");
                        let mut v = Vec::new();
                        collect_recursive(&src, &mut v);
                        v
                    };
                    if !other_rs
                        .iter()
                        .any(|f| counted_paths.contains(&f.to_string_lossy().to_string()))
                    {
                        count += 1;
                    }
                }
            }
        }
    }

    count
}

// ── Git churn ─────────────────────────────────────────────────────────────────

fn git_churn(path: &Path, root: &Path) -> usize {
    let Ok(rel) = path.strip_prefix(root) else { return 0 };
    let output = Command::new("git")
        .args(["log", "--since=90 days ago", "--oneline", "--", &rel.to_string_lossy()])
        .current_dir(root)
        .output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.lines().filter(|l| !l.trim().is_empty()).count()
        }
        Err(_) => 0,
    }
}

// ── File collection helpers ───────────────────────────────────────────────────

fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
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

fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_recursive(dir, &mut files);
    files.sort();
    files
}

// ── Scoring ───────────────────────────────────────────────────────────────────

struct FileMetrics {
    path: String,
    lines: usize,
    fanout: usize,
    fanin: usize,
    churn: usize,
    score_v2: f64,
}

impl FileMetrics {
    #[allow(clippy::cast_precision_loss)]
    fn compute(
        path: String,
        lines: usize,
        fanout: usize,
        fanin: usize,
        churn: usize,
    ) -> Self {
        let score_v2 = ((fanin + fanout) as f64) * (churn as f64);
        Self { path, lines, fanout, fanin, churn, score_v2 }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn usage() -> ! {
    eprintln!("Usage: check-fanout [--root <path>] [--top <n>] [--threshold <score>]");
    process::exit(2);
}

#[allow(clippy::too_many_lines)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut root = cwd;
    let mut top: usize = 10;
    let mut threshold: f64 = 300.0;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = PathBuf::from(args.get(i).unwrap_or_else(|| usage()));
            }
            "--top" => {
                i += 1;
                top = args.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                    eprintln!("--top requires a numeric argument");
                    usage()
                });
            }
            "--threshold" => {
                i += 1;
                threshold = args.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                    eprintln!("--threshold requires a numeric argument");
                    usage()
                });
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("Unknown option: {other}");
                usage();
            }
        }
        i += 1;
    }

    // Canonicalize root
    let root = root.canonicalize().unwrap_or(root);

    if !root.join("Cargo.toml").exists() {
        eprintln!("ERROR: {} does not look like a Rust workspace (no Cargo.toml)", root.display());
        process::exit(1);
    }

    let workspace_crates = get_workspace_crates(&root);
    println!("Workspace crates detected: {:?}", {
        let mut v: Vec<_> = workspace_crates.iter().collect();
        v.sort();
        v
    });

    let crates_dir = root.join("crates");
    let all_rs = collect_rs_files(&crates_dir);
    println!("Found {} Rust source files\n", all_rs.len());

    // Build metadata maps
    let mut module_map: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut is_crate_root_map: HashMap<PathBuf, bool> = HashMap::new();
    let mut crate_name_map: HashMap<PathBuf, String> = HashMap::new();

    for f in &all_rs {
        // parts: ["crates", "<crate-dir-name>", ...]
        let rel = f.strip_prefix(&root).unwrap_or(f.as_path());
        let parts: Vec<_> = rel.components().collect();
        let crate_dir_name = if parts.len() > 1 {
            parts[1].as_os_str().to_string_lossy().to_string()
        } else {
            "unknown".to_string()
        };
        let crate_name = crate_dir_name.replace('-', "_");
        let crate_root = root.join("crates").join(&crate_dir_name);
        let src_dir = crate_root.join("src");

        let mod_names = module_names_for_file(f, &crate_root, &crate_name);

        let rel_from_src: Vec<String> = f
            .strip_prefix(&src_dir)
            .map(|r| r.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect())
            .unwrap_or_default();
        let is_root = rel_from_src == ["lib.rs"] || rel_from_src == ["main.rs"];

        module_map.insert(f.clone(), mod_names);
        is_crate_root_map.insert(f.clone(), is_root);
        crate_name_map.insert(f.clone(), crate_name);
    }

    // Compute metrics
    let mut results: Vec<FileMetrics> = Vec::new();

    for f in &all_rs {
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(f.as_path())
            .to_string_lossy()
            .to_string();
        let lines = fs::read_to_string(f)
            .map_or(0, |t| t.lines().count());
        let (fanout, _) = compute_fanout(f);
        let fanin = compute_fanin(
            f,
            &all_rs,
            module_map.get(f).map_or(&[] as &[String], std::vec::Vec::as_slice),
            crate_name_map.get(f).map_or("", std::string::String::as_str),
            *is_crate_root_map.get(f).unwrap_or(&false),
            &root,
        );
        let churn = git_churn(f, &root);
        results.push(FileMetrics::compute(rel, lines, fanout, fanin, churn));
    }

    results.sort_by(|a, b| b.score_v2.partial_cmp(&a.score_v2).unwrap_or(std::cmp::Ordering::Equal));

    // Full table
    let sep = "=".repeat(110);
    println!("{sep}");
    println!(
        "{:<55} {:>6} {:>5} {:>5} {:>7} {:>13}",
        "File", "Lines", "FOut", "FIn", "Churn90", "V2(fi+fo)*ch"
    );
    println!("{sep}");

    let mut violations: Vec<&FileMetrics> = Vec::new();

    for r in &results {
        let flag = if r.score_v2 >= threshold { " <-- RISK" } else { "" };
        println!(
            "{:<55} {:>6} {:>5} {:>5} {:>7} {:>13.1}{}",
            r.path, r.lines, r.fanout, r.fanin, r.churn, r.score_v2, flag
        );
        if r.score_v2 >= threshold {
            violations.push(r);
        }
    }

    // Top-N
    println!("\n{}", "=".repeat(60));
    println!("TOP {top} by V2 score");
    println!("{}", "=".repeat(60));
    for (i, r) in results.iter().take(top).enumerate() {
        println!("{:>2}. [{:>7.1}] {}", i + 1, r.score_v2, r.path);
    }

    println!();
    if violations.is_empty() {
        println!("Fan-out check passed (threshold: {threshold}).");
        process::exit(0);
    } else {
        println!(
            "Fan-out check FAILED — {} file(s) exceed threshold {}:",
            violations.len(),
            threshold
        );
        for v in &violations {
            println!("  [{:.1}] {}", v.score_v2, v.path);
        }
        process::exit(1);
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── fanout_from_str: basic counting ───────────────────────────────────────

    #[test]
    fn fanout_counts_use_imports() {
        let src = r"
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
";
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 3, "expected tokio, serde, uuid");
        assert!(crates.contains("tokio"));
        assert!(crates.contains("serde"));
        assert!(crates.contains("uuid"));
    }

    #[test]
    fn fanout_does_not_count_std() {
        let src = r#"
use std::collections::HashMap;
use std::path::PathBuf;
let x = std::env::var("HOME");
"#;
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 0, "std should be excluded, got: {crates:?}");
    }

    #[test]
    fn fanout_does_not_count_crate_super_self() {
        let src = r"
use crate::model::Foo;
use super::helper;
use self::inner::Bar;
";
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 0, "self-references must not count, got: {crates:?}");
    }

    #[test]
    fn fanout_counts_tokio_once_not_sub_modules() {
        // `tokio` is the crate; `sync` is a sub-module and is in KEYWORD_EXCLUSIONS.
        let src = "use tokio::sync::Mutex;";
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 1, "only tokio should count, got: {crates:?}");
        assert!(crates.contains("tokio"));
        assert!(!crates.contains("sync"));
    }

    #[test]
    fn fanout_excludes_std_submodules_via_exclusion_list() {
        // `collections` appears as a path segment but is excluded
        let src = r"
let mut map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
";
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 0, "std and collections should both be excluded, got: {crates:?}");
    }

    #[test]
    fn fanout_counts_qualified_external_crate_usage() {
        // `regex::Regex` — `regex` is an external crate
        let src = r#"let re = regex::Regex::new(r"\d+").unwrap();"#;
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 1, "regex should count, got: {crates:?}");
        assert!(crates.contains("regex"));
    }

    #[test]
    fn fanout_skips_qualified_path_preceded_by_colon() {
        // In `foo::bar::baz`, once `foo` is matched `bar` and `baz` are preceded by `:`
        // and must not be double-counted.
        let src = "use serde_json::value::Value;";
        let (_, crates) = fanout_from_str(src);
        // serde_json is the crate; `value` is excluded by KEYWORD_EXCLUSIONS
        assert!(crates.contains("serde_json"), "serde_json should count, got: {crates:?}");
        assert!(!crates.contains("value"), "sub-module value must not count: {crates:?}");
    }

    #[test]
    fn fanout_skips_qualified_path_preceded_by_alphanumeric() {
        // A method call like `foo.bar::` — `bar` is preceded by `.` which is fine, but
        // the turbofish pattern `vec.collect::<Vec<_>>()` — `collect` is in exclusions.
        // Ensure an ordinary identifier directly glued before the match is excluded.
        // e.g. `Afoo::bar` — the `f` in `foo` is alphanumeric so it will be caught by
        // the lookbehind check and `foo` won't be added.
        let src = "Afoo::bar();"; // `foo` is preceded by `A` (alphanumeric)
        let (_, crates) = fanout_from_str(src);
        assert!(!crates.contains("foo"), "should be skipped due to preceding alphanumeric: {crates:?}");
    }

    // ── keyword exclusion list completeness ───────────────────────────────────

    #[test]
    fn std_is_in_keyword_exclusions() {
        assert!(
            KEYWORD_EXCLUSIONS.contains(&"std"),
            "std must be in KEYWORD_EXCLUSIONS"
        );
    }

    #[test]
    fn rust_keywords_are_in_exclusion_list() {
        for kw in &["crate", "super", "self", "mod", "use", "pub", "fn", "impl"] {
            assert!(
                KEYWORD_EXCLUSIONS.contains(kw),
                "Rust keyword '{kw}' must be in KEYWORD_EXCLUSIONS"
            );
        }
    }

    // ── edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn fanout_empty_source_returns_zero() {
        let (count, crates) = fanout_from_str("");
        assert_eq!(count, 0);
        assert!(crates.is_empty());
    }

    #[test]
    fn fanout_multiple_uses_of_same_crate_count_once() {
        let src = r"
use tokio::runtime::Runtime;
use tokio::task;
let _rt = tokio::runtime::Runtime::new();
";
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 1, "tokio should count exactly once, got: {crates:?}");
        assert!(crates.contains("tokio"));
    }

    #[test]
    fn fanout_does_not_count_numeric_turbofish() {
        // `u64` in turbofish context
        let src = r"let x = value.parse::<u64>().unwrap();";
        let (count, crates) = fanout_from_str(src);
        assert_eq!(count, 0, "numeric type turbofish must not count, got: {crates:?}");
    }

    // ── module_names_for_file ─────────────────────────────────────────────────

    #[test]
    fn module_names_lib_rs_returns_crate_name() {
        use std::path::PathBuf;
        let crate_root = PathBuf::from("/repo/crates/mylib");
        let rs_path = crate_root.join("src/lib.rs");
        let names = module_names_for_file(&rs_path, &crate_root, "mylib");
        assert_eq!(names, vec!["mylib"]);
    }

    #[test]
    fn module_names_main_rs_returns_crate_name() {
        use std::path::PathBuf;
        let crate_root = PathBuf::from("/repo/crates/mycli");
        let rs_path = crate_root.join("src/main.rs");
        let names = module_names_for_file(&rs_path, &crate_root, "mycli");
        assert_eq!(names, vec!["mycli"]);
    }

    #[test]
    fn module_names_submodule_returns_two_names() {
        use std::path::PathBuf;
        let crate_root = PathBuf::from("/repo/crates/mylib");
        let rs_path = crate_root.join("src/helpers.rs");
        let names = module_names_for_file(&rs_path, &crate_root, "mylib");
        assert_eq!(names, vec!["mylib::helpers", "helpers"]);
    }

    #[test]
    fn module_names_mod_rs_uses_parent_dir_name() {
        use std::path::PathBuf;
        let crate_root = PathBuf::from("/repo/crates/mylib");
        let rs_path = crate_root.join("src/commands/mod.rs");
        let names = module_names_for_file(&rs_path, &crate_root, "mylib");
        assert_eq!(names, vec!["mylib::commands", "commands"]);
    }

    #[test]
    fn module_names_outside_src_dir_returns_empty() {
        use std::path::PathBuf;
        let crate_root = PathBuf::from("/repo/crates/mylib");
        let rs_path = PathBuf::from("/repo/crates/other/src/lib.rs");
        let names = module_names_for_file(&rs_path, &crate_root, "mylib");
        assert!(names.is_empty());
    }
}
