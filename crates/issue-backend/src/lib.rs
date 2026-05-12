use async_trait::async_trait;
use std::sync::Arc;
use types::{Issue, IssueStatus};

// ── Config types ──────────────────────────────────────────────────────────────

/// Which storage back-end the server should use for issues.
#[derive(Debug, Clone, serde::Deserialize, Default, PartialEq, Eq)]
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

/// Configuration for the GitHub back-end.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GithubConfig {
    pub owner: String,
    pub repo: String,
}

/// Top-level `[issues]` configuration block from `ns2.toml`.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct IssueBackendConfig {
    #[serde(default)]
    pub backend: BackendKind,
    pub shell: Option<ShellConfig>,
    pub github: Option<GithubConfig>,
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
/// - `GitHub` → creates a `GitHubIssueBackend` (requires `GITHUB_TOKEN` env var
///   and `[issues.github]` config).
///
/// # Errors
///
/// Returns `Error::Other` if:
/// - `backend = shell` but `[issues.shell]` config is absent
/// - `backend = github` but `[issues.github]` config or `GITHUB_TOKEN` env var is missing
pub async fn from_config(
    config: &IssueBackendConfig,
    db: Arc<dyn db::Db>,
) -> Result<Arc<dyn IssueBackend>> {
    from_config_with_mapping(config, db, None).await
}

/// Like [`from_config`] but accepts an optional [`db::GitHubMappingStore`].
///
/// When `mapping` is `None` and the backend is GitHub, an in-process no-op mapping
/// store cannot be used — the caller is responsible for providing one.
///
/// For the GitHub backend, performs an initial sync after construction: fetches all
/// open issues from GitHub and populates the local mapping table for any issues
/// that carry an `ns2-id:<id>` label.
///
/// # Errors
///
/// Returns `Error::Other` if:
/// - `backend = shell` but `[issues.shell]` config is absent
/// - `backend = github` but `[issues.github]` config, `GITHUB_TOKEN` env var,
///   or `mapping` is missing
pub async fn from_config_with_mapping(
    config: &IssueBackendConfig,
    db: Arc<dyn db::Db>,
    mapping: Option<Arc<dyn db::GitHubMappingStore>>,
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
            let gh_config = config.github.as_ref().ok_or_else(|| {
                Error::Other(
                    "[issues.github] config required when backend = github".into(),
                )
            })?;
            let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
                Error::Other("GITHUB_TOKEN environment variable is required for GitHub backend".into())
            })?;
            let mapping_store = mapping.ok_or_else(|| {
                Error::Other(
                    "GitHubMappingStore is required for GitHub backend".into(),
                )
            })?;
            let backend = GitHubIssueBackend::new(
                gh_config.owner.clone(),
                gh_config.repo.clone(),
                token,
                mapping_store,
            );
            backend.initial_sync().await?;
            Ok(Arc::new(backend))
        }
    }
}

// ── SqliteIssueBackend ────────────────────────────────────────────────────────

/// An [`IssueBackend`] that delegates to the `SQLite` [`db::Db`] trait object.
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
    pub const fn new(command: String) -> Self {
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
            match stdin.write_all(&input).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    // Child exited before reading stdin — common for scripts
                    // that don't read stdin (e.g. `echo`) or exit immediately.
                    // Continue and check exit status below.
                }
                Err(e) => {
                    return Err(Error::Other(format!("failed to write to stdin: {e}")));
                }
            }
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

// ── GitHubIssueBackend ────────────────────────────────────────────────────────

/// Label prefix used to carry the ns2 ID on a GitHub issue.
const NS2_ID_LABEL_PREFIX: &str = "ns2-id:";

/// Status label prefix used by the GitHub backend.
const STATUS_LABEL_PREFIX: &str = "ns2-status:";

/// Assignee label prefix used by the GitHub backend.
const ASSIGNEE_LABEL_PREFIX: &str = "ns2-assignee:";

/// Convert an `IssueStatus` to its GitHub label string.
fn status_to_label(status: &IssueStatus) -> String {
    let s = match status {
        IssueStatus::Open => "open",
        IssueStatus::InProgress => "in_progress",
        IssueStatus::Completed => "completed",
        IssueStatus::Failed => "failed",
        IssueStatus::Waiting => "waiting",
        IssueStatus::Cancelled => "cancelled",
    };
    format!("{STATUS_LABEL_PREFIX}{s}")
}

