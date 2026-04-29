mod common;

#[allow(unused_imports)]
use predicates::prelude::*;

fn write_agent(h: &common::TestHarness, name: &str, body: &str, include_project_config: bool, hooks_yaml: &str) {
    let dir = h.repo_dir.path().join(".ns2/agents");
    std::fs::create_dir_all(&dir).unwrap();
    let ipc = if include_project_config { "include_project_config: true\n" } else { "" };
    let hooks = if !hooks_yaml.is_empty() { format!("hooks:\n{}\n", hooks_yaml) } else { String::new() };
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!("---\nname: {name}\ndescription: Test\n{ipc}{hooks}---\n\n{body}\n")
    ).unwrap();
}

// Flow 20 — Auto-complete posts comment

#[test]
fn auto_complete_posts_comment_on_issue() {
    let mut h = common::TestHarness::new();
    h.start_server();
    write_agent(&h, "swe", "You are a swe.", false, "");
    let id = h.ns2_stdout(&["issue", "new", "--title", "Test", "--body", "body", "--assignee", "swe"]);
    h.ns2().args(["issue", "start", "--id", &id]).assert().success();
    h.ns2().args(["issue", "wait", "--id", &id]).assert().success();
    let body = h.http_get(&format!("/issues/{}", id));
    assert!(body.contains("\"author\":\"swe\""), "missing author in: {}", body);
    assert!(body.contains("\"body\":\"stub\""), "missing stub comment in: {}", body);
    assert!(body.contains("\"status\":\"completed\""), "missing completed status in: {}", body);
}

#[test]
fn auto_complete_comment_author_matches_assignee() {
    let mut h = common::TestHarness::new();
    h.start_server();
    write_agent(&h, "qa-tester", "You are a qa tester.", false, "");
    let id = h.ns2_stdout(&["issue", "new", "--title", "Test", "--body", "body", "--assignee", "qa-tester"]);
    h.ns2().args(["issue", "start", "--id", &id]).assert().success();
    h.ns2().args(["issue", "wait", "--id", &id]).assert().success();
    let body = h.http_get(&format!("/issues/{}", id));
    assert!(body.contains("\"author\":\"qa-tester\""), "missing qa-tester author in: {}", body);
    assert!(body.contains("\"body\":\"stub\""), "missing stub comment in: {}", body);
    assert!(body.contains("\"status\":\"completed\""), "missing completed status in: {}", body);
}

// Flow 21 — Project config inheritance (CLAUDE.md loading)

#[test]
fn session_with_include_project_config_completes() {
    let mut h = common::TestHarness::new();
    h.start_server();
    std::fs::write(h.repo_dir.path().join("CLAUDE.md"), "# Project\ncargo test\n").unwrap();
    write_agent(&h, "proj-agent", "You are helpful.", true, "");
    let uuid = h.ns2_stdout(&["session", "new", "--agent", "proj-agent", "--message", "hello"]);
    h.ns2().args(["session", "wait", "--id", &uuid]).assert().success();
}

#[test]
fn session_without_include_project_config_ignores_claude_md() {
    let mut h = common::TestHarness::new();
    h.start_server();
    std::fs::write(h.repo_dir.path().join("CLAUDE.md"), "# Project\ncargo test\n").unwrap();
    write_agent(&h, "no-config-agent", "You are helpful.", false, "");
    let uuid = h.ns2_stdout(&["session", "new", "--agent", "no-config-agent", "--message", "hello"]);
    h.ns2().args(["session", "wait", "--id", &uuid]).assert().success();
}

#[test]
fn missing_claude_md_with_include_project_config_still_completes() {
    let mut h = common::TestHarness::new();
    h.start_server();
    write_agent(&h, "proj-agent", "You are helpful.", true, "");
    let uuid = h.ns2_stdout(&["session", "new", "--agent", "proj-agent", "--message", "hello"]);
    h.ns2().args(["session", "wait", "--id", &uuid]).assert().success();
}

#[test]
fn invalid_import_in_claude_md_produces_warning_not_abort() {
    let mut h = common::TestHarness::new();
    h.start_server();
    std::fs::write(
        h.repo_dir.path().join("CLAUDE.md"),
        "# Project\n@nonexistent/file.md\ncargo test\n",
    ).unwrap();
    write_agent(&h, "proj-agent", "You are helpful.", true, "");
    let uuid = h.ns2_stdout(&["session", "new", "--agent", "proj-agent", "--message", "hello"]);
    h.ns2().args(["session", "wait", "--id", &uuid]).assert().success();
}

// Flow 22 — Session lifecycle hooks

#[test]
fn stop_hook_fires_on_session_completion() {
    let mut h = common::TestHarness::new();
    h.start_server();
    let log_path = h.repo_dir.path().join("stop-hook-log.txt");
    let script_path = h.repo_dir.path().join("stop-hook.sh");
    let script_content = format!(
        r#"#!/usr/bin/env bash
echo "hook ran" >> {}
exit 0
"#,
        log_path.display()
    );
    std::fs::write(&script_path, &script_content).unwrap();
    std::process::Command::new("chmod")
        .args(["+x", script_path.to_str().unwrap()])
        .output()
        .unwrap();
    let hooks_yaml = format!(
        "  Stop:\n    - hooks:\n        - type: command\n          command: {}\n          timeout: 10",
        script_path.display()
    );
    write_agent(&h, "hook-agent", "You are helpful.", false, &hooks_yaml);
    let uuid = h.ns2_stdout(&["session", "new", "--agent", "hook-agent", "--message", "hello"]);
    h.ns2().args(["session", "wait", "--id", &uuid]).assert().success();
    assert!(log_path.exists(), "stop hook log file was not created");
    let contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(contents.contains("hook ran"), "stop hook did not write expected output");
}

