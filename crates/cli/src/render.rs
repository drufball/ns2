use events::{IssueEvent, SessionEvent};
use std::fmt::Write as _;
use std::path::PathBuf;
use types::{ContentBlock, Issue, IssueStatus, SessionStatus};

// ────────────────────────────────────────────────────────────────────────────
// Spinner / animated progress helpers

/// Braille spinner frames for animated "running" indicator.
pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Return the spinner character for the given tick index.
pub fn spinner_char(tick: usize) -> char {
    SPINNER_FRAMES[tick % SPINNER_FRAMES.len()]
}

/// Truncate a string to at most `max_chars` Unicode characters.
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}…", truncated.trim_end())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Issue table formatting

pub fn format_issue_row(issue: &Issue) -> String {
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

pub fn print_issue_row(issue: &Issue) {
    println!("{}", format_issue_row(issue));
}

pub fn format_issue_show(issue: &Issue) -> String {
    let mut out = String::new();
    writeln!(out, "id:         {}", issue.id).ok();
    writeln!(out, "title:      {}", issue.title).ok();
    writeln!(out, "status:     {}", issue.status).ok();
    writeln!(
        out,
        "assignee:   {}",
        issue.assignee.as_deref().unwrap_or("-")
    )
    .ok();
    writeln!(out, "branch:     {}", issue.branch).ok();
    if let Some(pid) = &issue.parent_id {
        writeln!(out, "parent:     {pid}").ok();
    }
    if !issue.blocked_on.is_empty() {
        writeln!(out, "blocked-on: {}", issue.blocked_on.join(", ")).ok();
    }
    writeln!(
        out,
        "created:    {}",
        issue.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    )
    .ok();
    writeln!(
        out,
        "updated:    {}",
        issue.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
    )
    .ok();
    out.push_str("\nbody:\n");
    out.push_str(&issue.body);
    out.push('\n');
    if !issue.comments.is_empty() {
        out.push_str("\ncomments:\n");
        for comment in &issue.comments {
            writeln!(
                out,
                "  [{}] {}: {}",
                comment.created_at.format("%Y-%m-%d %H:%M UTC"),
                comment.author,
                comment.body,
            )
            .ok();
        }
    }
    out
}

// ────────────────────────────────────────────────────────────────────────────
// Session event formatting

pub fn format_session_event(event: &SessionEvent) -> Option<String> {
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
        }
        | TurnDone { .. }
        | ToolUseStart { .. }
        | ToolUseDone { .. } => None,
        ContentBlockDone { block, .. } => match block {
            ContentBlock::Text { .. } => Some("\n".to_string()),
            ContentBlock::ToolUse { name, input, .. } => Some(format!("[tool: {name}({input})]\n")),
            ContentBlock::ToolResult { content, .. } => Some(format!("[result: {content}]\n")),
        },
        Done => Some("[done]\n".to_string()),
        Stopped { status, comment } => {
            let status_str = match status {
                events::StopEventStatus::Complete => "complete",
                events::StopEventStatus::Waiting => "waiting",
            };
            Some(comment.as_ref().map_or_else(
                || format!("[stopped: {status_str}]\n"),
                |c| format!("[stopped: {status_str} — {c}]\n"),
            ))
        }
        Error { message } => Some(format!("[error] {message}\n")),
    }
}

