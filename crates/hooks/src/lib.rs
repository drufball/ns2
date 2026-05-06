pub use types::{
    ExecutionStatus, FieldCondition, Hook, HookAction, HookExecution, HookFilter, HookSource,
    MessageTarget, Op,
};

// ── Filter evaluation ─────────────────────────────────────────────────────────

pub mod evaluate {
    use events::{IssueEvent, SessionEvent, SystemEvent};
    use super::{FieldCondition, Hook, HookFilter, HookSource, Op};

    /// Map a `SystemEvent` to its canonical event-type string(s).
    #[must_use]
    pub fn event_type_strings(event: &SystemEvent) -> Vec<&'static str> {
        match event {
            SystemEvent::Issue(IssueEvent::Created(_)) => vec!["issue.created"],
            SystemEvent::Issue(IssueEvent::StatusChanged { .. }) => vec!["issue.status_changed"],
            SystemEvent::Issue(IssueEvent::CommentAdded { .. }) => vec!["issue.comment_added"],
            SystemEvent::Session { event: SessionEvent::Done, .. } => vec!["session.done"],
            SystemEvent::Session { event: SessionEvent::TurnDone { .. }, .. } => {
                vec!["session.turn_done"]
            }
            SystemEvent::Session { event: SessionEvent::Error { .. }, .. } => {
                vec!["session.error"]
            }
            SystemEvent::Session { event: SessionEvent::TurnStarted { .. }, .. } => {
                vec!["session.turn_started"]
            }
            SystemEvent::Session { .. }
            | SystemEvent::External { .. }
            | SystemEvent::TimerFired { .. } => vec![],
        }
    }

    /// Returns `true` when `event` should trigger the given `hook`.
    #[must_use]
    pub fn matches_event(hook: &Hook, event: &SystemEvent) -> bool {
        let HookSource::Internal { event_types } = &hook.source else {
            return false;
        };

        let type_match = event_types
            .iter()
            .any(|et| et == "*" || event_type_strings(event).contains(&et.as_str()));
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
    use chrono::Utc;
    use db::HookStore;
    use events::SystemEvent;
    use super::{Hook, HookAction, HookExecution, ExecutionStatus, MessageTarget};
    use super::template::render_template;

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

    async fn execute_action_inner(
        hook: &Hook,
        event_json: &serde_json::Value,
        issue_svc: &issues::IssueService,
    ) -> Result<String, String> {
        match &hook.action {
            HookAction::SendMessage { target: MessageTarget::Issue(id), body } => {
                let rendered = render_template(body, event_json);
                issue_svc
                    .add_comment(id.clone(), "ns2-hook".to_string(), rendered)
                    .await
                    .map(|_| "comment added".to_string())
                    .map_err(|e| e.to_string())
            }
            HookAction::SendMessage { target: MessageTarget::Session(_), .. } => {
                Ok("session messaging not yet implemented".to_string())
            }
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
    use events::{IssueEvent, SessionEvent, SystemEvent};
    use types::{Issue, IssueStatus, IssueComment};
    use chrono::Utc;
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
            source: HookSource::Internal {
                event_types: event_types.into_iter().map(String::from).collect(),
            },
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
            to: IssueStatus::Running,
        });
        assert!(!evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_issue_status_changed() {
        let hook = make_internal_hook(vec!["issue.status_changed"], None);
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue: make_issue(),
            from: IssueStatus::Open,
            to: IssueStatus::Running,
        });
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_issue_comment_added() {
        let hook = make_internal_hook(vec!["issue.comment_added"], None);
        let comment =
            IssueComment { author: "user".into(), created_at: Utc::now(), body: "hi".into() };
        let event =
            SystemEvent::Issue(IssueEvent::CommentAdded { issue: make_issue(), comment });
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
            event: SessionEvent::TurnDone { turn_id: Uuid::new_v4() },
        };
        assert!(evaluate::matches_event(&hook, &event));
    }

    #[test]
    fn matches_session_error() {
        let hook = make_internal_hook(vec!["session.error"], None);
        let event = SystemEvent::Session {
            session_id: Uuid::new_v4(),
            event: SessionEvent::Error { message: "oops".into() },
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
    fn external_hook_does_not_match_live_events() {
        let hook = Hook {
            id: "ext1".into(),
            name: "external".into(),
            source: HookSource::External { secret: None },
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

    // ── FieldCondition evaluation tests ───────────────────────────────────────

    #[test]
    fn field_condition_eq_matches() {
        let filter = HookFilter {
            conditions: vec![FieldCondition {
                field: "data.issue.status".into(),
                op: Op::Eq,
                value: serde_json::json!("running"),
            }],
            expression: None,
        };
        let hook = make_internal_hook(vec!["issue.status_changed"], Some(filter));

        let mut issue = make_issue();
        issue.status = IssueStatus::Running;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::Running,
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
        issue.status = IssueStatus::Running;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::Running,
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
        issue.status = IssueStatus::Running;
        let event = SystemEvent::Issue(IssueEvent::StatusChanged {
            issue,
            from: IssueStatus::Open,
            to: IssueStatus::Running,
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

    // ── Hook ID generation ────────────────────────────────────────────────────

    #[test]
    fn generate_hook_id_is_4_chars() {
        let id = generate_hook_id();
        assert_eq!(id.len(), 4);
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn generate_hook_id_is_unique() {
        let ids: std::collections::HashSet<String> =
            (0..100).map(|_| generate_hook_id()).collect();
        assert!(ids.len() > 90);
    }
}
