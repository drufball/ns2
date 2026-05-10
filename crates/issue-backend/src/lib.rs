use async_trait::async_trait;
use std::sync::Arc;
use types::{Issue, IssueStatus};

// ── Config types ──────────────────────────────────────────────────────────────

/// Which storage back-end the server should use for issues.
#[derive(Debug, Clone, serde::Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Sqlite,
    Shell,
    GitHub,
}

/// Configuration for the shell back-end.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ShellConfig {
    pub command: String,
}

/// Top-level `[issues]` configuration block from `ns2.toml`.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct IssueBackendConfig {
    #[serde(default)]
    pub backend: BackendKind,
    pub shell: Option<ShellConfig>,
    // github config will be added in step 3
}

// ── IssueFilter ───────────────────────────────────────────────────────────────

/// Filter parameters for listing issues.
pub struct IssueFilter {
    pub status: Option<IssueStatus>,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
}

// ── IssueBackend trait ────────────────────────────────────────────────────────

/// Pluggable storage back-end for issues.
///
/// All issue CRUD operations (`create`, `get`, `list`, `save`) are routed through
/// this trait so that the `issues` service layer is decoupled from any specific
/// database technology.
#[async_trait]
pub trait IssueBackend: Send + Sync {
    /// Persist a new issue.
    async fn create(&self, issue: &Issue) -> Result<()>;
    /// Retrieve an issue by its id.
    async fn get(&self, id: &str) -> Result<Issue>;
    /// List issues, optionally filtered.
    async fn list(&self, filter: IssueFilter) -> Result<Vec<Issue>>;
    /// Persist changes to an existing issue.
    async fn save(&self, issue: &Issue) -> Result<()>;
    /// Delete an issue.  Not supported by all backends.
    async fn delete(&self, id: &str) -> Result<()>;
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Other(String),
}

// ── Factory function ──────────────────────────────────────────────────────────

/// Creates an `IssueBackend` from config.
///
/// - `Sqlite` → wraps the passed `db`.
/// - `Shell`  → creates a `ShellIssueBackend`.
/// - `GitHub` → returns an error (not yet implemented).
///
/// # Errors
///
/// Returns `Error::Other` if `backend = shell` but `[issues.shell]` config is
/// absent, or if `backend = github` (not yet implemented).
pub fn from_config(
    config: &IssueBackendConfig,
    db: Arc<dyn db::Db>,
) -> Result<Arc<dyn IssueBackend>> {
    match config.backend {
        BackendKind::Sqlite => Ok(Arc::new(SqliteIssueBackend::new(db))),
        BackendKind::Shell => {
            let cmd = config
                .shell
                .as_ref()
                .ok_or_else(|| {
                    Error::Other(
                        "[issues.shell] command required when backend = shell".into(),
                    )
                })?
                .command
                .clone();
            Ok(Arc::new(ShellIssueBackend::new(cmd)))
        }
        BackendKind::GitHub => {
            Err(Error::Other("GitHub backend not yet implemented".into()))
        }
    }
}

// ── SqliteIssueBackend ────────────────────────────────────────────────────────

/// An [`IssueBackend`] that delegates to the SQLite [`db::Db`] trait object.
pub struct SqliteIssueBackend {
    db: Arc<dyn db::Db>,
}

impl SqliteIssueBackend {
    /// Wrap an existing `Arc<dyn db::Db>`.
    #[must_use]
    pub fn new(db: Arc<dyn db::Db>) -> Self {
        Self { db }
    }
}

/// Map a `db::Error` to an `issue_backend::Error`.
fn map_db_err(e: db::Error) -> Error {
    match e {
        db::Error::NotFound => Error::NotFound,
        other => Error::Other(other.to_string()),
    }
}

#[async_trait]
impl IssueBackend for SqliteIssueBackend {
    async fn create(&self, issue: &Issue) -> Result<()> {
        self.db.create_issue(issue).await.map_err(map_db_err)
    }

    async fn get(&self, id: &str) -> Result<Issue> {
        self.db
            .get_issue(id.to_string())
            .await
            .map_err(map_db_err)
    }

    async fn list(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        self.db
            .list_issues(filter.status, filter.assignee, filter.parent_id)
            .await
            .map_err(map_db_err)
    }

    async fn save(&self, issue: &Issue) -> Result<()> {
        self.db.update_issue(issue).await.map_err(map_db_err)
    }

    async fn delete(&self, _id: &str) -> Result<()> {
        Err(Error::Other(
            "delete not supported by sqlite backend".into(),
        ))
    }
}

