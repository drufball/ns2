mod common;

use predicates::prelude::*;

// ── Flow 10: spec new ────────────────────────────────────────────────────────

#[test]
fn spec_new_creates_file_with_targets() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "spec", "new", "my.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created spec at my.spec.md"));

    let content = std::fs::read_to_string(
        harness.repo_dir.path().join("my.spec.md"),
    )
    .unwrap();
    assert!(content.contains("targets:"));
    assert!(content.contains("crates/cli/src/**/*.rs"));
    assert!(!content.contains("verified:"), "new spec must not have a verified field");
}

#[test]
fn spec_new_multiple_targets() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "spec", "new", "multi.spec.md",
            "--target", "crates/cli/src/**/*.rs",
            "--target", "crates/agents/src/**/*.rs",
        ])
        .assert()
        .success();

    let content = std::fs::read_to_string(
        harness.repo_dir.path().join("multi.spec.md"),
    )
    .unwrap();
    assert!(content.contains("crates/cli/src/**/*.rs"));
    assert!(content.contains("crates/agents/src/**/*.rs"));
}

#[test]
fn spec_new_on_existing_path_fails() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "spec", "new", "dup.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args([
            "spec", "new", "dup.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .failure();
}

#[test]
fn spec_new_without_target_fails() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args(["spec", "new", "notarget.spec.md"])
        .assert()
        .failure();
}

// ── Flow 11: spec sync ───────────────────────────────────────────────────────

