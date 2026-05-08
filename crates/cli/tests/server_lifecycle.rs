mod common;

use common::TestHarness;

#[test]
fn server_starts_and_health_responds_ok() {
    let mut h = TestHarness::new();
    h.start_server();
    let body = h.http_get("/health");
    assert!(
        body.contains(r#""status":"ok"#),
        "unexpected health body: {body}"
    );
}

#[test]
fn pid_file_written_and_removed() {
    let mut h = TestHarness::new();
    h.start_server();

    let pid_file = h
        .home_dir
        .path()
        .join(".ns2")
        .join(h.repo_dir.path().file_name().unwrap())
        .join(format!("server-{}.pid", h.port));

    assert!(
        pid_file.exists(),
        "pid file should exist after server start"
    );

    h.ns2()
        .args(["server", "stop", "--port", &h.port.to_string()])
        .assert()
        .success();

    assert!(
        !pid_file.exists(),
        "pid file should be removed after server stop"
    );
}

#[test]
fn server_stop_prints_pid() {
    let mut h = TestHarness::new();
    h.start_server();

    h.ns2()
        .args(["server", "stop", "--port", &h.port.to_string()])
        .assert()
        .success()
        .stderr(predicates::str::contains("Server stopped"));
}

#[test]
fn stopped_server_refuses_connections() {
    let mut h = TestHarness::new();
    h.start_server();

    h.ns2()
        .args(["server", "stop", "--port", &h.port.to_string()])
        .assert()
        .success();

    h.ns2().args(["session", "list"]).assert().failure();
}