// ── ShellIssueBackend ─────────────────────────────────────────────────────────

/// An [`IssueBackend`] that delegates all operations to a user-provided shell
/// script via JSON on stdin/stdout.
///
/// A new child process is spawned for **each** operation.
pub struct ShellIssueBackend {
    command: String,
}

impl ShellIssueBackend {
    /// Create a new backend that will invoke `command` for every operation.
    #[must_use]
    pub fn new(command: String) -> Self {
        Self { command }
    }

    /// Spawn the script, write `request` JSON to stdin, and return the parsed
    /// response JSON.
    async fn call(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        use tokio::io::AsyncWriteExt as _;
        use tokio::process::Command;

        // Determine the program and args by splitting on whitespace (simple
        // shell-style split: first token is the program, rest are args).
        let parts: Vec<&str> = self.command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(Error::Other(
                "failed to spawn shell backend script: empty command".into(),
            ));
        }
        let (prog, args) = (parts[0], &parts[1..]);

        let input = serde_json::to_vec(&request)
            .map_err(|e| Error::Other(format!("failed to serialize request: {e}")))?;

        let mut child = Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| {
                Error::Other(format!(
                    "failed to spawn shell backend script: {e}"
                ))
            })?;

        // Write to stdin then close so the child gets EOF.
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&input).await.map_err(|e| {
                Error::Other(format!("failed to write to stdin: {e}"))
            })?;
            // stdin is dropped here, closing the pipe
        }

        let output = child.wait_with_output().await.map_err(|e| {
            Error::Other(format!("failed to wait for shell backend script: {e}"))
        })?;

        if !output.status.success() {
            return Err(Error::Other(
                "shell backend script exited with non-zero status".into(),
            ));
        }

        let response: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|e| {
                Error::Other(format!("failed to parse response JSON: {e}"))
            })?;

        Ok(response)
    }
}

/// Map a shell response JSON to a `Result<()>`.
fn check_ok(resp: &serde_json::Value) -> Result<()> {
    if resp["ok"] == true {
        return Ok(());
    }
    if resp["not_found"] == true {
        return Err(Error::NotFound);
    }
    let msg = resp["error"]
        .as_str()
        .unwrap_or("shell backend returned an error")
        .to_string();
    Err(Error::Other(msg))
}

#[async_trait]
impl IssueBackend for ShellIssueBackend {
    async fn create(&self, issue: &Issue) -> Result<()> {
        let issue_json = serde_json::to_value(issue)
            .map_err(|e| Error::Other(format!("serialization error: {e}")))?;
        let resp = self
            .call(serde_json::json!({"op": "create", "issue": issue_json}))
            .await?;
        check_ok(&resp)
    }

    async fn get(&self, id: &str) -> Result<Issue> {
        let resp = self
            .call(serde_json::json!({"op": "get", "id": id}))
            .await?;
        if check_ok(&resp).is_err() {
            return Err(if resp["not_found"] == true {
                Error::NotFound
            } else {
                let msg = resp["error"]
                    .as_str()
                    .unwrap_or("shell backend returned an error")
                    .to_string();
                Error::Other(msg)
            });
        }
        let issue: Issue = serde_json::from_value(resp["issue"].clone()).map_err(|e| {
            Error::Other(format!("failed to deserialize issue: {e}"))
        })?;
        Ok(issue)
    }

    async fn list(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let filter_json = serde_json::json!({
            "status": filter.status.map(|s| s.to_string()),
            "assignee": filter.assignee,
            "parent_id": filter.parent_id,
        });
        let resp = self
            .call(serde_json::json!({"op": "list", "filter": filter_json}))
            .await?;
        if resp["ok"] != true {
            let msg = resp["error"]
                .as_str()
                .unwrap_or("shell backend returned an error")
                .to_string();
            return Err(Error::Other(msg));
        }
        let issues: Vec<Issue> =
            serde_json::from_value(resp["issues"].clone()).map_err(|e| {
                Error::Other(format!("failed to deserialize issues: {e}"))
            })?;
        Ok(issues)
    }