/// Parse an `IssueStatus` from a GitHub label string, or `None` if not a status label.
fn label_to_status(label: &str) -> Option<IssueStatus> {
    let suffix = label.strip_prefix(STATUS_LABEL_PREFIX)?;
    match suffix {
        "open" => Some(IssueStatus::Open),
        "in_progress" => Some(IssueStatus::InProgress),
        "completed" => Some(IssueStatus::Completed),
        "failed" => Some(IssueStatus::Failed),
        "waiting" => Some(IssueStatus::Waiting),
        "cancelled" => Some(IssueStatus::Cancelled),
        _ => None,
    }
}

/// Extract `ns2-assignee:<name>` value from a label list.
fn label_to_assignee(label: &str) -> Option<String> {
    let name = label.strip_prefix(ASSIGNEE_LABEL_PREFIX)?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Build the full set of labels for an issue (status + optional assignee).
fn issue_labels(issue: &Issue) -> Vec<String> {
    let mut labels = vec![status_to_label(&issue.status)];
    if let Some(ref assignee) = issue.assignee {
        labels.push(format!("{ASSIGNEE_LABEL_PREFIX}{assignee}"));
    }
    labels
}

/// An [`IssueBackend`] that stores issues as GitHub Issues via the REST API.
///
/// ns2 IDs remain canonical. A `SQLite` mapping table (`github_issue_mapping`)
/// tracks `github_issue_number → ns2_id`.
///
/// Status and assignee are carried via labels:
/// - `ns2-status:<status>` for the issue status
/// - `ns2-assignee:<name>` for the assignee
pub struct GitHubIssueBackend {
    owner: String,
    repo: String,
    token: String,
    mapping: Arc<dyn db::GitHubMappingStore>,
    client: reqwest::Client,
    /// Override base URL (used in tests to point at a mock server).
    base_url: String,
}

impl GitHubIssueBackend {
    /// Create a new `GitHubIssueBackend`.
    #[must_use]
    pub fn new(
        owner: String,
        repo: String,
        token: String,
        mapping: Arc<dyn db::GitHubMappingStore>,
    ) -> Self {
        Self::with_base_url(
            owner,
            repo,
            token,
            mapping,
            "https://api.github.com".to_string(),
        )
    }

    /// Create a new `GitHubIssueBackend` with a custom base URL (for testing).
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest` client cannot be built (this should
    /// not happen under normal conditions).
    #[must_use]
    pub fn with_base_url(
        owner: String,
        repo: String,
        token: String,
        mapping: Arc<dyn db::GitHubMappingStore>,
        base_url: String,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("ns2/1.0")
            .build()
            .expect("failed to build reqwest client");
        Self {
            owner,
            repo,
            token,
            mapping,
            client,
            base_url,
        }
    }

    fn issues_url(&self) -> String {
        format!(
            "{}/repos/{}/{}/issues",
            self.base_url, self.owner, self.repo
        )
    }

    fn issue_url(&self, number: i64) -> String {
        format!(
            "{}/repos/{}/{}/issues/{}",
            self.base_url, self.owner, self.repo, number
        )
    }

    fn auth_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// Fetch all open issues from GitHub and populate the local mapping table.
    ///
    /// For each GitHub issue that carries an `ns2-id:<id>` label, ensures the
    /// mapping table has an entry.  Issues without the label are skipped — we
    /// don't auto-create ns2 IDs for existing GitHub issues we don't own.
    ///
    /// # Errors
    ///
    /// Returns `Error::Other` if the GitHub API request fails or returns a
    /// non-success status code.
    pub async fn initial_sync(&self) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/issues?state=open&per_page=100",
            self.base_url, self.owner, self.repo
        );

        let resp = self
            .auth_request(reqwest::Method::GET, &url)
            .send()
            .await
            .map_err(|e| Error::Other(format!("GitHub API request failed during initial sync: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "GitHub API error {status} during initial sync: {text}"
            )));
        }

        let gh_issues: Vec<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("Failed to parse GitHub response during initial sync: {e}")))?;

        for gh in &gh_issues {
            let Some(number) = gh["number"].as_i64() else {
                continue;
            };

            // Look for an ns2-id:<id> label.
            let labels: Vec<String> = gh["labels"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l["name"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let ns2_id = labels
                .iter()
                .find_map(|l| l.strip_prefix(NS2_ID_LABEL_PREFIX).map(String::from));

            let Some(ns2_id) = ns2_id else {
                tracing::debug!(
                    github_number = number,
                    "initial_sync: no ns2-id label on GitHub issue, skipping"
                );
                continue;
            };

            self.mapping
                .upsert_mapping(&ns2_id, number)
                .await
                .map_err(|e| Error::Other(format!("Failed to store GitHub mapping during initial sync: {e}")))?;

            tracing::debug!(
                ns2_id = %ns2_id,
                github_number = number,
                "initial_sync: populated mapping from ns2-id label"
            );
        }

        tracing::info!(
            owner = %self.owner,
            repo = %self.repo,
            "GitHubIssueBackend: initial sync complete"
        );

        Ok(())
    }
}

/// Convert a GitHub API issue JSON to a `types::Issue`.
fn github_to_issue(gh: &serde_json::Value, ns2_id: &str) -> Issue {
    use chrono::Utc;
    let title = gh["title"].as_str().unwrap_or("").to_string();
    let body = gh["body"].as_str().unwrap_or("").to_string();

    let labels: Vec<String> = gh["labels"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let status = labels
        .iter()
        .find_map(|l| label_to_status(l))
        .unwrap_or(IssueStatus::Open);

    let assignee = labels.iter().find_map(|l| label_to_assignee(l));

    let created_at = gh["created_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc));
    let updated_at = gh["updated_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc));

    Issue {
        id: ns2_id.to_string(),
        title,
        body,
        status,
        branch: String::new(),
        assignee,
        session_id: None,
        parent_id: None,
        blocked_on: vec![],
        comments: vec![],
        ancestor_ids: vec![],
        created_at,
        updated_at,
    }
}

#[async_trait]
impl IssueBackend for GitHubIssueBackend {
    async fn create(&self, issue: &Issue) -> Result<()> {
        let labels = issue_labels(issue);
        let body = serde_json::json!({
            "title": issue.title,
            "body": issue.body,
            "labels": labels,
        });

        let resp = self
            .auth_request(reqwest::Method::POST, &self.issues_url())
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Other(format!("GitHub API request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "GitHub API error {status}: {text}"
            )));
        }

        let gh: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("Failed to parse GitHub response: {e}")))?;

        let number = gh["number"]
            .as_i64()
            .ok_or_else(|| Error::Other("GitHub response missing 'number' field".into()))?;

        self.mapping
            .upsert_mapping(&issue.id, number)
            .await
            .map_err(|e| Error::Other(format!("Failed to store GitHub mapping: {e}")))?;

        tracing::debug!(
            issue_id = %issue.id,
            github_number = number,
            "GitHubIssueBackend::create: created GitHub issue"
        );

        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Issue> {
        let number = self
            .mapping
            .get_github_number(id)
            .await
            .map_err(|e| Error::Other(format!("Mapping lookup failed: {e}")))?
            .ok_or(Error::NotFound)?;

        let url = self.issue_url(number);
        let resp = self
            .auth_request(reqwest::Method::GET, &url)
            .send()
            .await
            .map_err(|e| Error::Other(format!("GitHub API request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::NotFound);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!("GitHub API error {status}: {text}")));
        }

        let gh: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("Failed to parse GitHub response: {e}")))?;

        Ok(github_to_issue(&gh, id))
    }

    async fn list(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let mut url = self.issues_url();

        // Build query params
        let mut params: Vec<(&str, String)> = vec![("state", "open".to_string())];
        if let Some(ref status) = filter.status {
            let label = status_to_label(status);
            params.push(("labels", label));
        }

        let query = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        url.push('?');
        url.push_str(&query);

        let resp = self
            .auth_request(reqwest::Method::GET, &url)
            .send()
            .await
            .map_err(|e| Error::Other(format!("GitHub API request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!("GitHub API error {status}: {text}")));
        }

        let gh_issues: Vec<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("Failed to parse GitHub response: {e}")))?;

        let mut issues = Vec::new();
        for gh in &gh_issues {
            let Some(number) = gh["number"].as_i64() else {
                continue;
            };
            // Look up ns2_id from mapping
            let ns2_id = self
                .mapping
                .get_ns2_id(number)
                .await
                .map_err(|e| Error::Other(format!("Mapping lookup failed: {e}")))?;

            let Some(ns2_id) = ns2_id else {
                tracing::debug!(
                    github_number = number,
                    "GitHubIssueBackend::list: no ns2_id for GitHub issue, skipping"
                );
                continue;
            };

            // Apply assignee filter if provided
            if let Some(ref assignee_filter) = filter.assignee {
                let labels: Vec<String> = gh["labels"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|l| l["name"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let issue_assignee = labels.iter().find_map(|l| label_to_assignee(l));
                if issue_assignee.as_deref() != Some(assignee_filter.as_str()) {
                    continue;
                }
            }

            let issue = github_to_issue(gh, &ns2_id);
            issues.push(issue);
        }

        Ok(issues)
    }

    async fn save(&self, issue: &Issue) -> Result<()> {
        let number = self
            .mapping
            .get_github_number(&issue.id)
            .await
            .map_err(|e| Error::Other(format!("Mapping lookup failed: {e}")))?
            .ok_or(Error::NotFound)?;

        let labels = issue_labels(issue);
        let body = serde_json::json!({
            "title": issue.title,
            "body": issue.body,
            "labels": labels,
        });

        let url = self.issue_url(number);
        let resp = self
            .auth_request(reqwest::Method::PATCH, &url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Other(format!("GitHub API request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::NotFound);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!("GitHub API error {status}: {text}")));
        }

        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let number = self
            .mapping
            .get_github_number(id)
            .await
            .map_err(|e| Error::Other(format!("Mapping lookup failed: {e}")))?
            .ok_or(Error::NotFound)?;

        let url = self.issue_url(number);
        let body = serde_json::json!({"state": "closed"});
        let resp = self
            .auth_request(reqwest::Method::PATCH, &url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Other(format!("GitHub API request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::NotFound);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!("GitHub API error {status}: {text}")));
        }

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::significant_drop_tightening)]
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
            ancestor_ids: vec![],
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── SqliteIssueBackend integration tests ──────────────────────────────────

    #[tokio::test]
    async fn sqlite_backend_create_and_get() {
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        let fetched = backend.get("ab12").await.unwrap();
        assert_eq!(fetched.id, "ab12");
        assert_eq!(fetched.title, "Test issue");
    }

    #[tokio::test]
    async fn sqlite_backend_get_not_found_returns_not_found_error() {
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let result = backend.get("xxxx").await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "expected NotFound, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_backend_list_returns_created_issue() {
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
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
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
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
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = SqliteIssueBackend::new(Arc::clone(&db));

        let result = backend.delete("ab12").await;
        assert!(
            matches!(result, Err(Error::Other(_))),
            "expected Other error for delete, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_backend_list_with_status_filter() {
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
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
    fn write_test_script(content: &str) -> tempfile::TempPath {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = f.as_file().metadata().unwrap().permissions();
            perms.set_mode(0o755);
            f.as_file().set_permissions(perms).unwrap();
        }
        // Close the write handle before exec so Linux doesn't return ETXTBSY.
        f.into_temp_path()
    }

    #[tokio::test]
    async fn shell_backend_create_returns_ok() {
        let script = write_test_script(
            "#!/bin/sh\necho '{\"ok\": true}'\n",
        );
        let backend = ShellIssueBackend::new(
            script.to_str().unwrap().to_string(),
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
            script.to_str().unwrap().to_string(),
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
            script.to_str().unwrap().to_string(),
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
            script.to_str().unwrap().to_string(),
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
            script.to_str().unwrap().to_string(),
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
            script.to_str().unwrap().to_string(),
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
            script.to_str().unwrap().to_string(),
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
            script.to_str().unwrap().to_string(),
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

    /// Verify that `create` sends `{"op": "create", ...}` to the script stdin.
    ///
    /// The script reads stdin, parses the JSON, and returns ok=false with an
    /// error message containing the actual `op` value it received — unless the
    /// op equals "create", in which case it returns ok=true.
    #[tokio::test]
    async fn shell_backend_create_sends_correct_op() {
        let script = write_test_script(
            r#"#!/bin/sh
input=$(cat)
op=$(echo "$input" | grep -o '"op":"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')
if [ "$op" = "create" ]; then
  echo '{"ok": true}'
else
  printf '{"ok": false, "error": "wrong op: %s"}\n' "$op"
fi
"#,
        );
        let backend =
            ShellIssueBackend::new(script.to_str().unwrap().to_string());
        let issue = make_issue("ab12");
        let result = backend.create(&issue).await;
        assert!(
            result.is_ok(),
            "create should send op=create, but got: {result:?}"
        );
    }

    /// Verify that `get` sends `{"op": "get", ...}` to the script stdin.
    #[tokio::test]
    async fn shell_backend_get_sends_correct_op() {
        let ij = issue_json("ab12");
        let script = write_test_script(&format!(
            r#"#!/bin/sh
input=$(cat)
op=$(echo "$input" | grep -o '"op":"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')
if [ "$op" = "get" ]; then
  printf '{{"ok": true, "issue": %s}}\n' '{ij}'
else
  printf '{{"ok": false, "error": "wrong op: %s"}}\n' "$op"
fi
"#
        ));
        let backend =
            ShellIssueBackend::new(script.to_str().unwrap().to_string());
        let result = backend.get("ab12").await;
        assert!(
            result.is_ok(),
            "get should send op=get, but got: {result:?}"
        );
    }

    /// Verify that `list` sends `{"op": "list", ...}` to the script stdin.
    #[tokio::test]
    async fn shell_backend_list_sends_correct_op() {
        let script = write_test_script(
            r#"#!/bin/sh
input=$(cat)
op=$(echo "$input" | grep -o '"op":"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')
if [ "$op" = "list" ]; then
  echo '{"ok": true, "issues": []}'
else
  printf '{"ok": false, "error": "wrong op: %s"}\n' "$op"
fi
"#,
        );
        let backend =
            ShellIssueBackend::new(script.to_str().unwrap().to_string());
        let result = backend
            .list(IssueFilter {
                status: None,
                assignee: None,
                parent_id: None,
            })
            .await;
        assert!(
            result.is_ok(),
            "list should send op=list, but got: {result:?}"
        );
    }

    /// Verify that `save` sends `{"op": "save", ...}` to the script stdin.
    #[tokio::test]
    async fn shell_backend_save_sends_correct_op() {
        let script = write_test_script(
            r#"#!/bin/sh
input=$(cat)
op=$(echo "$input" | grep -o '"op":"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')
if [ "$op" = "save" ]; then
  echo '{"ok": true}'
else
  printf '{"ok": false, "error": "wrong op: %s"}\n' "$op"
fi
"#,
        );
        let backend =
            ShellIssueBackend::new(script.to_str().unwrap().to_string());
        let issue = make_issue("ab12");
        let result = backend.save(&issue).await;
        assert!(
            result.is_ok(),
            "save should send op=save, but got: {result:?}"
        );
    }

    /// Verify that `delete` sends `{"op": "delete", ...}` to the script stdin.
    #[tokio::test]
    async fn shell_backend_delete_sends_correct_op() {
        let script = write_test_script(
            r#"#!/bin/sh
input=$(cat)
op=$(echo "$input" | grep -o '"op":"[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')
if [ "$op" = "delete" ]; then
  echo '{"ok": true}'
else
  printf '{"ok": false, "error": "wrong op: %s"}\n' "$op"
fi
"#,
        );
        let backend =
            ShellIssueBackend::new(script.to_str().unwrap().to_string());
        let result = backend.delete("ab12").await;
        assert!(
            result.is_ok(),
            "delete should send op=delete, but got: {result:?}"
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
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = from_config(&config, Arc::clone(&db)).await.unwrap();

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();
        let fetched = backend.get("ab12").await.unwrap();
        assert_eq!(fetched.id, "ab12");
    }

    #[tokio::test]
    async fn from_config_shell_without_shell_config_returns_error() {
        let config = IssueBackendConfig {
            backend: BackendKind::Shell,
            shell: None,
            github: None,
        };
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let result = from_config(&config, Arc::clone(&db)).await;
        assert!(
            matches!(result, Err(Error::Other(ref msg)) if msg.contains("command required")),
            "expected error about missing command"
        );
    }

    #[tokio::test]
    async fn from_config_github_without_github_config_returns_error() {
        let config = IssueBackendConfig {
            backend: BackendKind::GitHub,
            shell: None,
            github: None,
        };
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let result = from_config(&config, Arc::clone(&db)).await;
        assert!(
            matches!(result, Err(Error::Other(ref msg)) if msg.contains("[issues.github]")),
            "expected error about missing github config"
        );
    }

    #[tokio::test]
    async fn from_config_github_without_token_returns_error() {
        // Remove the GITHUB_TOKEN env var for this test
        std::env::remove_var("GITHUB_TOKEN");
        let config = IssueBackendConfig {
            backend: BackendKind::GitHub,
            shell: None,
            github: Some(GithubConfig {
                owner: "owner".into(),
                repo: "repo".into(),
            }),
        };
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let result = from_config(&config, Arc::clone(&db)).await;
        assert!(
            matches!(result, Err(Error::Other(ref msg)) if msg.contains("GITHUB_TOKEN")),
            "expected error about missing GITHUB_TOKEN"
        );
    }

    #[tokio::test]
    async fn from_config_shell_creates_shell_backend() {
        let script = write_test_script("#!/bin/sh\necho '{\"ok\": true}'\n");
        let config = IssueBackendConfig {
            backend: BackendKind::Shell,
            shell: Some(ShellConfig {
                command: script.to_str().unwrap().to_string(),
            }),
            github: None,
        };
        let (db, _, _, _) = db::connect("sqlite::memory:").await.unwrap();
        let backend = from_config(&config, Arc::clone(&db)).await.unwrap();
        // Should be able to call create without error (script returns ok)
        let result = backend.create(&make_issue("ab12")).await;
        assert!(result.is_ok(), "shell backend create should succeed, got: {result:?}");
    }

    // ── GitHubIssueBackend tests (with mockito) ───────────────────────────────

    /// Build an in-memory `GitHubMappingStore` for tests.
    async fn make_mapping_store() -> Arc<dyn db::GitHubMappingStore> {
        let (_, _, _, mapping) = db::connect("sqlite::memory:").await.unwrap();
        mapping
    }

    /// Build a `GitHubIssueBackend` pointing at a mockito server.
    fn make_github_backend(base_url: &str, mapping: Arc<dyn db::GitHubMappingStore>) -> GitHubIssueBackend {
        GitHubIssueBackend::with_base_url(
            "owner".to_string(),
            "repo".to_string(),
            "fake-token".to_string(),
            mapping,
            base_url.to_string(),
        )
    }

    fn gh_issue_json(number: i64, title: &str, status: &str, assignee: Option<&str>) -> serde_json::Value {
        let mut labels = vec![
            serde_json::json!({"name": format!("ns2-status:{status}")}),
        ];
        if let Some(a) = assignee {
            labels.push(serde_json::json!({"name": format!("ns2-assignee:{a}")}));
        }
        serde_json::json!({
            "number": number,
            "title": title,
            "body": "some body",
            "labels": labels,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
        })
    }

    #[tokio::test]
    async fn github_backend_create_stores_mapping() {
        let mut server = mockito::Server::new_async().await;
        let gh_resp = gh_issue_json(42, "Test issue", "open", None);

        let mock = server
            .mock("POST", "/repos/owner/repo/issues")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(gh_resp.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        let issue = make_issue("ab12");
        backend.create(&issue).await.unwrap();

        // Verify mapping was stored
        let number = mapping.get_github_number("ab12").await.unwrap();
        assert_eq!(number, Some(42));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn github_backend_get_returns_issue() {
        let mut server = mockito::Server::new_async().await;
        let gh_resp = gh_issue_json(42, "Test issue", "open", None);

        let mock = server
            .mock("GET", "/repos/owner/repo/issues/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_resp.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        mapping.upsert_mapping("ab12", 42).await.unwrap();
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        let issue = backend.get("ab12").await.unwrap();
        assert_eq!(issue.id, "ab12");
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.status, IssueStatus::Open);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn github_backend_get_not_found_when_no_mapping() {
        let mapping = make_mapping_store().await;
        let backend = make_github_backend("http://localhost", Arc::clone(&mapping));

        let result = backend.get("xxxx").await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "expected NotFound when no mapping, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn github_backend_get_not_found_when_github_returns_404() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/owner/repo/issues/42")
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"message": "Not Found"}"#)
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        mapping.upsert_mapping("ab12", 42).await.unwrap();
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        let result = backend.get("ab12").await;
        assert!(
            matches!(result, Err(Error::NotFound)),
            "expected NotFound from 404 response, got: {result:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn github_backend_list_returns_mapped_issues() {
        let mut server = mockito::Server::new_async().await;
        let gh_list = serde_json::json!([
            gh_issue_json(42, "Issue 1", "open", None),
            gh_issue_json(43, "Issue 2", "open", Some("agent1")),
        ]);

        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/repos/owner/repo/issues\?".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_list.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        mapping.upsert_mapping("ab12", 42).await.unwrap();
        mapping.upsert_mapping("cd34", 43).await.unwrap();
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        let issues = backend
            .list(IssueFilter {
                status: None,
                assignee: None,
                parent_id: None,
            })
            .await
            .unwrap();

        assert_eq!(issues.len(), 2);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn github_backend_list_skips_issues_without_mapping() {
        let mut server = mockito::Server::new_async().await;
        let gh_list = serde_json::json!([
            gh_issue_json(42, "Issue 1", "open", None),
            gh_issue_json(99, "Unknown issue", "open", None), // no mapping
        ]);

        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/repos/owner/repo/issues\?".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_list.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        mapping.upsert_mapping("ab12", 42).await.unwrap();
        // Note: no mapping for 99
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        let issues = backend
            .list(IssueFilter {
                status: None,
                assignee: None,
                parent_id: None,
            })
            .await
            .unwrap();

        assert_eq!(issues.len(), 1, "only the mapped issue should be returned");
        assert_eq!(issues[0].id, "ab12");
        mock.assert_async().await;
    }

    /// Verify that `list` with an assignee filter only returns issues carrying
    /// the matching `ns2-assignee:<name>` label, even when GitHub returns
    /// multiple issues (since GitHub doesn't understand ns2 assignee labels).
    #[tokio::test]
    async fn github_backend_list_with_assignee_filter() {
        let mut server = mockito::Server::new_async().await;
        // Two issues: one assigned to "agent1", one assigned to "agent2".
        let gh_list = serde_json::json!([
            gh_issue_json(42, "Issue 1", "open", Some("agent1")),
            gh_issue_json(43, "Issue 2", "open", Some("agent2")),
            gh_issue_json(44, "Issue 3", "open", None),       // no assignee
        ]);

        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/repos/owner/repo/issues\?".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_list.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        mapping.upsert_mapping("ab12", 42).await.unwrap();
        mapping.upsert_mapping("cd34", 43).await.unwrap();
        mapping.upsert_mapping("ef56", 44).await.unwrap();
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        // Filter by assignee = "agent1" — only issue 42 should come back.
        let issues = backend
            .list(IssueFilter {
                status: None,
                assignee: Some("agent1".to_string()),
                parent_id: None,
            })
            .await
            .unwrap();

        assert_eq!(
            issues.len(),
            1,
            "only issues assigned to agent1 should be returned, got {} issues",
            issues.len()
        );
        assert_eq!(issues[0].id, "ab12", "expected issue ab12 (GitHub #42)");
        assert_eq!(
            issues[0].assignee.as_deref(),
            Some("agent1"),
            "returned issue must carry the agent1 assignee"
        );

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn github_backend_save_patches_issue() {
        let mut server = mockito::Server::new_async().await;
        let gh_resp = gh_issue_json(42, "Updated title", "in_progress", Some("agent1"));

        let mock = server
            .mock("PATCH", "/repos/owner/repo/issues/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_resp.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        mapping.upsert_mapping("ab12", 42).await.unwrap();
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        let mut issue = make_issue("ab12");
        issue.title = "Updated title".into();
        issue.status = IssueStatus::InProgress;
        issue.assignee = Some("agent1".into());
        backend.save(&issue).await.unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn github_backend_delete_closes_issue() {
        let mut server = mockito::Server::new_async().await;
        let gh_resp = gh_issue_json(42, "Test", "open", None);

        let mock = server
            .mock("PATCH", "/repos/owner/repo/issues/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_resp.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        mapping.upsert_mapping("ab12", 42).await.unwrap();
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        backend.delete("ab12").await.unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn github_backend_status_label_round_trip() {
        let statuses = [
            IssueStatus::Open,
            IssueStatus::InProgress,
            IssueStatus::Completed,
            IssueStatus::Failed,
            IssueStatus::Waiting,
            IssueStatus::Cancelled,
        ];
        for status in &statuses {
            let label = status_to_label(status);
            let parsed = label_to_status(&label);
            assert_eq!(parsed, Some(status.clone()), "round-trip failed for {status:?}");
        }
    }

    #[test]
    fn github_backend_config_deserializes() {
        let toml_str = r#"
backend = "github"
[github]
owner = "drufball"
repo = "ns2"
"#;
        let config: IssueBackendConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backend, BackendKind::GitHub);
        let gh = config.github.as_ref().unwrap();
        assert_eq!(gh.owner, "drufball");
        assert_eq!(gh.repo, "ns2");
    }

    // ── initial_sync tests ─────────────────────────────────────────────────────

    /// Build a GitHub issue JSON that carries an ns2-id label.
    fn gh_issue_with_ns2_id(number: i64, ns2_id: &str) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "title": "some issue",
            "body": "body",
            "labels": [
                {"name": format!("ns2-id:{ns2_id}")},
                {"name": "ns2-status:open"},
            ],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
        })
    }

    /// Build a GitHub issue JSON that has NO ns2-id label.
    fn gh_issue_without_ns2_id(number: i64) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "title": "external issue",
            "body": "body",
            "labels": [],
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
        })
    }

    #[tokio::test]
    async fn initial_sync_populates_mapping_for_issues_with_ns2_id_label() {
        let mut server = mockito::Server::new_async().await;
        let gh_list = serde_json::json!([
            gh_issue_with_ns2_id(10, "ab12"),
            gh_issue_with_ns2_id(11, "cd34"),
        ]);

        let mock = server
            .mock("GET", "/repos/owner/repo/issues?state=open&per_page=100")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_list.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        backend.initial_sync().await.unwrap();

        let num_ab12 = mapping.get_github_number("ab12").await.unwrap();
        let num_cd34 = mapping.get_github_number("cd34").await.unwrap();
        assert_eq!(num_ab12, Some(10), "ab12 should be mapped to GH #10");
        assert_eq!(num_cd34, Some(11), "cd34 should be mapped to GH #11");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn initial_sync_skips_issues_without_ns2_id_label() {
        let mut server = mockito::Server::new_async().await;
        let gh_list = serde_json::json!([
            gh_issue_without_ns2_id(99),
        ]);

        let mock = server
            .mock("GET", "/repos/owner/repo/issues?state=open&per_page=100")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_list.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        backend.initial_sync().await.unwrap();

        // No mapping should have been created for the issue without ns2-id label.
        let ns2_id = mapping.get_ns2_id(99).await.unwrap();
        assert!(ns2_id.is_none(), "expected no mapping for issue without ns2-id label");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn initial_sync_returns_error_on_api_failure() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/repos/owner/repo/issues?state=open&per_page=100")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        let result = backend.initial_sync().await;
        assert!(
            matches!(result, Err(Error::Other(ref msg)) if msg.contains("initial sync")),
            "expected Other error mentioning initial sync, got: {result:?}"
        );

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn initial_sync_does_not_overwrite_existing_mappings() {
        let mut server = mockito::Server::new_async().await;
        let gh_list = serde_json::json!([
            gh_issue_with_ns2_id(10, "ab12"),
        ]);

        let mock = server
            .mock("GET", "/repos/owner/repo/issues?state=open&per_page=100")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(gh_list.to_string())
            .create_async()
            .await;

        let mapping = make_mapping_store().await;
        // Pre-populate with the same mapping.
        mapping.upsert_mapping("ab12", 10).await.unwrap();
        let backend = make_github_backend(&server.url(), Arc::clone(&mapping));

        // Should succeed even though the mapping already exists.
        backend.initial_sync().await.unwrap();

        let num = mapping.get_github_number("ab12").await.unwrap();
        assert_eq!(num, Some(10), "mapping should still be 10 after sync");

        mock.assert_async().await;
    }
}
