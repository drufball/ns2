use std::collections::HashMap;
use serde_json::json;
use types::{Issue, IssueStatus};
use crate::client::{handle_connection_error, print_error_response};
use crate::render::{format_issue_show, print_issue_row, render_issue_tree, IssueTreeNode};

pub(crate) fn issue_is_terminal(status: &IssueStatus) -> bool {
    matches!(status, IssueStatus::Completed | IssueStatus::Failed)
}

/// Check whether every node in the issue tree (roots AND all descendants) is terminal.
/// Used by `issue wait` to decide when to stop polling.
pub(crate) fn all_nodes_terminal(roots: &[IssueTreeNode]) -> bool {
    fn node_terminal(node: &IssueTreeNode) -> bool {
        issue_is_terminal(&node.issue.status) && node.children.iter().all(node_terminal)
    }
    roots.iter().all(node_terminal)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_new(
    server: &str,
    title: String,
    body: String,
    assignee: Option<String>,
    parent: Option<String>,
    blocked_on: Vec<String>,
    start: bool,
    branch: Option<String>,
) {
    let client = reqwest::Client::new();

    if start && assignee.is_none() {
        eprintln!("Error: --start requires --assignee (start needs an agent to run)");
        std::process::exit(1);
    }
    if let Some(ref a) = assignee {
        if let Some(dir) = agents::agents_dir() {
            if agents::load_agent(&dir, a).is_none() {
                eprintln!("Error: agent type '{a}' not found in .ns2/agents/");
                std::process::exit(1);
            }
        }
    }
    let url = format!("{}/issues", server);
    let req_body = json!({
        "title": title,
        "body": body,
        "assignee": assignee,
        "parent_id": parent,
        "blocked_on": blocked_on,
        "branch": branch,
    });
    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let issue: Issue = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    eprintln!("Created issue: {} ({})", issue.title, issue.id);
    println!("{}", issue.id);

    if start {
        let start_url = format!("{}/issues/{}/start", server, issue.id);
        let start_resp = client.post(&start_url).send().await.unwrap_or_else(|e| {
            handle_connection_error(&e);
        });
        if !start_resp.status().is_success() {
            if start_resp.status() == reqwest::StatusCode::NOT_FOUND {
                eprintln!("Error: issue not found: {}", issue.id);
                std::process::exit(1);
            }
            print_error_response(start_resp).await;
        }
        let started: Issue = start_resp.json().await.unwrap_or_else(|e| {
            eprintln!("Error parsing response: {e}");
            std::process::exit(1);
        });
        eprintln!("Started issue {}. Session: {}", started.id, started.session_id.map(|id| id.to_string()).unwrap_or_default());
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_edit(
    server: &str,
    id: String,
    title: Option<String>,
    body: Option<String>,
    assignee: Option<String>,
    parent: Option<String>,
    blocked_on: Option<Vec<String>>,
    branch: Option<String>,
) {
    let client = reqwest::Client::new();
    let url = format!("{}/issues/{}", server, id);
    let mut req_body = serde_json::Map::new();
    if let Some(t) = title {
        req_body.insert("title".into(), json!(t));
    }
    if let Some(b) = body {
        req_body.insert("body".into(), json!(b));
    }
    if let Some(a) = assignee {
        if a.is_empty() {
            req_body.insert("assignee".into(), json!(null));
        } else {
            if let Some(dir) = agents::agents_dir() {
                if agents::load_agent(&dir, &a).is_none() {
                    eprintln!("Error: agent type '{a}' not found in .ns2/agents/");
                    std::process::exit(1);
                }
            }
            req_body.insert("assignee".into(), json!(a));
        }
    }
    if let Some(p) = parent {
        if p.is_empty() {
            req_body.insert("parent_id".into(), json!(null));
        } else {
            req_body.insert("parent_id".into(), json!(p));
        }
    }
    if let Some(bo) = blocked_on {
        req_body.insert("blocked_on".into(), json!(bo));
    }
    if let Some(br) = branch {
        req_body.insert("branch".into(), json!(br));
    }
    let resp = client
        .patch(&url)
        .json(&serde_json::Value::Object(req_body))
        .send()
        .await
        .unwrap_or_else(|e| handle_connection_error(&e));
    if !resp.status().is_success() {
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            eprintln!("Error: issue not found: {id}");
            std::process::exit(1);
        }
        print_error_response(resp).await;
    }
    let issue: Issue = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    eprintln!("Updated issue {}.", issue.id);
}

pub(crate) async fn run_comment(server: &str, id: String, body: String, author: String) {
    let client = reqwest::Client::new();
    let url = format!("{}/issues/{}/comments", server, id);
    let req_body = json!({ "author": author, "body": body });
    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            eprintln!("Error: issue not found: {id}");
            std::process::exit(1);
        }
        print_error_response(resp).await;
    }
    eprintln!("Comment added to issue {id}.");
}

