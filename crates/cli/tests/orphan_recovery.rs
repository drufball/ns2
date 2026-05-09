mod common;

use predicates::prelude::*;

fn write_swe_agent(h: &common::TestHarness) {
    let agents_dir = h.repo_dir.path().join(".ns2/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("swe.md"),
        "---\nname: swe\ndescription: Software engineer\n---\n\nYou are a software engineer.\n",
    )
    .unwrap();
}

// ─── Orphan recovery tests ────────────────────────────────────────────────────

#[test]
fn orphan_session_marked_failed_on_restart() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let uuid = h.ns2_stdout(&["session", "new", "--message", "hello"]);
    h.ns2_stdout(&["session", "wait", "--id", &uuid]);

    h.http_patch(
        &format!("/sessions/{uuid}/status"),
        r#"{"status":"running"}"#,
    );

    h.stop_server();
    h.start_server();

    let out = h.ns2_stdout(&["session", "list", "--id", &uuid]);
    assert!(
        out.contains("failed"),
        "orphaned session must be 'failed' after restart, got: {out}"
    );
}

#[test]
fn orphan_session_with_linked_issue_posts_comment_and_fails_issue() {
    let mut h = common::TestHarness::new();
    write_swe_agent(&h);
    h.start_server();

    let issue_id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Test",
        "--body",
        "body",
        "--assignee",
        "swe",
    ]);
    h.ns2_stdout(&["issue", "set-status", "--id", &issue_id, "--status", "in_progress"]);
    h.ns2_stdout(&["issue", "wait", "--id", &issue_id]);

    h.http_patch(
        &format!("/issues/{issue_id}/status"),
        r#"{"status":"running"}"#,
    );

    let issue_json = h.http_get(&format!("/issues/{issue_id}"));
    let session_id = {
        let v: serde_json::Value = serde_json::from_str(&issue_json).unwrap();
        v["session_id"].as_str().unwrap().to_string()
    };

    h.http_patch(
        &format!("/sessions/{session_id}/status"),
        r#"{"status":"running"}"#,
    );

    h.stop_server();
    h.start_server();

    std::thread::sleep(std::time::Duration::from_millis(500));

    let body = h.http_get(&format!("/issues/{issue_id}"));
    assert!(
        body.contains(r#""status":"failed""#) || body.contains(r#""status": "failed""#),
        "issue must be 'failed' after orphan sweep, got: {body}",
    );
    assert!(
        body.contains("session lost on server restart"),
        "issue must have a comment with 'session lost on server restart', got: {body}",
    );
}

// ─── Reopen tests ─────────────────────────────────────────────────────────────

#[test]
fn reopen_failed_issue_transitions_to_open() {
    let mut h = common::TestHarness::new();
    write_swe_agent(&h);
    h.start_server();

    let issue_id = h.ns2_stdout(&[
        "issue",
        "new",
        "--title",
        "Reopenable",
        "--body",
        "body",
        "--assignee",
        "swe",
    ]);

    h.http_patch(
        &format!("/issues/{issue_id}/status"),
        r#"{"status":"failed"}"#,
    );

    h.ns2()
        .args(["issue", "reopen", "--id", &issue_id])
        .assert()
        .success()
        .stderr(predicate::str::contains("reopened"));

    let body = h.http_get(&format!("/issues/{issue_id}"));
    assert!(
        body.contains(r#""status":"open""#) || body.contains(r#""status": "open""#),
        "issue must be 'open' after reopen, got: {body}",
    );
}

#[test]
fn reopen_open_issue_fails() {
    let mut h = common::TestHarness::new();
    write_swe_agent(&h);
    h.start_server();

    let issue_id = h.ns2_stdout(&["issue", "new", "--title", "Open issue", "--body", "body"]);

    h.ns2()
        .args(["issue", "reopen", "--id", &issue_id])
        .assert()
        .failure();
}

#[test]
fn reopen_nonexistent_issue_fails() {
    let mut h = common::TestHarness::new();
    h.start_server();

    h.ns2()
        .args(["issue", "reopen", "--id", "zzzz"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("issue not found").or(predicate::str::contains("not found")),
        );
}
