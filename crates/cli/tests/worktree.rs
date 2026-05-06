mod common;

use common::TestHarness;

fn write_agent(h: &TestHarness, name: &str) {
    let dir = h.repo_dir.path().join(".ns2/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!("---\nname: {name}\ndescription: Test\n---\n\nYou are a test agent.\n"),
    )
    .unwrap();
}

fn extract_json_str(json: &str, key: &str) -> String {
    let pattern = format!("\"{key}\":\"");
    let start = json.find(&pattern).expect("key not found") + pattern.len();
    let end = json[start..].find('"').expect("closing quote not found") + start;
    json[start..end].to_string()
}

#[test]
fn worktree_list_empty_when_none_exist() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("No worktrees found."));
}

#[test]
fn worktree_create_creates_directory() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-alpha"])
        .assert()
        .success();

    let wt_path = h.worktree_base().join("feature").join("wt-alpha");
    assert!(wt_path.is_dir(), "worktree directory should exist");
    assert!(wt_path.join(".git").is_file(), "worktree .git should be a file");
}

#[test]
fn worktree_list_shows_created_worktrees() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-one"])
        .assert()
        .success();
    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-two"])
        .assert()
        .success();

    let out = h.ns2_stdout(&["worktree", "list"]);
    assert!(out.contains("feature/wt-one"), "list should show feature/wt-one");
    assert!(out.contains("feature/wt-two"), "list should show feature/wt-two");
}

#[test]
fn worktree_create_idempotent() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-alpha"])
        .assert()
        .success();
    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-alpha"])
        .assert()
        .success();
}

#[test]
fn worktree_delete_merged_branch() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-beta"])
        .assert()
        .success();

    let wt_path = h.worktree_base().join("feature").join("wt-beta");
    assert!(wt_path.is_dir());

    h.ns2()
        .args(["worktree", "delete", "--branch", "feature/wt-beta"])
        .assert()
        .success();

    assert!(!wt_path.exists(), "worktree directory should be gone after delete");
}

#[test]
fn worktree_delete_nonexistent_fails() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "delete", "--branch", "feature/no-such"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no worktree found"));
}

#[test]
fn worktree_delete_unmerged_fails() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-alpha"])
        .assert()
        .success();

    let wt_path = h.worktree_base().join("feature").join("wt-alpha");
    std::fs::write(wt_path.join("new-file.txt"), "content\n").unwrap();
    std::process::Command::new("git")
        .args(["-C", wt_path.to_str().unwrap(), "add", "."])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", wt_path.to_str().unwrap(), "commit", "-m", "unmerged commit"])
        .env("HOME", h.home_dir.path())
        .output()
        .unwrap();

    h.ns2()
        .args(["worktree", "delete", "--branch", "feature/wt-alpha"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unmerged"));
}

#[test]
fn worktree_delete_force_deletes_unmerged() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();

    h.ns2()
        .args(["worktree", "create", "--branch", "feature/wt-alpha"])
        .assert()
        .success();

    let wt_path = h.worktree_base().join("feature").join("wt-alpha");
    std::fs::write(wt_path.join("new-file.txt"), "content\n").unwrap();
    std::process::Command::new("git")
        .args(["-C", wt_path.to_str().unwrap(), "add", "."])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", wt_path.to_str().unwrap(), "commit", "-m", "unmerged commit"])
        .env("HOME", h.home_dir.path())
        .output()
        .unwrap();

    h.ns2()
        .args(["worktree", "delete", "--branch", "feature/wt-alpha", "--force"])
        .assert()
        .success();

    assert!(!wt_path.exists(), "worktree directory should be gone after force delete");
}

#[test]
fn issue_start_creates_worktree_for_issue_with_branch() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();
    write_agent(&h, "swe");

    let id = h.ns2_stdout(&[
        "issue", "new",
        "--title", "Add dashboard",
        "--body", "body",
        "--assignee", "swe",
    ]);

    let json = h.http_get(&format!("/issues/{id}"));
    let branch = extract_json_str(&json, "branch");

    h.ns2().args(["issue", "start", "--id", &id]).assert().success();
    h.ns2().args(["issue", "wait", "--id", &id]).assert().success();

    let branch_path = h.worktree_base().join(&branch);
    assert!(branch_path.is_dir(), "worktree directory should exist at {}", branch_path.display());
    assert!(
        branch_path.join(".git").is_file(),
        "worktree .git should be a file at {}",
        branch_path.join(".git").display()
    );
}

#[test]
fn issue_start_reuses_existing_worktree() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();
    write_agent(&h, "swe");

    let id_a = h.ns2_stdout(&[
        "issue", "new",
        "--title", "Issue A",
        "--body", "body",
        "--assignee", "swe",
        "--branch", "feature/shared",
    ]);
    h.ns2().args(["issue", "start", "--id", &id_a]).assert().success();
    h.ns2().args(["issue", "wait", "--id", &id_a]).assert().success();

    let id_b = h.ns2_stdout(&[
        "issue", "new",
        "--title", "Issue B",
        "--body", "body",
        "--assignee", "swe",
        "--branch", "feature/shared",
    ]);
    h.ns2().args(["issue", "start", "--id", &id_b]).assert().success();
    h.ns2().args(["issue", "wait", "--id", &id_b]).assert().success();

    let wt_path = h.worktree_base().join("feature").join("shared");
    assert!(wt_path.is_dir(), "worktree should exist");

    let git_list = std::process::Command::new("git")
        .args(["-C", h.repo_dir.path().to_str().unwrap(), "worktree", "list"])
        .env("HOME", h.home_dir.path())
        .output()
        .unwrap();
    let git_list_out = String::from_utf8_lossy(&git_list.stdout);
    let count = git_list_out.lines().filter(|l| l.contains("feature/shared")).count();
    assert!(count <= 1, "git worktree list should not have duplicate entries for feature/shared");
}

#[test]
fn worktree_not_deleted_after_session_completes() {
    let mut h = TestHarness::new();
    h.start_server();
    h.setup_origin();
    write_agent(&h, "swe");

    let id = h.ns2_stdout(&[
        "issue", "new",
        "--title", "Persist worktree",
        "--body", "body",
        "--assignee", "swe",
    ]);

    let json = h.http_get(&format!("/issues/{id}"));
    let branch = extract_json_str(&json, "branch");

    h.ns2().args(["issue", "start", "--id", &id]).assert().success();
    h.ns2().args(["issue", "wait", "--id", &id]).assert().success();

    let branch_path = h.worktree_base().join(&branch);
    assert!(
        branch_path.is_dir(),
        "worktree should still exist after session completes: {}",
        branch_path.display()
    );
}
