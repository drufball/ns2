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
pub(crate) fn generate_issue_id_for_test() -> String {
    issues::generate_issue_id()
}

#[cfg(test)]
pub(crate) fn slugify(title: &str) -> String {
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

pub(crate) async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddCommentRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state.issue_service.add_comment(id, req.author, req.body).await?;
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
