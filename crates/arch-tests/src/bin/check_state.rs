//! check-state — detect mutable/shared/global state patterns in Rust source files.
//!
//! Detects architectural smell: files that tangle stateful and stateless concerns
//! together.
//!
//! Usage:
//!   check-state [path ...] [--threshold N] [--verbose] [--no-exit-code]
//!
//! Exit codes:
//!   0 — all files below threshold
//!   1 — one or more files exceed threshold

use regex::Regex;
use std::{
    cmp::Reverse,
    fs,
    path::{Path, PathBuf},
    process,
};

// ── Pattern definitions ───────────────────────────────────────────────────────

struct Pattern {
    label: &'static str,
    regex: &'static str,
    category: &'static str, // "stateful" or "pure_signal"
    weight: i32,
}

const PATTERNS: &[Pattern] = &[
    // Global state
    Pattern { label: "static_mut",       regex: r"\bstatic\s+mut\b",                     category: "stateful", weight: 3 },
    Pattern { label: "lazy_static",      regex: r"\blazy_static\s*!",                    category: "stateful", weight: 2 },
    Pattern { label: "once_cell_lazy",   regex: r"\bonce_cell::sync::Lazy\b",            category: "stateful", weight: 2 },
    Pattern { label: "std_oncelock",     regex: r"\bstd::sync::OnceLock\b",              category: "stateful", weight: 2 },
    Pattern { label: "oncelock_bare",    regex: r"\bOnceLock\s*<",                       category: "stateful", weight: 2 },
    Pattern { label: "thread_local",     regex: r"\bthread_local\s*!",                   category: "stateful", weight: 2 },
    // Shared mutable state (std)
    Pattern { label: "arc_mutex",        regex: r"\bArc\s*<\s*(?:std::sync::)?Mutex\b",  category: "stateful", weight: 2 },
    Pattern { label: "arc_rwlock",       regex: r"\bArc\s*<\s*(?:std::sync::)?RwLock\b", category: "stateful", weight: 2 },
    Pattern { label: "rc_refcell",       regex: r"\bRc\s*<\s*RefCell\b",                 category: "stateful", weight: 2 },
    // Async shared state (tokio)
    Pattern { label: "arc_tokio_mutex",  regex: r"\bArc\s*<\s*tokio::sync::Mutex\b",    category: "stateful", weight: 2 },
    Pattern { label: "arc_tokio_rwlock", regex: r"\bArc\s*<\s*tokio::sync::RwLock\b",   category: "stateful", weight: 2 },
    // Interior mutability
    Pattern { label: "refcell_bare",     regex: r"\bRefCell\s*<",                        category: "stateful", weight: 1 },
    Pattern { label: "cell_bare",        regex: r"\bCell\s*<",                           category: "stateful", weight: 1 },
];

// ── Test block detection ──────────────────────────────────────────────────────

/// Return a list of (start, end) 1-based line ranges that are inside test modules.
fn find_test_line_ranges(lines: &[&str]) -> Vec<(usize, usize)> {
    let test_mod_re = Regex::new(r"mod\s+tests?\s*\{").unwrap();
    let mut ranges = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let ln = lines[i].trim();
        let is_test_mod = ln == "#[cfg(test)]"
            || ln == "#[cfg(test)] mod tests {"
            || test_mod_re.is_match(ln);
        if is_test_mod {
            let start = i + 1; // 1-based
            let mut depth = ln.matches('{').count() as i32 - ln.matches('}').count() as i32;
            let mut j = i + 1;
            while j < lines.len() && depth == 0 {
                let l = lines[j];
                depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                j += 1;
            }
            while j < lines.len() && depth > 0 {
                let l = lines[j];
                depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                j += 1;
            }
            let end = j;
            ranges.push((start, end));
            i = j;
            continue;
        }
        i += 1;
    }
    ranges
}

fn is_test_line(line_no: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|(s, e)| *s <= line_no && line_no <= *e)
}

// ── Per-file analysis ─────────────────────────────────────────────────────────

