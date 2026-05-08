pub async fn run_list() {
    let git_root = workspace::git_root_sync().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    let config = workspace::read_ns2_config(&git_root);
    let entries = workspace::list_worktrees(&git_root, &config.worktree_base).await;
    if entries.is_empty() {
        println!("No worktrees found.");
    } else {
        println!("{:<40}  path", "branch");
        for entry in &entries {
            println!("{:<40}  {}", entry.branch, entry.path.display());
        }
    }
}

pub async fn run_create(branch: String) {
    let git_root = workspace::git_root_sync().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    let config = workspace::read_ns2_config(&git_root);
    let worktree_path = config.worktree_base.join(&branch);
    if let Some(path) = workspace::ensure_worktree(&git_root, &worktree_path, &branch).await {
        eprintln!(
            "Created worktree for branch {} at {}",
            branch,
            path.display()
        );
    } else {
        eprintln!("Error: failed to create worktree for branch {branch}");
        std::process::exit(1);
    }
}

pub async fn run_delete(branch: String, force: bool) {
    let git_root = workspace::git_root_sync().unwrap_or_else(|| {
        eprintln!("Error: not inside a git repository");
        std::process::exit(1);
    });
    let config = workspace::read_ns2_config(&git_root);
    match workspace::delete_worktree(&git_root, &config.worktree_base, &branch, force).await {
        Ok(_path) => {
            eprintln!("Deleted worktree for branch {branch}");
        }
        Err(workspace::DeleteWorktreeError::NotFound(_)) => {
            eprintln!("Error: no worktree found for branch {branch}");
            std::process::exit(1);
        }
        Err(workspace::DeleteWorktreeError::UnmergedCommits(_)) => {
            eprintln!("Error: branch {branch} has unmerged commits. Use --force to delete anyway.");
            std::process::exit(1);
        }
        Err(workspace::DeleteWorktreeError::GitFailed(msg)) => {
            eprintln!("Error: {msg}");
            std::process::exit(1);
        }
    }
}
