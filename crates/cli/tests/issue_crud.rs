mod common;

use common::TestHarness;
use predicates::prelude::*;

// ─── Flow 55: issue wait --timeout ───────────────────────────────────────────

#[test]
fn issue_wait_timeout_exits_nonzero_on_non_terminal_issue() {
    let mut h = TestHarness::new();
    h.start_server();

    // An issue in 'open' state with no session — never finishes on its own.
    let id = h.ns2_stdout(&["issue", "new", "--title", "t", "--body", "b"]);

    h.ns2()
        .args(["issue", "wait", "--id", &id, "--timeout", "1"])
        .assert()
        .failure();
}

fn write_agent(h: &TestHarness, name: &str) {
    let dir = h.repo_dir.path().join(".ns2/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!("---\nname: {name}\ndescription: Test agent\n---\n\nYou are a test agent.\n"),
    )
    .unwrap();
}

// Flow 14 — Issue list and filtering

#[test]
fn issue_list_empty_shows_no_issues() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2()
        .args(["issue", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No issues found."));
}

#[test]
fn issue_new_prints_id_to_stdout() {
    let mut h = TestHarness::new();
    h.start_server();
    let stdout = h.ns2_stdout(&["issue", "new", "--title", "My Issue", "--body", "body text"]);
    assert_eq!(stdout.len(), 4, "expected 4-char ID, got: {stdout}");
    assert!(
        stdout.chars().all(|c| c.is_ascii_alphanumeric()),
        "ID should be alphanumeric, got: {stdout}"
    );
}

#[test]
fn issue_list_shows_all_issues() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2_stdout(&["issue", "new", "--title", "Alpha", "--body", "b"]);
    h.ns2_stdout(&["issue", "new", "--title", "Beta", "--body", "b"]);
    h.ns2_stdout(&["issue", "new", "--title", "Gamma", "--body", "b"]);
    let out = h.ns2_stdout(&["issue", "list"]);
    assert!(out.contains("Alpha"), "list should contain Alpha");
    assert!(out.contains("Beta"), "list should contain Beta");
    assert!(out.contains("Gamma"), "list should contain Gamma");
    assert!(out.contains("id"), "list should contain id column");
    assert!(out.contains("title"), "list should contain title column");
    assert!(out.contains("status"), "list should contain status column");
    assert!(
        out.contains("assignee"),
        "list should contain assignee column"
    );
    assert!(out.contains("branch"), "list should contain branch column");
    assert!(
        out.contains("created_at"),
        "list should contain created_at column"
    );
}

#[test]
fn issue_list_filter_by_status_open() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2_stdout(&["issue", "new", "--title", "Open One", "--body", "b"]);
    h.ns2_stdout(&["issue", "new", "--title", "Open Two", "--body", "b"]);
    let out = h.ns2_stdout(&["issue", "list", "--status", "open"]);
    assert!(
        out.contains("Open One"),
        "filter open should include Open One"
    );
    assert!(
        out.contains("Open Two"),
        "filter open should include Open Two"
    );
}

#[test]
fn issue_list_filter_by_status_completed_empty() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2_stdout(&["issue", "new", "--title", "Open Issue", "--body", "b"]);
    h.ns2()
        .args(["issue", "list", "--status", "completed"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No issues found."));
}

#[test]
fn issue_list_filter_by_assignee() {
    let mut h = TestHarness::new();
    h.start_server();
    write_agent(&h, "swe");
    write_agent(&h, "qa");
    h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "SWE Task",
        "--body",
        "b",
        "--assignee",
        "swe",
    ]);
    h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "QA Task",
        "--body",
        "b",
        "--assignee",
        "qa",
    ]);
    h.ns2_stdout(&["issue", "new", "--title", "No Assignee Task", "--body", "b"]);

    let swe_out = h.ns2_stdout(&["issue", "list", "--assignee", "swe"]);
    assert!(
        swe_out.contains("SWE Task"),
        "swe filter should include SWE Task"
    );
    assert!(
        !swe_out.contains("QA Task"),
        "swe filter should not include QA Task"
    );
    assert!(
        !swe_out.contains("No Assignee Task"),
        "swe filter should not include No Assignee Task"
    );

    let qa_out = h.ns2_stdout(&["issue", "list", "--assignee", "qa"]);
    assert!(
        qa_out.contains("QA Task"),
        "qa filter should include QA Task"
    );
    assert!(
        !qa_out.contains("SWE Task"),
        "qa filter should not include SWE Task"
    );
}

