use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

/// Resolve the session's working directory based on its associated issue.
///
/// - If the session has an associated issue with a non-empty `branch`:
///   reads `ns2.toml`, computes `<base>/<branch>`, ensures the worktree
///   exists, and returns the worktree path.
/// - Otherwise: returns `None` (use git root as cwd).
pub async fn resolve_session_cwd(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
) -> Option<PathBuf> {
    resolve_session_cwd_with_root(db, session_id, workspace::git_root().await).await
}

/// Inner implementation that accepts an explicit `git_root` — injectable for tests.
pub async fn resolve_session_cwd_with_root(
    db: &Arc<dyn db::Db>,
    session_id: Uuid,
    git_root: Option<PathBuf>,
) -> Option<PathBuf> {
    let issues = db.list_issues_by_session_id(session_id).await.ok()?;
    let branch = issues.into_iter().find_map(|i| {
        if i.branch.is_empty() { None } else { Some(i.branch) }
    })?;

    let git_root = git_root?;
    let config = workspace::read_ns2_config(&git_root);
    let worktree_path = config.worktree_base.join(&branch);

    workspace::ensure_worktree(&git_root, &worktree_path, &branch).await
}
