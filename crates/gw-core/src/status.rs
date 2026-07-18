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

/// A subagent running inside a session, replayed from start/end events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subagent {
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub summary: Option<String>,
    pub since: DateTime<Utc>,
}

/// Events must be in log order with timestamps stamped by the store.
/// Attention is eventually consistent: any later event clears it, so status
/// is a function of the last event (plus `now` for staleness). An idle agent
/// never goes stale; only an apparently-working one does.
///
/// Subagent and focus events are status-neutral and skipped entirely: a
/// subagent finishing or foreground-session change must not clear the
/// parent's Attention or revive a Done session.
pub fn derive(events: &[Event], now: DateTime<Utc>, stale_after: Duration) -> Option<Derived> {
    let focus = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind, EventKind::SessionFocus));
    let events: Vec<&Event> = events
        .iter()
        .filter(|e| !is_subagent(e) && !matches!(e.kind, EventKind::SessionFocus))
        .collect();
    let ts = |e: &Event| e.ts.expect("stored events carry ts");
    let Some(last) = events.last().copied() else {
        return focus.map(|event| Derived {
            status: SessionStatus::Idle,
            since: ts(event),
            detail: None,
        });
    };
    let derived = |status, detail| {
        Some(Derived {
            status,
            since: ts(last),
            detail,
        })
    };
    match &last.kind {
        EventKind::SessionFocus => unreachable!("focus events are filtered above"),
        EventKind::SessionEnd => derived(SessionStatus::Ended, None),
        EventKind::SessionStart { .. } => derived(SessionStatus::Idle, None),
        EventKind::Attention { attention, summary } => {
            derived(SessionStatus::Attention(*attention), summary.clone())
        }
        EventKind::TurnEnd { summary } => derived(SessionStatus::Done, summary.clone()),
        EventKind::TurnError { reason, summary } => {
            let detail = match (reason, summary) {
                (Some(reason), Some(summary)) => Some(format!("{reason}: {summary}")),
                (reason, summary) => reason.clone().or_else(|| summary.clone()),
            };
            derived(SessionStatus::Error, detail)
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
                .map(|e| ts(e))
                .unwrap();
            Some(Derived {
                status,
                since,
                detail: working_detail(&events),
            })
        }
        EventKind::SubagentStart { .. } | EventKind::SubagentEnd { .. } => {
            unreachable!("subagent events are filtered above")
        }
    }
}

fn is_subagent(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::SubagentStart { .. } | EventKind::SubagentEnd { .. }
    )
}

/// Replay subagent start/end pairs into the currently running set, in start
/// order. A session boundary clears the set: subagents cannot outlive their
/// session, and this bounds ghosts from a missed end event.
pub fn subagents(events: &[Event]) -> Vec<Subagent> {
    let mut running: Vec<(&str, Subagent)> = Vec::new();
    for event in events {
        match &event.kind {
            EventKind::SubagentStart {
                agent,
                agent_type,
                model,
                summary,
            } => {
                running.retain(|(id, _)| id != agent);
                running.push((
                    agent,
                    Subagent {
                        agent_type: agent_type.clone(),
                        model: model.clone(),
                        summary: summary.clone(),
                        since: event.ts.expect("stored events carry ts"),
                    },
                ));
            }
            EventKind::SubagentEnd { agent } => running.retain(|(id, _)| id != agent),
            EventKind::SessionStart { .. } | EventKind::SessionEnd => running.clear(),
            _ => {}
        }
    }
    running.into_iter().map(|(_, subagent)| subagent).collect()
}