pub fn print_session_event(event: &SessionEvent, to_stderr: bool) {
    use events::SessionEvent::*;
    use std::io::Write;
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
// Issue event rendering (for `ns2 issue watch`)

/// Render a single `IssueEvent` to a human-readable one-liner.
///
/// Format:
/// - `[created]        open  – <title>`
/// - `[status_changed] <from> → <to>`
/// - `[comment_added]  <author>: "<body>"`
pub fn format_issue_event(event: &IssueEvent) -> String {
    match event {
        IssueEvent::Created(issue) => {
            format!("[created]        {}  – {}", issue.status, issue.title)
        }
        IssueEvent::StatusChanged { from, to, .. } => {
            format!("[status_changed] {from} → {to}")
        }
        IssueEvent::CommentAdded { comment, .. } => {
            format!("[comment_added]  {}: \"{}\"", comment.author, comment.body)
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// SSE frame parsing

pub fn parse_sse_frames(buffer: &mut String, new_data: &str) -> Vec<String> {
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

pub fn format_sync_error(spec_path: &str, stale: &[PathBuf]) -> String {
    let mut out = format!("[error] spec {spec_path} has stale files:\n");
    for f in stale {
        writeln!(out, "  {}", f.display()).ok();
    }
    out
}

pub fn format_sync_warning(spec_path: &str, stale: &[PathBuf]) -> String {
    let mut out = format!("[warning] spec {spec_path} has stale files:\n");
    for f in stale {
        writeln!(out, "  {}", f.display()).ok();
    }
    out
}

// ────────────────────────────────────────────────────────────────────────────
// Issue tree rendering

/// A node in the issue tree for rendering.
pub struct IssueTreeNode {
    pub(crate) issue: types::Issue,
    pub(crate) snippet: Option<String>,
    pub(crate) children: Vec<Self>,
}

/// Return the symbol and status label for an issue status.
pub fn issue_status_symbol(status: &IssueStatus, tick: usize) -> (String, &'static str) {
    match status {
        IssueStatus::Running => (spinner_char(tick).to_string(), "running"),
        IssueStatus::Completed => ("✔".to_string(), "completed"),
        IssueStatus::Failed => ("✗".to_string(), "failed"),
        IssueStatus::Open => ("●".to_string(), "open"),
        IssueStatus::Cancelled => ("⊘".to_string(), "cancelled"),
        IssueStatus::Waiting => ("⏸".to_string(), "waiting"),
    }
}

/// Render a single tree line for one issue node.
///
/// - `prefix` is the indentation/connector string (e.g., "├── ", "└── ", "│   ├── ").
/// - `tick` is the spinner frame counter.
/// - `is_root` controls whether to include the snippet.
pub fn render_tree_line(node: &IssueTreeNode, prefix: &str, tick: usize, is_root: bool) -> String {
    let (sym, status_label) = issue_status_symbol(&node.issue.status, tick);
    let id = &node.issue.id;

    let title_part = truncate_str(&node.issue.title, 30);

    if is_root {
        let snippet_part = node.snippet.as_ref().map_or_else(String::new, |snippet| {
            // Sanitize: replace newlines/carriage-returns with spaces so the
            // line-count assumption used for ANSI cursor-up redraw is not broken.
            let clean: String = snippet
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();
            let s = truncate_str(&clean, 30);
            if s.is_empty() {
                String::new()
            } else {
                format!(": {s}")
            }
        });
        format!("{prefix}[{id}] {title_part}{snippet_part}  {sym} {status_label}")
    } else {
        format!("{prefix}[{id}] {title_part}  {sym} {status_label}")
    }
}

/// Render the full issue tree to a vector of lines.
pub fn render_issue_tree(roots: &[IssueTreeNode], tick: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (ri, root) in roots.iter().enumerate() {
        let is_last_root = ri + 1 == roots.len();
        let root_prefix = if roots.len() > 1 {
            if is_last_root {
                "└── "
            } else {
                "├── "
            }
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
            if is_last_root {
                "    "
            } else {
                "│   "
            }
        } else {
            ""
        };
        render_children(&root.children, child_indent, tick, &mut lines);
    }
    lines
}

fn render_children(
    children: &[IssueTreeNode],
    parent_indent: &str,
    tick: usize,
    lines: &mut Vec<String>,
) {
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
pub fn session_status_symbol(status: &SessionStatus, tick: usize) -> (String, &'static str) {
    match status {
        SessionStatus::Running => (spinner_char(tick).to_string(), "running"),
        SessionStatus::Created => (spinner_char(tick).to_string(), "created"),
        SessionStatus::Completed => ("✔".to_string(), "completed"),
        SessionStatus::Failed => ("✗".to_string(), "failed"),
        SessionStatus::Cancelled => ("●".to_string(), "cancelled"),
        SessionStatus::Waiting => ("⏸".to_string(), "waiting"),
    }
}

/// Render one progress line for a session.
///
/// Format: `[<id-prefix>] <name>: <snippet>  <sym> <status>`
///
/// - `id` is the full session UUID string; the first 8 chars are shown.
/// - `name` is the session name (empty string renders as `-`).
/// - `snippet` is an optional last-content text snippet (truncated to 40 chars).
pub fn render_session_line(
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

// ────────────────────────────────────────────────────────────────────────────
// Tests for format_issue_event

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use events::{IssueEvent, SessionEvent, StopEventStatus};
    use types::{Issue, IssueComment, IssueStatus, SessionStatus};

    fn make_issue(id: &str, title: &str, status: IssueStatus) -> Issue {
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            body: String::new(),
            status,
            branch: "main".into(),
            assignee: None,
            session_id: None,
            parent_id: None,
            blocked_on: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn format_issue_event_created_contains_created_tag_status_and_title() {
        let issue = make_issue("ab12", "Add a greeting", IssueStatus::Open);
        let line = format_issue_event(&IssueEvent::Created(issue));
        assert!(line.contains("[created]"), "must contain [created] tag");
        assert!(line.contains("open"), "must contain status");
        assert!(line.contains("Add a greeting"), "must contain title");
        assert!(line.contains('–'), "must contain em-dash separator");
    }

    #[test]
    fn format_issue_event_status_changed_shows_arrow() {
        let issue = make_issue("ab12", "Test", IssueStatus::Running);
        let line = format_issue_event(&IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        });
        assert!(
            line.contains("[status_changed]"),
            "must contain [status_changed] tag"
        );
        assert!(line.contains("open"), "must contain 'from' status");
        assert!(line.contains("running"), "must contain 'to' status");
        assert!(line.contains('→'), "must contain arrow");
    }

    #[test]
    fn format_issue_event_status_changed_running_to_completed() {
        let issue = make_issue("ab12", "Test", IssueStatus::Completed);
        let line = format_issue_event(&IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Running,
            to: IssueStatus::Completed,
        });
        assert!(
            line.contains("running → completed"),
            "should show running → completed"
        );
    }

    #[test]
    fn format_issue_event_comment_added_shows_author_and_body() {
        let issue = make_issue("ab12", "Test", IssueStatus::Running);
        let comment = IssueComment {
            author: "swe".into(),
            body: "Agent completed the task and created hello.txt.".into(),
            created_at: Utc::now(),
        };
        let line = format_issue_event(&IssueEvent::CommentAdded { issue, comment });
        assert!(
            line.contains("[comment_added]"),
            "must contain [comment_added] tag"
        );
        assert!(line.contains("swe"), "must contain author");
        assert!(
            line.contains("Agent completed the task"),
            "must contain comment body"
        );
        assert!(line.contains('"'), "comment body must be quoted");
    }

    // ── format_session_event: Stopped variants ────────────────────────────────

    #[test]
    fn format_session_event_stopped_complete_with_comment() {
        let event = SessionEvent::Stopped {
            status: StopEventStatus::Complete,
            comment: Some("done".into()),
        };
        let result = format_session_event(&event);
        assert_eq!(result, Some("[stopped: complete — done]\n".to_string()));
    }

    #[test]
    fn format_session_event_stopped_waiting_no_comment() {
        let event = SessionEvent::Stopped {
            status: StopEventStatus::Waiting,
            comment: None,
        };
        let result = format_session_event(&event);
        assert_eq!(result, Some("[stopped: waiting]\n".to_string()));
    }

    // ── Status symbol tests for Waiting ──────────────────────────────────────

    #[test]
    fn issue_status_symbol_waiting() {
        let (sym, label) = issue_status_symbol(&IssueStatus::Waiting, 0);
        assert_eq!(sym, "⏸");
        assert_eq!(label, "waiting");
    }

    #[test]
    fn session_status_symbol_waiting() {
        let (sym, label) = session_status_symbol(&SessionStatus::Waiting, 0);
        assert_eq!(sym, "⏸");
        assert_eq!(label, "waiting");
    }
}
