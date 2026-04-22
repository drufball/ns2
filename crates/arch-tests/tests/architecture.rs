use cargo_metadata::{DependencyKind, MetadataCommand};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Build a map of workspace crate name -> direct workspace dependency names,
/// considering only normal (non-dev, non-build) dependencies.
fn build_dep_graph() -> HashMap<String, HashSet<String>> {
    let metadata = MetadataCommand::new()
        .manifest_path(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../Cargo.toml"),
        )
        .exec()
        .expect("failed to run cargo metadata");

    let workspace_names: HashSet<String> = metadata
        .workspace_packages()
        .iter()
        .map(|p| p.name.clone())
        .collect();

    let mut graph: HashMap<String, HashSet<String>> = HashMap::new();

    for package in metadata.workspace_packages() {
        let direct_workspace_deps: HashSet<String> = package
            .dependencies
            .iter()
            .filter(|d| d.kind == DependencyKind::Normal)
            .filter(|d| workspace_names.contains(&d.name))
            .map(|d| d.name.clone())
            .collect();

        graph.insert(package.name.clone(), direct_workspace_deps);
    }

    graph
}

/// Compute all transitive workspace dependencies of `start` (not including `start` itself).
fn transitive_deps(graph: &HashMap<String, HashSet<String>>, start: &str) -> HashSet<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    if let Some(direct) = graph.get(start) {
        for dep in direct {
            queue.push_back(dep.clone());
        }
    }

    while let Some(current) = queue.pop_front() {
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current.clone());
        if let Some(deps) = graph.get(&current) {
            for dep in deps {
                if !visited.contains(dep) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }

    visited
}

/// Assert that `from` does not (transitively) depend on `to`.
fn assert_no_dep(graph: &HashMap<String, HashSet<String>>, from: &str, to: &str) {
    let all_deps = transitive_deps(graph, from);
    assert!(
        !all_deps.contains(to),
        "Architectural violation: `{from}` must not depend on `{to}` (found in transitive deps). \
         Dependency path exists: {from} -> ... -> {to}. \
         See architecture.spec.md for the rules."
    );
}