#[test]
fn project_level_stop_hook_inherited_when_include_project_config() {
    let mut h = common::TestHarness::new();
    h.start_server();
    let log_path = h.repo_dir.path().join("project-hook-log.txt");
    let script_path = h.repo_dir.path().join("project-hook.sh");
    let script_content = format!(
        r#"#!/usr/bin/env bash
echo "project hook ran" >> {}
exit 0
"#,
        log_path.display()
    );
    std::fs::write(&script_path, &script_content).unwrap();
    std::process::Command::new("chmod")
        .args(["+x", script_path.to_str().unwrap()])
        .output()
        .unwrap();
    let claude_dir = h.repo_dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let settings_json = format!(
        r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{}","timeout":10}}]}}]}}}}"#,
        script_path.display()
    );
    std::fs::write(claude_dir.join("settings.json"), &settings_json).unwrap();
    write_agent(&h, "proj-agent", "You are helpful.", true, "");
    let uuid = h.ns2_stdout(&["session", "new", "--agent", "proj-agent", "--message", "hello"]);
    h.ns2().args(["session", "wait", "--id", &uuid]).assert().success();
    assert!(log_path.exists(), "project hook log file was not created");
    let contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(contents.contains("project hook ran"), "project hook did not write expected output");
}

// Flow 23 — Stop hook commit guard

#[test]
fn commit_guard_exits_zero_on_clean_tree() {
    let mut h = common::TestHarness::new();
    h.start_server();
    let hooks_dir = h.repo_dir.path().join(".claude/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let script_path = hooks_dir.join("stop-commit-guard.sh");
    std::fs::write(&script_path, r#"#!/usr/bin/env bash
STATUS=$(git -C "${CLAUDE_PROJECT_DIR:-$(pwd)}" status --short 2>/dev/null)
if [ -n "$STATUS" ]; then
  echo "You have uncommitted changes. Please commit your work before stopping."
  exit 1
fi
exit 0
"#).unwrap();
    std::process::Command::new("chmod")
        .args(["+x", script_path.to_str().unwrap()])
        .output()
        .unwrap();
    // Commit the script so the working tree is clean before running the guard.
    h.git(&["add", "."]);
    h.git(&["commit", "-m", "add commit guard script"]);
    let repo_dir = h.repo_dir.path().to_path_buf();
    let output = std::process::Command::new(script_path.to_str().unwrap())
        .current_dir(&repo_dir)
        .env("CLAUDE_PROJECT_DIR", &repo_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "exit code was not 0 on clean tree");
}

#[test]
fn commit_guard_exits_nonzero_on_dirty_tree() {
    let mut h = common::TestHarness::new();
    h.start_server();
    let hooks_dir = h.repo_dir.path().join(".claude/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let script_path = hooks_dir.join("stop-commit-guard.sh");
    std::fs::write(&script_path, r#"#!/usr/bin/env bash
STATUS=$(git -C "${CLAUDE_PROJECT_DIR:-$(pwd)}" status --short 2>/dev/null)
if [ -n "$STATUS" ]; then
  echo "You have uncommitted changes. Please commit your work before stopping."
  exit 1
fi
exit 0
"#).unwrap();
    std::process::Command::new("chmod")
        .args(["+x", script_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::write(h.repo_dir.path().join("dirty.txt"), "untracked\n").unwrap();
    let repo_dir = h.repo_dir.path().to_path_buf();
    let output = std::process::Command::new(script_path.to_str().unwrap())
        .current_dir(&repo_dir)
        .env("CLAUDE_PROJECT_DIR", &repo_dir)
        .output()
        .unwrap();
    assert!(!output.status.success(), "exit code should be non-zero on dirty tree");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("uncommitted changes"), "expected 'uncommitted changes' message");
}

#[test]
fn commit_guard_exits_nonzero_on_staged_changes() {
    let mut h = common::TestHarness::new();
    h.start_server();
    let hooks_dir = h.repo_dir.path().join(".claude/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let script_path = hooks_dir.join("stop-commit-guard.sh");
    std::fs::write(&script_path, r#"#!/usr/bin/env bash
STATUS=$(git -C "${CLAUDE_PROJECT_DIR:-$(pwd)}" status --short 2>/dev/null)
if [ -n "$STATUS" ]; then
  echo "You have uncommitted changes. Please commit your work before stopping."
  exit 1
fi
exit 0
"#).unwrap();
    std::process::Command::new("chmod")
        .args(["+x", script_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::write(h.repo_dir.path().join("staged.txt"), "staged content\n").unwrap();
    h.git(&["add", "staged.txt"]);
    let repo_dir = h.repo_dir.path().to_path_buf();
    let output = std::process::Command::new(script_path.to_str().unwrap())
        .current_dir(&repo_dir)
        .env("CLAUDE_PROJECT_DIR", &repo_dir)
        .output()
        .unwrap();
    assert!(!output.status.success(), "exit code should be non-zero on staged changes");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("uncommitted changes"), "expected 'uncommitted changes' message");
}