#[test]
fn issue_edit_title_reflected_in_list() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&["issue", "new", "--title", "Old Title", "--body", "b"]);
    h.ns2()
        .args(["issue", "edit", "--id", &id, "--title", "New Title"])
        .assert()
        .success();
    let out = h.ns2_stdout(&["issue", "list"]);
    assert!(out.contains("New Title"), "list should show new title");
    assert!(!out.contains("Old Title"), "list should not show old title");
}

#[test]
fn issue_edit_assignee_reflected_in_list() {
    let mut h = TestHarness::new();
    h.start_server();
    write_agent(&h, "swe");
    let id = h.ns2_stdout(&["issue", "new", "--title", "My Task", "--body", "b"]);
    h.ns2()
        .args(["issue", "edit", "--id", &id, "--assignee", "swe"])
        .assert()
        .success();
    let out = h.ns2_stdout(&["issue", "list"]);
    assert!(out.contains("swe"), "list should show updated assignee");
}

#[test]
fn issue_edit_clear_assignee() {
    let mut h = TestHarness::new();
    h.start_server();
    write_agent(&h, "swe");
    let id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Assigned Task",
        "--body",
        "b",
        "--assignee",
        "swe",
    ]);
    h.ns2()
        .args(["issue", "edit", "--id", &id, "--assignee", ""])
        .assert()
        .success();
    h.ns2()
        .args(["issue", "list", "--assignee", "swe"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No issues found."));
}

// Flow 15 — Issue relationships

#[test]
fn issue_child_inherits_parent_branch() {
    let mut h = TestHarness::new();
    h.start_server();
    let parent_id = h.ns2_stdout(&["issue", "new", "--title", "Parent Issue", "--body", "b"]);
    let child_id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Child Issue",
        "--body",
        "b",
        "--parent",
        &parent_id,
    ]);
    let parent_json = h.http_get(&format!("/issues/{parent_id}"));
    let child_json = h.http_get(&format!("/issues/{child_id}"));
    assert!(
        parent_json.contains("\"branch\""),
        "parent should have branch field"
    );
    assert!(
        child_json.contains("\"branch\""),
        "child should have branch field"
    );
    let parent_branch = extract_branch(&parent_json);
    let child_branch = extract_branch(&child_json);
    assert_eq!(
        parent_branch, child_branch,
        "child should inherit parent branch"
    );
}

#[test]
fn issue_list_filter_by_parent() {
    let mut h = TestHarness::new();
    h.start_server();
    let parent_id = h.ns2_stdout(&["issue", "new", "--title", "Parent", "--body", "b"]);
    h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Child One",
        "--body",
        "b",
        "--parent",
        &parent_id,
    ]);
    h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Child Two",
        "--body",
        "b",
        "--parent",
        &parent_id,
    ]);
    h.ns2_stdout(&["issue", "new", "--title", "Unrelated", "--body", "b"]);
    let out = h.ns2_stdout(&["issue", "list", "--parent", &parent_id]);
    assert!(
        out.contains("Child One"),
        "filter by parent should include Child One"
    );
    assert!(
        out.contains("Child Two"),
        "filter by parent should include Child Two"
    );
    assert!(
        !out.contains("Unrelated"),
        "filter by parent should not include Unrelated"
    );
    assert!(
        !out.contains("Parent"),
        "filter by parent should not include Parent itself"
    );
}

#[test]
fn issue_list_filter_by_blocked_on() {
    let mut h = TestHarness::new();
    h.start_server();
    let source_id = h.ns2_stdout(&["issue", "new", "--title", "Blocker", "--body", "b"]);
    let dependent_id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Blocked",
        "--body",
        "b",
        "--blocked-on",
        &source_id,
    ]);
    h.ns2_stdout(&["issue", "new", "--title", "Unblocked", "--body", "b"]);
    let out = h.ns2_stdout(&["issue", "list", "--blocked-on", &source_id]);
    assert!(
        out.contains("Blocked"),
        "filter by blocked-on should include blocked issue"
    );
    assert!(
        !out.contains("Unblocked"),
        "filter by blocked-on should not include unblocked issue"
    );
    assert!(
        !out.contains("Blocker"),
        "filter by blocked-on should not include blocker itself"
    );
    let _ = dependent_id;
}

#[test]
fn issue_edit_add_parent() {
    let mut h = TestHarness::new();
    h.start_server();
    let parent_id = h.ns2_stdout(&["issue", "new", "--title", "Parent", "--body", "b"]);
    let child_id = h.ns2_stdout(&["issue", "new", "--title", "Orphan", "--body", "b"]);
    h.ns2()
        .args(["issue", "edit", "--id", &child_id, "--parent", &parent_id])
        .assert()
        .success();
    let out = h.ns2_stdout(&["issue", "list", "--parent", &parent_id]);
    assert!(
        out.contains("Orphan"),
        "after editing parent, issue should appear in parent filter"
    );
}

