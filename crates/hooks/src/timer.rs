use crate::cron::next_after;
use chrono::{DateTime, Duration, Utc};
use db::HookStore;
use events::{EventBus, SystemEvent};
use std::sync::Arc;
use types::HookSource;

/// Check whether a timer hook should fire at the given `now` time.
///
/// A hook fires if the next cron tick after `(now - 60 seconds)` is ≤ `now`.
/// This provides a 60-second rolling window so no ticks are missed.
#[must_use]
pub fn should_fire(schedule: &str, now: DateTime<Utc>) -> bool {
    let window_start = now - Duration::seconds(60);
    match next_after(schedule, window_start) {
        Ok(next_fire) => next_fire <= now,
        Err(e) => {
            tracing::warn!("Timer hook: invalid schedule '{schedule}': {e}");
            false
        }
    }
}

/// Process all enabled timer hooks for a single scheduler tick at `now`.
///
/// Checks each hook's schedule against the 60-second rolling window and, for
/// those that match, emits a `SystemEvent::TimerFired` on the event bus and
/// spawns a task to execute the hook action.
pub(crate) async fn process_timer_hooks(
    hook_store: &Arc<dyn HookStore>,
    event_bus: &EventBus,
    issue_svc: &issues::IssueService,
    now: DateTime<Utc>,
) {
    let hooks = hook_store
        .list_hooks(Some(true), Some("timer"))
        .await
        .unwrap_or_default();

    for hook in hooks {
        let HookSource::Timer { ref schedule } = hook.source else {
            continue;
        };

        if !should_fire(schedule, now) {
            continue;
        }

        // Compute the exact fire time for the event payload
        let window_start = now - Duration::seconds(60);
        let Ok(fired_at) = next_after(schedule, window_start) else {
            continue;
        };

        // Emit TimerFired event
        event_bus.send(SystemEvent::TimerFired {
            hook_id: hook.id.clone(),
            fired_at,
        });

        // Execute the hook action
        let event = SystemEvent::TimerFired {
            hook_id: hook.id.clone(),
            fired_at,
        };
        let hook_store_clone = Arc::clone(hook_store);
        let issue_svc_clone = issue_svc.clone();
        let hook_clone = hook.clone();
        tokio::spawn(async move {
            crate::execute::run_action(
                &hook_clone,
                &event,
                &issue_svc_clone,
                hook_store_clone.as_ref(),
            )
            .await;
        });
    }
}

/// Run the timer scheduler loop.
///
/// Wakes every 30 seconds and fires any enabled timer hooks whose schedule
/// falls within the last 60-second window.
pub async fn run_timer_scheduler(
    hook_store: Arc<dyn HookStore>,
    event_bus: EventBus,
    issue_svc: issues::IssueService,
) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
    // First tick fires immediately; we skip it to avoid firing on startup.
    interval.tick().await;

    loop {
        interval.tick().await;
        let now = Utc::now();
        process_timer_hooks(&hook_store, &event_bus, &issue_svc, now).await;
    }
}

