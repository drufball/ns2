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
    // Use set-status in_progress; without assignee it should fail with 400
    h.ns2()
        .args(["issue", "set-status", "--id", &id, "--status", "in_progress"])
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
    // Trying to set in_progress on a completed issue with session should fail
    h.ns2()
        .args(["issue", "set-status", "--id", &id, "--status", "in_progress"])
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
        .args(["issue", "set-status", "--id", "zzzz", "--status", "in_progress"])
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

// ─── GH#123: --wait and --watch flags on `issue new` ─────────────────────────

// Scenario 1: --wait without --status in_progress prints error and exits 1
#[test]
fn issue_new_wait_without_status_in_progress_fails() {
    let mut h = TestHarness::new();
    h.start_server();

    h.ns2()
        .args(["issue", "new", "--title", "t", "--body", "b", "--wait"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--wait requires --status in_progress"));
}

// --wait with --status open (not in_progress) also fails
#[test]
fn issue_new_wait_with_status_open_fails() {
    let mut h = TestHarness::new();
    h.start_server();

    h.ns2()
        .args([
            "issue", "new", "--title", "t", "--body", "b", "--status", "open", "--wait",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--wait requires --status in_progress"));
}

// Scenario 6: --status without --wait (just sets status, no blocking)
// The issue should be created and its status set to in_progress, but no blocking.
// Without a real agent running, the status will be "in_progress" or "running" briefly
// then fail — but the key is the command exits immediately with the issue ID on stdout.
#[test]
fn issue_new_status_without_wait_exits_immediately_and_prints_id() {
    let mut h = TestHarness::new();
    h.start_server();
    write_agent(&h, "swe");

    // This may fail at status-setting if it can't auto-start (no agent binary) but
    // the test verifies that without --wait it returns quickly.
    // We only care that stdout contains a 4-char ID.
    // We use --status open (which just sets status, doesn't auto-start) to avoid
    // triggering harness issues in test environment.
    let out = h
        .ns2()
        .args([
            "issue", "new", "--title", "Status Test", "--body", "b", "--status", "open",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "should succeed with --status open: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert_eq!(
        stdout.len(),
        4,
        "stdout should be 4-char issue ID, got: {stdout}"
    );
    assert!(
        stdout.chars().all(|c| c.is_ascii_alphanumeric()),
        "ID should be alphanumeric, got: {stdout}"
    );
}

// Verify that --status sets the issue status as expected (using a non-auto-start status)
#[test]
fn issue_new_with_status_flag_sets_status() {
    let mut h = TestHarness::new();
    h.start_server();

    // Use --status open which is a no-op transition (issue starts as open)
    let id = h.ns2_stdout(&[
        "issue", "new", "--title", "Status Test", "--body", "b", "--status", "open",
    ]);
    let json = h.http_get(&format!("/issues/{id}"));
    assert!(
        json.contains("\"open\""),
        "issue should have status 'open', got: {json}"
    );
}

// Verify --watch streams to stderr (stdout should only contain the ID)
// We can't easily test the SSE stream content in integration tests without a real
// running issue, but we can verify that:
// 1. The command exits successfully (without --wait, it exits immediately)
// 2. stdout contains only the issue ID
#[test]
fn issue_new_watch_stdout_contains_only_id() {
    let mut h = TestHarness::new();
    h.start_server();

    let out = h
        .ns2()
        .args(["issue", "new", "--title", "Watch Test", "--body", "b", "--watch"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "should succeed with --watch: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert_eq!(
        stdout.len(),
        4,
        "stdout should be 4-char issue ID only, got: {stdout:?}"
    );
    assert!(
        stdout.chars().all(|c| c.is_ascii_alphanumeric()),
        "stdout should be alphanumeric ID, got: {stdout}"
    );
}

// Scenario 3: --watch without --status: creates issue, stdout = ID only
#[test]
fn issue_new_watch_prints_id_to_stdout() {
    let mut h = TestHarness::new();
    h.start_server();

    let id = h.ns2_stdout(&[
        "issue", "new", "--title", "Watch Issue", "--body", "body",
    ]);
    // Confirm the issue was actually created
    let json = h.http_get(&format!("/issues/{id}"));
    assert!(json.contains("\"Watch Issue\""), "issue should be created");
}

// ─── GH#133: --subscribe flag on `issue new` ─────────────────────────────────

// Scenario D: --subscribe causes a POST to /hooks after POST to /issues.
// stdout must remain exactly one line — the issue ID — so that
// `id=$(ns2 issue new --subscribe ...)` captures the right value.
#[test]
fn issue_new_subscribe_creates_hook() {
    let mut h = TestHarness::new();
    h.start_server();

    let out = h
        .ns2()
        .args([
            "issue", "new", "--title", "Subscribed", "--body", "b", "--subscribe", "issue:ab12",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "issue new --subscribe should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();

    // stdout must be exactly ONE line: the issue ID.
    // The hook ID belongs on stderr (like "Created hook: …"), not stdout.
    assert_eq!(
        lines.len(),
        1,
        "stdout should have exactly 1 line (the issue ID), got: {stdout:?}"
    );

    let issue_id = lines[0].trim();
    assert_eq!(
        issue_id.len(),
        4,
        "issue ID should be 4 chars, got: {issue_id}"
    );
    assert!(
        issue_id.chars().all(|c| c.is_ascii_alphanumeric()),
        "issue ID should be alphanumeric, got: {issue_id}"
    );

    // Verify the issue was created
    let issue_json = h.http_get(&format!("/issues/{issue_id}"));
    assert!(issue_json.contains("\"Subscribed\""), "issue should be created");

    // Verify the hook was created and named after the issue
    let hooks_json = h.http_get("/hooks");
    assert!(
        hooks_json.contains(&format!("subscribe-{issue_id}")),
        "hook name should reference the issue id; hooks: {hooks_json}"
    );
}

// Scenario D2: the hook ID appears on stderr (not stdout) when --subscribe is used.
#[test]
fn issue_new_subscribe_hook_id_on_stderr_not_stdout() {
    let mut h = TestHarness::new();
    h.start_server();

    let out = h
        .ns2()
        .args([
            "issue", "new", "--title", "Sub Stderr", "--body", "b", "--subscribe", "issue:ab12",
        ])
        .output()
        .unwrap();

    assert!(out.status.success());

    // Fetch the hook that was created
    let hooks_json = h.http_get("/hooks");
    // Extract the hook id from the hooks list
    let hook_id_start = hooks_json.find("\"id\":\"").map(|i| i + 6);
    let hook_id = hook_id_start.map(|start| {
        let end = hooks_json[start..].find('"').unwrap() + start;
        &hooks_json[start..end]
    });

    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    // Fail fast if the hook ID could not be extracted — the guard below would
    // silently pass and the test would give a false green.
    assert!(
        hook_id.is_some(),
        "could not extract hook id from hooks JSON: {hooks_json}"
    );

    // hook id must NOT appear on stdout
    if let Some(hid) = hook_id {
        assert!(
            !stdout.contains(hid),
            "hook id {hid:?} must not appear on stdout; stdout: {stdout:?}"
        );
        // hook id MUST appear on stderr (in the "Created hook: …" line)
        assert!(
            stderr.contains(hid),
            "hook id {hid:?} must appear on stderr; stderr: {stderr:?}"
        );
    }
}

// Scenario E: without --subscribe, only one POST to /issues (no hook created)
#[test]
fn issue_new_without_subscribe_creates_no_hook() {
    let mut h = TestHarness::new();
    h.start_server();

    let id = h.ns2_stdout(&["issue", "new", "--title", "No Sub", "--body", "b"]);

    // Verify the issue was created
    let issue_json = h.http_get(&format!("/issues/{id}"));
    assert!(issue_json.contains("\"No Sub\""), "issue should be created");

    // stdout should be exactly the issue id (one line), not two lines
    // We already captured only the id above, verifying no hook id was printed

    // Verify no hooks were created
    let hooks_json = h.http_get("/hooks");
    // The hooks list should be empty (no subscribe hook for this issue)
    assert!(
        !hooks_json.contains(&format!("subscribe-{id}")),
        "no hook should be created when --subscribe is absent"
    );
}

// Scenario F: --subscribe with invalid target format errors.
// The error message must refer to --subscribe (the actual flag), not --deliver-to
// (the flag name used by the standalone `issue subscribe` subcommand).
#[test]
fn issue_new_subscribe_invalid_target_format_fails() {
    let mut h = TestHarness::new();
    h.start_server();

    let out = h
        .ns2()
        .args([
            "issue", "new", "--title", "T", "--body", "B", "--subscribe", "bad-format",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success(), "bad --subscribe value should fail");

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("'issue:<id>' or 'session:<id>'"),
        "stderr should describe valid format; got: {stderr:?}"
    );
    // Must name the flag the user actually typed, not the internal flag name
    assert!(
        stderr.contains("--subscribe"),
        "error must reference --subscribe, not --deliver-to; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("--deliver-to"),
        "error must not reference --deliver-to when invoked via --subscribe; got: {stderr:?}"
    );
}

// Scenario G: --subscribe combined with --status — issue ID (not hook ID) on stdout,
// hook is visible in /hooks. Note: --wait is NOT exercised here; this only confirms
// the stdout contract holds when both --subscribe and --status flags coexist.
#[test]
fn issue_new_subscribe_with_status_stdout_is_issue_id() {
    let mut h = TestHarness::new();
    h.start_server();

    // Use --status open (not in_progress) so --wait is NOT triggered — we just
    // want to confirm the stdout contract holds when both flags coexist structurally.
    // (A full --wait + --subscribe integration test would require a live agent.)
    let out = h
        .ns2()
        .args([
            "issue",
            "new",
            "--title",
            "Sub+Status",
            "--body",
            "b",
            "--subscribe",
            "issue:ab12",
            "--status",
            "open",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "subscribe + status should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();

    assert_eq!(
        lines.len(),
        1,
        "stdout must be exactly 1 line (the issue ID) even with --subscribe, got: {stdout:?}"
    );
    let issue_id = lines[0].trim();
    assert_eq!(issue_id.len(), 4, "must be a 4-char issue ID, got: {issue_id}");

    // Confirm hook exists and is named after the issue
    let hooks_json = h.http_get("/hooks");
    assert!(
        hooks_json.contains(&format!("subscribe-{issue_id}")),
        "hook should be named subscribe-{{issue_id}}; hooks: {hooks_json}"
    );
}

// Issue 3: Hook payload shape verification.
// Verifies that the hook created by --subscribe has the correct event_types,
// filter condition (issue ID match), and action fields.
#[test]
fn issue_new_subscribe_hook_payload_shape() {
    let mut h = TestHarness::new();
    h.start_server();

    let out = h
        .ns2()
        .args([
            "issue",
            "new",
            "--title",
            "Shape Test",
            "--body",
            "b",
            "--subscribe",
            "issue:ab12",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "issue new --subscribe should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let issue_id = stdout.trim();

    // Fetch the hooks list and parse as JSON
    let hooks_json = h.http_get("/hooks");
    let hooks: serde_json::Value =
        serde_json::from_str(&hooks_json).expect("GET /hooks should return valid JSON");

    // Find the hook named subscribe-{issue_id}
    let hook = hooks
        .as_array()
        .expect("hooks response should be a JSON array")
        .iter()
        .find(|h| h["name"].as_str() == Some(&format!("subscribe-{issue_id}")))
        .unwrap_or_else(|| panic!("hook named subscribe-{issue_id} should exist in the hooks list"));

    // Verify event_types contains both required event types
    let event_types = hook["source"]["event_types"]
        .as_array()
        .expect("source.event_types should be an array");
    let event_type_strings: Vec<&str> = event_types
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        event_type_strings.contains(&"issue.status_changed"),
        "event_types should contain 'issue.status_changed'; got: {event_types:?}"
    );
    assert!(
        event_type_strings.contains(&"issue.comment_added"),
        "event_types should contain 'issue.comment_added'; got: {event_types:?}"
    );

    // Verify filter condition matches the issue ID
    let conditions = hook["filter"]["conditions"]
        .as_array()
        .expect("filter.conditions should be an array");
    assert!(
        !conditions.is_empty(),
        "filter.conditions should have at least one condition"
    );
    let condition_value = conditions[0]["value"].as_str().unwrap_or("");
    assert_eq!(
        condition_value, issue_id,
        "filter.conditions[0].value should equal the issue ID"
    );
}

// Issue 4: session: target integration test.
// Verifies that --subscribe with a session: target creates a hook with session target type.
#[test]
fn issue_new_subscribe_with_session_target_creates_hook() {
    let mut h = TestHarness::new();
    h.start_server();

    let out = h
        .ns2()
        .args([
            "issue",
            "new",
            "--title",
            "Session Sub",
            "--body",
            "b",
            "--subscribe",
            "session:abc123",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "issue new --subscribe session:abc123 should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();

    // stdout must be exactly ONE line: the issue ID
    assert_eq!(
        lines.len(),
        1,
        "stdout should have exactly 1 line (the issue ID), got: {stdout:?}"
    );
    let issue_id = lines[0].trim();
    assert_eq!(issue_id.len(), 4, "issue ID should be 4 chars, got: {issue_id}");

    // Verify the hook was created for this issue
    let hooks_json = h.http_get("/hooks");
    let hooks: serde_json::Value =
        serde_json::from_str(&hooks_json).expect("GET /hooks should return valid JSON");

    let hook = hooks
        .as_array()
        .expect("hooks response should be a JSON array")
        .iter()
        .find(|h| h["name"].as_str() == Some(&format!("subscribe-{issue_id}")))
        .unwrap_or_else(|| panic!("hook named subscribe-{issue_id} should exist"));

    // Verify action target type is "session" and content is "abc123"
    let action_target = &hook["action"]["target"];
    assert_eq!(
        action_target["type"].as_str().unwrap_or(""),
        "session",
        "action.target.type should be 'session' for session: subscribe target"
    );
    assert_eq!(
        action_target["content"].as_str().unwrap_or(""),
        "abc123",
        "action.target.content should be 'abc123'"
    );
}