pub(crate) async fn run_start(server: &str, id: String) {
    let client = reqwest::Client::new();
    let url = format!("{}/issues/{}/start", server, id);
    let resp = client.post(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            eprintln!("Error: issue not found: {id}");
            std::process::exit(1);
        }
        print_error_response(resp).await;
    }
    let issue: Issue = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    eprintln!("Started issue {id}. Session: {}", issue.session_id.map(|id| id.to_string()).unwrap_or_default());
}

pub(crate) async fn run_complete(server: &str, id: String, comment: String) {
    let client = reqwest::Client::new();
    let url = format!("{}/issues/{}/complete", server, id);
    let req_body = json!({ "comment": comment });
    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            eprintln!("Error: issue not found: {id}");
            std::process::exit(1);
        }
        print_error_response(resp).await;
    }
    eprintln!("Issue {id} marked as completed.");
}

pub(crate) async fn run_reopen(server: &str, id: String, comment: Option<String>, start: bool) {
    let client = reqwest::Client::new();
    let url = format!("{}/issues/{}/reopen", server, id);
    let req_body = json!({ "comment": comment });
    let resp = client.post(&url).json(&req_body).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Error: issue not found: {id}");
        std::process::exit(1);
    }
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    eprintln!("Issue {id} reopened.");

    if start {
        let start_url = format!("{}/issues/{}/start", server, id);
        let start_resp = client.post(&start_url).send().await.unwrap_or_else(|e| {
            handle_connection_error(&e);
        });
        if !start_resp.status().is_success() {
            if start_resp.status() == reqwest::StatusCode::NOT_FOUND {
                eprintln!("Error: issue not found: {id}");
                std::process::exit(1);
            }
            print_error_response(start_resp).await;
        }
        let started: Issue = start_resp.json().await.unwrap_or_else(|e| {
            eprintln!("Error parsing response: {e}");
            std::process::exit(1);
        });
        eprintln!("Started issue {id}. Session: {}", started.session_id.map(|id| id.to_string()).unwrap_or_default());
    }
}

pub(crate) async fn run_list(
    server: &str,
    status: Option<String>,
    assignee: Option<String>,
    parent: Option<String>,
    blocked_on: Option<String>,
) {
    let client = reqwest::Client::new();
    let mut url = format!("{}/issues", server);
    let mut params: Vec<String> = vec![];
    if let Some(s) = &status {
        params.push(format!("status={s}"));
    }
    if let Some(a) = &assignee {
        params.push(format!("assignee={a}"));
    }
    if let Some(p) = &parent {
        params.push(format!("parent_id={p}"));
    }
    if let Some(bo) = &blocked_on {
        params.push(format!("blocked_on={bo}"));
    }
    if !params.is_empty() {
        url = format!("{url}?{}", params.join("&"));
    }
    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if !resp.status().is_success() {
        eprintln!("Error: {}", resp.status());
        std::process::exit(1);
    }
    let issues: Vec<Issue> = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    if issues.is_empty() {
        println!("No issues found.");
    } else {
        println!("{:<6}  {:<30}  {:<10}  {:<12}  {:<25}  created_at", "id", "title", "status", "assignee", "branch");
        for issue in &issues {
            print_issue_row(issue);
        }
    }
}