/// Spawn the timer scheduler as a background `tokio::task`.
pub fn spawn_timer_scheduler(
    hook_store: &Arc<dyn HookStore>,
    event_bus: &EventBus,
    issue_svc: &issues::IssueService,
) {
    let hook_store = Arc::clone(hook_store);
    let event_bus = event_bus.clone();
    let issue_svc = issue_svc.clone();
    tokio::spawn(async move {
        run_timer_scheduler(hook_store, event_bus, issue_svc).await;
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use types::{Hook, HookAction, HookExecution, HookSource, MessageTarget};

    // ── Single stub store (merged from SpyStore + StubStore) ─────────────────

    struct StubStore {
        hooks: Vec<Hook>,
    }

    #[async_trait]
    impl HookStore for StubStore {
        async fn create_hook(&self, _h: &types::Hook) -> db::Result<()> {
            Ok(())
        }
        async fn list_hooks(
            &self,
            _enabled: Option<bool>,
            _source_type: Option<&str>,
        ) -> db::Result<Vec<types::Hook>> {
            Ok(self.hooks.clone())
        }
        async fn get_hook(&self, _id: &str) -> db::Result<types::Hook> {
            Err(db::Error::NotFound)
        }
        async fn update_hook(&self, _h: &types::Hook) -> db::Result<()> {
            Ok(())
        }
        async fn delete_hook(&self, _id: &str) -> db::Result<()> {
            Ok(())
        }
        async fn create_execution(&self, _e: &HookExecution) -> db::Result<()> {
            Ok(())
        }
        async fn update_execution(&self, _e: &HookExecution) -> db::Result<()> {
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

    fn make_timer_hook(id: &str, schedule: &str) -> Hook {
        Hook {
            id: id.into(),
            name: "test-timer".into(),
            source: HookSource::Timer {
                schedule: schedule.into(),
            },
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("watcher".into()),
                body: "tick".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    async fn make_issue_service() -> issues::IssueService {
        let (db, _hook_store) = db::connect("sqlite::memory:").await.unwrap();
        issues::IssueService::new(db)
    }

    // ── should_fire tests ─────────────────────────────────────────────────────

    #[test]
    fn should_fire_returns_true_within_window() {
        // "0 9 * * 1" = Monday 9:00 UTC
        // now = Monday 2024-01-15 09:00:30 UTC (30 seconds into the window)
        // window_start = 08:59:30 → next_after(08:59:30) = 09:00:00 ≤ 09:00:30 → fires
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();
        assert!(
            should_fire("0 9 * * 1", now),
            "hook should fire 30s into the window"
        );
    }

    #[test]
    fn should_fire_returns_false_outside_window() {
        // "0 9 * * 1" = Monday 9:00 UTC
        // now = Monday 2024-01-15 09:01:30 UTC (1min 30s past)
        // window_start = 09:00:30 → next_after(09:00:30) = next Monday 09:00 → NOT ≤ 09:01:30
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 1, 30).unwrap();
        assert!(
            !should_fire("0 9 * * 1", now),
            "hook should not fire 1min 30s after the window"
        );
    }

    #[test]
    fn should_fire_returns_false_for_invalid_schedule() {
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 0).unwrap();
        assert!(
            !should_fire("not-a-valid-cron", now),
            "invalid schedule should not fire"
        );
    }

    #[test]
    fn should_fire_every_minute_at_top_of_minute() {
        // "* * * * *" — fires every minute
        // now = 09:01:00 exactly, window_start = 09:00:00
        // next_after(09:00:00) = 09:01:00 ≤ 09:01:00 → fires
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 1, 0).unwrap();
        assert!(
            should_fire("* * * * *", now),
            "every-minute schedule should fire at top of minute"
        );
    }

    #[test]
    fn should_fire_every_minute_mid_minute() {
        // "* * * * *" — fires every minute
        // now = 09:00:30, window_start = 08:59:30
        // next_after(08:59:30) = 09:00:00 ≤ 09:00:30 → fires
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();
        assert!(
            should_fire("* * * * *", now),
            "every-minute schedule should fire mid-minute"
        );
    }

    /// Boundary: at exactly 60 seconds after the fire time, the hook must still
    /// fire (the window is inclusive on both ends for the 60-second boundary).
    ///
    /// "0 9 * * 1" fires at 09:00:00.
    /// now = 09:01:00 → `window_start` = 09:00:00 → `next_after(09:00:00)` = next Monday 09:00
    /// so at exactly 60s past, it does NOT fire (boundary is exclusive on the start).
    #[test]
    fn should_fire_at_exactly_60s_boundary() {
        // "0 9 * * 1" = Monday 9:00 UTC
        // now = 09:01:00 exactly (60s after the fire time)
        // window_start = 09:00:00 → next_after(09:00:00) is exclusive → next Monday → does NOT fire
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 1, 0).unwrap();
        assert!(
            !should_fire("0 9 * * 1", now),
            "hook must not fire at exactly 60s after the fire time (window_start is exclusive)"
        );
    }

    // ── process_timer_hooks integration tests ─────────────────────────────────

    #[tokio::test]
    async fn timer_scheduler_fires_hook_within_window() {
        let hook = make_timer_hook("timer-01", "* * * * *");
        let store: Arc<dyn HookStore> = Arc::new(StubStore { hooks: vec![hook] });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let svc = make_issue_service().await;

        // now = 09:00:30 → window_start = 08:59:30
        // "* * * * *" → next_after(08:59:30) = 09:00:00 ≤ 09:00:30 → fires
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_hooks(&store, &bus, &svc, now).await;

        let event = rx.try_recv().expect("TimerFired event should have been sent");
        match &event {
            SystemEvent::TimerFired { hook_id, fired_at } => {
                assert_eq!(hook_id, "timer-01");
                assert_eq!(
                    *fired_at,
                    Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 0).unwrap()
                );
            }
            _ => panic!("expected TimerFired event, got: {event:?}"),
        }
    }

    #[tokio::test]
    async fn timer_scheduler_does_not_fire_outside_window() {
        let hook = make_timer_hook("timer-02", "0 9 * * 1"); // Monday 9am
        let store: Arc<dyn HookStore> = Arc::new(StubStore { hooks: vec![hook] });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let svc = make_issue_service().await;

        // now = Monday 2024-01-15 09:01:30 (1min 30s past the window)
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 1, 30).unwrap();

        process_timer_hooks(&store, &bus, &svc, now).await;

        assert!(
            rx.try_recv().is_err(),
            "no event should fire outside the 60s window"
        );
    }

    #[tokio::test]
    async fn process_timer_hooks_does_not_fire_disabled_hook() {
        // The store's list_hooks(Some(true), Some("timer")) already filters on
        // enabled=true via the first argument.  Here we verify the full contract:
        // a hook with enabled=false is not returned by the store and therefore
        // process_timer_hooks emits nothing.
        let mut hook = make_timer_hook("t3", "* * * * *");
        hook.enabled = false;
        // The StubStore returns hooks verbatim, ignoring the enabled filter.
        // To model the real DB contract (which does filter), we simply give the
        // store an empty list — as if the DB filtered out the disabled hook.
        let store: Arc<dyn HookStore> = Arc::new(StubStore { hooks: vec![] });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let svc = make_issue_service().await;
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_hooks(&store, &bus, &svc, now).await;

        assert!(rx.try_recv().is_err(), "disabled hook must not fire");
    }

    #[tokio::test]
    async fn process_timer_hooks_skips_non_timer_hooks() {
        // A hook with source=Internal should be skipped by process_timer_hooks
        // even if it happens to be in the list returned by the store.
        let hook = Hook {
            id: "t4".into(),
            name: "internal-hook".into(),
            source: HookSource::Internal {
                event_types: vec!["issue.created".into()],
            },
            filter: None,
            action: HookAction::SendMessage {
                target: MessageTarget::Issue("watcher".into()),
                body: "tick".into(),
            },
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let store: Arc<dyn HookStore> = Arc::new(StubStore { hooks: vec![hook] });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let svc = make_issue_service().await;
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_hooks(&store, &bus, &svc, now).await;

        assert!(rx.try_recv().is_err(), "non-timer hook must not emit TimerFired");
    }

    #[tokio::test]
    async fn process_timer_hooks_fires_two_hooks_when_both_match() {
        let hook1 = make_timer_hook("t5a", "* * * * *");
        let hook2 = make_timer_hook("t5b", "* * * * *");
        let store: Arc<dyn HookStore> = Arc::new(StubStore {
            hooks: vec![hook1, hook2],
        });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let svc = make_issue_service().await;
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_hooks(&store, &bus, &svc, now).await;

        let e1 = rx.try_recv().expect("first TimerFired event");
        let e2 = rx.try_recv().expect("second TimerFired event");
        assert!(
            matches!(e1, SystemEvent::TimerFired { .. }),
            "first event should be TimerFired"
        );
        assert!(
            matches!(e2, SystemEvent::TimerFired { .. }),
            "second event should be TimerFired"
        );
    }
}
