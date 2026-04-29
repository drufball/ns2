//! Cohesion analysis for Rust source files — Rust-adapted LCOM4.
//!
//! Algorithm:
//!   1. Parse every top-level `fn` and `impl` method via `syn`
//!   2. For each function, record which file-local names it references
//!      (file-local = any struct/enum/fn/type defined in the same file)
//!   3. Build an undirected graph: two functions share an edge iff they both
//!      reference at least one common file-local name
//!   4. LCOM4 = number of connected components (higher → less cohesive)
//!   5. Cohesion score = 1 - (components-1)/(functions-1)  [0..1]
//!   6. Orphan ratio = fraction of functions that are isolated singletons
//!
//! The compound "concern score" used for flagging combines LCOM4 with file
//! size so that tiny files with a few standalone utilities aren't penalized:
//!   concern_score = components * sqrt(total_lines / 100)
//! Files with concern_score > 10 (empirically tuned) are flagged.
//!
//! Usage: cohesion <path/to/file.rs> [path2.rs ...]

use std::{
    collections::{HashMap, HashSet},
    env,
    fs,
    path::Path,
};
use syn::{
    visit::Visit,
    File, Ident, ImplItem, ItemImpl, Type, TypePath, ExprMethodCall, ExprPath,
};
use proc_macro2::Span;

// ── Name collector ────────────────────────────────────────────────────────────

/// Walks any AST node and collects every identifier that appears.
struct NameCollector {
    names: HashSet<String>,
}

impl<'ast> Visit<'ast> for NameCollector {
    fn visit_expr_path(&mut self, i: &'ast ExprPath) {
        for seg in &i.path.segments {
            self.names.insert(seg.ident.to_string());
        }
        syn::visit::visit_expr_path(self, i);
    }

    fn visit_expr_method_call(&mut self, i: &'ast ExprMethodCall) {
        self.names.insert(i.method.to_string());
        syn::visit::visit_expr_method_call(self, i);
    }

    fn visit_type_path(&mut self, i: &'ast TypePath) {
        for seg in &i.path.segments {
            self.names.insert(seg.ident.to_string());
        }
        syn::visit::visit_type_path(self, i);
    }

    fn visit_pat_struct(&mut self, i: &'ast syn::PatStruct) {
        for seg in &i.path.segments {
            self.names.insert(seg.ident.to_string());
        }
        syn::visit::visit_pat_struct(self, i);
    }

    fn visit_pat_tuple_struct(&mut self, i: &'ast syn::PatTupleStruct) {
        for seg in &i.path.segments {
            self.names.insert(seg.ident.to_string());
        }
        syn::visit::visit_pat_tuple_struct(self, i);
    }

    fn visit_ident(&mut self, i: &'ast Ident) {
        self.names.insert(i.to_string());
    }
}

fn names_in_block(block: &syn::Block) -> HashSet<String> {
    let mut c = NameCollector { names: HashSet::new() };
    c.visit_block(block);
    c.names
}

fn names_in_sig(sig: &syn::Signature) -> HashSet<String> {
    let mut c = NameCollector { names: HashSet::new() };
    for param in &sig.inputs {
        if let syn::FnArg::Typed(pt) = param {
            c.visit_type(&pt.ty);
        }
    }
    if let syn::ReturnType::Type(_, ty) = &sig.output {
        c.visit_type(ty);
    }
    c.names
}

// ── AST extraction ────────────────────────────────────────────────────────────

#[derive(Debug)]
struct FnInfo {
    display_name: String,
    refs: HashSet<String>,   // names referenced in body + signature
    line: usize,             // 1-indexed start line
}

struct FileLocals {
    defined: HashSet<String>,
}

fn span_line(span: Span) -> usize {
    span.start().line
}

