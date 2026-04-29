use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Deserializer};
use types::{Issue, IssueComment, IssueStatus};
use uuid::Uuid;

use crate::state::{spawn_harness_sync, AppState};
use super::Error;

// ─── Helpers ──────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) fn generate_issue_id_for_test() -> String {
    generate_issue_id()
}

fn generate_issue_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let id = Uuid::new_v4();
    let bytes = id.as_bytes();
    (0..4)
        .map(|i| ALPHABET[(bytes[i] as usize) % ALPHABET.len()] as char)
        .collect()
}

/// Convert a title string into a URL-safe slug.
/// - Lowercase all characters
/// - Replace any run of non-alphanumeric characters with a single `-`
/// - Trim leading/trailing `-`
pub(crate) fn slugify(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut result = String::new();
    let mut in_sep = false;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            result.push(ch);
            in_sep = false;
        } else if !in_sep {
            result.push('-');
            in_sep = true;
        }
    }
    result.trim_matches('-').to_string()
}

// Wraps a present JSON field (including null) in Some, leaving absent fields as None.
// Used with #[serde(default, deserialize_with = "deserialize_some")] to distinguish
// "field absent" (None) from "field explicitly null" (Some(None)).
fn deserialize_some<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

// ─── Request / Response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct CreateIssueRequest {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) assignee: Option<String>,
    pub(crate) parent_id: Option<String>,
    pub(crate) blocked_on: Option<Vec<String>>,
    pub(crate) branch: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct EditIssueRequest {
    pub(crate) title: Option<String>,
    pub(crate) body: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub(crate) assignee: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub(crate) parent_id: Option<Option<String>>,
    pub(crate) blocked_on: Option<Vec<String>>,
    pub(crate) branch: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ListIssuesQuery {
    pub(crate) status: Option<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) parent_id: Option<String>,
    pub(crate) blocked_on: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AddCommentRequest {
    pub(crate) author: String,
    pub(crate) body: String,
}

#[derive(Deserialize)]
pub(crate) struct CompleteIssueRequest {
    pub(crate) comment: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct ReopenIssueRequest {
    pub(crate) comment: Option<String>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

pub(crate) async fn create_issue(
    State(state): State<AppState>,
    Json(req): Json<CreateIssueRequest>,
) -> std::result::Result<(StatusCode, Json<Issue>), Error> {
    let now = Utc::now();
    let id = generate_issue_id();

    // Compute the branch:
    // 1. Explicit branch in request → use as-is
    // 2. parent_id provided → inherit parent's branch
    // 3. Otherwise → generate slug from "<id>-<slugified-title>"
    let branch = if let Some(b) = req.branch {
        b
    } else if let Some(ref parent_id) = req.parent_id {
        match state.db.get_issue(parent_id.clone()).await {
            Ok(parent) => parent.branch,
            Err(_) => format!("{}-{}", id, slugify(&req.title)),
        }
    } else {
        format!("{}-{}", id, slugify(&req.title))
    };

    let issue = Issue {
        id,
        title: req.title,
        body: req.body,
        status: IssueStatus::Open,
        branch,
        assignee: req.assignee,
        session_id: None,
        parent_id: req.parent_id,
        blocked_on: req.blocked_on.unwrap_or_default(),
        comments: vec![],
        created_at: now,
        updated_at: now,
    };
    state.db.create_issue(&issue).await?;
    Ok((StatusCode::CREATED, Json(issue)))
}

pub(crate) async fn get_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.db.get_issue(id).await?;
    Ok(Json(issue))
}

pub(crate) async fn list_issues(
    State(state): State<AppState>,
    Query(params): Query<ListIssuesQuery>,
) -> std::result::Result<Json<Vec<Issue>>, Error> {
    let status = params
        .status
        .as_deref()
        .map(|s| s.parse::<IssueStatus>().map_err(Error::BadRequest))
        .transpose()?;
    let mut issues = state
        .db
        .list_issues(status, params.assignee, params.parent_id)
        .await?;
    if let Some(blocked_on_filter) = &params.blocked_on {
        issues.retain(|i| i.blocked_on.contains(blocked_on_filter));
    }
    Ok(Json(issues))
}

pub(crate) async fn edit_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EditIssueRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let mut issue = state.db.get_issue(id.clone()).await?;
    if let Some(title) = req.title {
        issue.title = title;
    }
    if let Some(body) = req.body {
        issue.body = body;
    }
    if let Some(assignee_opt) = req.assignee {
        issue.assignee = assignee_opt;
    }
    if let Some(parent_opt) = req.parent_id {
        issue.parent_id = parent_opt;
    }
    if let Some(blocked_on) = req.blocked_on {
        issue.blocked_on = blocked_on;
    }
    if let Some(branch) = req.branch {
        issue.branch = branch;
    }
    issue.updated_at = Utc::now();
    state.db.update_issue(&issue).await?;
    Ok(Json(issue))
}

pub(crate) async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddCommentRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let mut issue = state.db.get_issue(id.clone()).await?;
    issue.comments.push(IssueComment {
        author: req.author,
        created_at: Utc::now(),
        body: req.body,
    });
    issue.updated_at = Utc::now();
    state.db.update_issue(&issue).await?;
    Ok(Json(issue))
}

pub(crate) async fn start_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Issue>, Error> {
    let outcome = state.issue_service.start_issue(id).await?;
    let msg_tx = spawn_harness_sync(&state, outcome.session, Some(outcome.issue.id.clone()));
    msg_tx.send(outcome.initial_message).await.ok();
    Ok(Json(outcome.issue))
}

pub(crate) async fn complete_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CompleteIssueRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.issue_service.complete_issue(id, req.comment).await?;
    Ok(Json(issue))
}

pub(crate) async fn reopen_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ReopenIssueRequest>>,
) -> std::result::Result<Json<Issue>, Error> {
    let comment = body.and_then(|Json(req)| req.comment);
    let issue = state.issue_service.reopen_issue(id, comment).await?;
    Ok(Json(issue))
}
