use super::common::TestHarness;
use predicates::prelude::*;

#[test]
fn session_list_fails_when_server_down() {
    let h = TestHarness::new();
    h.ns2()
        .args(["session", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("server is not running"));
}

#[test]
fn session_new_fails_when_server_down() {
    let h = TestHarness::new();
    h.ns2()
        .args(["session", "new", "--message", "hello"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("server is not running"));
}

#[test]
fn session_tail_fails_when_server_down() {
    let h = TestHarness::new();
    h.ns2()
        .args([
            "session",
            "tail",
            "--id",
            "00000000-0000-0000-0000-000000000000",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("server is not running"));
}

#[test]
fn session_send_fails_when_server_down() {
    let h = TestHarness::new();
    h.ns2()
        .args([
            "session",
            "send",
            "--id",
            "00000000-0000-0000-0000-000000000000",
            "--message",
            "hi",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("server is not running"));
}

#[test]
fn error_message_suggests_fix() {
    let h = TestHarness::new();
    h.ns2()
        .args(["session", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ns2 server start"));
}