#[test]
fn issue_edit_clear_parent() {
    let mut h = TestHarness::new();
    h.start_server();
    let parent_id = h.ns2_stdout(&["issue", "new", "--title", "Parent", "--body", "b"]);
    let child_id = h.ns2_stdout(&[
        "issue", "new", "--title", "Child", "--body", "b", "--parent", &parent_id,
    ]);
    h.ns2()
        .args(["issue", "edit", "--id", &child_id, "--parent", ""])
        .assert()
        .success();
    h.ns2()
        .args(["issue", "list", "--parent", &parent_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("No issues found."));
}

#[test]
fn issue_edit_replace_blocked_on_list() {
    let mut h = TestHarness::new();
    h.start_server();
    let blocker1_id = h.ns2_stdout(&["issue", "new", "--title", "Blocker One", "--body", "b"]);
    let blocker2_id = h.ns2_stdout(&["issue", "new", "--title", "Blocker Two", "--body", "b"]);
    let blocked_id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Blocked Issue",
        "--body",
        "b",
        "--blocked-on",
        &blocker1_id,
        "--blocked-on",
        &blocker2_id,
    ]);
    h.ns2()
        .args([
            "issue",
            "edit",
            "--id",
            &blocked_id,
            "--blocked-on",
            &blocker2_id,
        ])
        .assert()
        .success();
    h.ns2()
        .args(["issue", "list", "--blocked-on", &blocker1_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("No issues found."));
    let out = h.ns2_stdout(&["issue", "list", "--blocked-on", &blocker2_id]);
    assert!(
        out.contains("Blocked Issue"),
        "issue should still be blocked by blocker2"
    );
}

#[test]
fn issue_edit_clear_blocked_on() {
    let mut h = TestHarness::new();
    h.start_server();
    let source_id = h.ns2_stdout(&["issue", "new", "--title", "Blocker", "--body", "b"]);
    let dependent_id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Blocked",
        "--body",
        "b",
        "--blocked-on",
        &source_id,
    ]);
    h.ns2()
        .args(["issue", "edit", "--id", &dependent_id, "--blocked-on"])
        .assert()
        .success();
    h.ns2()
        .args(["issue", "list", "--blocked-on", &source_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("No issues found."));
}

// Flow 16 — Issue error cases

#[test]
fn issue_start_without_assignee_fails() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&["issue", "new", "--title", "No Assignee", "--body", "b"]);
    h.ns2()
        .args(["issue", "start", "--id", &id])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no assignee").or(predicate::str::contains("No assignee")),
        );
}

#[test]
fn issue_start_already_completed_fails() {
    let mut h = TestHarness::new();
    h.start_server();
    write_agent(&h, "swe");
    let id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Done Issue",
        "--body",
        "b",
        "--assignee",
        "swe",
    ]);
    h.ns2()
        .args(["issue", "complete", "--id", &id, "--comment", "done"])
        .assert()
        .success();
    h.ns2()
        .args(["issue", "start", "--id", &id])
        .assert()
        .failure();
}

#[test]
fn issue_complete_already_completed_fails() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&["issue", "new", "--title", "Completable", "--body", "b"]);
    h.ns2()
        .args(["issue", "complete", "--id", &id, "--comment", "first"])
        .assert()
        .success();
    h.ns2()
        .args(["issue", "complete", "--id", &id, "--comment", "second"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("terminal state")
                .or(predicate::str::contains("already completed")),
        );
}

#[test]
fn issue_operations_on_nonexistent_id_fail() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2()
        .args(["issue", "start", "--id", "zzzz"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("issue not found: zzzz").or(predicate::str::contains("zzzz")),
        );
}

#[test]
fn issue_comment_on_nonexistent_id_fails() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2()
        .args(["issue", "comment", "--id", "zzzz", "--body", "hello"])
        .assert()
        .failure();
}

#[test]
fn issue_new_with_nonexistent_assignee_fails() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2()
        .args([
            "issue",
            "new",
            "--title",
            "Bad Assignee",
            "--body",
            "b",
            "--assignee",
            "nonexistent",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("not found in .ns2/agents")
                .or(predicate::str::contains("nonexistent")),
        );
}

