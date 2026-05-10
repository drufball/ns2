pub use types::{
    ExecutionStatus, FieldCondition, Hook, HookAction, HookExecution, HookFilter, MessageTarget, Op,
};

pub mod cron;
pub mod timer;

// ── Filter evaluation ─────────────────────────────────────────────────────────

pub mod evaluate {
    use super::{FieldCondition, Hook, HookFilter, Op};
    use events::{IssueEvent, SessionEvent, SystemEvent};

    /// Map a `SystemEvent` to its canonical event-type string(s).
    #[must_use]
    pub fn event_type_strings(event: &SystemEvent) -> Vec<String> {
        match event {
            SystemEvent::Issue(IssueEvent::Created(_)) => vec!["issue.created".into()],
            SystemEvent::Issue(IssueEvent::StatusChanged { .. }) => {
                vec!["issue.status_changed".into()]
            }
            SystemEvent::Issue(IssueEvent::CommentAdded { .. }) => {
                vec!["issue.comment_added".into()]
            }
            SystemEvent::Session {
                event: SessionEvent::Done,
                ..
            } => vec!["session.done".into()],
            SystemEvent::Session {
                event: SessionEvent::TurnDone { .. },
                ..
            } => {
                vec!["session.turn_done".into()]
            }
            SystemEvent::Session {
                event: SessionEvent::Error { .. },
                ..
            } => {
                vec!["session.error".into()]
            }
            SystemEvent::Session {
                event: SessionEvent::TurnStarted { .. },
                ..
            } => {
                vec!["session.turn_started".into()]
            }
            SystemEvent::Session { .. } => vec![],
            SystemEvent::External { event_name, .. } => {
                vec![format!("external.{event_name}")]
            }
            SystemEvent::TimerFired { event_name, .. } => {
                vec![format!("timer.{event_name}")]
            }
        }
    }

    /// Returns `true` when `event` should trigger the given `hook`.
    #[must_use]
    pub fn matches_event(hook: &Hook, event: &SystemEvent) -> bool {
        let type_strings = event_type_strings(event);
        let type_match = hook.event_names.iter().any(|et| {
            et == "*" || type_strings.iter().any(|s| s == et)
        });
        if !type_match {
            return false;
        }

        if let Some(filter) = &hook.filter {
            let event_json = serde_json::to_value(event).unwrap_or_default();
            if !evaluate_filter(filter, &event_json) {
                return false;
            }
        }

        true
    }

    fn evaluate_filter(filter: &HookFilter, event_json: &serde_json::Value) -> bool {
        for cond in &filter.conditions {
            if !evaluate_condition(cond, event_json) {
                return false;
            }
        }
        true
    }

    pub(crate) fn navigate_path<'a>(
        json: &'a serde_json::Value,
        path: &str,
    ) -> Option<&'a serde_json::Value> {
        let mut current = json;
        for key in path.split('.') {
            current = current.get(key)?;
        }
        Some(current)
    }

    fn evaluate_condition(cond: &FieldCondition, event_json: &serde_json::Value) -> bool {
        let actual = navigate_path(event_json, &cond.field);
        match &cond.op {
            Op::Eq => actual == Some(&cond.value),
            Op::NotEq => actual != Some(&cond.value),
            Op::Contains => match (actual, &cond.value) {
                (Some(serde_json::Value::String(s)), serde_json::Value::String(v)) => {
                    s.contains(v.as_str())
                }
                (Some(serde_json::Value::Array(arr)), v) => arr.contains(v),
                _ => false,
            },
            Op::Matches => match (actual, &cond.value) {
                (Some(serde_json::Value::String(s)), serde_json::Value::String(pattern)) => {
                    glob_match(pattern, s)
                }
                _ => false,
            },
        }
    }

    pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        let parts: Vec<&str> = pattern.split('*').collect();
        let mut remaining = text;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if i == parts.len() - 1 {
                return remaining.ends_with(part);
            } else if let Some(pos) = remaining.find(part) {
                remaining = &remaining[pos + part.len()..];
            } else {
                return false;
            }
        }
        true
    }
}

// ── Template rendering ────────────────────────────────────────────────────────

