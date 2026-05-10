use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Deserializer};
use types::{Issue, IssueStatus, SessionStatus};

use super::Error;
use crate::state::AppState;

// ─── Helpers ──────────────────────────────────────────────────────────────────

#[cfg(test)]
pub fn generate_issue_id_for_test() -> String {
    issues::generate_issue_id()
}

#[cfg(test)]
pub fn slugify(title: &str) -> String {
    issues::slugify(title)
}

// Builds a user-facing error message when an issue cannot be set to in_progress
// due to its current status. Cancelled issues get an extra hint.
fn bad_status_error(status: &IssueStatus) -> String {
    let base = format!("cannot set in_progress on issue in {status} state");
    if *status == IssueStatus::Cancelled {
        format!("{base}; use `ns2 issue reopen --id <id>` first")
    } else {
        base
    }
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
    let issue = state
        .issue_service
        .create_issue(issues::CreateIssueInput {
            title: req.title,
            body: req.body,
            assignee: req.assignee,
            parent_id: req.parent_id,
            blocked_on: req.blocked_on.unwrap_or_default(),
            branch: req.branch,
        })
        .await?;
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
    let issue = state
        .issue_service
        .edit_issue(
            id,
            issues::EditIssueInput {
                title: req.title,
                body: req.body,
                assignee: req.assignee,
                parent_id: req.parent_id,
                blocked_on: req.blocked_on,
                branch: req.branch,
            },
        )
        .await?;
    Ok(Json(issue))
}

pub async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddCommentRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let issue = state
        .issue_service
        .add_comment(id, req.author, req.body)
        .await?;
    Ok(Json(issue))
}

/// Internal helper: start an issue by calling `issue_service.start_issue()`.
/// The global issue lifecycle subscriber will handle spawning the harness
/// when it receives the `IssueEvent::StatusChanged { to: InProgress }` event.
async fn do_start_issue(
    state: &AppState,
    id: String,
) -> std::result::Result<Json<Issue>, Error> {
    let outcome = state.issue_service.start_issue(id).await?;
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

/// PATCH /issues/:id/status — update an issue's status.
///
/// When the new status is `in_progress`, this handler:
/// 1. Checks the issue has an assignee (returns 400 if not).
/// 2. If the issue has a linked session in `Failed` state, marks that session
///    `Cancelled` and clears the `session_id` on the issue.
/// 3. If the issue is in `Waiting` state (has an existing session), directly
///    spawns the harness against the existing session (resume mode).
/// 4. Otherwise calls `issue_service.start_issue()` and spawns the harness.
/// 5. Returns the issue in `in_progress` state.
///
/// For any other status the issue's status is updated directly in the DB.
pub async fn update_issue_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateIssueStatusRequest>,
) -> std::result::Result<Json<Issue>, Error> {
    let new_status = req
        .status
        .parse::<IssueStatus>()
        .map_err(Error::BadRequest)?;

    if new_status == IssueStatus::InProgress {
        // Fetch the current issue to validate preconditions.
        let issue = state.db.get_issue(id.clone()).await?;

        // Must have an assignee.
        if issue.assignee.is_none() {
            return Err(Error::BadRequest(
                "issue has no assignee; set one with `issue edit --assignee <agent>`".into(),
            ));
        }

        // If the issue is Waiting with an existing session, delegate to
        // IssueService::resume_issue() which owns the Waiting→InProgress
        // transition and publishes the StatusChanged event so the global
        // lifecycle subscriber can resume the harness.
        if issue.status == types::IssueStatus::Waiting && issue.session_id.is_some() {
            let updated = state.issue_service.resume_issue(id).await?;
            return Ok(Json(updated));
        }
        // A Waiting issue with no session_id cannot be resumed; fall through
        // to the checks below, which will produce the appropriate error.

        // If the issue has a Failed session, cancel it and clear session_id so
        // start_issue creates a fresh one.
        if let Some(session_id) = issue.session_id {
            let session = state.db.get_session(session_id).await?;
            if session.status == SessionStatus::Failed {
                // Mark old session cancelled.
                let _ = state
                    .db
                    .update_session_status(session_id, SessionStatus::Cancelled)
                    .await;
                // Clear session_id on the issue and set to Open so start_issue accepts it.
                let mut updated_issue = issue.clone();
                updated_issue.session_id = None;
                updated_issue.status = types::IssueStatus::Open;
                state.db.update_issue(&updated_issue).await?;
            } else if issue.status != types::IssueStatus::Open {
                // Issue has a session that isn't failed (e.g. running/completed/cancelled)
                // and the issue is not Open — cannot restart.
                return Err(Error::BadRequest(bad_status_error(&issue.status)));
            }
        } else if issue.status == types::IssueStatus::Failed {
            // Issue has no session but is in Failed state — allow restart
            // (set back to Open so start_issue accepts it).
            let mut updated_issue = issue.clone();
            updated_issue.status = types::IssueStatus::Open;
            updated_issue.session_id = None;
            state.db.update_issue(&updated_issue).await?;
        } else if issue.status != types::IssueStatus::Open {
            // Issue has no session and is not Open or Failed — cannot restart.
            return Err(Error::BadRequest(bad_status_error(&issue.status)));
        }

        // Delegate to the internal start logic.
        return do_start_issue(&state, id).await;
    }

    // For all other statuses: simple field update.
    let mut issue = state.db.get_issue(id.clone()).await?;
    issue.status = new_status;
    state.db.update_issue(&issue).await?;
    let updated = state.db.get_issue(id).await?;
    Ok(Json(updated))
}