#[test]
fn spec_sync_clean_exits_zero() {
    let harness = common::TestHarness::new();
    harness.setup_codebase_layout();

    let main_rs = harness.repo_dir.path().join("crates/cli/src/main.rs");
    std::process::Command::new("touch")
        .args(["-t", "202001010000", main_rs.to_str().unwrap()])
        .status()
        .unwrap();

    harness
        .ns2()
        .args([
            "spec", "new", "cli.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["spec", "verify", "cli.spec.md"])
        .assert()
        .success();

    // Commit the spec so CI's git-ancestry staleness check sees it as newer than main.rs.
    harness.git(&["add", "cli.spec.md"]);
    harness.git(&["commit", "-m", "verify spec"]);

    harness
        .ns2()
        .args(["spec", "sync", "cli.spec.md"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn spec_sync_stale_after_touch_exits_nonzero() {
    let harness = common::TestHarness::new();
    harness.setup_codebase_layout();

    let main_rs = harness.repo_dir.path().join("crates/cli/src/main.rs");
    std::process::Command::new("touch")
        .args(["-t", "202001010000", main_rs.to_str().unwrap()])
        .status()
        .unwrap();

    harness
        .ns2()
        .args([
            "spec", "new", "cli.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["spec", "verify", "cli.spec.md"])
        .assert()
        .success();

    std::fs::write(&main_rs, "fn main() { println!(\"modified\"); }\n").unwrap();

    harness
        .ns2()
        .args(["spec", "sync", "cli.spec.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cli.spec.md"))
        .stderr(predicate::str::contains("main.rs"));
}

#[test]
fn spec_sync_no_path_finds_all_specs() {
    let harness = common::TestHarness::new();
    harness.setup_codebase_layout();

    let main_rs = harness.repo_dir.path().join("crates/cli/src/main.rs");
    std::process::Command::new("touch")
        .args(["-t", "202001010000", main_rs.to_str().unwrap()])
        .status()
        .unwrap();

    harness
        .ns2()
        .args([
            "spec", "new", "cli.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["spec", "verify", "cli.spec.md"])
        .assert()
        .success();

    std::fs::write(&main_rs, "fn main() { println!(\"modified\"); }\n").unwrap();

    harness
        .ns2()
        .args(["spec", "sync"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cli.spec.md"));
}

#[test]
fn spec_sync_unverified_spec_treats_all_files_stale() {
    let harness = common::TestHarness::new();
    harness.setup_codebase_layout();

    harness
        .ns2()
        .args([
            "spec", "new", "cli.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["spec", "sync", "cli.spec.md"])
        .assert()
        .failure();
}

#[test]
fn spec_sync_skips_specs_without_targets() {
    let harness = common::TestHarness::new();
    harness.setup_codebase_layout();

    harness
        .ns2()
        .args(["spec", "sync"])
        .assert()
        .success();
}

// ── Flow 12: spec verify ─────────────────────────────────────────────────────

#[test]
fn spec_verify_writes_verified_timestamp() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "spec", "new", "my.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["spec", "verify", "my.spec.md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Verified my.spec.md"));

    let content = std::fs::read_to_string(
        harness.repo_dir.path().join("my.spec.md"),
    )
    .unwrap();
    assert!(content.contains("verified:"), "verified field must be written to file");
}

#[test]
fn spec_verify_updates_existing_timestamp() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "spec", "new", "my.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["spec", "verify", "my.spec.md"])
        .assert()
        .success();

    let first_content = std::fs::read_to_string(
        harness.repo_dir.path().join("my.spec.md"),
    )
    .unwrap();
    let first_verified = first_content
        .lines()
        .find(|l| l.starts_with("verified:"))
        .unwrap()
        .to_string();

    harness
        .ns2()
        .args(["spec", "verify", "my.spec.md"])
        .assert()
        .success();

    let second_content = std::fs::read_to_string(
        harness.repo_dir.path().join("my.spec.md"),
    )
    .unwrap();
    let second_verified = second_content
        .lines()
        .find(|l| l.starts_with("verified:"))
        .unwrap()
        .to_string();

    assert!(
        second_verified >= first_verified,
        "second verified timestamp must be >= first: first={first_verified}, second={second_verified}"
    );
}

#[test]
fn spec_verify_preserves_body_and_targets() {
    use std::io::Write;
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "spec", "new", "my.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    let spec_path = harness.repo_dir.path().join("my.spec.md");
    let mut f = std::fs::OpenOptions::new().append(true).open(&spec_path).unwrap();
    writeln!(f, "\n# My Spec\n\nSome body.").unwrap();

    harness
        .ns2()
        .args(["spec", "verify", "my.spec.md"])
        .assert()
        .success();

    let content = std::fs::read_to_string(&spec_path).unwrap();
    assert!(content.contains("crates/cli/src/**/*.rs"), "targets must be preserved");
    assert!(content.contains("# My Spec"), "body heading must be preserved");
    assert!(content.contains("Some body."), "body text must be preserved");
}

#[test]
fn spec_verify_multiple_paths_at_once() {
    let harness = common::TestHarness::new();
    for name in &["a.spec.md", "b.spec.md", "c.spec.md"] {
        harness
            .ns2()
            .args([
                "spec", "new", name,
                "--target", "crates/cli/src/**/*.rs",
            ])
            .assert()
            .success();
    }

    harness
        .ns2()
        .args(["spec", "verify", "a.spec.md", "b.spec.md", "c.spec.md"])
        .assert()
        .success();

    for name in &["a.spec.md", "b.spec.md", "c.spec.md"] {
        let content = std::fs::read_to_string(harness.repo_dir.path().join(name)).unwrap();
        assert!(content.contains("verified:"), "{name} must have a verified timestamp");
    }
}

#[test]
fn spec_verify_partial_failure_still_verifies_valid() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "spec", "new", "good1.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();
    harness
        .ns2()
        .args([
            "spec", "new", "good2.spec.md",
            "--target", "crates/cli/src/**/*.rs",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args([
            "spec", "verify",
            "good1.spec.md",
            "does-not-exist.spec.md",
            "good2.spec.md",
        ])
        .assert()
        .failure();

    for name in &["good1.spec.md", "good2.spec.md"] {
        let content = std::fs::read_to_string(harness.repo_dir.path().join(name)).unwrap();
        assert!(content.contains("verified:"), "{name} must still be verified");
    }
}

#[test]
fn spec_verify_nonexistent_path_fails() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args(["spec", "verify", "does-not-exist.spec.md"])
        .assert()
        .failure();
}

#[test]
fn spec_verify_file_without_targets_fails() {
    let harness = common::TestHarness::new();
    let plain = harness.repo_dir.path().join("plain.md");
    std::fs::write(&plain, "# Just a plain markdown file\n\nNo frontmatter.\n").unwrap();

    harness
        .ns2()
        .args(["spec", "verify", "plain.md"])
        .assert()
        .failure();
}