fn impl_type_name(imp: &ItemImpl) -> Option<String> {
    if let Type::Path(tp) = &*imp.self_ty {
        tp.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

fn extract(file: &File) -> (Vec<FnInfo>, FileLocals) {
    let mut functions: Vec<FnInfo> = Vec::new();
    let mut defined: HashSet<String> = HashSet::new();

    // Pass 1: collect every name defined at file scope
    for item in &file.items {
        match item {
            syn::Item::Fn(f)     => { defined.insert(f.sig.ident.to_string()); }
            syn::Item::Struct(s) => { defined.insert(s.ident.to_string()); }
            syn::Item::Enum(e)   => { defined.insert(e.ident.to_string()); }
            syn::Item::Type(t)   => { defined.insert(t.ident.to_string()); }
            syn::Item::Const(c)  => { defined.insert(c.ident.to_string()); }
            syn::Item::Static(s) => { defined.insert(s.ident.to_string()); }
            syn::Item::Impl(imp) => {
                if let Some(n) = impl_type_name(imp) { defined.insert(n); }
                for ii in &imp.items {
                    if let ImplItem::Fn(m) = ii { defined.insert(m.sig.ident.to_string()); }
                }
            }
            _ => {}
        }
    }

    // Pass 2: extract per-function reference sets
    for item in &file.items {
        match item {
            syn::Item::Fn(f) => {
                let refs: HashSet<String> = names_in_block(&f.block)
                    .union(&names_in_sig(&f.sig))
                    .cloned()
                    .collect();
                functions.push(FnInfo {
                    display_name: format!("fn {}", f.sig.ident),
                    refs,
                    line: span_line(f.sig.fn_token.span),
                });
            }
            syn::Item::Impl(imp) => {
                let type_name = impl_type_name(imp);
                for ii in &imp.items {
                    if let ImplItem::Fn(m) = ii {
                        let mut refs: HashSet<String> = names_in_block(&m.block)
                            .union(&names_in_sig(&m.sig))
                            .cloned()
                            .collect();
                        // All methods on the same impl share their self-type name
                        if let Some(ref n) = type_name { refs.insert(n.clone()); }
                        let display = match &type_name {
                            Some(n) => format!("{}::{}", n, m.sig.ident),
                            None    => format!("impl::{}", m.sig.ident),
                        };
                        functions.push(FnInfo {
                            display_name: display,
                            refs,
                            line: span_line(m.sig.fn_token.span),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    (functions, FileLocals { defined })
}

// ── LCOM4 via union-find ──────────────────────────────────────────────────────

struct UnionFind { parent: Vec<usize>, rank: Vec<usize> }

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind { parent: (0..n).collect(), rank: vec![0; n] }
    }
    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x { self.parent[x] = self.find(self.parent[x]); }
        self.parent[x]
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb { return; }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less    => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal   => { self.parent[rb] = ra; self.rank[ra] += 1; }
        }
    }
    fn components(&mut self, n: usize) -> Vec<Vec<usize>> {
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n { groups.entry(self.find(i)).or_default().push(i); }
        groups.into_values().collect()
    }
}

fn lcom4(functions: &[FnInfo], locals: &FileLocals) -> Vec<Vec<usize>> {
    let n = functions.len();
    if n == 0 { return vec![]; }

    let mut uf = UnionFind::new(n);
    let mut name_to_fns: HashMap<&str, Vec<usize>> = HashMap::new();

    for (i, f) in functions.iter().enumerate() {
        for name in &f.refs {
            if locals.defined.contains(name) {
                name_to_fns.entry(name).or_default().push(i);
            }
        }
    }

    for fns in name_to_fns.values() {
        for w in fns.windows(2) { uf.union(w[0], w[1]); }
    }

    uf.components(n)
}

// ── Metrics ───────────────────────────────────────────────────────────────────

struct Metrics {
    n_fns: usize,
    n_lines: usize,
    n_components: usize,
    cohesion_score: f64,   // 1 - (C-1)/(N-1), 0..1
    orphan_ratio: f64,     // fraction of singletons
    concern_score: f64,    // compound: components * sqrt(lines/100)
    flagged: bool,
}

impl Metrics {
    fn compute(n_fns: usize, n_lines: usize, components: &[Vec<usize>], threshold: f64) -> Self {
        let n_components = components.len();
        let cohesion_score = if n_fns <= 1 {
            1.0
        } else {
            1.0 - ((n_components as f64 - 1.0) / (n_fns as f64 - 1.0))
        };
        let orphan_ratio = components.iter().filter(|c| c.len() == 1).count() as f64
            / n_fns.max(1) as f64;
        // Concern score weights component count by file size.
        // sqrt(lines/100): a 100-line file contributes weight 1, 3900-line file ≈ 6.2
        let size_weight = ((n_lines as f64) / 100.0).sqrt();
        let concern_score = n_components as f64 * size_weight;

        // Flagged if concern score > 12 (calibrated so worktree.rs ≈ 10.9 passes,
        // agents/src/lib.rs ≈ 20.7 is flagged, and the three target files all score > 30)
        let flagged = concern_score > threshold;

        Metrics { n_fns, n_lines, n_components, cohesion_score, orphan_ratio, concern_score, flagged }
    }
}

// ── Reporting ─────────────────────────────────────────────────────────────────

fn print_report(path: &str, functions: &[FnInfo], components: &[Vec<usize>], m: &Metrics) {
    let verdict = if m.flagged { "FLAG" } else { "OK  " };
    println!("═══════════════════════════════════════════════════════════════");
    println!("File: {}  [{}]", path, verdict);
    println!("═══════════════════════════════════════════════════════════════");
    println!("  Lines of code      : {}", m.n_lines);
    println!("  Functions/methods  : {}", m.n_fns);
    println!("  LCOM4 components   : {}", m.n_components);
    println!("  Cohesion score     : {:.3}  (1.0=perfect, 0.0=fully scattered)", m.cohesion_score);
    println!("  Orphan ratio       : {:.3}  (fraction of singleton clusters)", m.orphan_ratio);
    println!("  Concern score      : {:.1}  (components × √(lines/100); >10 = flag)", m.concern_score);
    println!();

    if m.n_components == 1 {
        println!("  All functions share a common resource thread (cohesive).");
    } else {
        println!("  {} distinct concern cluster(s) detected:", m.n_components);
    }
    println!();

    let mut sorted: Vec<&Vec<usize>> = components.iter().collect();
    sorted.sort_by_key(|c| c.iter().map(|&i| functions[i].line).min().unwrap_or(0));

    for (ci, cluster) in sorted.iter().enumerate() {
        let mut fns: Vec<&FnInfo> = cluster.iter().map(|&i| &functions[i]).collect();
        fns.sort_by_key(|f| f.line);
        let singleton_note = if cluster.len() == 1 { "  [orphan]" } else { "" };
        println!("  Cluster {} ({} fns){}:", ci + 1, fns.len(), singleton_note);
        for f in &fns {
            println!("    line {:>5}  {}", f.line, f.display_name);
        }
        println!();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── UnionFind ────────────────────────────────────────────────────────────

    #[test]
    fn union_find_single_node() {
        let mut uf = UnionFind::new(1);
        assert_eq!(uf.find(0), 0);
        let comps = uf.components(1);
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0], vec![0]);
    }

    #[test]
    fn union_find_no_unions_gives_n_components() {
        let mut uf = UnionFind::new(4);
        let comps = uf.components(4);
        assert_eq!(comps.len(), 4);
    }

    #[test]
    fn union_find_union_merges_two_nodes() {
        let mut uf = UnionFind::new(3);
        uf.union(0, 1);
        // find(0) and find(1) should be the same root
        assert_eq!(uf.find(0), uf.find(1));
        assert_ne!(uf.find(0), uf.find(2));
        let comps = uf.components(3);
        assert_eq!(comps.len(), 2);
    }

    #[test]
    fn union_find_transitive_merge() {
        let mut uf = UnionFind::new(3);
        uf.union(0, 1);
        uf.union(1, 2);
        assert_eq!(uf.find(0), uf.find(2));
        let comps = uf.components(3);
        assert_eq!(comps.len(), 1);
        let mut only = comps.into_iter().next().unwrap();
        only.sort();
        assert_eq!(only, vec![0, 1, 2]);
    }

    #[test]
    fn union_find_idempotent_union() {
        let mut uf = UnionFind::new(2);
        uf.union(0, 1);
        uf.union(0, 1); // second call should be a no-op
        uf.union(1, 0);
        let comps = uf.components(2);
        assert_eq!(comps.len(), 1);
    }

    #[test]
    fn union_find_components_returns_all_members() {
        let mut uf = UnionFind::new(5);
        uf.union(0, 2);
        uf.union(2, 4);
        // component {0,2,4} and singletons {1}, {3}
        let comps = uf.components(5);
        assert_eq!(comps.len(), 3);
        let mut large: Vec<usize> = comps
            .into_iter()
            .max_by_key(|c| c.len())
            .unwrap();
        large.sort();
        assert_eq!(large, vec![0, 2, 4]);
    }

    // ── lcom4 ────────────────────────────────────────────────────────────────

    fn make_fn(name: &str, refs: &[&str]) -> FnInfo {
        FnInfo {
            display_name: name.to_string(),
            refs: refs.iter().map(|s| s.to_string()).collect(),
            line: 1,
        }
    }

    fn make_locals(names: &[&str]) -> FileLocals {
        FileLocals {
            defined: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn lcom4_empty_file_returns_no_components() {
        let comps = lcom4(&[], &make_locals(&[]));
        assert_eq!(comps.len(), 0);
    }

    #[test]
    fn lcom4_single_function_is_one_component() {
        let fns = vec![make_fn("foo", &["Foo"])];
        let locals = make_locals(&["Foo"]);
        let comps = lcom4(&fns, &locals);
        assert_eq!(comps.len(), 1);
    }

    #[test]
    fn lcom4_two_functions_sharing_local_are_one_component() {
        let fns = vec![
            make_fn("foo", &["SharedThing"]),
            make_fn("bar", &["SharedThing"]),
        ];
        let locals = make_locals(&["SharedThing"]);
        let comps = lcom4(&fns, &locals);
        assert_eq!(comps.len(), 1);
    }

    #[test]
    fn lcom4_two_functions_sharing_no_local_are_two_components() {
        let fns = vec![
            make_fn("foo", &["Aaa"]),
            make_fn("bar", &["Bbb"]),
        ];
        let locals = make_locals(&["Aaa", "Bbb"]);
        let comps = lcom4(&fns, &locals);
        assert_eq!(comps.len(), 2);
    }

    #[test]
    fn lcom4_external_refs_do_not_connect_functions() {
        // Both fns reference "String" but it is not a file-local name.
        let fns = vec![
            make_fn("foo", &["String"]),
            make_fn("bar", &["String"]),
        ];
        let locals = make_locals(&[]); // "String" is not local
        let comps = lcom4(&fns, &locals);
        assert_eq!(comps.len(), 2);
    }

    #[test]
    fn lcom4_transitive_sharing_collapses_to_one_component() {
        // A shares X with B; B shares Y with C; A and C share nothing directly.
        // Expected: all three in one component.
        let fns = vec![
            make_fn("a", &["X"]),
            make_fn("b", &["X", "Y"]),
            make_fn("c", &["Y"]),
        ];
        let locals = make_locals(&["X", "Y"]);
        let comps = lcom4(&fns, &locals);
        assert_eq!(comps.len(), 1);
    }

    #[test]
    fn lcom4_three_functions_two_clusters() {
        let fns = vec![
            make_fn("a", &["Alpha"]),
            make_fn("b", &["Alpha"]),
            make_fn("c", &["Beta"]),
        ];
        let locals = make_locals(&["Alpha", "Beta"]);
        let comps = lcom4(&fns, &locals);
        assert_eq!(comps.len(), 2);
        let mut sizes: Vec<usize> = comps.iter().map(|c| c.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![1, 2]);
    }

    // ── Metrics::compute ─────────────────────────────────────────────────────

    #[test]
    fn metrics_single_function_is_perfect_cohesion() {
        // 1 function = 1 component; cohesion defined as 1.0
        let comps = vec![vec![0usize]];
        let m = Metrics::compute(1, 50, &comps, 12.0);
        assert_eq!(m.n_fns, 1);
        assert_eq!(m.n_components, 1);
        assert!((m.cohesion_score - 1.0).abs() < 1e-9);
        assert!(!m.flagged);
    }

    #[test]
    fn metrics_two_functions_one_component_is_perfect_cohesion() {
        let comps = vec![vec![0usize, 1]];
        let m = Metrics::compute(2, 100, &comps, 12.0);
        assert!((m.cohesion_score - 1.0).abs() < 1e-9);
        // concern_score = 1 * sqrt(100/100) = 1.0 → not flagged
        assert!((m.concern_score - 1.0).abs() < 1e-9);
        assert!(!m.flagged);
    }

    #[test]
    fn metrics_two_functions_two_components_zero_cohesion() {
        let comps = vec![vec![0usize], vec![1]];
        let m = Metrics::compute(2, 100, &comps, 12.0);
        // cohesion = 1 - (2-1)/(2-1) = 0.0
        assert!((m.cohesion_score - 0.0).abs() < 1e-9);
        assert_eq!(m.orphan_ratio, 1.0);
    }

    #[test]
    fn metrics_concern_score_formula() {
        // 3 components, 400 lines → concern = 3 * sqrt(4.0) = 6.0
        let comps = vec![vec![0usize], vec![1], vec![2]];
        let m = Metrics::compute(3, 400, &comps, 12.0);
        assert!((m.concern_score - 6.0).abs() < 1e-9);
        assert!(!m.flagged); // 6.0 < 12.0
    }

    #[test]
    fn metrics_flagged_when_concern_score_exceeds_threshold() {
        // 20 components, 10000 lines → concern = 20 * sqrt(100) = 200.0
        let comps: Vec<Vec<usize>> = (0..20).map(|i| vec![i]).collect();
        let m = Metrics::compute(20, 10_000, &comps, 12.0);
        assert!(m.concern_score > 12.0);
        assert!(m.flagged);
    }

    #[test]
    fn metrics_orphan_ratio_correct() {
        // 3 singletons + 1 cluster of 2 → 5 fns, 3 orphans → ratio = 3/5 = 0.6
        let comps = vec![vec![0usize], vec![1], vec![2], vec![3, 4]];
        let m = Metrics::compute(5, 100, &comps, 12.0);
        assert!((m.orphan_ratio - 0.6).abs() < 1e-9);
    }

    #[test]
    fn metrics_zero_functions_does_not_panic() {
        // Edge case: empty file — components will be empty too
        let comps: Vec<Vec<usize>> = vec![];
        // n_fns=0, n_lines=0: concern_score = 0 * sqrt(0) = 0
        let m = Metrics::compute(0, 0, &comps, 12.0);
        assert_eq!(m.n_components, 0);
        assert!((m.cohesion_score - 1.0).abs() < 1e-9); // 0 or 1 fn → 1.0
        assert!(!m.flagged);
    }

    // ── extract ───────────────────────────────────────────────────────────────

    fn parse(src: &str) -> syn::File {
        syn::parse_str(src).expect("parse failed")
    }

    #[test]
    fn extract_empty_file_no_functions_no_locals() {
        let file = parse("");
        let (fns, locals) = extract(&file);
        assert!(fns.is_empty());
        assert!(locals.defined.is_empty());
    }

    #[test]
    fn extract_top_level_fn_collected() {
        let file = parse("fn foo() {}");
        let (fns, locals) = extract(&file);
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].display_name, "fn foo");
        assert!(locals.defined.contains("foo"));
    }

    #[test]
    fn extract_struct_in_locals() {
        let file = parse("struct Foo; fn bar() {}");
        let (_, locals) = extract(&file);
        assert!(locals.defined.contains("Foo"));
        assert!(locals.defined.contains("bar"));
    }

    #[test]
    fn extract_impl_methods_collected() {
        let file = parse("struct Foo; impl Foo { fn new() -> Foo { Foo } fn run(&self) {} }");
        let (fns, locals) = extract(&file);
        let names: Vec<&str> = fns.iter().map(|f| f.display_name.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("new")), "expected new method: {:?}", names);
        assert!(names.iter().any(|n| n.contains("run")), "expected run method: {:?}", names);
        assert!(locals.defined.contains("Foo"));
    }

    #[test]
    fn extract_fn_body_refs_captured() {
        let file = parse("struct Bar; fn foo() { let _x = Bar; }");
        let (fns, _) = extract(&file);
        assert_eq!(fns.len(), 1);
        assert!(fns[0].refs.contains("Bar"), "refs should contain Bar: {:?}", fns[0].refs);
    }

    // ── impl_type_name ────────────────────────────────────────────────────────

    #[test]
    fn impl_type_name_extracts_struct_name() {
        let file = parse("struct Foo; impl Foo { fn method(&self) {} }");
        for item in &file.items {
            if let syn::Item::Impl(imp) = item {
                assert_eq!(impl_type_name(imp), Some("Foo".to_string()));
                return;
            }
        }
        panic!("no impl block found");
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse optional --threshold <value> flag
    let mut threshold = 12.0f64;
    let mut file_args: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--threshold" {
            i += 1;
            if i < args.len() {
                threshold = args[i].parse().unwrap_or_else(|_| {
                    eprintln!("Invalid threshold value: {}", args[i]);
                    std::process::exit(1);
                });
            }
        } else {
            file_args.push(&args[i]);
        }
        i += 1;
    }

    if file_args.is_empty() {
        eprintln!("Usage: cohesion [--threshold <f64>] <file.rs> [file2.rs ...]");
        eprintln!("  --threshold  Concern score threshold for flagging (default: 12.0)");
        eprintln!("               concern_score = LCOM4_components × sqrt(lines/100)");
        std::process::exit(1);
    }

    struct Row {
        short_name: String,
        m: Metrics,
    }
    let mut rows: Vec<Row> = Vec::new();

    for path_str in &file_args {
        let content = match fs::read_to_string(path_str) {
            Ok(c) => c,
            Err(e) => { eprintln!("Error reading {}: {}", path_str, e); continue; }
        };
        let n_lines = content.lines().count();

        let file: File = match syn::parse_str(&content) {
            Ok(f) => f,
            Err(e) => { eprintln!("Parse error in {}: {}", path_str, e); continue; }
        };

        let (functions, locals) = extract(&file);
        let components = lcom4(&functions, &locals);
        let m = Metrics::compute(functions.len(), n_lines, &components, threshold);

        print_report(path_str, &functions, &components, &m);

        let short_name = Path::new(*path_str)
            .components()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .iter()
            .rev()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        rows.push(Row { short_name, m });
    }

    if rows.len() > 1 {
        println!("═══════════════════════════════════════════════════════════════");
        println!("SUMMARY  (flag threshold: concern_score > {:.1})", threshold);
        println!("═══════════════════════════════════════════════════════════════");
        println!(
            "{:<45} {:>6} {:>6} {:>6} {:>8} {:>8} {:>12}",
            "File", "Lines", "Fns", "LCOM4", "Cohesion", "Orphans", "ConcernScore"
        );
        println!("{}", "─".repeat(100));
        for r in &rows {
            let flag = if r.m.flagged { " ← FLAGGED" } else { "" };
            println!(
                "{:<45} {:>6} {:>6} {:>6} {:>8.3} {:>8.3} {:>12.1}{}",
                r.short_name, r.m.n_lines, r.m.n_fns, r.m.n_components,
                r.m.cohesion_score, r.m.orphan_ratio, r.m.concern_score, flag
            );
        }
    }
}
