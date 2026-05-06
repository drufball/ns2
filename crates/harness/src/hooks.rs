use agents::{AgentHooks, HookCommand, HookEntry};
use regex::Regex;
use std::path::Path;
use uuid::Uuid;

/// Run a single hook command with JSON on stdin.
/// Returns `(exit_code, stdout, stderr)`.
/// Kills the process and returns `exit_code=1` on timeout.
/// When `cwd` is `Some`, the subprocess is started with that working directory
/// so hooks run inside the agent's worktree rather than the server's cwd.
pub async fn run_hook(cmd: &HookCommand, stdin_json: &str, cwd: Option<&Path>) -> (i32, String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;
    use tokio::time::{sleep, Duration};

    let mut builder = Command::new("sh");
    builder
        .arg("-c")
        .arg(&cmd.command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = cwd {
        builder.current_dir(dir);
    }
    let mut child = match builder.spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("failed to spawn hook command '{}': {e}", cmd.command);
            return (1, String::new(), format!("failed to spawn: {e}"));
        }
    };

    // Write stdin and close the pipe
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_json.as_bytes()).await;
    }

    // Take stdout/stderr handles for reading
    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();

    // Read stdout and stderr concurrently with a timeout on the entire operation
    let duration = Duration::from_secs(cmd.timeout);
    tokio::select! {
        result = async {
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();
            if let Some(ref mut h) = stdout_handle {
                let _ = h.read_to_end(&mut stdout_buf).await;
            }
            if let Some(ref mut h) = stderr_handle {
                let _ = h.read_to_end(&mut stderr_buf).await;
            }
            let status = child.wait().await;
            (status, stdout_buf, stderr_buf)
        } => {
            let (status, stdout_buf, stderr_buf) = result;
            let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(1);
            let stdout = String::from_utf8_lossy(&stdout_buf).into_owned();
            let stderr = String::from_utf8_lossy(&stderr_buf).into_owned();
            (exit_code, stdout, stderr)
        }
        () = sleep(duration) => {
            tracing::warn!("hook command '{}' timed out after {}s", cmd.command, cmd.timeout);
            let _ = child.kill().await;
            (1, String::new(), format!("hook timed out after {}s", cmd.timeout))
        }
    }
}

/// Find all `HookEntry`s whose matcher regex matches `tool_name`.
pub fn matching_hook_entries<'a>(entries: &'a [HookEntry], tool_name: &str) -> Vec<&'a HookEntry> {
    entries
        .iter()
        .filter(|e| {
            e.matcher
                .as_deref()
                .is_some_and(|pat| Regex::new(pat).is_ok_and(|re| re.is_match(tool_name)))
        })
        .collect()
}

/// Run `PreToolUse` hooks for `tool_name`.
/// Returns `Some(blocked_message)` if any hook exits non-zero (tool should be skipped),
/// or `None` if all hooks pass.
/// `cwd` is forwarded to the hook subprocess so it runs in the session's worktree.
pub async fn run_pre_tool_use_hooks(
    hooks: &AgentHooks,
    tool_name: &str,
    tool_input: &serde_json::Value,
    cwd: Option<&Path>,
) -> Option<String> {
    let stdin = serde_json::json!({
        "tool_name": tool_name,
        "tool_input": tool_input,
    });
    let stdin_str = stdin.to_string();

    for entry in matching_hook_entries(&hooks.pre_tool_use, tool_name) {
        for cmd in &entry.hooks {
            let (exit_code, _stdout, stderr) = run_hook(cmd, &stdin_str, cwd).await;
            if exit_code != 0 {
                return Some(if stderr.is_empty() {
                    format!("Hook blocked tool '{tool_name}' (exit {exit_code})")
                } else {
                    stderr
                });
            }
        }
    }
    None
}

/// Run `PostToolUse` hooks for `tool_name`. Exit code is always ignored.
/// `cwd` is forwarded to the hook subprocess so it runs in the session's worktree.
pub async fn run_post_tool_use_hooks(
    hooks: &AgentHooks,
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_result: &str,
    cwd: Option<&Path>,
) {
    let stdin = serde_json::json!({
        "tool_name": tool_name,
        "tool_input": tool_input,
        "tool_result": tool_result,
    });
    let stdin_str = stdin.to_string();

    for entry in matching_hook_entries(&hooks.post_tool_use, tool_name) {
        for cmd in &entry.hooks {
            run_hook(cmd, &stdin_str, cwd).await;
        }
    }
}

/// Run Stop hooks.
/// Returns `Some(injected_message)` if any hook exits non-zero (the message should be
/// injected into the conversation to continue the loop), or `None` to allow completion.
///
/// Exit code 127 (command not found) is treated as a configuration error and fails
/// open — the hook is skipped and the session is allowed to complete normally.
/// This prevents an infinite loop when a hook script cannot be found (e.g. because
/// `$CLAUDE_PROJECT_DIR` is unset and the path cannot be resolved).
///
/// `cwd` sets the working directory for the hook subprocess so it runs inside the
/// session's worktree rather than the server process's cwd.
pub async fn run_stop_hooks(hooks: &AgentHooks, session_id: Uuid, cwd: Option<&Path>) -> Option<String> {
    let stdin = serde_json::json!({ "session_id": session_id.to_string() });
    let stdin_str = stdin.to_string();

    for entry in &hooks.stop {
        for cmd in &entry.hooks {
            let (exit_code, stdout, _stderr) = run_hook(cmd, &stdin_str, cwd).await;
            if exit_code != 0 {
                // Exit 127 = command not found: configuration error, not a deliberate
                // block. Fail open so the agent is not trapped in an infinite loop.
                if exit_code == 127 {
                    tracing::warn!(
                        "stop hook command not found (exit 127): '{}' — failing open",
                        cmd.command
                    );
                    continue;
                }
                return Some(if stdout.is_empty() {
                    format!("Stop hook exited with code {exit_code}")
                } else {
                    stdout
                });
            }
        }
    }
    None
}