    async fn save(&self, issue: &Issue) -> Result<()> {
        let issue_json = serde_json::to_value(issue)
            .map_err(|e| Error::Other(format!("serialization error: {e}")))?;
        let resp = self
            .call(serde_json::json!({"op": "save", "issue": issue_json}))
            .await?;
        check_ok(&resp)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let resp = self
            .call(serde_json::json!({"op": "delete", "id": id}))
            .await?;
        check_ok(&resp)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_issue(id: &str) -> Issue {
        Issue {
            id: id.into(),
            title: "Test issue".into(),
            body: "Details".into(),
            status: IssueStatus::Open,
            branch: String::new(),
            assignee: None,
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── SqliteIssueBackend integration tests ──────────────────────────────────

    #[tokio::test]
    async fn sqlite_backend_create_and_get() {
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        let fetched = backend.get("ab12").await.unwrap();
        assert_eq!(fetched.id, "ab12");
        assert_eq!(fetched.title, "Test issue");
    }

    #[tokio::test]
    async fn sqlite_backend_get_not_found_returns_not_found_error() {
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let result = backend.get("xxxx").await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "expected NotFound, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_backend_list_returns_created_issue() {
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        let issues = backend
            .list(IssueFilter {
                status: None,
                assignee: None,
                parent_id: None,
            })
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "ab12");
    }

    #[tokio::test]
    async fn sqlite_backend_save_updates_issue() {
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        let mut updated = backend.get("ab12").await.unwrap();
        updated.title = "Updated title".into();
        updated.status = IssueStatus::InProgress;
        updated.updated_at = Utc::now();
        backend.save(&updated).await.unwrap();

        let fetched = backend.get("ab12").await.unwrap();
        assert_eq!(fetched.title, "Updated title");
        assert_eq!(fetched.status, IssueStatus::InProgress);
    }

    #[tokio::test]
    async fn sqlite_backend_delete_returns_not_supported_error() {
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let result = backend.delete("ab12").await;
        assert!(
            matches!(result, Err(Error::Other(_))),
            "expected Other error for delete, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_backend_list_with_status_filter() {
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let open_issue = make_issue("aa11");
        backend.create(&open_issue).await.unwrap();

        let mut completed_issue = make_issue("bb22");
        completed_issue.status = IssueStatus::Completed;
        backend.create(&completed_issue).await.unwrap();

        let open_only = backend
            .list(IssueFilter {
                status: Some(IssueStatus::Open),
                assignee: None,
                parent_id: None,
            })
            .await
            .unwrap();

        assert_eq!(open_only.len(), 1);
        assert_eq!(open_only[0].id, "aa11");
    }

    // ── ShellIssueBackend tests ───────────────────────────────────────────────

    /// Helper: build a valid Issue JSON string for use in shell scripts.
    fn issue_json(id: &str) -> String {
        let issue = make_issue(id);
        serde_json::to_string(&issue).unwrap()
    }

    /// Helper: write a test shell script to a temp file and return the path.
    fn write_test_script(content: &str) -> tempfile::NamedTempFile {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        // Make executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = f.as_file().metadata().unwrap().permissions();
            perms.set_mode(0o755);
            f.as_file().set_permissions(perms).unwrap();
        }
        f
    }

    #[tokio::test]
    async fn shell_backend_create_returns_ok() {
        let script = write_test_script(
            "#!/bin/sh\necho '{\"ok\": true}'\n",
        );
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let issue = make_issue("ab12");
        let result = backend.create(&issue).await;
        assert!(result.is_ok(), "create should return Ok(()), got: {result:?}");
    }

    #[tokio::test]
    async fn shell_backend_get_returns_issue() {
        let ij = issue_json("ab12");
        let script_content = format!(
            "#!/bin/sh\necho '{{\"ok\": true, \"issue\": {ij}}}'\n"
        );
        let script = write_test_script(&script_content);
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let result = backend.get("ab12").await;
        assert!(result.is_ok(), "get should return Ok(issue), got: {result:?}");
        assert_eq!(result.unwrap().id, "ab12");
    }

    #[tokio::test]
    async fn shell_backend_list_returns_empty_vec() {
        let script = write_test_script(
            "#!/bin/sh\necho '{\"ok\": true, \"issues\": []}'\n",
        );
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let result = backend
            .list(IssueFilter {
                status: None,
                assignee: None,
                parent_id: None,
            })
            .await;
        assert!(result.is_ok(), "list should return Ok([]), got: {result:?}");
        assert!(result.unwrap().is_empty(), "list should return empty vec");
    }

    #[tokio::test]
    async fn shell_backend_save_returns_ok() {
        let script = write_test_script(
            "#!/bin/sh\necho '{\"ok\": true}'\n",
        );
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let issue = make_issue("ab12");
        let result = backend.save(&issue).await;
        assert!(result.is_ok(), "save should return Ok(()), got: {result:?}");
    }

    #[tokio::test]
    async fn shell_backend_delete_returns_ok() {
        let script = write_test_script(
            "#!/bin/sh\necho '{\"ok\": true}'\n",
        );
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let result = backend.delete("ab12").await;
        assert!(result.is_ok(), "delete should return Ok(()), got: {result:?}");
    }

    #[tokio::test]
    async fn shell_backend_not_found_response_maps_to_not_found_error() {
        let script = write_test_script(
            "#!/bin/sh\necho '{\"ok\": false, \"not_found\": true, \"error\": \"not found\"}'\n",
        );
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let result = backend.get("xxxx").await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "expected NotFound, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn shell_backend_error_response_maps_to_other_error() {
        let script = write_test_script(
            "#!/bin/sh\necho '{\"ok\": false, \"error\": \"some error\"}'\n",
        );
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let result = backend.get("ab12").await;
        assert!(
            matches!(result, Err(Error::Other(ref msg)) if msg.contains("some error")),
            "expected Other(some error), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn shell_backend_nonzero_exit_maps_to_other_error() {
        let script = write_test_script("#!/bin/sh\nexit 1\n");
        let backend = ShellIssueBackend::new(
            script.path().to_str().unwrap().to_string(),
        );
        let result = backend.delete("ab12").await;
        assert!(
            matches!(result, Err(Error::Other(ref msg)) if msg.contains("non-zero")),
            "expected Other(non-zero exit), got: {result:?}"
        );
    }

    #[tokio::test]
    async fn shell_backend_spawn_failure_maps_to_other_error() {
        let backend = ShellIssueBackend::new("/nonexistent/script.sh".to_string());
        let result = backend.delete("ab12").await;
        assert!(
            matches!(result, Err(Error::Other(ref msg)) if msg.contains("failed to spawn")),
            "expected Other(failed to spawn...), got: {result:?}"
        );
    }

    // ── IssueBackendConfig deserialisation ────────────────────────────────────

    #[test]
    fn backend_config_defaults_to_sqlite() {
        let config = IssueBackendConfig::default();
        assert_eq!(config.backend, BackendKind::Sqlite);
        assert!(config.shell.is_none());
    }

    #[test]
    fn backend_config_deserializes_shell() {
        let toml_str = r#"
backend = "shell"
[shell]
command = "/path/to/backend.sh"
"#;
        let config: IssueBackendConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backend, BackendKind::Shell);
        assert_eq!(config.shell.as_ref().unwrap().command, "/path/to/backend.sh");
    }

    #[test]
    fn backend_config_deserializes_sqlite_explicitly() {
        let toml_str = r#"backend = "sqlite""#;
        let config: IssueBackendConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backend, BackendKind::Sqlite);
    }

    #[test]
    fn backend_config_deserializes_github() {
        let toml_str = r#"backend = "github""#;
        let config: IssueBackendConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backend, BackendKind::GitHub);
    }