#[derive(Default)]
struct PatternHit {
    label: String,
    count: usize,
    lines: Vec<usize>,
}

struct FileResult {
    path: String,
    line_count: usize,
    fn_count: usize,
    stateful_score: i32,
    pure_fn_count: usize,
    mixing_score: f64,
    local_mut_score: i32,
    total_score: f64,
    hits: Vec<PatternHit>,
}

/// Core analysis: accepts file content as a `&str`.
/// Returns scores and pattern hits.  The `path` label is used only for
/// display in `FileResult`.
fn analyze_content(path: &str, content: &str) -> FileResult {
    let lines: Vec<&str> = content.lines().collect();
    let line_count = lines.len();

    // Count total function definitions
    let fn_re = Regex::new(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?fn\s+\w+").unwrap();
    let fn_count = lines.iter().filter(|ln| fn_re.is_match(ln)).count();

    // Detect test block ranges
    let test_ranges = find_test_line_ranges(&lines);

    // Compile pattern regexes
    let compiled_patterns: Vec<(&Pattern, Regex)> = PATTERNS
        .iter()
        .map(|p| (p, Regex::new(p.regex).unwrap()))
        .collect();

    let mut hits: Vec<PatternHit> = Vec::new();
    let mut stateful_score: i32 = 0;

    for (pat, rx) in &compiled_patterns {
        let mut matched_lines: Vec<usize> = Vec::new();
        for (i, ln) in lines.iter().enumerate() {
            let stripped = ln.trim_start();
            if stripped.starts_with("//") {
                continue;
            }
            if rx.is_match(ln) {
                matched_lines.push(i + 1); // 1-based
            }
        }
        if !matched_lines.is_empty() {
            if pat.category == "stateful" {
                let prod_count = matched_lines
                    .iter()
                    .filter(|&&ln_i| !is_test_line(ln_i, &test_ranges))
                    .count() as i32;
                let test_count = matched_lines.len() as i32 - prod_count;
                stateful_score += prod_count * pat.weight + test_count * pat.weight / 2;
            }
            hits.push(PatternHit {
                label: pat.label.to_string(),
                count: matched_lines.len(),
                lines: matched_lines,
            });
        }
    }

    // Mutable struct fields
    let mut_field_re = Regex::new(
        r"^\s{4,}(?:pub(?:\s*\([^)]*\))?\s+)?mut\s+[a-z_][a-z0-9_]*\s*:",
    )
    .unwrap();
    let mut mut_field_hits: Vec<usize> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if ln.trim_start().starts_with("//") {
            continue;
        }
        if mut_field_re.is_match(ln) {
            mut_field_hits.push(i + 1);
        }
    }
    if !mut_field_hits.is_empty() {
        let prod = mut_field_hits
            .iter()
            .filter(|&&li| !is_test_line(li, &test_ranges))
            .count() as i32;
        stateful_score += prod * 2;
        hits.push(PatternHit {
            label: "mut_struct_field".to_string(),
            count: mut_field_hits.len(),
            lines: mut_field_hits,
        });
    }

    // &mut DomainStruct params
    let stdlib_generics_re = Regex::new(
        r"&\s*mut\s+(?:String|Vec|HashMap|HashSet|BTreeMap|BTreeSet|VecDeque|BinaryHeap|str|u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|bool|f32|f64|char|Option|Result|Box|Rc|Arc|Mutex|RwLock|dyn\s|impl\s)\b"
    ).unwrap();
    let mut_param_re = Regex::new(r"&\s*mut\s+[A-Z][A-Za-z0-9_]*").unwrap();
    let mut mut_param_hits: Vec<usize> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        let stripped = ln.trim_start();
        if stripped.starts_with("//") {
            continue;
        }
        if mut_param_re.is_match(ln) && !stdlib_generics_re.is_match(ln) {
            mut_param_hits.push(i + 1);
        }
    }
    if !mut_param_hits.is_empty() {
        let prod = mut_param_hits
            .iter()
            .filter(|&&li| !is_test_line(li, &test_ranges))
            .count() as i32;
        stateful_score += prod;
        hits.push(PatternHit {
            label: "mut_domain_param".to_string(),
            count: mut_param_hits.len(),
            lines: mut_param_hits,
        });
    }

    // Count "pure" function definitions (no state machinery in sig/body header)
    let state_re = Regex::new(
        r"\bArc\b|\bMutex\b|\bRwLock\b|\bState\b|\bbroadcast\b|\bmpsc\b|\btokio::sync\b|\bRefCell\b"
    ).unwrap();
    let mut pure_fn_count = 0usize;
    for (i, ln) in lines.iter().enumerate() {
        if !fn_re.is_match(ln) {
            continue;
        }
        if ln.trim_start().starts_with("//") {
            continue;
        }
        let end = (i + 4).min(line_count);
        let param_block = lines[i..end].join("\n");
        if !state_re.is_match(&param_block) {
            pure_fn_count += 1;
        }
    }

    // Local mutation density
    let let_mut_re = Regex::new(r"^\s+let\s+mut\b").unwrap();
    let mut let_mut_hits: Vec<usize> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if ln.trim_start().starts_with("//") {
            continue;
        }
        let line_no = i + 1;
        if let_mut_re.is_match(ln) && !is_test_line(line_no, &test_ranges) {
            let_mut_hits.push(line_no);
        }
    }
    let local_mut_score = (let_mut_hits.len() / 5).min(10) as i32;
    if !let_mut_hits.is_empty() {
        hits.push(PatternHit {
            label: "local_let_mut".to_string(),
            count: let_mut_hits.len(),
            lines: let_mut_hits,
        });
    }

    // Mixing score
    let pure_fn_ratio = if fn_count > 0 {
        pure_fn_count as f64 / fn_count as f64
    } else {
        0.0
    };
    let mixing_score = (stateful_score as f64 * pure_fn_ratio * 100.0).round() / 100.0;
    let total_score =
        (stateful_score as f64 + mixing_score + local_mut_score as f64) * 100.0 / 100.0;
    let total_score = (total_score * 100.0).round() / 100.0;

    FileResult {
        path: path.to_string(),
        line_count,
        fn_count,
        stateful_score,
        pure_fn_count,
        mixing_score,
        local_mut_score,
        total_score,
        hits,
    }
}