/// Assert that `from` does not directly depend on `to`.
fn assert_no_direct_dep(graph: &HashMap<String, HashSet<String>>, from: &str, to: &str) {
    let direct = graph.get(from).cloned().unwrap_or_default();
    assert!(
        !direct.contains(to),
        "Architectural violation: `{from}` must not directly depend on `{to}`. \
         See architecture.spec.md for the rules."
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `types` is the leaf crate — it must have zero workspace dependencies.
#[test]
fn types_has_no_workspace_deps() {
    let graph = build_dep_graph();
    let deps = transitive_deps(&graph, "types");
    assert!(
        deps.is_empty(),
        "Architectural violation: `types` must have zero workspace dependencies, \
         but found: {deps:?}. \
         See architecture.spec.md: \"Everything else depends on this; it depends on nothing.\""
    );
}

/// `tools` has no knowledge of sessions or turns — must not depend on db, harness, server, tui, or cli.
#[test]
fn tools_does_not_know_about_sessions() {
    let graph = build_dep_graph();
    for forbidden in &["db", "harness", "server", "tui", "cli"] {
        assert_no_dep(&graph, "tools", forbidden);
    }
}

/// `db` owns only SQLite access — must not depend on anthropic, tools, harness, server, tui, or cli.
#[test]
fn db_owns_only_sqlite_access() {
    let graph = build_dep_graph();
    for forbidden in &["anthropic", "tools", "harness", "server", "tui", "cli"] {
        assert_no_dep(&graph, "db", forbidden);
    }
}

/// `anthropic` owns only the Anthropic HTTP client — must not depend on db, tools, harness, server, tui, or cli.
#[test]
fn anthropic_owns_only_http_client() {
    let graph = build_dep_graph();
    for forbidden in &["db", "tools", "harness", "server", "tui", "cli"] {
        assert_no_dep(&graph, "anthropic", forbidden);
    }
}

/// `harness` has no knowledge of HTTP or subscribers — must not depend on server, tui, or cli.
#[test]
fn harness_does_not_know_about_http_or_subscribers() {
    let graph = build_dep_graph();
    for forbidden in &["server", "tui", "cli"] {
        assert_no_dep(&graph, "harness", forbidden);
    }
}

/// `tui` is a thin client connecting via HTTP only — must not depend on db, harness, tools, anthropic, server, or cli.
#[test]
fn tui_is_thin_client_with_no_internal_deps() {
    let graph = build_dep_graph();
    for forbidden in &["db", "harness", "tools", "anthropic", "server", "cli"] {
        assert_no_dep(&graph, "tui", forbidden);
    }
}

/// `server` must not depend on tui or cli (it is consumed by cli, not the other way around).
#[test]
fn server_does_not_depend_on_tui_or_cli() {
    let graph = build_dep_graph();
    for forbidden in &["tui", "cli"] {
        assert_no_dep(&graph, "server", forbidden);
    }
}

/// `workspace` is a pure git operations crate — must not depend on db, anthropic, harness, server, tui, or cli.
#[test]
fn workspace_has_no_application_deps() {
    let graph = build_dep_graph();
    for forbidden in &["db", "anthropic", "harness", "server", "tui", "cli"] {
        assert_no_dep(&graph, "workspace", forbidden);
    }
}

// ---------------------------------------------------------------------------
// Coverage-ignore allowlist helpers
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("failed to canonicalize workspace root")
}

#[derive(serde::Deserialize)]
struct CoverageIgnores {
    #[serde(default)]
    ignore: Vec<IgnoreEntry>,
    #[serde(default)]
    file_ignore: Vec<FileIgnoreEntry>,
}

#[derive(serde::Deserialize)]
struct IgnoreEntry {
    file: String,
    #[allow(dead_code)]
    item: String,
    #[allow(dead_code)]
    reason: String,
}

#[derive(serde::Deserialize)]
struct FileIgnoreEntry {
    path: String,
    #[allow(dead_code)]
    reason: String,
}

fn load_coverage_ignores() -> CoverageIgnores {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("coverage-ignores.toml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    toml::from_str(&contents)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

/// Walk all `.rs` files under `root`, skipping the `arch-tests` crate directory,
/// and return workspace-relative paths of files that contain `#[coverage(off)]`.
fn find_coverage_off_files(workspace_root: &Path) -> Vec<String> {
    let arch_tests_dir = workspace_root
        .join("crates")
        .join("arch-tests")
        .canonicalize()
        .expect("failed to canonicalize arch-tests dir");

    let crates_dir = workspace_root.join("crates");
    let mut found: Vec<String> = Vec::new();
    collect_coverage_off_files(&crates_dir, workspace_root, &arch_tests_dir, &mut found);
    found.sort();
    found
}

fn collect_coverage_off_files(
    dir: &Path,
    workspace_root: &Path,
    arch_tests_dir: &Path,
    found: &mut Vec<String>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = match entry.path().canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if path.is_dir() {
            // Skip the arch-tests crate entirely to avoid false positives.
            if path.starts_with(arch_tests_dir) {
                continue;
            }
            collect_coverage_off_files(&path, workspace_root, arch_tests_dir, found);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let contents = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if contents.contains("#[coverage(off)]") {
                // Convert to a workspace-relative path with forward slashes.
                let rel = path
                    .strip_prefix(workspace_root)
                    .expect("path should be under workspace root")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push(rel);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Coverage-ignore tests
// ---------------------------------------------------------------------------

/// Every `#[coverage(off)]` in the workspace must be registered in `coverage-ignores.toml`.
#[test]
fn test_coverage_ignores_are_registered() {
    let root = workspace_root();
    let ignores = load_coverage_ignores();
    let registered: HashSet<String> = ignores.ignore.iter().map(|e| e.file.clone()).collect();

    let files_with_coverage_off = find_coverage_off_files(&root);

    let unregistered: Vec<&String> = files_with_coverage_off
        .iter()
        .filter(|f| !registered.contains(*f))
        .collect();

    assert!(
        unregistered.is_empty(),
        "Found #[coverage(off)] in files not registered in crates/arch-tests/coverage-ignores.toml:\n\
         {}\n\n\
         Add an [[ignore]] entry for each file above to crates/arch-tests/coverage-ignores.toml \
         with `file`, `item`, and `reason` fields.",
        unregistered
            .iter()
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every entry in `coverage-ignores.toml` must correspond to a file that still
/// contains at least one `#[coverage(off)]` annotation.
#[test]
fn test_no_stale_coverage_ignores() {
    let root = workspace_root();
    let ignores = load_coverage_ignores();
    let files_with_coverage_off: HashSet<String> =
        find_coverage_off_files(&root).into_iter().collect();

    let stale: Vec<&str> = ignores
        .ignore
        .iter()
        .filter(|e| !files_with_coverage_off.contains(&e.file))
        .map(|e| e.file.as_str())
        .collect();

    assert!(
        stale.is_empty(),
        "Stale entries in crates/arch-tests/coverage-ignores.toml \
         (registered but no #[coverage(off)] found in the file):\n\
         {}\n\n\
         Remove or update these entries.",
        stale
            .iter()
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every [[file_ignore]] entry in coverage-ignores.toml must point to a file that
/// actually exists in the workspace, so stale entries don't silently widen the exclusion.
#[test]
fn test_no_stale_file_ignores() {
    let root = workspace_root();
    let ignores = load_coverage_ignores();

    let stale: Vec<&str> = ignores
        .file_ignore
        .iter()
        .filter(|e| {
            // The path is a regex substring — check that at least one real file matches it.
            let pattern = &e.path;
            !walkdir_any_match(&root, pattern)
        })
        .map(|e| e.path.as_str())
        .collect();

    assert!(
        stale.is_empty(),
        "Stale [[file_ignore]] entries in coverage-ignores.toml \
         (no file in the workspace matches the path pattern):\n\
         {}\n\n\
         Remove or update these entries.",
        stale
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn walkdir_any_match(workspace_root: &Path, substring: &str) -> bool {
    let crates_dir = workspace_root.join("crates");
    walkdir_match_inner(&crates_dir, workspace_root, substring)
}

fn walkdir_match_inner(dir: &Path, workspace_root: &Path, substring: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(path) = entry.path().canonicalize() else {
            continue;
        };
        if path.is_dir() {
            if walkdir_match_inner(&path, workspace_root, substring) {
                return true;
            }
        } else {
            let rel = path
                .strip_prefix(workspace_root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel.contains(substring) {
                return true;
            }
        }
    }
    false
}

/// Verify direct dependency edges match the spec — no direct dep means no accidental
/// coupling that might later grow into something transitive.
#[test]
fn direct_dep_edges_match_spec() {
    let graph = build_dep_graph();

    // types -> nothing
    assert_no_direct_dep(&graph, "types", "db");
    assert_no_direct_dep(&graph, "types", "anthropic");
    assert_no_direct_dep(&graph, "types", "tools");
    assert_no_direct_dep(&graph, "types", "harness");
    assert_no_direct_dep(&graph, "types", "server");
    assert_no_direct_dep(&graph, "types", "tui");
    assert_no_direct_dep(&graph, "types", "cli");

    // tools -> not sessions/turns
    assert_no_direct_dep(&graph, "tools", "db");
    assert_no_direct_dep(&graph, "tools", "harness");
    assert_no_direct_dep(&graph, "tools", "server");

    // db -> no upper layers
    assert_no_direct_dep(&graph, "db", "anthropic");
    assert_no_direct_dep(&graph, "db", "tools");
    assert_no_direct_dep(&graph, "db", "harness");
    assert_no_direct_dep(&graph, "db", "server");

    // anthropic -> no lower or peer layers
    assert_no_direct_dep(&graph, "anthropic", "db");
    assert_no_direct_dep(&graph, "anthropic", "tools");
    assert_no_direct_dep(&graph, "anthropic", "harness");
    assert_no_direct_dep(&graph, "anthropic", "server");

    // workspace -> no application layer
    assert_no_direct_dep(&graph, "workspace", "db");
    assert_no_direct_dep(&graph, "workspace", "anthropic");
    assert_no_direct_dep(&graph, "workspace", "harness");
    assert_no_direct_dep(&graph, "workspace", "server");
    assert_no_direct_dep(&graph, "workspace", "tui");
    assert_no_direct_dep(&graph, "workspace", "cli");

    // harness -> no HTTP layer
    assert_no_direct_dep(&graph, "harness", "server");
    assert_no_direct_dep(&graph, "harness", "tui");
    assert_no_direct_dep(&graph, "harness", "cli");

    // server -> no client layer
    assert_no_direct_dep(&graph, "server", "tui");
    assert_no_direct_dep(&graph, "server", "cli");
}