pub(crate) async fn run_show(server: &str, id: String, json: bool) {
    let client = reqwest::Client::new();
    let url = format!("{}/issues/{}", server, id);
    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
        handle_connection_error(&e);
    });
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Error: issue not found: {id}");
        std::process::exit(1);
    }
    if !resp.status().is_success() {
        print_error_response(resp).await;
    }
    let issue: Issue = resp.json().await.unwrap_or_else(|e| {
        eprintln!("Error parsing response: {e}");
        std::process::exit(1);
    });
    if json {
        let out = serde_json::to_string_pretty(&issue).unwrap_or_else(|e| {
            eprintln!("Error serializing to JSON: {e}");
            std::process::exit(1);
        });
        println!("{out}");
    } else {
        print!("{}", format_issue_show(&issue));
    }
}

pub(crate) async fn run_wait(server: &str, ids: Vec<String>) {
    if ids.is_empty() {
        eprintln!("Error: at least one --id is required");
        std::process::exit(1);
    }

    let client = reqwest::Client::new();

    // Helper: fetch an issue tree rooted at `id` recursively.
    async fn fetch_issue_tree(
        client: &reqwest::Client,
        server: &str,
        id: &str,
    ) -> Option<IssueTreeNode> {
        let url = format!("{}/issues/{}", server, id);
        let resp = client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let issue: types::Issue = resp.json().await.ok()?;

        // Fetch children (issues with this parent_id)
        let children_url = format!("{}/issues?parent_id={}", server, id);
        let children_resp = client.get(&children_url).send().await.ok()?;
        let child_issues: Vec<types::Issue> = if children_resp.status().is_success() {
            children_resp.json().await.unwrap_or_default()
        } else {
            vec![]
        };

        // Recursively fetch children
        let mut children = Vec::new();
        for child in &child_issues {
            // Use Box::pin to handle recursive async
            if let Some(node) = Box::pin(fetch_issue_tree(client, server, &child.id)).await {
                children.push(node);
            }
        }

        Some(IssueTreeNode { issue, snippet: None, children })
    }

    // Helper: fetch last text snippet for a running issue with a session.
    async fn fetch_snippet(
        client: &reqwest::Client,
        server: &str,
        session_id: uuid::Uuid,
    ) -> Option<String> {
        let url = format!("{}/sessions/{}/last_text", server, session_id);
        let resp = client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        v["text"].as_str().map(|s| s.to_string())
    }

    // Recursively attach snippets to running nodes.
    fn attach_snippets(
        node: &mut IssueTreeNode,
        snippets: &HashMap<uuid::Uuid, Option<String>>,
    ) {
        if node.issue.status == IssueStatus::Running {
            if let Some(session_id) = node.issue.session_id {
                if let Some(snippet_opt) = snippets.get(&session_id) {
                    node.snippet = snippet_opt.clone();
                }
            }
        }
        for child in &mut node.children {
            attach_snippets(child, snippets);
        }
    }

    // Collect all running session IDs from a tree.
    fn collect_running_sessions(node: &IssueTreeNode, out: &mut Vec<uuid::Uuid>) {
        if node.issue.status == IssueStatus::Running {
            if let Some(sid) = node.issue.session_id {
                out.push(sid);
            }
        }
        for child in &node.children {
            collect_running_sessions(child, out);
        }
    }

    use std::io::Write;

    let mut tick: usize = 0;
    let mut prev_line_count = 0usize;
    let mut any_failed = false;
    let mut final_statuses: Vec<(String, IssueStatus)> = Vec::new();

    loop {
        // Fetch trees for all root IDs
        let mut roots: Vec<IssueTreeNode> = Vec::new();
        let mut fetch_error = false;
        for id in &ids {
            match fetch_issue_tree(&client, server, id).await {
                Some(node) => roots.push(node),
                None => {
                    eprintln!("Error: issue not found: {id}");
                    fetch_error = true;
                }
            }
        }
        if fetch_error {
            std::process::exit(1);
        }

        // Collect running sessions and fetch snippets
        let mut session_ids: Vec<uuid::Uuid> = Vec::new();
        for root in &roots {
            collect_running_sessions(root, &mut session_ids);
        }
        let mut snippets: HashMap<uuid::Uuid, Option<String>> = HashMap::new();
        for sid in &session_ids {
            let snippet = fetch_snippet(&client, server, *sid).await;
            snippets.insert(*sid, snippet);
        }

        // Attach snippets to running nodes
        for root in &mut roots {
            attach_snippets(root, &snippets);
        }

        // Render the tree
        let lines = render_issue_tree(&roots, tick);

        // Clear previous frame (cursor-up + clear line for each previous line)
        let stderr = std::io::stderr();
        let mut out = stderr.lock();
        for _ in 0..prev_line_count {
            write!(out, "\x1b[1A\x1b[2K").ok();
        }

        // Write new frame
        for line in &lines {
            writeln!(out, "{}", line).ok();
        }
        out.flush().ok();
        prev_line_count = lines.len();

        // Check if all done
        if all_nodes_terminal(&roots) {
            for root in &roots {
                if root.issue.status == IssueStatus::Failed {
                    any_failed = true;
                }
                final_statuses.push((root.issue.id.clone(), root.issue.status.clone()));
            }
            break;
        }

        tick = tick.wrapping_add(1);
        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    }

    // Print stdout contract: one line per waited issue
    for (id, status) in &final_statuses {
        println!("{id}  {status}");
    }

    if any_failed {
        eprintln!("One or more issues failed.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::IssueTreeNode;
    use chrono::Utc;

    fn make_issue(status: types::IssueStatus) -> types::Issue {
        types::Issue {
            id: "i-001".to_string(),
            title: "Test".to_string(),
            body: String::new(),
            status,
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

    fn make_node(status: types::IssueStatus, children: Vec<IssueTreeNode>) -> IssueTreeNode {
        IssueTreeNode { issue: make_issue(status), snippet: None, children }
    }

    #[test]
    fn issue_is_terminal_completed() {
        assert!(issue_is_terminal(&types::IssueStatus::Completed));
    }

    #[test]
    fn issue_is_terminal_failed() {
        assert!(issue_is_terminal(&types::IssueStatus::Failed));
    }

    #[test]
    fn issue_is_not_terminal_open() {
        assert!(!issue_is_terminal(&types::IssueStatus::Open));
    }

    #[test]
    fn issue_is_not_terminal_running() {
        assert!(!issue_is_terminal(&types::IssueStatus::Running));
    }

    #[test]
    fn all_nodes_terminal_empty_roots() {
        assert!(all_nodes_terminal(&[]));
    }

    #[test]
    fn all_nodes_terminal_single_completed_leaf() {
        let roots = vec![make_node(types::IssueStatus::Completed, vec![])];
        assert!(all_nodes_terminal(&roots));
    }

    #[test]
    fn all_nodes_terminal_single_running_is_false() {
        let roots = vec![make_node(types::IssueStatus::Running, vec![])];
        assert!(!all_nodes_terminal(&roots));
    }

    #[test]
    fn all_nodes_terminal_terminal_root_with_terminal_child() {
        let child = make_node(types::IssueStatus::Completed, vec![]);
        let root = make_node(types::IssueStatus::Failed, vec![child]);
        assert!(all_nodes_terminal(&[root]));
    }

    #[test]
    fn all_nodes_terminal_terminal_root_with_running_child() {
        let child = make_node(types::IssueStatus::Running, vec![]);
        let root = make_node(types::IssueStatus::Completed, vec![child]);
        assert!(!all_nodes_terminal(&[root]));
    }

    #[test]
    fn all_nodes_terminal_multiple_roots_all_terminal() {
        let roots = vec![
            make_node(types::IssueStatus::Completed, vec![]),
            make_node(types::IssueStatus::Failed, vec![]),
        ];
        assert!(all_nodes_terminal(&roots));
    }

    #[test]
    fn all_nodes_terminal_multiple_roots_one_non_terminal() {
        let roots = vec![
            make_node(types::IssueStatus::Completed, vec![]),
            make_node(types::IssueStatus::Running, vec![]),
        ];
        assert!(!all_nodes_terminal(&roots));
    }
}
