//! Status derivation: a pure function from an event sequence to a status.
//! The only stateful input is `now`, for staleness.

use chrono::{DateTime, Duration, Utc};

use crate::protocol::{AttentionKind, Event, EventKind};

/// Variant order is priority order: most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SessionStatus {
    Attention(AttentionKind),
    Error,
    Stale,
    Working,
    Done,
    Idle,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub status: SessionStatus,
    /// When the current status was established.
    pub since: DateTime<Utc>,
    /// One-line context for the status: what is running (Working), what was
    /// concluded (Done), what is awaited (Attention), what failed (Error).
    pub detail: Option<String>,
}

/// Events must be in log order with timestamps stamped by the store.
/// Attention is eventually consistent: any later event clears it, so status
/// is a function of the last event (plus `now` for staleness). An idle agent
/// never goes stale; only an apparently-working one does.
pub fn derive(events: &[Event], now: DateTime<Utc>, stale_after: Duration) -> Option<Derived> {
    let ts = |e: &Event| e.ts.expect("stored events carry ts");
    let last = events.last()?;
    let derived = |status, detail| {
        Some(Derived {
            status,
            since: ts(last),
            detail,
        })
    };
    match &last.kind {
        EventKind::SessionEnd => derived(SessionStatus::Ended, None),
        EventKind::SessionStart { .. } => derived(SessionStatus::Idle, None),
        EventKind::Attention { attention, summary } => {
            derived(SessionStatus::Attention(*attention), summary.clone())
        }
        EventKind::TurnEnd { summary } => derived(SessionStatus::Done, summary.clone()),
        EventKind::TurnError { reason, summary } => {
            let detail = match summary {
                Some(summary) => format!("{reason}: {summary}"),
                None => reason.clone(),
            };
            derived(SessionStatus::Error, Some(detail))
        }
        EventKind::TurnStart { .. } | EventKind::Heartbeat { .. } => {
            let status = if now - ts(last) > stale_after {
                SessionStatus::Stale
            } else {
                SessionStatus::Working
            };
            let since = events
                .iter()
                .rev()
                .take_while(|e| {
                    matches!(
                        e.kind,
                        EventKind::TurnStart { .. } | EventKind::Heartbeat { .. }
                    )
                })
                .last()
                .map(ts)
                .unwrap();
            Some(Derived {
                status,
                since,
                detail: working_detail(events),
            })
        }
    }
}

/// "activity · task": the latest heartbeat's activity plus the current
/// turn's prompt excerpt. The turn's start is searched past interruptions
/// (attention events sit inside a turn), stopping at any turn boundary.
fn working_detail(events: &[Event]) -> Option<String> {
    let activity = match &events.last()?.kind {
        EventKind::Heartbeat { activity } => activity.clone(),
        _ => None,
    };
    let task = events
        .iter()
        .rev()
        .take_while(|e| {
            !matches!(
                e.kind,
                EventKind::TurnEnd { .. }
                    | EventKind::TurnError { .. }
                    | EventKind::SessionStart { .. }
                    | EventKind::SessionEnd
            )
        })
        .find_map(|e| match &e.kind {
            EventKind::TurnStart { summary } => Some(summary.clone()),
            _ => None,
        })
        .flatten();
    match (activity, task) {
        (Some(activity), Some(task)) => Some(format!("{activity} · {task}")),
        (Some(activity), None) => Some(activity),
        (None, Some(task)) => Some(task),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(secs: i64, kind: EventKind) -> Event {
        Event {
            v: 1,
            ts: Some(DateTime::from_timestamp(secs, 0).unwrap()),
            session: "s".into(),
            kind,
        }
    }

    fn attention() -> EventKind {
        EventKind::Attention {
            attention: AttentionKind::Approval,
            summary: None,
        }
    }

    fn turn_start(summary: Option<&str>) -> EventKind {
        EventKind::TurnStart {
            summary: summary.map(str::to_owned),
        }
    }

    fn heartbeat(activity: Option<&str>) -> EventKind {
        EventKind::Heartbeat {
            activity: activity.map(str::to_owned),
        }
    }

    const STALE: Duration = Duration::minutes(30);

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn empty_log_is_none() {
        assert_eq!(derive(&[], at(0), STALE), None);
    }

    #[test]
    fn working_since_turn_start_with_activity_and_task_detail() {
        let events = [
            ev(0, EventKind::SessionStart { model: None }),
            ev(10, turn_start(Some("fix the tests"))),
            ev(20, heartbeat(Some("Bash"))),
            ev(30, heartbeat(Some("Edit"))),
        ];
        let d = derive(&events, at(40), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Working);
        assert_eq!(d.since, at(10));
        assert_eq!(d.detail.as_deref(), Some("Edit · fix the tests"));
    }

    #[test]
    fn task_detail_survives_attention_interruptions() {
        let events = [
            ev(0, turn_start(Some("fix the tests"))),
            ev(10, attention()),
            ev(20, heartbeat(None)),
        ];
        let d = derive(&events, at(30), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Working);
        assert_eq!(d.detail.as_deref(), Some("fix the tests"));
        assert_eq!(d.since, at(20));
    }

    #[test]
    fn attention_is_cleared_by_any_later_event() {
        let events = [ev(0, attention()), ev(10, heartbeat(None))];
        let d = derive(&events, at(20), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Working);

        let d = derive(&events[..1], at(20), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Attention(AttentionKind::Approval));
    }

    #[test]
    fn silent_working_goes_stale_but_done_never_does() {
        let working = [ev(0, turn_start(None))];
        assert_eq!(
            derive(&working, at(60 * 60), STALE).unwrap().status,
            SessionStatus::Stale
        );

        let done = [ev(0, EventKind::TurnEnd { summary: None })];
        assert_eq!(
            derive(&done, at(60 * 60), STALE).unwrap().status,
            SessionStatus::Done
        );
    }

    #[test]
    fn turn_end_is_done_with_summary_and_fresh_session_is_idle() {
        let events = [
            ev(0, turn_start(None)),
            ev(
                50,
                EventKind::TurnEnd {
                    summary: Some("all green".into()),
                },
            ),
        ];
        let d = derive(&events, at(60), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Done);
        assert_eq!(d.since, at(50));
        assert_eq!(d.detail.as_deref(), Some("all green"));

        let fresh = [ev(
            0,
            EventKind::SessionStart {
                model: Some("m".into()),
            },
        )];
        assert_eq!(
            derive(&fresh, at(10), STALE).unwrap().status,
            SessionStatus::Idle
        );
    }

    #[test]
    fn turn_error_is_error_with_reason_detail() {
        let bare = [ev(
            0,
            EventKind::TurnError {
                reason: "rate_limit".into(),
                summary: None,
            },
        )];
        let d = derive(&bare, at(10), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Error);
        assert_eq!(d.detail.as_deref(), Some("rate_limit"));

        let explained = [ev(
            0,
            EventKind::TurnError {
                reason: "billing_error".into(),
                summary: Some("credit exhausted".into()),
            },
        )];
        let d = derive(&explained, at(10), STALE).unwrap();
        assert_eq!(d.detail.as_deref(), Some("billing_error: credit exhausted"));
    }

    #[test]
    fn session_end_wins() {
        let events = [ev(0, attention()), ev(10, EventKind::SessionEnd)];
        assert_eq!(
            derive(&events, at(20), STALE).unwrap().status,
            SessionStatus::Ended
        );
    }
}