pub mod template {
    /// Render a minijinja template with the event JSON as context variable `event`.
    pub fn render_template(template_str: &str, event_json: &serde_json::Value) -> String {
        let mut env = minijinja::Environment::new();
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
        match env.render_str(template_str, minijinja::context! { event => event_json }) {
            Ok(rendered) => rendered,
            Err(e) => {
                tracing::warn!("Failed to render hook template: {e}");
                template_str.to_string()
            }
        }
    }
}

// ── Action execution ──────────────────────────────────────────────────────────

pub mod execute {
    use super::template::render_template;
    use super::{ExecutionStatus, Hook, HookAction, HookExecution, MessageTarget};
    use chrono::Utc;
    use db::HookStore;
    use events::SystemEvent;

    pub async fn run_action(
        hook: &Hook,
        event: &SystemEvent,
        issue_svc: &issues::IssueService,
        hook_store: &dyn HookStore,
    ) {
        let event_json = serde_json::to_value(event).unwrap_or_default();
        let exec_id = uuid::Uuid::new_v4().to_string();
        let mut exec = HookExecution {
            id: exec_id,
            hook_id: hook.id.clone(),
            triggered_at: Utc::now(),
            event_payload: event_json.clone(),
            status: ExecutionStatus::Running,
            result: None,
            completed_at: None,
        };
        let _ = hook_store.create_execution(&exec).await;

        let result = execute_action_inner(hook, &event_json, issue_svc).await;

        exec.completed_at = Some(Utc::now());
        match result {
            Ok(msg) => {
                exec.status = ExecutionStatus::Completed;
                exec.result = Some(msg);
            }
            Err(e) => {
                exec.status = ExecutionStatus::Failed;
                exec.result = Some(e);
            }
        }
        let _ = hook_store.update_execution(&exec).await;
    }

    pub(crate) async fn execute_action_inner(
        hook: &Hook,
        event_json: &serde_json::Value,
        issue_svc: &issues::IssueService,
    ) -> Result<String, String> {
        match &hook.action {
            HookAction::SendMessage {
                target: MessageTarget::Issue(id),
                body,
            } => {
                let rendered = render_template(body, event_json);
                issue_svc
                    .add_comment(id.clone(), "ns2-hook".to_string(), rendered)
                    .await
                    .map(|_| "comment added".to_string())
                    .map_err(|e| e.to_string())
            }
            HookAction::SendMessage {
                target: MessageTarget::Session(_),
                ..
            } => Ok("session messaging not yet implemented".to_string()),
            HookAction::CreateIssue { .. } => {
                Ok("create_issue action not yet implemented".to_string())
            }
            HookAction::RunShell { .. } => Ok("run_shell action not yet implemented".to_string()),
        }
    }
}

// ── Hook ID generation ────────────────────────────────────────────────────────