    // ── from_config tests ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn from_config_sqlite_creates_and_retrieves_issue() {
        let config = IssueBackendConfig::default(); // backend = Sqlite
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = from_config(&config, Arc::clone(&db)).unwrap();

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();
        let fetched = backend.get("ab12").await.unwrap();
        assert_eq!(fetched.id, "ab12");
    }

    #[test]
    fn from_config_shell_without_shell_config_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = IssueBackendConfig {
                backend: BackendKind::Shell,
                shell: None,
            };
            let (db, _) = db::connect("sqlite::memory:").await.unwrap();
            let result = from_config(&config, Arc::clone(&db));
            assert!(
                matches!(result, Err(Error::Other(ref msg)) if msg.contains("command required")),
                "expected error about missing command"
            );
        });
    }

    #[test]
    fn from_config_github_returns_not_implemented_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = IssueBackendConfig {
                backend: BackendKind::GitHub,
                shell: None,
            };
            let (db, _) = db::connect("sqlite::memory:").await.unwrap();
            let result = from_config(&config, Arc::clone(&db));
            assert!(
                matches!(result, Err(Error::Other(ref msg)) if msg.contains("not yet implemented")),
                "expected not-yet-implemented error"
            );
        });
    }

    #[tokio::test]
    async fn from_config_shell_creates_shell_backend() {
        let script = write_test_script("#!/bin/sh\necho '{\"ok\": true}'\n");
        let config = IssueBackendConfig {
            backend: BackendKind::Shell,
            shell: Some(ShellConfig {
                command: script.path().to_str().unwrap().to_string(),
            }),
        };
        let (db, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = from_config(&config, Arc::clone(&db)).unwrap();
        // Should be able to call create without error (script returns ok)
        let result = backend.create(&make_issue("ab12")).await;
        assert!(result.is_ok(), "shell backend create should succeed, got: {result:?}");
    }
}