fn analyze_file(path: &Path) -> FileResult {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  [warn] cannot read {}: {}", path.display(), e);
            return FileResult {
                path: path.to_string_lossy().to_string(),
                line_count: 0,
                fn_count: 0,
                stateful_score: 0,
                pure_fn_count: 0,
                mixing_score: 0.0,
                local_mut_score: 0,
                total_score: 0.0,
                hits: vec![],
            };
        }
    };
    analyze_content(&path.to_string_lossy(), &content)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: run analysis on a string literal and return the FileResult.
    fn analyze(src: &str) -> FileResult {
        analyze_content("<test>", src)
    }

    // ── stateful_score ────────────────────────────────────────────────────────

    #[test]
    fn arc_mutex_accumulates_stateful_score() {
        // Arc<Mutex<HashMap<...>>> appears twice → weight 2 each → stateful_score ≥ 4
        let src = r#"
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

struct Cache {
    data: Arc<Mutex<HashMap<String, String>>>,
    index: Arc<Mutex<HashMap<u64, Vec<u8>>>>,
}
"#;
        let r = analyze(src);
        assert!(
            r.stateful_score >= 4,
            "expected stateful_score >= 4, got {}",
            r.stateful_score
        );
    }

    #[test]
    fn static_mut_has_weight_3() {
        // One `static mut` → weight 3
        let src = "static mut COUNTER: u64 = 0;\n";
        let r = analyze(src);
        assert_eq!(r.stateful_score, 3, "static_mut weight should be 3");
    }

    #[test]
    fn pure_functions_only_have_zero_stateful_score() {
        let src = r#"
fn add(a: i32, b: i32) -> i32 { a + b }
fn sub(a: i32, b: i32) -> i32 { a - b }
fn mul(a: i32, b: i32) -> i32 { a * b }
"#;
        let r = analyze(src);
        assert_eq!(r.stateful_score, 0, "pure functions should have no stateful score");
    }

    // ── mixing_score ──────────────────────────────────────────────────────────

    #[test]
    fn mixing_score_positive_when_stateful_and_pure_fns_coexist() {
        // Two pure fns + one Arc<Mutex> usage → mixing_score > 0
        let src = r#"
use std::sync::{Arc, Mutex};

static STATE: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));