/// "activity · task": the latest heartbeat's activity plus the current
/// turn's prompt excerpt. The turn's start is searched past interruptions
/// (attention events sit inside a turn), stopping at any turn boundary.
fn working_detail(events: &[&Event]) -> Option<String> {
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
    fn focus_is_status_neutral_and_focus_only_is_idle() {
        let focus = ev(20, EventKind::SessionFocus);
        for (kind, expected) in [
            (
                EventKind::TurnEnd {
                    summary: Some("done".into()),
                },
                SessionStatus::Done,
            ),
            (
                EventKind::TurnError {
                    reason: Some("bad".into()),
                    summary: None,
                },
                SessionStatus::Error,
            ),
            (turn_start(Some("work")), SessionStatus::Working),
            (
                attention(),
                SessionStatus::Attention(AttentionKind::Approval),
            ),
        ] {
            let events = [ev(10, kind), focus.clone()];
            let derived = derive(&events, at(21), STALE).unwrap();
            assert_eq!(derived.status, expected);
            assert_eq!(derived.since, at(10));
        }
        let derived = derive(&[focus], at(21), STALE).unwrap();
        assert_eq!(derived.status, SessionStatus::Idle);
        assert_eq!(derived.since, at(20));
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
                reason: Some("rate_limit".into()),
                summary: None,
            },
        )];
        let d = derive(&bare, at(10), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Error);
        assert_eq!(d.detail.as_deref(), Some("rate_limit"));

        let explained = [ev(
            0,
            EventKind::TurnError {
                reason: Some("billing_error".into()),
                summary: Some("credit exhausted".into()),
            },
        )];
        let d = derive(&explained, at(10), STALE).unwrap();
        assert_eq!(d.detail.as_deref(), Some("billing_error: credit exhausted"));
    }

    fn subagent_start(agent: &str, agent_type: Option<&str>, model: Option<&str>) -> EventKind {
        EventKind::SubagentStart {
            agent: agent.into(),
            agent_type: agent_type.map(str::to_owned),
            model: model.map(str::to_owned),
            summary: None,
        }
    }

    fn subagent_end(agent: &str) -> EventKind {
        EventKind::SubagentEnd {
            agent: agent.into(),
        }
    }

    #[test]
    fn subagent_events_are_status_neutral() {
        // A subagent finishing must not clear a pending approval.
        let blocked = [ev(0, attention()), ev(10, subagent_end("a1"))];
        let d = derive(&blocked, at(20), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Attention(AttentionKind::Approval));
        assert_eq!(d.since, at(0));

        // Nor revive a finished turn.
        let done = [
            ev(0, EventKind::TurnEnd { summary: None }),
            ev(10, subagent_end("a1")),
        ];
        assert_eq!(
            derive(&done, at(20), STALE).unwrap().status,
            SessionStatus::Done
        );

        // A log holding only subagent events derives nothing.
        assert_eq!(derive(&[ev(0, subagent_end("a1"))], at(10), STALE), None);

        // Interleaved subagent events don't break the working-since scan.
        let working = [
            ev(0, turn_start(Some("fix the tests"))),
            ev(10, subagent_start("a1", Some("Explore"), None)),
            ev(20, heartbeat(Some("Bash"))),
        ];
        let d = derive(&working, at(30), STALE).unwrap();
        assert_eq!(d.status, SessionStatus::Working);
        assert_eq!(d.since, at(0));
        assert_eq!(d.detail.as_deref(), Some("Bash · fix the tests"));
    }

    #[test]
    fn subagents_replays_starts_and_ends() {
        let events = [
            ev(0, subagent_start("a1", Some("Explore"), Some("haiku"))),
            ev(10, subagent_start("a2", Some("Plan"), None)),
            ev(20, subagent_end("a1")),
        ];
        let running = subagents(&events);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].agent_type.as_deref(), Some("Plan"));
        assert_eq!(running[0].since, at(10));

        // A restarted id replaces the earlier entry instead of duplicating it.
        let restarted = [
            ev(0, subagent_start("a1", Some("Explore"), None)),
            ev(10, subagent_start("a1", Some("Explore"), Some("opus"))),
        ];
        let running = subagents(&restarted);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].model.as_deref(), Some("opus"));
        assert_eq!(running[0].since, at(10));
    }

    #[test]
    fn session_boundaries_clear_subagents() {
        let events = [
            ev(0, subagent_start("a1", None, None)),
            ev(10, EventKind::SessionEnd),
            ev(20, EventKind::SessionStart { model: None }),
            ev(30, subagent_start("a2", None, None)),
        ];
        let running = subagents(&events);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].since, at(30));
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
