use crate::cron::next_after;
use chrono::{DateTime, Duration, Utc};
use db::EventStore;
use events::{EventBus, SystemEvent};
use std::sync::Arc;

/// Check whether a timer event should fire at the given `now` time.
///
/// An event fires if the next cron tick after `(now - 60 seconds)` is ≤ `now`.
/// This provides a 60-second rolling window so no ticks are missed.
#[must_use]
pub fn should_fire(schedule: &str, now: DateTime<Utc>) -> bool {
    let window_start = now - Duration::seconds(60);
    match next_after(schedule, window_start) {
        Ok(next_fire) => next_fire <= now,
        Err(e) => {
            tracing::warn!("Timer event: invalid schedule '{schedule}': {e}");
            false
        }
    }
}

/// Process all enabled timer events for a single scheduler tick at `now`.
///
/// Checks each event's schedule against the 60-second rolling window and, for
/// those that match, emits a `SystemEvent::TimerFired` on the event bus.
pub(crate) async fn process_timer_events(
    event_store: &Arc<dyn EventStore>,
    event_bus: &EventBus,
    now: DateTime<Utc>,
) {
    let events = event_store.list_events().await.unwrap_or_default();

    for event in events {
        if !event.enabled {
            continue;
        }
        let types::EventKind::Timer { ref schedule } = event.kind else {
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
            event_id: event.id.clone(),
            event_name: event.name.clone(),
            fired_at,
        });
    }
}

/// Run the timer scheduler loop.
///
/// Wakes every 30 seconds and fires any enabled timer events whose schedule
/// falls within the last 60-second window.
pub async fn run_timer_scheduler(event_store: Arc<dyn EventStore>, event_bus: EventBus) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
    // First tick fires immediately; we skip it to avoid firing on startup.
    interval.tick().await;

    loop {
        interval.tick().await;
        let now = Utc::now();
        process_timer_events(&event_store, &event_bus, now).await;
    }
}

/// Spawn the timer scheduler as a background `tokio::task`.
pub fn spawn_timer_scheduler(event_store: &Arc<dyn EventStore>, event_bus: &EventBus) {
    let event_store = Arc::clone(event_store);
    let event_bus = event_bus.clone();
    tokio::spawn(async move {
        run_timer_scheduler(event_store, event_bus).await;
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use types::{Event, EventKind};

    // ── Single stub store ─────────────────────────────────────────────────────

    struct StubEventStore {
        events: Vec<Event>,
    }

    #[async_trait]
    impl EventStore for StubEventStore {
        async fn create_event(&self, _e: &Event) -> db::Result<()> {
            Ok(())
        }
        async fn get_event(&self, _id: &str) -> db::Result<Event> {
            Err(db::Error::NotFound)
        }
        async fn get_event_by_name(&self, _name: &str) -> db::Result<Event> {
            Err(db::Error::NotFound)
        }
        async fn list_events(&self) -> db::Result<Vec<Event>> {
            Ok(self.events.clone())
        }
        async fn delete_event(&self, _id: &str) -> db::Result<()> {
            Ok(())
        }
    }

    fn make_timer_event(id: &str, name: &str, schedule: &str) -> Event {
        Event {
            id: id.into(),
            name: name.into(),
            kind: EventKind::Timer {
                schedule: schedule.into(),
            },
            description: None,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[allow(dead_code)]
    async fn make_issue_service() -> issues::IssueService {
        let (db, _hook_store, _event_store) = db::connect("sqlite::memory:").await.unwrap();
        let backend: std::sync::Arc<dyn issue_backend::IssueBackend> =
            std::sync::Arc::new(issue_backend::SqliteIssueBackend::new(std::sync::Arc::clone(&db)));
        issues::IssueService::new(db, backend)
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
            "event should fire 30s into the window"
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
            "event should not fire 1min 30s after the window"
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

    /// Boundary: at exactly 60 seconds after the fire time, the event must still
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
            "event must not fire at exactly 60s after the fire time (window_start is exclusive)"
        );
    }

    // ── process_timer_events tests ────────────────────────────────────────────

    #[tokio::test]
    async fn timer_scheduler_fires_event_within_window() {
        let event = make_timer_event("timer-01", "heartbeat", "* * * * *");
        let store: Arc<dyn EventStore> = Arc::new(StubEventStore {
            events: vec![event],
        });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();

        // now = 09:00:30 → window_start = 08:59:30
        // "* * * * *" → next_after(08:59:30) = 09:00:00 ≤ 09:00:30 → fires
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_events(&store, &bus, now).await;

        let sys_event = rx.try_recv().expect("TimerFired event should have been sent");
        match &sys_event {
            SystemEvent::TimerFired {
                event_id,
                event_name,
                fired_at,
            } => {
                assert_eq!(event_id, "timer-01");
                assert_eq!(event_name, "heartbeat");
                assert_eq!(
                    *fired_at,
                    Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 0).unwrap()
                );
            }
            _ => panic!("expected TimerFired event, got: {sys_event:?}"),
        }
    }

    #[tokio::test]
    async fn timer_scheduler_does_not_fire_outside_window() {
        let event = make_timer_event("timer-02", "weekly", "0 9 * * 1");
        let store: Arc<dyn EventStore> = Arc::new(StubEventStore {
            events: vec![event],
        });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();

        // now = Monday 2024-01-15 09:01:30 (1min 30s past the window)
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 1, 30).unwrap();

        process_timer_events(&store, &bus, now).await;

        assert!(
            rx.try_recv().is_err(),
            "no event should fire outside the 60s window"
        );
    }

    #[tokio::test]
    async fn process_timer_events_does_not_fire_disabled_event() {
        let mut event = make_timer_event("t3", "disabled-timer", "* * * * *");
        event.enabled = false;
        // Store returns the disabled event; process_timer_events should skip it
        let store: Arc<dyn EventStore> = Arc::new(StubEventStore {
            events: vec![event],
        });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_events(&store, &bus, now).await;

        assert!(rx.try_recv().is_err(), "disabled event must not fire");
    }

    #[tokio::test]
    async fn process_timer_events_skips_webhook_events() {
        // A Webhook event should be skipped by process_timer_events
        let event = Event {
            id: "w1".into(),
            name: "ci-complete".into(),
            kind: EventKind::Webhook { secret: None },
            description: None,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let store: Arc<dyn EventStore> = Arc::new(StubEventStore {
            events: vec![event],
        });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_events(&store, &bus, now).await;

        assert!(rx.try_recv().is_err(), "webhook event must not emit TimerFired");
    }

    #[tokio::test]
    async fn process_timer_events_fires_two_events_when_both_match() {
        let event1 = make_timer_event("t5a", "tick1", "* * * * *");
        let event2 = make_timer_event("t5b", "tick2", "* * * * *");
        let store: Arc<dyn EventStore> = Arc::new(StubEventStore {
            events: vec![event1, event2],
        });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_events(&store, &bus, now).await;

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

    #[tokio::test]
    async fn process_timer_events_emits_correct_event_name() {
        let event = make_timer_event("timer-name-01", "my-timer", "* * * * *");
        let store: Arc<dyn EventStore> = Arc::new(StubEventStore {
            events: vec![event],
        });
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();

        process_timer_events(&store, &bus, now).await;

        let sys_event = rx.try_recv().expect("TimerFired event expected");
        match &sys_event {
            SystemEvent::TimerFired { event_name, .. } => {
                assert_eq!(event_name, "my-timer");
            }
            _ => panic!("expected TimerFired, got: {sys_event:?}"),
        }
    }
}
