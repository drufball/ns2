mod common;

use predicates::prelude::*;

#[test]
fn agent_list_when_no_agents_dir_shows_message() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args(["agent", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No agents found"));
}

#[test]
fn agent_new_creates_file_with_correct_content() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "reviewer",
            "--description", "Reviews pull requests",
            "--body", "You are a careful code reviewer.",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Created agent 'reviewer'"));

    let content = std::fs::read_to_string(
        harness.repo_dir.path().join(".ns2/agents/reviewer.md"),
    )
    .unwrap();
    assert!(content.contains("name: reviewer"));
    assert!(content.contains("description: Reviews pull requests"));
    assert!(content.contains("You are a careful code reviewer."));
}

#[test]
fn agent_list_shows_created_agent() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "reviewer",
            "--description", "Reviews pull requests",
            "--body", "You are a careful code reviewer.",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["agent", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reviewer"))
        .stdout(predicate::str::contains("Reviews pull requests"));
}

#[test]
fn agent_list_sorted_alphabetically() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "zebra",
            "--description", "Last agent",
            "--body", "body",
        ])
        .assert()
        .success();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "alpha",
            "--description", "First agent",
            "--body", "body",
        ])
        .assert()
        .success();

    let stdout = harness.ns2_stdout(&["agent", "list"]);
    let alpha_pos = stdout.find("alpha").unwrap();
    let zebra_pos = stdout.find("zebra").unwrap();
    assert!(alpha_pos < zebra_pos, "alpha must appear before zebra in listing");
}

#[test]
fn agent_edit_description_only_preserves_body() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "coder",
            "--description", "Original description",
            "--body", "Original body text.",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args([
            "agent", "edit",
            "--name", "coder",
            "--description", "Updated description",
        ])
        .assert()
        .success();

    let content = std::fs::read_to_string(
        harness.repo_dir.path().join(".ns2/agents/coder.md"),
    )
    .unwrap();
    assert!(content.contains("Updated description"));
    assert!(content.contains("Original body text."));
}

#[test]
fn agent_edit_body_only_preserves_description() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "coder",
            "--description", "Original description",
            "--body", "Original body text.",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args([
            "agent", "edit",
            "--name", "coder",
            "--body", "Updated body text.",
        ])
        .assert()
        .success();

    let content = std::fs::read_to_string(
        harness.repo_dir.path().join(".ns2/agents/coder.md"),
    )
    .unwrap();
    assert!(content.contains("Original description"));
    assert!(content.contains("Updated body text."));
}

#[test]
fn agent_edit_without_flags_fails() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "coder",
            "--description", "desc",
            "--body", "body",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args(["agent", "edit", "--name", "coder"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("at least one"));
}

#[test]
fn agent_new_duplicate_name_fails() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "coder",
            "--description", "desc",
            "--body", "body",
        ])
        .assert()
        .success();

    harness
        .ns2()
        .args([
            "agent", "new",
            "--name", "coder",
            "--description", "desc",
            "--body", "body",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn agent_new_requires_name() {
    let harness = common::TestHarness::new();
    harness
        .ns2()
        .args([
            "agent", "new",
            "--description", "desc",
            "--body", "body",
        ])
        .assert()
        .failure();
}