#[must_use]
pub fn generate_hook_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let id = uuid::Uuid::new_v4();
    let bytes = id.as_bytes();
    (0..4)
        .map(|i| ALPHABET[(bytes[i] as usize) % ALPHABET.len()] as char)
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use events::{IssueEvent, SessionEvent, SystemEvent};
    use types::{Issue, IssueComment, IssueStatus};
    use uuid::Uuid;

    fn make_issue() -> Issue {
        Issue {
            id: "ab12".into(),
            title: "Test".into(),
            body: "Body".into(),
            status: IssueStatus::Open,
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

    fn make_internal_hook(event_types: Vec<&str>, filter: Option<HookFilter>) -> Hook {
        Hook {
            id: "h001".into(),
            name: "test hook".into(),
            event_names: event_types.into_iter().map(String::from).collect(),
            filter,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("watcher".into()),
                body: "Issue {{ event.data.issue.id }} changed".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ── evaluate::matches_event tests ─────────────────────────────────────────

    #[test]
    fn matches_issue_created_event_type() {
        let hook = make_internal_hook(vec!["issue.created"], None);
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn does_not_match_wrong_event_type() {
        let hook = make_internal_hook(vec!["issue.created"], None);
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(),
            from: IssueStatus::Open,
            to: IssueStatus::InProgress,
        });
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_issue_status_changed() {
        let hook = make_internal_hook(vec!["issue.status_changed"], None);
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(),
            from: IssueStatus::Open,
            to: IssueStatus::InProgress,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_issue_comment_added() {
        let hook = make_internal_hook(vec!["issue.comment_added"], None);
        let comment = IssueComment {
            author: "user".into(),
            created_at: Utc::now(),
            body: "hi".into(),
        };
        let event = SystemEvent::Issue(IssueEvent::CommentAdded {
            issue: make_issue(),
            comment,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_done() {
        let hook = make_internal_hook(vec!["session.done"], None);
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_turn_done() {
        let hook = make_internal_hook(vec!["session.turn_done"], None);
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::TurnDone {
                turn_id: Uuid::new_v4(),
            },
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_error() {
        let hook = make_internal_hook(vec!["session.error"], None);
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Error {
                message: "oops".into(),
            },
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_turn_started() {
        use types::Turn;
        let hook = make_internal_hook(vec!["session.turn_started"], None);
        let turn = Turn {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            token_count: None,
            created_at: Utc::now(),
        };
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::TurnStarted { turn },
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn wildcard_matches_any_event() {
        let hook = make_internal_hook(vec!["*"], None);
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(evaluate::matches_event(&hook, &event));

        let event2 = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Done,
        };
        assert!(evaluate::matches_event(&hook, &event2));
    }

    #[test]
    fn hook_with_no_event_names_does_not_match_any_event() {
        let hook = Hook {
            id: "ext1".into(),
            name: "empty-hook".into(),
            event_names: vec![],
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("x".into()),
                body: "hi".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn hook_matches_external_event_by_name() {
        let hook = Hook {
            id: "ext2".into(),
            name: "ci-hook".into(),
            event_names: vec!["external.ci-complete".into()],
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("x".into()),
                body: "CI done".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let event = SystemEvent::External {
            event_id: "e001".into(),
            event_name: "ci-complete".into(),
            payload: serde_json::json!({}),
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn hook_does_not_match_wrong_external_event_name() {
        let hook = Hook {
            id: "ext3".into(),
            name: "ci-hook".into(),
            event_names: vec!["external.ci-complete".into()],
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("x".into()),
                body: "CI done".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let event = SystemEvent::External {
            event_id: "e002".into(),
            event_name: "deploy-done".into(),
            payload: serde_json::json!({}),
        };
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn hook_matches_timer_event_by_name() {
        let hook = Hook {
            id: "t1".into(),
            name: "heartbeat-hook".into(),
            event_names: vec!["timer.heartbeat".into()],
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("x".into()),
                body: "tick".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let event = SystemEvent::TimerFired {
            event_id: "t001".into(),
            event_name: "heartbeat".into(),
            fired_at: Utc::now(),
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn hook_matches_issue_status_changed_with_event_names() {
        let hook = Hook {
            id: "h1".into(),
            name: "status-hook".into(),
            event_names: vec!["issue.status_changed".into()],
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("x".into()),
                body: "changed".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(),
            from: IssueStatus::Open,
            to: IssueStatus::InProgress,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    // ── FieldCondition evaluation tests ───────────────────────────────────────

    #[test]
    fn field_condition_eq_matches() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.issue.status".into(),
                op: Op::Eq,
                value: serde_json::json!("in_progress"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.status_changed"], Some(filter));

        let mut issue = make_issue();
        issue.status = IssueStatus::InProgress;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::InProgress,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_eq_no_match() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.issue.status".into(),
                op: Op::Eq,
                value: serde_json::json!("completed"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.status_changed"], Some(filter));

        let mut issue = make_issue();
        issue.status = IssueStatus::InProgress;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::InProgress,
        });
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_not_eq_matches() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.issue.status".into(),
                op: Op::NotEq,
                value: serde_json::json!("open"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.status_changed"], Some(filter));

        let mut issue = make_issue();
        issue.status = IssueStatus::InProgress;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::InProgress,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_contains_string() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Contains,
                value: serde_json::json!("Test"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(evaluate::matches_event(&hook, &event));
    }

    // ── minijinja template rendering tests ────────────────────────────────────

    #[test]
    fn template_renders_event_issue_id() {
        let event_json = serde_json::json!({
            "type": "issue",
            "data": { "type": "status_changed", "issue": { "id": "ab12", "status": "running" } }
        });
        let rendered = template::render_template(
            "Issue {{ event.data.issue.id }} is now {{ event.data.issue.status }}",
            &event_json,
        );
        assert_eq!(rendered, "Issue ab12 is now running");
    }

    #[test]
    fn template_with_no_vars_returns_literal() {
        let event_json = serde_json::json!({});
        let rendered = template::render_template("Hello, world!", &event_json);
        assert_eq!(rendered, "Hello, world!");
    }

    #[test]
    fn template_missing_var_renders_empty() {
        let event_json = serde_json::json!({});
        let rendered = template::render_template("{{ event.missing.field }}", &event_json);
        assert!(rendered.is_empty(), "expected empty, got: {rendered:?}");
    }

    // ── Op::Contains on arrays ────────────────────────────────────────────────

    #[test]
    fn field_condition_contains_array_matches() {
        // blocked_on: vec!["dep-1", "urgent"] — looking for "urgent"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.blocked_on".into(),
                op: Op::Contains,
                value: serde_json::json!("urgent"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.blocked_on = vec!["dep-1".into(), "urgent".into()];
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_contains_array_no_match() {
        // blocked_on: vec!["bug"] — looking for "enhancement"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.blocked_on".into(),
                op: Op::Contains,
                value: serde_json::json!("enhancement"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.blocked_on = vec!["bug".into()];
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(!evaluate::matches_event(&hook, &event));
    }

    // ── Op::Matches (via matches_event) ───────────────────────────────────────

    #[test]
    fn field_condition_matches_exact() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("Test"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_exact_no_match() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("notexact"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let event = SystemEvent::Issue(IssueEvent::Created(make_issue()));
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_prefix_wildcard() {
        // pattern "Test*" matches "Test" (the issue title)
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("Test*"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.title = "Test issue".into();
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_prefix_wildcard_no_match() {
        // pattern "foo*" does not match "barfoo"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("foo*"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.title = "barfoo".into();
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_suffix_wildcard() {
        // pattern "*bar" matches "foobar"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("*bar"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.title = "foobar".into();
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_suffix_wildcard_no_match() {
        // pattern "*bar" does not match "foobaz"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("*bar"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.title = "foobaz".into();
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_middle_wildcard() {
        // pattern "foo*bar" matches "foo-baz-bar"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("foo*bar"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.title = "foo-baz-bar".into();
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_middle_wildcard_no_match() {
        // pattern "foo*bar" does not match "foo-baz-baz"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("foo*bar"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.title = "foo-baz-baz".into();
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn field_condition_matches_star_matches_anything() {
        // pattern "*" matches "anything"
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.title".into(),
                op: Op::Matches,
                value: serde_json::json!("*"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.created"], Some(filter));
        let mut issue = make_issue();
        issue.title = "anything".into();
        let event = SystemEvent::Issue(IssueEvent::Created(issue));
        assert!(evaluate::matches_event(&hook, &event));
    }

    // ── glob_match direct tests ───────────────────────────────────────────────

    #[test]
    fn glob_match_star_matches_anything() {
        assert!(evaluate::glob_match("*", "anything"));
        assert!(evaluate::glob_match("*", ""));
    }

    #[test]
    fn glob_match_prefix_wildcard() {
        assert!(evaluate::glob_match("foo*", "foobar"));
        assert!(!evaluate::glob_match("foo*", "barfoo"));
    }

    #[test]
    fn glob_match_suffix_wildcard() {
        assert!(evaluate::glob_match("*bar", "foobar"));
        assert!(!evaluate::glob_match("*bar", "foobaz"));
    }

    #[test]
    fn glob_match_middle_wildcard() {
        assert!(evaluate::glob_match("foo*bar", "foo-baz-bar"));
        assert!(!evaluate::glob_match("foo*bar", "foo-baz-baz"));
    }

    #[test]
    fn glob_match_exact() {
        assert!(evaluate::glob_match("exact", "exact"));
        assert!(!evaluate::glob_match("exact", "notexact"));
    }

    #[test]
    fn glob_match_double_wildcard_match() {
        assert!(evaluate::glob_match("a*b*c", "aXbYc"));
    }

    #[test]
    fn glob_match_double_wildcard_no_match() {
        assert!(!evaluate::glob_match("a*b*c", "aXbYd"));
    }

    #[test]
    fn glob_match_double_wildcard_order_matters() {
        assert!(!evaluate::glob_match("a*b*c", "aXcYb"));
    }

    #[test]
    fn glob_match_triple_wildcard() {
        assert!(evaluate::glob_match("*a*b*", "XaYbZ"));
    }

    // ── Hook ID generation ────────────────────────────────────────────────────

    #[test]
    fn generate_hook_id_is_4_chars() {
        let id = generate_hook_id();
        assert_eq!(id.len(), 4);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn generate_hook_id_is_unique() {
        let ids: std::collections::HashSet<String> = (0..100).map(|_| generate_hook_id()).collect();
        assert!(ids.len() > 90);
    }

    #[test]
    fn generate_hook_id_uses_full_alphabet() {
        let ids: Vec<String> = (0..10_000).map(|_| generate_hook_id()).collect();
        let chars: std::collections::HashSet<char> = ids.concat().chars().collect();
        assert!(
            chars.len() >= 30,
            "expected at least 30 distinct chars, got {}",
            chars.len()
        );
    }

    // ── execute module tests ──────────────────────────────────────────────────

    mod execute_tests {
        use super::*;
        use async_trait::async_trait;
        use chrono::Utc;
        use std::sync::{Arc, Mutex};

        // ── Helper: build an IssueService backed by an in-memory SQLite db ────────

        async fn make_issue_service() -> issues::IssueService {
            let (db, _hook_store, _event_store) = db::connect("sqlite::memory:").await.unwrap();
            issues::IssueService::new(db)
        }

        // ── Stub HookStore ────────────────────────────────────────────────────────

        /// Captures all calls to `create_execution` / `update_execution` for assertions.
        struct SpyHookStore {
            created: Mutex<Vec<HookExecution>>,
            updated: Mutex<Vec<HookExecution>>,
        }

        impl SpyHookStore {
            fn new() -> Arc<Self> {
                Arc::new(Self {
                    created: Mutex::new(vec![]),
                    updated: Mutex::new(vec![]),
                })
            }
        }

        #[async_trait]
        impl db::HookStore for SpyHookStore {
            async fn create_hook(&self, _hook: &types::Hook) -> db::Result<()> {
                Ok(())
            }

            async fn list_hooks(
                &self,
                _enabled: Option<bool>,
            ) -> db::Result<Vec<types::Hook>> {
                Ok(vec![])
            }

            async fn get_hook(&self, _id: &str) -> db::Result<types::Hook> {
                Err(db::Error::NotFound)
            }

            async fn update_hook(&self, _hook: &types::Hook) -> db::Result<()> {
                Ok(())
            }

            async fn delete_hook(&self, _id: &str) -> db::Result<()> {
                Ok(())
            }

            async fn create_execution(&self, exec: &HookExecution) -> db::Result<()> {
                self.created.lock().unwrap().push(exec.clone());
                Ok(())
            }

            async fn update_execution(&self, exec: &HookExecution) -> db::Result<()> {
                self.updated.lock().unwrap().push(exec.clone());
                Ok(())
            }

            async fn list_executions(
                &self,
                _hook_id: &str,
                _limit: usize,
            ) -> db::Result<Vec<HookExecution>> {
                Ok(vec![])
            }
        }

        // ── Hook factory for execute tests ────────────────────────────────────────

        fn make_send_message_hook(issue_id: &str, body: &str) -> Hook {
            Hook {
                id: "exec01".into(),
                name: "exec-hook".into(),
                event_names: vec!["issue.created".into()],
                filter: None,
                action: HookAction::SendMessage {
                    target: MessageTarget::Issue(issue_id.into()),
                    body: body.into(),
                },
                enabled: true,
                created_by: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }
        }

        fn make_hook_with_action(action: HookAction) -> Hook {
            Hook {
                id: "exec02".into(),
                name: "exec-hook-2".into(),
                event_names: vec!["issue.created".into()],
                filter: None,
                action,
                enabled: true,
                created_by: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }
        }

        fn dummy_event() -> events::SystemEvent {
            events::SystemEvent::Issue(events::IssueEvent::Created(types::Issue {
                id: "dummy".into(),
                title: "Dummy".into(),
                body: "Body".into(),
                status: types::IssueStatus::Open,
                branch: "main".into(),
                assignee: None,
                session_id: None,
                parent_id: None,
                blocked_on: vec![],
                comments: vec![],
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }))
        }

        // ── execute_action_inner tests ────────────────────────────────────────────

        #[tokio::test]
        async fn execute_action_inner_sends_comment_to_issue() {
            let svc = make_issue_service().await;

            // Create an issue to comment on
            let issue = svc
                .create_issue(issues::CreateIssueInput {
                    title: "Watch01".into(),
                    body: "body".into(),
                    assignee: None,
                    parent_id: None,
                    blocked_on: vec![],
                    branch: None,
                })
                .await
                .unwrap();
            let issue_id = issue.id.clone();

            let hook = make_send_message_hook(&issue_id, "hello");
            let event_json = serde_json::to_value(dummy_event()).unwrap();

            let result = execute::execute_action_inner(&hook, &event_json, &svc).await;

            assert!(result.is_ok(), "expected Ok, got {result:?}");
            assert_eq!(result.unwrap(), "comment added");

            // Verify the comment was actually added to the issue
            let updated = svc
                .add_comment(issue_id.clone(), "check".into(), "verify".into())
                .await
                .unwrap();
            // The first comment should be from the hook
            assert!(
                updated.comments.len() >= 2,
                "expected at least 2 comments (hook + check), got: {}",
                updated.comments.len()
            );
            let hook_comment = &updated.comments[0];
            assert_eq!(hook_comment.author, "ns2-hook");
            assert_eq!(hook_comment.body, "hello");
        }

        #[tokio::test]
        async fn execute_action_inner_sends_rendered_template_to_issue() {
            let svc = make_issue_service().await;

            let issue = svc
                .create_issue(issues::CreateIssueInput {
                    title: "Watch01".into(),
                    body: "body".into(),
                    assignee: None,
                    parent_id: None,
                    blocked_on: vec![],
                    branch: None,
                })
                .await
                .unwrap();
            let issue_id = issue.id.clone();

            let hook = make_send_message_hook(&issue_id, "issue {{ event.data.id }} changed");
            let event_json = serde_json::json!({"data": {"id": "test-id"}});

            let result = execute::execute_action_inner(&hook, &event_json, &svc).await;

            assert!(result.is_ok(), "expected Ok, got {result:?}");

            // Verify rendered comment was added
            let updated_issue = svc
                .add_comment(issue_id, "check".into(), "x".into())
                .await
                .unwrap();
            let hook_comment = &updated_issue.comments[0];
            assert_eq!(hook_comment.author, "ns2-hook");
            assert_eq!(hook_comment.body, "issue test-id changed");
        }

        #[tokio::test]
        async fn execute_action_inner_returns_err_for_nonexistent_issue() {
            let svc = make_issue_service().await;

            let hook = make_send_message_hook("no-such-issue", "hello");
            let event_json = serde_json::json!({});

            let result = execute::execute_action_inner(&hook, &event_json, &svc).await;

            assert!(
                result.is_err(),
                "expected Err for nonexistent issue, got {result:?}"
            );
            let err_msg = result.unwrap_err();
            assert!(!err_msg.is_empty(), "error message should not be empty");
        }

        #[tokio::test]
        async fn execute_action_inner_session_target_returns_ok_not_implemented() {
            let svc = make_issue_service().await;

            let hook = make_hook_with_action(HookAction::SendMessage {
                target: MessageTarget::Session("some-session".into()),
                body: "hi".into(),
            });
            let event_json = serde_json::json!({});

            let result = execute::execute_action_inner(&hook, &event_json, &svc).await;

            assert!(result.is_ok(), "expected Ok for session target");
            assert!(
                result.unwrap().contains("not yet implemented"),
                "expected 'not yet implemented' message"
            );
        }

        #[tokio::test]
        async fn execute_action_inner_create_issue_returns_ok_not_implemented() {
            let svc = make_issue_service().await;

            let hook = make_hook_with_action(HookAction::CreateIssue {
                title: "New issue".into(),
                body: "body".into(),
                assignee: None,
                parent: None,
                start: false,
            });
            let event_json = serde_json::json!({});

            let result = execute::execute_action_inner(&hook, &event_json, &svc).await;

            assert!(result.is_ok(), "expected Ok for CreateIssue action");
            assert!(
                result.unwrap().contains("not yet implemented"),
                "expected 'not yet implemented' message"
            );
        }

        #[tokio::test]
        async fn execute_action_inner_run_shell_returns_ok_not_implemented() {
            let svc = make_issue_service().await;

            let hook = make_hook_with_action(HookAction::RunShell {
                command: "echo hello".into(),
                timeout_secs: 30,
                blocking: false,
            });
            let event_json = serde_json::json!({});

            let result = execute::execute_action_inner(&hook, &event_json, &svc).await;

            assert!(result.is_ok(), "expected Ok for RunShell action");
            assert!(
                result.unwrap().contains("not yet implemented"),
                "expected 'not yet implemented' message"
            );
        }

        // ── run_action tests ──────────────────────────────────────────────────────

        #[tokio::test]
        async fn run_action_creates_running_execution_then_updates_to_completed() {
            let svc = make_issue_service().await;

            // Create the issue the hook will comment on
            let issue = svc
                .create_issue(issues::CreateIssueInput {
                    title: "Watch".into(),
                    body: "body".into(),
                    assignee: None,
                    parent_id: None,
                    blocked_on: vec![],
                    branch: None,
                })
                .await
                .unwrap();

            let hook = make_send_message_hook(&issue.id, "triggered");
            let event = dummy_event();
            let spy = SpyHookStore::new();

            execute::run_action(&hook, &event, &svc, spy.as_ref()).await;

            let created = spy.created.lock().unwrap();
            assert_eq!(created.len(), 1, "create_execution should be called once");
            assert_eq!(
                created[0].status,
                ExecutionStatus::Running,
                "initial execution status must be Running"
            );
            assert_eq!(created[0].hook_id, "exec01");
            drop(created);

            let updated = spy.updated.lock().unwrap();
            assert_eq!(updated.len(), 1, "update_execution should be called once");
            assert_eq!(
                updated[0].status,
                ExecutionStatus::Completed,
                "final execution status must be Completed"
            );
            assert_eq!(updated[0].result.as_deref(), Some("comment added"));
            drop(updated);
        }

        #[tokio::test]
        async fn run_action_updates_execution_to_failed_when_action_errors() {
            let svc = make_issue_service().await;

            // Targeting a nonexistent issue
            let hook = make_send_message_hook("no-such-issue", "triggered");
            let event = dummy_event();
            let spy = SpyHookStore::new();

            execute::run_action(&hook, &event, &svc, spy.as_ref()).await;

            let created = spy.created.lock().unwrap();
            assert_eq!(created.len(), 1, "create_execution should be called once");
            assert_eq!(created[0].status, ExecutionStatus::Running);
            drop(created);

            let updated = spy.updated.lock().unwrap();
            assert_eq!(updated.len(), 1, "update_execution should be called once");
            assert_eq!(
                updated[0].status,
                ExecutionStatus::Failed,
                "final execution status must be Failed when action errors"
            );
            assert!(
                updated[0].result.is_some(),
                "result should contain error message"
            );
            drop(updated);
        }

        #[tokio::test]
        async fn run_action_completed_at_is_set_after_run() {
            let svc = make_issue_service().await;

            let issue = svc
                .create_issue(issues::CreateIssueInput {
                    title: "Watch".into(),
                    body: "body".into(),
                    assignee: None,
                    parent_id: None,
                    blocked_on: vec![],
                    branch: None,
                })
                .await
                .unwrap();

            let hook = make_send_message_hook(&issue.id, "ping");
            let event = dummy_event();
            let spy = SpyHookStore::new();

            execute::run_action(&hook, &event, &svc, spy.as_ref()).await;

            let updated = spy.updated.lock().unwrap();
            assert!(
                updated[0].completed_at.is_some(),
                "completed_at must be set after run"
            );
            drop(updated);
        }
    }
}