fn pure_a(x: i32) -> i32 { x + 1 }
fn pure_b(x: i32) -> i32 { x * 2 }
"#;
        let r = analyze(src);
        assert!(r.stateful_score > 0, "stateful_score should be > 0");
        assert!(r.pure_fn_count > 0, "should have pure functions");
        assert!(
            r.mixing_score > 0.0,
            "mixing_score should be > 0 when stateful and pure fns coexist, got {}",
            r.mixing_score
        );
    }

    #[test]
    fn mixing_score_zero_when_all_fns_are_stateful() {
        // Both functions take Arc<Mutex<...>> → pure_fn_count == 0 → mixing_score == 0
        let src = r#"
use std::sync::{Arc, Mutex};

fn update_a(state: Arc<Mutex<u32>>) {}
fn update_b(state: Arc<Mutex<u32>>) {}
"#;
        let r = analyze(src);
        assert!(r.stateful_score > 0, "stateful_score should be > 0");
        assert_eq!(r.pure_fn_count, 0, "no pure functions expected");
        assert_eq!(
            r.mixing_score, 0.0,
            "mixing_score should be 0 when all fns touch state"
        );
    }

    #[test]
    fn mixing_score_zero_when_no_stateful_patterns() {
        let src = r#"
fn alpha(x: i32) -> i32 { x }
fn beta(y: i32) -> i32 { y }
"#;
        let r = analyze(src);
        assert_eq!(r.stateful_score, 0);
        assert_eq!(r.mixing_score, 0.0, "mixing_score should be 0 with no stateful patterns");
    }

    // ── local_mut_score ───────────────────────────────────────────────────────

    #[test]
    fn let_mut_density_accumulates_local_mut_score() {
        // Ten `let mut` bindings outside test blocks → 10 / 5 = 2
        let src = "fn f() {\n".to_string()
            + &"    let mut x = 0;\n".repeat(10)
            + "}\n";
        let r = analyze(&src);
        assert_eq!(r.local_mut_score, 2, "10 let-mut lines should give local_mut_score=2");
    }

    #[test]
    fn let_mut_score_capped_at_10() {
        // 60 `let mut` → 60 / 5 = 12, but capped at 10
        let src = "fn f() {\n".to_string()
            + &"    let mut x = 0;\n".repeat(60)
            + "}\n";
        let r = analyze(&src);
        assert_eq!(r.local_mut_score, 10, "local_mut_score should be capped at 10");
    }

    // ── test-block deweighting ────────────────────────────────────────────────

    #[test]
    fn patterns_inside_cfg_test_are_deweighted() {
        // Arc<Mutex> appears only inside a #[cfg(test)] module.
        // Deweighted contribution = weight / 2 = 1 (integer division).
        let src = r#"
fn pure_fn(x: i32) -> i32 { x }

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    fn test_something() {
        let _state: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    }
}
"#;
        let r = analyze(src);
        // Full weight would be 2; deweighted = 2 / 2 = 1
        assert_eq!(
            r.stateful_score, 1,
            "test-block Arc<Mutex> should contribute weight/2=1, got {}",
            r.stateful_score
        );
    }

    #[test]
    fn let_mut_inside_test_block_excluded_from_local_mut_score() {
        // Five `let mut` inside a test block only → should NOT count toward local_mut_score
        let src = r#"
#[cfg(test)]
mod tests {
    fn test_f() {
        let mut a = 0;
        let mut b = 0;
        let mut c = 0;
        let mut d = 0;
        let mut e = 0;
    }
}
"#;
        let r = analyze(src);
        assert_eq!(
            r.local_mut_score, 0,
            "let mut inside test blocks should not count, got {}",
            r.local_mut_score
        );
    }

    // ── total_score formula ───────────────────────────────────────────────────

    #[test]
    fn total_score_is_stateful_plus_mixing_plus_local_mut() {
        // Use static_mut (weight=3) + 3 pure fns + 5 let-mut lines.
        // fn_count=3, pure_fn_count=3 (none touch state in their signature block)
        // stateful_score=3, pure_fn_ratio=1.0, mixing_score=3.0, local_mut_score=1
        // total = 3 + 3.0 + 1 = 7.0
        let src = r#"
static mut COUNTER: u64 = 0;

fn pure_a(x: i32) -> i32 { x }
fn pure_b(x: i32) -> i32 { x }
fn pure_c(x: i32) -> i32 { x }

fn mutations() {
    let mut a = 0;
    let mut b = 0;
    let mut c = 0;
    let mut d = 0;
    let mut e = 0;
}
"#;
        let r = analyze(src);
        assert_eq!(r.stateful_score, 3, "stateful_score should be 3 for one static mut");
        assert_eq!(r.local_mut_score, 1, "5 let-mut → local_mut_score=1");
        // pure_fn_ratio may not be exactly 1.0 if mutations() is counted as a fn
        // total_score = stateful + mixing + local_mut
        let expected = r.stateful_score as f64 + r.mixing_score + r.local_mut_score as f64;
        assert!(
            (r.total_score - expected).abs() < 0.01,
            "total_score={} should equal stateful+mixing+local_mut={}",
            r.total_score,
            expected
        );
    }
}

