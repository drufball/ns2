use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Deserializer};
use types::{Issue, IssueStatus};

use crate::harness_spawn::spawn_harness_sync;
use crate::state::AppState;
use super::Error;

// ─── Helpers ──────────────────────────────────────────────────────────────────

#[cfg(test)]
pub fn generate_issue_id_for_test() -> String {
    issues::generate_issue_id()
}

#[cfg(test)]
pub fn slugify(title: &str) -> String {
    issues::slugify(title)
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
pub struct CreateIssueRequest {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) assignee: Option<String>,
    pub(crate) parent_id: Option<String>,
    pub(crate) blocked_on: Option<Vec<String>>,
    pub(crate) branch: Option<String>,
}

#[derive(Deserialize)]
pub struct EditIssueRequest {
    pub(crate) title: Option<String>,
    pub(crate) body: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    #[allow(clippy::option_option)]
    pub(crate) assignee: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some")]
    #[allow(clippy::option_option)]
    pub(crate) parent_id: Option<Option<String>>,
    pub(crate) blocked_on: Option<Vec<String>>,
    pub(crate) branch: Option<String>,
}

#[derive(Deserialize)]
pub struct ListIssuesQuery {
    pub(crate) status: Option<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) parent_id: Option<String>,
    pub(crate) blocked_on: Option<String>,
}

#[derive(Deserialize)]
pub struct AddCommentRequest {
    pub(crate) author: String,
    pub(crate) body: String,
}

#[derive(Deserialize)]
pub struct CompleteIssueRequest {
    pub(crate) comment: String,
}

#[derive(Deserialize, Default)]
pub struct ReopenIssueRequest {
    pub(crate) comment: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateIssueStatusRequest {
    pub(crate) status: String,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

pub async fn create_issue(
    State(state): State<AppState>,
    Json(req): Json<CreateIssueRequest>,
) -> std::result::Result<(StatusCode, Json<Issue>), Error> {
    let issue = state.issue_service.create_issue(issues::CreateIssueInput {
        title: req.title,
        body: req.body,
        assignee: req.assignee,
        parent_id: req.parent_id,
        blocked_on: req.blocked_on.unwrap_or_default(),
        branch: req.branch,
    }).await?;
    Ok((StatusCode::CREATED, Json(issue)))
}

pub async fn get_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.db.get_issue(id).await?;
    Ok(Json(issue))
}

pub async fn list_issues(
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

pub async fn edit_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EditIssueRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.issue_service.edit_issue(id, issues::EditIssueInput {
        title: req.title,
        body: req.body,
        assignee: req.assignee,
        parent_id: req.parent_id,
        blocked_on: req.blocked_on,
        branch: req.branch,
    }).await?;
    Ok(Json(issue))
}

pub async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddCommentRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.issue_service.add_comment(id, req.author, req.body).await?;
    Ok(Json(issue))
}

pub async fn start_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Issue>, Error> {
    let outcome = state.issue_service.start_issue(id).await?;
    let msg_tx = spawn_harness_sync(&state, outcome.session, Some(outcome.issue.id.clone()));
    msg_tx.send(outcome.initial_message).await.ok();
    Ok(Json(outcome.issue))
}

pub async fn complete_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CompleteIssueRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.issue_service.complete_issue(id, req.comment).await?;
    Ok(Json(issue))
}

pub async fn reopen_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ReopenIssueRequest>>,
) -> std::result::Result<Json<Issue>, Error> {
    let comment = body.and_then(|Json(req)| req.comment);
    let issue = state.issue_service.reopen_issue(id, comment).await?;
    Ok(Json(issue))
}

/// POST /issues/:id/cancel — cancel a running or open issue.
///
/// Marks the issue `cancelled`. If the issue has a linked session, also drops
/// the session's msg sender (terminating the harness) and marks the session
/// `cancelled` in the DB.
pub async fn cancel_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.issue_service.cancel_issue(id).await?;

    if let Some(session_id) = issue.session_id {
        // Drop the msg sender so the harness exits cleanly.
        {
            let mut senders = state.msg_senders.lock().await;
            senders.remove(&session_id);
        }
        // Update session status to cancelled.
        let _ = state
            .db
            .update_session_status(session_id, types::SessionStatus::Cancelled)
            .await;
    }

    Ok(Json(issue))
}

pub async fn update_issue_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateIssueStatusRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let new_status = req.status.parse::<IssueStatus>().map_err(Error::BadRequest)?;
    let mut issue = state.db.get_issue(id.clone()).await?;
    issue.status = new_status;
    state.db.update_issue(&issue).await?;
    let updated = state.db.get_issue(id).await?;
    Ok(Json(updated))
}