#[test]
fn issue_complete_requires_comment() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&["issue", "new", "--title", "Needs Comment", "--body", "b"]);
    h.ns2()
        .args(["issue", "complete", "--id", &id])
        .assert()
        .failure();
}

// Flow 24 — Issue branch field

#[test]
fn issue_new_auto_generates_branch() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&["issue", "new", "--title", "My New Feature", "--body", "b"]);
    let json = h.http_get(&format!("/issues/{id}"));
    assert!(json.contains("\"branch\""), "branch field should exist");
    let branch = extract_branch(&json);
    assert!(
        branch
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "branch '{branch}' should only contain lowercase alphanumeric and dashes"
    );
    assert!(
        branch.contains('-'),
        "branch '{branch}' should contain a dash separator"
    );
}

#[test]
fn issue_child_inherits_parent_branch_field() {
    let mut h = TestHarness::new();
    h.start_server();
    let parent_id = h.ns2_stdout(&["issue", "new", "--title", "Parent Feature", "--body", "b"]);
    let child_id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Child Feature",
        "--body",
        "b",
        "--parent",
        &parent_id,
    ]);
    let parent_json = h.http_get(&format!("/issues/{parent_id}"));
    let child_json = h.http_get(&format!("/issues/{child_id}"));
    let parent_branch = extract_branch(&parent_json);
    let child_branch = extract_branch(&child_json);
    assert_eq!(
        parent_branch, child_branch,
        "child branch should equal parent branch"
    );
}

#[test]
fn issue_explicit_branch_stored_as_provided() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Hotfix",
        "--body",
        "b",
        "--branch",
        "hotfix/my-fix",
    ]);
    let json = h.http_get(&format!("/issues/{id}"));
    assert!(json.contains("\"branch\""), "branch field should exist");
    assert!(
        json.contains("\"hotfix/my-fix\""),
        "branch should be stored as-is"
    );
}

#[test]
fn issue_list_shows_branch_column() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2_stdout(&["issue", "new", "--title", "Branch Test", "--body", "b"]);
    let out = h.ns2_stdout(&["issue", "list"]);
    assert!(
        out.contains("branch"),
        "list output should contain branch column"
    );
}

#[test]
fn issue_edit_branch_updates_value() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&["issue", "new", "--title", "Editable Branch", "--body", "b"]);
    h.ns2()
        .args(["issue", "edit", "--id", &id, "--branch", "new/branch"])
        .assert()
        .success();
    let json = h.http_get(&format!("/issues/{id}"));
    assert!(
        json.contains("\"new/branch\""),
        "branch should be updated to new/branch"
    );
}

fn extract_branch(json: &str) -> String {
    let key = "\"branch\":\"";
    let start = json.find(key).expect("branch key not found in JSON") + key.len();
    let end = json[start..]
        .find('"')
        .expect("branch value not terminated")
        + start;
    json[start..end].to_string()
}

// Flow: issue show

#[test]
fn issue_show_prints_title_body_and_status() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Show Me",
        "--body",
        "Full description here",
    ]);
    let out = h.ns2_stdout(&["issue", "show", "--id", &id]);
    assert!(out.contains("Show Me"), "show should contain title");
    assert!(
        out.contains("Full description here"),
        "show should contain body"
    );
    assert!(out.contains("open"), "show should contain status");
}

#[test]
fn issue_show_json_outputs_valid_json_with_expected_fields() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "JSON Issue",
        "--body",
        "json body",
    ]);
    let out = h.ns2_stdout(&["issue", "show", "--id", &id, "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&out).expect("--json output should be valid JSON");
    assert_eq!(parsed["id"].as_str().unwrap(), id, "JSON id should match");
    assert_eq!(parsed["title"].as_str().unwrap(), "JSON Issue");
    assert_eq!(parsed["body"].as_str().unwrap(), "json body");
    assert_eq!(parsed["status"].as_str().unwrap(), "open");
}

#[test]
fn issue_show_missing_id_exits_nonzero() {
    let mut h = TestHarness::new();
    h.start_server();
    h.ns2()
        .args(["issue", "show", "--id", "zzzz"])
        .assert()
        .failure();
}

#[test]
fn issue_show_displays_comments() {
    let mut h = TestHarness::new();
    h.start_server();
    let id = h.ns2_stdout(&["issue", "new", "--title", "With Comment", "--body", "b"]);
    h.ns2()
        .args(["issue", "comment", "--id", &id, "--body", "a comment here"])
        .assert()
        .success();
    let out = h.ns2_stdout(&["issue", "show", "--id", &id]);
    assert!(
        out.contains("a comment here"),
        "show should display comments"
    );
}