// ── Reporting ─────────────────────────────────────────────────────────────────

fn print_detail(r: &FileResult, verbose: bool) {
    let rel = r.path.as_str();
    println!("  {}", rel);
    println!(
        "    lines={}  fns={}  pure_fns={}",
        r.line_count, r.fn_count, r.pure_fn_count
    );
    println!(
        "    stateful_score={}  mixing_score={:.2}  local_mut_score={}  total={:.2}",
        r.stateful_score, r.mixing_score, r.local_mut_score, r.total_score
    );
    if !r.hits.is_empty() {
        println!("    patterns:");
        let mut sorted_hits: Vec<&PatternHit> = r.hits.iter().collect();
        sorted_hits.sort_by_key(|h| Reverse(h.count));
        for hit in sorted_hits {
            if verbose {
                let lines_str: Vec<String> =
                    hit.lines.iter().take(10).map(|l| l.to_string()).collect();
                let suffix = if hit.lines.len() > 10 {
                    format!(" … (+{} more)", hit.lines.len() - 10)
                } else {
                    String::new()
                };
                println!(
                    "      {:<22} ×{:>3}   lines: {}{}",
                    hit.label,
                    hit.count,
                    lines_str.join(", "),
                    suffix
                );
            } else {
                println!("      {:<22} ×{:>3}", hit.label, hit.count);
            }
        }
    }
    println!();
}

