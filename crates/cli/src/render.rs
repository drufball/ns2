use std::path::PathBuf;
use types::{Issue, SessionStatus, ContentBlock, IssueStatus};
use events::SessionEvent;

// ────────────────────────────────────────────────────────────────────────────
// Spinner / animated progress helpers

/// Braille spinner frames for animated "running" indicator.
pub(crate) const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Return the spinner character for the given tick index.
pub(crate) fn spinner_char(tick: usize) -> char {
    SPINNER_FRAMES[tick % SPINNER_FRAMES.len()]
}

/// Truncate a string to at most `max_chars` Unicode characters.
pub(crate) fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}…", truncated.trim_end())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Issue table formatting

pub(crate) fn format_issue_row(issue: &Issue) -> String {
    format!(
        "{:<6}  {:<30}  {:<10}  {:<12}  {:<25}  {}",
        issue.id,
        if issue.title.chars().count() > 30 {
            issue.title.chars().take(30).collect::<String>()
        } else {
            issue.title.clone()
        },
        issue.status.to_string(),
        issue.assignee.as_deref().unwrap_or("-"),
        if issue.branch.chars().count() > 25 {
            issue.branch.chars().take(25).collect::<String>()
        } else {
            issue.branch.clone()
        },
        issue.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
    )
}

pub(crate) fn print_issue_row(issue: &Issue) {
    println!("{}", format_issue_row(issue));
}

pub(crate) fn format_issue_show(issue: &Issue) -> String {
    let mut out = String::new();
    out.push_str(&format!("id:         {}\n", issue.id));
    out.push_str(&format!("title:      {}\n", issue.title));
    out.push_str(&format!("status:     {}\n", issue.status));
    out.push_str(&format!("assignee:   {}\n", issue.assignee.as_deref().unwrap_or("-")));
    out.push_str(&format!("branch:     {}\n", issue.branch));
    if let Some(pid) = &issue.parent_id {
        out.push_str(&format!("parent:     {pid}\n"));
    }
    if !issue.blocked_on.is_empty() {
        out.push_str(&format!("blocked-on: {}\n", issue.blocked_on.join(", ")));
    }
    out.push_str(&format!("created:    {}\n", issue.created_at.format("%Y-%m-%d %H:%M:%S UTC")));
    out.push_str(&format!("updated:    {}\n", issue.updated_at.format("%Y-%m-%d %H:%M:%S UTC")));
    out.push_str("\nbody:\n");
    out.push_str(&issue.body);
    out.push('\n');
    if !issue.comments.is_empty() {
        out.push_str("\ncomments:\n");
        for comment in &issue.comments {
            out.push_str(&format!(
                "  [{}] {}: {}\n",
                comment.created_at.format("%Y-%m-%d %H:%M UTC"),
                comment.author,
                comment.body,
            ));
        }
    }
    out
}

// ────────────────────────────────────────────────────────────────────────────
// Session event formatting

pub(crate) fn format_session_event(event: &SessionEvent) -> Option<String> {
    use events::SessionEvent::*;
    match event {
        TurnStarted { turn } => Some(format!("[turn {}]\n", turn.id)),
        ContentBlockDelta {
            delta: types::ContentBlockDelta::TextDelta { text },
            ..
        } => Some(text.clone()),
        ContentBlockDelta {
            delta: types::ContentBlockDelta::InputJsonDelta { .. },
            ..
        } => None,
        ContentBlockDone { block, .. } => match block {
            ContentBlock::Text { .. } => Some("\n".to_string()),
            ContentBlock::ToolUse { name, input, .. } => {
                Some(format!("[tool: {}({})]\n", name, input))
            }
            ContentBlock::ToolResult { content, .. } => {
                Some(format!("[result: {}]\n", content))
            }
        },
        TurnDone { .. } => None,
        Done => Some("[done]\n".to_string()),
        Error { message } => Some(format!("[error] {message}\n")),
        ToolUseStart { .. } | ToolUseDone { .. } => None,
    }
}

