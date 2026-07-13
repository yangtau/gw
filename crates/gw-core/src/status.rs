//! Status derivation: a pure function from an event sequence to a status.
//! The only stateful input is `now`, for staleness.

use chrono::{DateTime, Duration, Utc};

use crate::protocol::{AttentionKind, Event, EventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Attention(AttentionKind),
    Working,
    Idle,
    Stale,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Derived {
    pub status: SessionStatus,
    /// When the current status was established.
    pub since: DateTime<Utc>,
}

/// Events must be in log order with timestamps stamped by the store.
/// Attention is eventually consistent: any later event clears it, so status
/// is a function of the last event (plus `now` for staleness). An idle agent
/// never goes stale; only an apparently-working one does.
pub fn derive(events: &[Event], now: DateTime<Utc>, stale_after: Duration) -> Option<Derived> {
    let ts = |e: &Event| e.ts.expect("stored events carry ts");
    let last = events.last()?;
    let derived = |status| Some(Derived { status, since: ts(last) });
    match last.kind {
        EventKind::SessionEnd => derived(SessionStatus::Ended),
        EventKind::Attention { attention, .. } => derived(SessionStatus::Attention(attention)),
        EventKind::TurnEnd | EventKind::SessionStart => derived(SessionStatus::Idle),
        EventKind::TurnStart | EventKind::Heartbeat => {
            let status = if now - ts(last) > stale_after {
                SessionStatus::Stale
            } else {
                SessionStatus::Working
            };
            let since = events
                .iter()
                .rev()
                .take_while(|e| matches!(e.kind, EventKind::TurnStart | EventKind::Heartbeat))
                .last()
                .map(ts)
                .unwrap();
            Some(Derived { status, since })
        }
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
        EventKind::Attention { attention: AttentionKind::Approval, summary: None }
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
    fn working_since_turn_start_across_heartbeats() {
        let events = [
            ev(0, EventKind::SessionStart),
            ev(10, EventKind::TurnStart),
            ev(20, EventKind::Heartbeat),
            ev(30, EventKind::Heartbeat),
        ];
        let d = derive(&events, at(40), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Working);
        assert_eq!(d.since, at(10));
    }

    #[test]
    fn attention_is_cleared_by_any_later_event() {
        let events = [ev(0, attention()), ev(10, EventKind::Heartbeat)];
        let d = derive(&events, at(20), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Working);

        let d = derive(&events[..1], at(20), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Attention(AttentionKind::Approval));
    }

    #[test]
    fn silent_working_goes_stale_but_idle_never_does() {
        let working = [ev(0, EventKind::TurnStart)];
        assert_eq!(derive(&working, at(60 * 60), STALE).unwrap().status, SessionStatus::Stale);

        let idle = [ev(0, EventKind::TurnEnd)];
        assert_eq!(derive(&idle, at(60 * 60), STALE).unwrap().status, SessionStatus::Idle);
    }

    #[test]
    fn turn_end_is_idle_since_that_moment() {
        let events = [ev(0, EventKind::TurnStart), ev(50, EventKind::TurnEnd)];
        let d = derive(&events, at(60), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Idle);
        assert_eq!(d.since, at(50));
    }

    #[test]
    fn session_end_wins() {
        let events = [ev(0, attention()), ev(10, EventKind::SessionEnd)];
        assert_eq!(derive(&events, at(20), STALE).unwrap().status, SessionStatus::Ended);
    }
}