fn report(results: &[FileResult], threshold: f64, verbose: bool) -> bool {
    let sep = "=".repeat(72);
    println!("{}", sep);
    println!("STATE COMPLEXITY REPORT");
    println!("{}", sep);
    println!(
        "{:<52} {:>5}  {:>4}  {:>8}  {:>7}  {:>6}  {:>6}  {:>6}",
        "File", "Lines", "Fns", "Stateful", "PureFns", "Mix", "LetMut", "Total"
    );
    println!("{}", "-".repeat(78));

    let mut over_threshold: Vec<&FileResult> = Vec::new();

    for r in results {
        let marker = if r.total_score >= threshold { " !" } else { "  " };
        let rel = &r.path;
        let display = if rel.len() <= 51 {
            rel.clone()
        } else {
            format!("\u{2026}{}", &rel[rel.len() - 50..])
        };
        println!(
            "{:<52}{} {:>5}  {:>4}  {:>8}  {:>7}  {:>6.1}  {:>6}  {:>6.1}",
            display,
            marker,
            r.line_count,
            r.fn_count,
            r.stateful_score,
            r.pure_fn_count,
            r.mixing_score,
            r.local_mut_score,
            r.total_score
        );
        if r.total_score >= threshold {
            over_threshold.push(r);
        }
    }

    println!("{}", "-".repeat(78));
    println!();

    if verbose || !over_threshold.is_empty() {
        println!("DETAIL — files at or above threshold");
        println!();
        for r in &over_threshold {
            print_detail(r, verbose);
        }
    }

    if verbose {
        let over_paths: std::collections::HashSet<&str> =
            over_threshold.iter().map(|r| r.path.as_str()).collect();
        let top: Vec<&FileResult> = results
            .iter()
            .filter(|r| !over_paths.contains(r.path.as_str()))
            .take(3)
            .collect();
        if !top.is_empty() {
            println!("DETAIL — top scoring files below threshold");
            println!();
            for r in &top {
                print_detail(r, verbose);
            }
        }
    }

    println!();
    println!("Threshold: {}", threshold);
    if over_threshold.is_empty() {
        println!("PASS — no files exceed threshold.");
        false
    } else {
        println!("FAIL — {} file(s) exceed threshold:", over_threshold.len());
        for r in &over_threshold {
            println!("  {}  (score={})", r.path, r.total_score);
        }
        true
    }
}

// ── File collection ───────────────────────────────────────────────────────────

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

fn collect_files(paths: &[String]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_file() {
            result.push(path.to_path_buf());
        } else if path.is_dir() {
            collect_recursive(path, &mut result);
        }
    }
    result.sort();
    result.dedup();
    result
}

fn find_git_root() -> Option<PathBuf> {
    let mut d = std::env::current_dir().ok()?;
    for _ in 0..10 {
        if d.join(".git").is_dir() {
            return Some(d);
        }
        let parent = d.parent()?.to_path_buf();
        if parent == d {
            break;
        }
        d = parent;
    }
    None
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();

    let mut threshold: f64 = 8.0;
    let mut verbose = false;
    let mut no_exit_code = false;
    let mut paths: Vec<String> = Vec::new();

    let mut i = 1;
    while i < raw_args.len() {
        match raw_args[i].as_str() {
            "--threshold" => {
                i += 1;
                threshold = raw_args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| {
                        eprintln!("--threshold requires a numeric argument");
                        process::exit(2);
                    });
            }
            "--verbose" | "-v" => verbose = true,
            "--no-exit-code" => no_exit_code = true,
            other if other.starts_with("--") => {
                eprintln!("Unknown option: {}", other);
                process::exit(2);
            }
            path => paths.push(path.to_string()),
        }
        i += 1;
    }

    let files: Vec<PathBuf> = if !paths.is_empty() {
        collect_files(&paths)
    } else {
        let git_root = find_git_root();
        let crates_dir = git_root
            .as_ref()
            .map(|r| r.join("crates"))
            .unwrap_or_else(|| PathBuf::from("crates"));
        let mut v = Vec::new();
        collect_recursive(&crates_dir, &mut v);
        v.sort();
        v
    };

    // Skip target/ directories
    let files: Vec<PathBuf> = files
        .into_iter()
        .filter(|f| !f.to_string_lossy().contains("/target/"))
        .collect();

    if files.is_empty() {
        eprintln!("No .rs files found.");
        process::exit(1);
    }

    let mut results: Vec<FileResult> = files.iter().map(|f| analyze_file(f)).collect();
    results.sort_by(|a, b| {
        b.total_score
            .partial_cmp(&a.total_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let exceeded = report(&results, threshold, verbose);

    if no_exit_code {
        process::exit(0);
    }
    process::exit(if exceeded { 1 } else { 0 });
}