pub(crate) fn print_session_event(event: &SessionEvent, to_stderr: bool) {
    use std::io::Write;
    use events::SessionEvent::*;
    match event {
        // Text deltas stream without a newline; flush so the terminal shows them immediately.
        ContentBlockDelta {
            delta: types::ContentBlockDelta::TextDelta { .. },
            ..
        } => {
            if let Some(text) = format_session_event(event) {
                if to_stderr {
                    eprint!("{text}");
                    std::io::stderr().flush().ok();
                } else {
                    print!("{text}");
                    std::io::stdout().flush().ok();
                }
            }
        }
        // Errors always go to stderr.
        Error { message } => eprintln!("[error] {message}"),
        // Everything else: print the formatted string (which already includes a newline).
        _ => {
            if let Some(output) = format_session_event(event) {
                if to_stderr {
                    eprint!("{output}");
                } else {
                    print!("{output}");
                }
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// SSE frame parsing

pub(crate) fn parse_sse_frames(buffer: &mut String, new_data: &str) -> Vec<String> {
    buffer.push_str(new_data);
    let mut frames = Vec::new();
    while let Some(pos) = buffer.find("\n\n") {
        let frame = buffer[..pos].to_string();
        *buffer = buffer[pos + 2..].to_string();
        frames.push(frame);
    }
    frames
}

// ────────────────────────────────────────────────────────────────────────────
// Spec sync formatting

pub(crate) fn format_sync_error(spec_path: &str, stale: &[PathBuf]) -> String {
    let mut out = format!("[error] spec {spec_path} has stale files:\n");
    for f in stale {
        out.push_str(&format!("  {}\n", f.display()));
    }
    out
}

pub(crate) fn format_sync_warning(spec_path: &str, stale: &[PathBuf]) -> String {
    let mut out = format!("[warning] spec {spec_path} has stale files:\n");
    for f in stale {
        out.push_str(&format!("  {}\n", f.display()));
    }
    out
}

// ────────────────────────────────────────────────────────────────────────────
// Issue tree rendering

/// A node in the issue tree for rendering.
pub(crate) struct IssueTreeNode {
    pub(crate) issue: types::Issue,
    pub(crate) snippet: Option<String>,
    pub(crate) children: Vec<IssueTreeNode>,
}

/// Return the symbol and status label for an issue status.
pub(crate) fn issue_status_symbol(status: &IssueStatus, tick: usize) -> (String, &'static str) {
    match status {
        IssueStatus::Running => (spinner_char(tick).to_string(), "running"),
        IssueStatus::Completed => ("✔".to_string(), "completed"),
        IssueStatus::Failed => ("✗".to_string(), "failed"),
        IssueStatus::Open => ("●".to_string(), "open"),
        IssueStatus::Cancelled => ("⊘".to_string(), "cancelled"),
    }
}

/// Render a single tree line for one issue node.
///
/// - `prefix` is the indentation/connector string (e.g., "├── ", "└── ", "│   ├── ").
/// - `tick` is the spinner frame counter.
/// - `is_root` controls whether to include the snippet.
pub(crate) fn render_tree_line(node: &IssueTreeNode, prefix: &str, tick: usize, is_root: bool) -> String {
    let (sym, status_label) = issue_status_symbol(&node.issue.status, tick);
    let id = &node.issue.id;

    let title_part = truncate_str(&node.issue.title, 30);

    if is_root {
        let snippet_part = if let Some(ref snippet) = node.snippet {
            // Sanitize: replace newlines/carriage-returns with spaces so the
            // line-count assumption used for ANSI cursor-up redraw is not broken.
            let clean: String = snippet.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
            let s = truncate_str(&clean, 30);
            if s.is_empty() {
                String::new()
            } else {
                format!(": {s}")
            }
        } else {
            String::new()
        };
        format!("{prefix}[{id}] {title_part}{snippet_part}  {sym} {status_label}")
    } else {
        format!("{prefix}[{id}] {title_part}  {sym} {status_label}")
    }
}

/// Render the full issue tree to a vector of lines.
pub(crate) fn render_issue_tree(roots: &[IssueTreeNode], tick: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (ri, root) in roots.iter().enumerate() {
        let is_last_root = ri + 1 == roots.len();
        let root_prefix = if roots.len() > 1 {
            if !is_last_root { "├── " } else { "└── " }
        } else {
            ""
        };
        lines.push(render_tree_line(root, root_prefix, tick, true));
        // When there are multiple roots, children must be indented relative to the
        // root's own connector so the tree lines up correctly:
        //   ├── Root A          <- root_prefix = "├── "
        //   │   ├── Child 1     <- parent_indent = "│   "
        //   │   └── Child 2
        //   └── Root B          <- root_prefix = "└── "
        //       └── Child 3     <- parent_indent = "    "
        let child_indent = if roots.len() > 1 {
            if !is_last_root { "│   " } else { "    " }
        } else {
            ""
        };
        render_children(&root.children, child_indent, tick, &mut lines);
    }
    lines
}

fn render_children(children: &[IssueTreeNode], parent_indent: &str, tick: usize, lines: &mut Vec<String>) {
    for (i, child) in children.iter().enumerate() {
        let is_last = i + 1 == children.len();
        let connector = if is_last { "└── " } else { "├── " };
        let prefix = format!("{parent_indent}{connector}");
        lines.push(render_tree_line(child, &prefix, tick, false));

        // Recurse into grandchildren
        let child_indent = if is_last {
            format!("{parent_indent}    ")
        } else {
            format!("{parent_indent}│   ")
        };
        render_children(&child.children, &child_indent, tick, lines);
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Session wait progress rendering

/// Return `(symbol, label)` for the given session status at a tick.
/// Running sessions get an animated braille spinner; terminal sessions get a static symbol.
pub(crate) fn session_status_symbol(status: &SessionStatus, tick: usize) -> (String, &'static str) {
    match status {
        SessionStatus::Running => (spinner_char(tick).to_string(), "running"),
        SessionStatus::Created => (spinner_char(tick).to_string(), "created"),
        SessionStatus::Completed => ("✔".to_string(), "completed"),
        SessionStatus::Failed => ("✗".to_string(), "failed"),
        SessionStatus::Cancelled => ("●".to_string(), "cancelled"),
    }
}

/// Render one progress line for a session.
///
/// Format: `[<id-prefix>] <name>: <snippet>  <sym> <status>`
///
/// - `id` is the full session UUID string; the first 8 chars are shown.
/// - `name` is the session name (empty string renders as `-`).
/// - `snippet` is an optional last-content text snippet (truncated to 40 chars).
pub(crate) fn render_session_line(
    id: &str,
    name: &str,
    snippet: Option<&str>,
    status: &SessionStatus,
    tick: usize,
) -> String {
    let id_prefix = &id[..id.len().min(8)];
    let display_name = if name.is_empty() { "-" } else { name };
    let (sym, label) = session_status_symbol(status, tick);
    let snippet_part = match snippet {
        Some(s) if !s.trim().is_empty() => {
            let trimmed = s.trim().replace(['\n', '\r'], " ");
            format!("{}  ", truncate_str(&trimmed, 40))
        }
        _ => String::new(),
    };
    format!("[{id_prefix}] {display_name}: {snippet_part}{sym} {label}")
}
