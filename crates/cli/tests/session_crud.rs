mod common;

use predicates::prelude::*;

// ─── Flow 55: --timeout flag ──────────────────────────────────────────────────

#[test]
fn session_wait_timeout_exits_nonzero_on_non_terminal_session() {
    let mut h = common::TestHarness::new();
    h.start_server();

    // A session without a message stays in 'created' — never finishes.
    let uuid = h.ns2_stdout(&["session", "new"]);

    h.ns2()
        .args(["session", "wait", "--id", &uuid, "--timeout", "1"])
        .assert()
        .failure();
}

// ─── Flow 02: session create / list ──────────────────────────────────────────

#[test]
fn session_new_without_message_has_created_status() {
    let mut h = common::TestHarness::new();
    h.start_server();

    h.ns2_stdout(&["session", "new"]);

    let list = h.ns2_stdout(&["session", "list"]);
    assert!(
        list.contains("created"),
        "list output must contain 'created', got: {list}"
    );
}

#[test]
fn session_new_with_name_shows_in_list() {
    let mut h = common::TestHarness::new();
    h.start_server();

    h.ns2_stdout(&["session", "new", "--name", "my-session"]);

    let list = h.ns2_stdout(&["session", "list"]);
    assert!(
        list.contains("my-session"),
        "list must contain the session name, got: {list}"
    );
}

#[test]
fn session_list_empty_shows_no_sessions() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let out = h.ns2_stdout(&["session", "list"]);
    assert_eq!(out, "No sessions found.");
}

#[test]
fn session_list_filter_by_status_running_returns_empty() {
    let mut h = common::TestHarness::new();
    h.start_server();

    h.ns2_stdout(&["session", "new"]);

    let out = h.ns2_stdout(&["session", "list", "--status", "running"]);
    assert_eq!(out, "No sessions found.");
}

#[test]
fn session_list_filter_by_id() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let uuid1 = h.ns2_stdout(&["session", "new", "--name", "first"]);
    let uuid2 = h.ns2_stdout(&["session", "new", "--name", "second"]);

    let out = h.ns2_stdout(&["session", "list", "--id", &uuid1]);
    assert!(
        out.contains(&uuid1),
        "filtered list must contain the requested id, got: {out}"
    );
    assert!(
        !out.contains(&uuid2),
        "filtered list must NOT contain the other id, got: {out}"
    );
}

// ─── Flow 17: session wait ────────────────────────────────────────────────────

#[test]
fn session_wait_completes_when_stub_finishes() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let uuid = h.ns2_stdout(&["session", "new", "--message", "hello"]);

    let out = h.ns2_stdout(&["session", "wait", "--id", &uuid]);
    assert!(
        out.contains("completed"),
        "wait output must contain 'completed', got: {out}"
    );
}

#[test]
fn session_wait_on_already_terminal_session_returns_immediately() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let uuid = h.ns2_stdout(&["session", "new", "--message", "hello"]);
    h.ns2_stdout(&["session", "wait", "--id", &uuid]);

    let out = h.ns2_stdout(&["session", "wait", "--id", &uuid]);
    assert!(
        out.contains("completed"),
        "second wait must also show 'completed', got: {out}"
    );
}

#[test]
fn session_wait_multiple_sessions() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let uuid1 = h.ns2_stdout(&["session", "new", "--message", "task one"]);
    let uuid2 = h.ns2_stdout(&["session", "new", "--message", "task two"]);

    let out = h.ns2_stdout(&["session", "wait", "--id", &uuid1, "--id", &uuid2]);
    assert!(
        out.contains("completed"),
        "wait output must contain 'completed', got: {out}"
    );
    assert!(
        out.contains(&uuid1),
        "output must include uuid1, got: {out}"
    );
    assert!(
        out.contains(&uuid2),
        "output must include uuid2, got: {out}"
    );
}

#[test]
fn session_wait_failed_session_exits_nonzero() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let uuid = h.ns2_stdout(&["session", "new", "--message", "hello"]);
    h.ns2_stdout(&["session", "wait", "--id", &uuid]);

    h.http_patch(
        &format!("/sessions/{uuid}/status"),
        r#"{"status":"failed"}"#,
    );

    h.ns2()
        .args(["session", "wait", "--id", &uuid])
        .assert()
        .failure()
        .stdout(predicate::str::contains("failed"));
}

#[test]
fn session_wait_cancelled_session_exits_zero() {
    let mut h = common::TestHarness::new();
    h.start_server();

    let uuid = h.ns2_stdout(&["session", "new", "--message", "hello"]);
    h.ns2_stdout(&["session", "wait", "--id", &uuid]);

    h.http_patch(
        &format!("/sessions/{uuid}/status"),
        r#"{"status":"cancelled"}"#,
    );

    let out = h
        .ns2()
        .args(["session", "wait", "--id", &uuid])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).unwrap();
    assert!(
        stdout.contains("cancelled"),
        "output must contain 'cancelled', got: {stdout}"
    );
}

#[test]
fn session_wait_nonexistent_id_exits_nonzero() {
    let mut h = common::TestHarness::new();
    h.start_server();

    h.ns2()
        .args([
            "session",
            "wait",
            "--id",
            "00000000-0000-0000-0000-000000000001",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("session not found").or(predicate::str::contains("not found")),
        );
}

#[test]
fn session_wait_no_ids_exits_nonzero() {
    let mut h = common::TestHarness::new();
    h.start_server();

    h.ns2().args(["session", "wait"]).assert().failure();
}
