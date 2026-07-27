//! Session interpretation: pure replay of an Event Log into Status, Session
//! lifecycle, running Subagents, and recent Activity. The only stateful input
//! is `now`, for staleness.

use chrono::{DateTime, Duration, Utc};

use crate::protocol::{AttentionKind, Event, EventKind};

const ACTIVITY_TAIL: usize = 64;

/// Variant order is priority order: most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Attention(AttentionKind),
    Error,
    Stale,
    Working,
    Done,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interpretation {
    pub status: Status,
    /// When the current Status was established; None before the first
    /// Status-relevant event.
    pub since: Option<DateTime<Utc>>,
    /// One-line context for the status: what is running (Working), what was
    /// concluded (Done), what is awaited (Attention), what failed (Error).
    pub detail: Option<String>,
    /// Whether the latest Status-relevant event ended the Session. Ended is
    /// not a Status: a still-live Agent whose Session ended is Idle.
    pub ended: bool,
    /// Subagents currently running inside this Session, in start order.
    pub subagents: Vec<Subagent>,
    /// Recent Activity in Event Log order.
    pub activity: Vec<ActivityEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Focus,
    Session,
    Turn,
    Tool,
    Approval,
    Question,
    Done,
    Error,
    SubagentStarted,
    SubagentEnded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    pub at: Option<DateTime<Utc>>,
    pub kind: ActivityKind,
    pub detail: String,
}

/// A subagent running inside a session, replayed from start/end events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subagent {
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub summary: Option<String>,
    pub since: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DerivedStatus {
    status: Status,
    since: DateTime<Utc>,
    detail: Option<String>,
    ended: bool,
}

/// Events must be in log order with timestamps stamped by the store.
/// Attention is eventually consistent: any later event clears it, so status
/// is a function of the last event (plus `now` for staleness). An idle agent
/// never goes stale; only an apparently-working one does.
///
/// Subagent and focus events are status-neutral and skipped entirely: a
/// subagent finishing or foreground-session change must not clear the
/// parent's Attention or revive a Done session.
pub fn interpret(events: &[Event], now: DateTime<Utc>, stale_after: Duration) -> Interpretation {
    let derived = derive_status(events, now, stale_after);
    Interpretation {
        status: derived
            .as_ref()
            .map(|derived| derived.status)
            .unwrap_or(Status::Idle),
        since: derived.as_ref().map(|derived| derived.since),
        detail: derived.as_ref().and_then(|derived| derived.detail.clone()),
        ended: derived.is_some_and(|derived| derived.ended),
        subagents: derive_subagents(events),
        activity: derive_activity(events),
    }
}

fn derive_status(
    events: &[Event],
    now: DateTime<Utc>,
    stale_after: Duration,
) -> Option<DerivedStatus> {
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
        return focus.map(|event| DerivedStatus {
            status: Status::Idle,
            since: ts(event),
            detail: None,
            ended: false,
        });
    };
    let derived = |status, detail, ended| {
        Some(DerivedStatus {
            status,
            since: ts(last),
            detail,
            ended,
        })
    };
    match &last.kind {
        EventKind::SessionFocus => unreachable!("focus events are filtered above"),
        EventKind::SessionEnd => derived(Status::Idle, None, true),
        EventKind::SessionStart { .. } => derived(Status::Idle, None, false),
        EventKind::Attention { attention, summary } => {
            derived(Status::Attention(*attention), summary.clone(), false)
        }
        EventKind::TurnEnd { summary } => derived(Status::Done, summary.clone(), false),
        EventKind::TurnError { reason, summary } => {
            let detail = match (reason, summary) {
                (Some(reason), Some(summary)) => Some(format!("{reason}: {summary}")),
                (reason, summary) => reason.clone().or_else(|| summary.clone()),
            };
            derived(Status::Error, detail, false)
        }
        EventKind::TurnStart { .. } | EventKind::Heartbeat { .. } => {
            let status = if now - ts(last) > stale_after {
                Status::Stale
            } else {
                Status::Working
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
            Some(DerivedStatus {
                status,
                since,
                detail: working_detail(&events),
                ended: false,
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
/// order. A turn or session boundary clears the set: subagents cannot outlive
/// the turn that spawned them, so a turn ending (or the next one starting)
/// bounds ghosts left by a missed end event — otherwise a Done agent keeps
/// showing subagents forever.
fn derive_subagents(events: &[Event]) -> Vec<Subagent> {
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
            EventKind::TurnStart { .. }
            | EventKind::TurnEnd { .. }
            | EventKind::TurnError { .. }
            | EventKind::SessionStart { .. }
            | EventKind::SessionEnd => running.clear(),
            _ => {}
        }
    }
    running.into_iter().map(|(_, subagent)| subagent).collect()
}

fn derive_activity(events: &[Event]) -> Vec<ActivityEntry> {
    events[events.len().saturating_sub(ACTIVITY_TAIL)..]
        .iter()
        .map(|event| {
            let (kind, detail) = match &event.kind {
                EventKind::SessionFocus => (ActivityKind::Focus, "foreground".into()),
                EventKind::SessionStart { model } => {
                    (ActivityKind::Session, model.clone().unwrap_or_default())
                }
                EventKind::TurnStart { summary } => {
                    (ActivityKind::Turn, summary.clone().unwrap_or_default())
                }
                EventKind::Heartbeat { activity } => {
                    (ActivityKind::Tool, activity.clone().unwrap_or_default())
                }
                EventKind::Attention { attention, summary } => {
                    let kind = match attention {
                        AttentionKind::Approval => ActivityKind::Approval,
                        AttentionKind::Question => ActivityKind::Question,
                    };
                    (kind, summary.clone().unwrap_or_default())
                }
                EventKind::TurnEnd { summary } => {
                    (ActivityKind::Done, summary.clone().unwrap_or_default())
                }
                EventKind::TurnError { reason, summary } => (
                    ActivityKind::Error,
                    summary
                        .clone()
                        .or_else(|| reason.clone())
                        .unwrap_or_default(),
                ),
                EventKind::SubagentStart {
                    agent_type,
                    summary,
                    ..
                } => (
                    ActivityKind::SubagentStarted,
                    [agent_type.as_deref(), summary.as_deref()]
                        .into_iter()
                        .flatten()
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · "),
                ),
                EventKind::SubagentEnd { agent } => (ActivityKind::SubagentEnded, agent.clone()),
                EventKind::SessionEnd => (ActivityKind::Session, "ended".into()),
            };
            ActivityEntry {
                at: event.ts,
                kind,
                detail,
            }
        })
        .collect()
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

    fn state(events: &[Event], now: DateTime<Utc>) -> Interpretation {
        interpret(events, now, STALE)
    }

    #[test]
    fn empty_log_is_idle() {
        let state = state(&[], at(0));
        assert_eq!(state.status, Status::Idle);
        assert_eq!(state.since, None);
        assert!(!state.ended);
        assert!(state.subagents.is_empty());
        assert!(state.activity.is_empty());
    }

    #[test]
    fn focus_is_status_neutral_and_focus_only_is_idle() {
        let focus = ev(20, EventKind::SessionFocus);
        for (kind, expected) in [
            (
                EventKind::TurnEnd {
                    summary: Some("done".into()),
                },
                Status::Done,
            ),
            (
                EventKind::TurnError {
                    reason: Some("bad".into()),
                    summary: None,
                },
                Status::Error,
            ),
            (turn_start(Some("work")), Status::Working),
            (attention(), Status::Attention(AttentionKind::Approval)),
        ] {
            let events = [ev(10, kind), focus.clone()];
            let derived = state(&events, at(21));
            assert_eq!(derived.status, expected);
            assert_eq!(derived.since, Some(at(10)));
        }
        let derived = state(&[focus], at(21));
        assert_eq!(derived.status, Status::Idle);
        assert_eq!(derived.since, Some(at(20)));
    }

    #[test]
    fn working_since_turn_start_with_activity_and_task_detail() {
        let events = [
            ev(0, EventKind::SessionStart { model: None }),
            ev(10, turn_start(Some("fix the tests"))),
            ev(20, heartbeat(Some("Bash"))),
            ev(30, heartbeat(Some("Edit"))),
        ];
        let d = state(&events, at(40));
        assert_eq!(d.status, Status::Working);
        assert_eq!(d.since, Some(at(10)));
        assert_eq!(d.detail.as_deref(), Some("Edit · fix the tests"));
    }

    #[test]
    fn task_detail_survives_attention_interruptions() {
        let events = [
            ev(0, turn_start(Some("fix the tests"))),
            ev(10, attention()),
            ev(20, heartbeat(None)),
        ];
        let d = state(&events, at(30));
        assert_eq!(d.status, Status::Working);
        assert_eq!(d.detail.as_deref(), Some("fix the tests"));
        assert_eq!(d.since, Some(at(20)));
    }

    #[test]
    fn attention_is_cleared_by_any_later_event() {
        let events = [ev(0, attention()), ev(10, heartbeat(None))];
        let d = state(&events, at(20));
        assert_eq!(d.status, Status::Working);

        let d = state(&events[..1], at(20));
        assert_eq!(d.status, Status::Attention(AttentionKind::Approval));
    }

    #[test]
    fn silent_working_goes_stale_but_done_never_does() {
        let working = [ev(0, turn_start(None))];
        assert_eq!(state(&working, at(60 * 60)).status, Status::Stale);

        let done = [ev(0, EventKind::TurnEnd { summary: None })];
        assert_eq!(state(&done, at(60 * 60)).status, Status::Done);
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
        let d = state(&events, at(60));
        assert_eq!(d.status, Status::Done);
        assert_eq!(d.since, Some(at(50)));
        assert_eq!(d.detail.as_deref(), Some("all green"));

        let fresh = [ev(
            0,
            EventKind::SessionStart {
                model: Some("m".into()),
            },
        )];
        assert_eq!(state(&fresh, at(10)).status, Status::Idle);
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
        let d = state(&bare, at(10));
        assert_eq!(d.status, Status::Error);
        assert_eq!(d.detail.as_deref(), Some("rate_limit"));

        let explained = [ev(
            0,
            EventKind::TurnError {
                reason: Some("billing_error".into()),
                summary: Some("credit exhausted".into()),
            },
        )];
        let d = state(&explained, at(10));
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
        let d = state(&blocked, at(20));
        assert_eq!(d.status, Status::Attention(AttentionKind::Approval));
        assert_eq!(d.since, Some(at(0)));

        // Nor revive a finished turn.
        let done = [
            ev(0, EventKind::TurnEnd { summary: None }),
            ev(10, subagent_end("a1")),
        ];
        assert_eq!(state(&done, at(20)).status, Status::Done);

        // A log holding only subagent events is Idle without a Status timestamp.
        let subagent_only = state(&[ev(0, subagent_end("a1"))], at(10));
        assert_eq!(subagent_only.status, Status::Idle);
        assert_eq!(subagent_only.since, None);

        // Interleaved subagent events don't break the working-since scan.
        let working = [
            ev(0, turn_start(Some("fix the tests"))),
            ev(10, subagent_start("a1", Some("Explore"), None)),
            ev(20, heartbeat(Some("Bash"))),
        ];
        let d = state(&working, at(30));
        assert_eq!(d.status, Status::Working);
        assert_eq!(d.since, Some(at(0)));
        assert_eq!(d.detail.as_deref(), Some("Bash · fix the tests"));
    }

    #[test]
    fn subagents_replays_starts_and_ends() {
        let events = [
            ev(0, subagent_start("a1", Some("Explore"), Some("haiku"))),
            ev(10, subagent_start("a2", Some("Plan"), None)),
            ev(20, subagent_end("a1")),
        ];
        let running = state(&events, at(30)).subagents;
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].agent_type.as_deref(), Some("Plan"));
        assert_eq!(running[0].since, at(10));

        // A restarted id replaces the earlier entry instead of duplicating it.
        let restarted = [
            ev(0, subagent_start("a1", Some("Explore"), None)),
            ev(10, subagent_start("a1", Some("Explore"), Some("opus"))),
        ];
        let running = state(&restarted, at(20)).subagents;
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
        let running = state(&events, at(40)).subagents;
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].since, at(30));
    }

    #[test]
    fn turn_boundaries_clear_subagents() {
        // A subagent cannot outlive its turn: a missed end event must not keep
        // a Done agent showing subagents forever.
        let done = [
            ev(0, turn_start(None)),
            ev(10, subagent_start("a1", Some("Explore"), None)),
            ev(20, EventKind::TurnEnd { summary: None }),
        ];
        assert!(state(&done, at(30)).subagents.is_empty());

        // A turn erroring clears them too.
        let errored = [
            ev(0, subagent_start("a1", None, None)),
            ev(
                10,
                EventKind::TurnError {
                    reason: None,
                    summary: None,
                },
            ),
        ];
        assert!(state(&errored, at(20)).subagents.is_empty());

        // The next turn starting bounds a ghost from a missed TurnEnd.
        let next_turn = [
            ev(0, subagent_start("a1", None, None)),
            ev(10, turn_start(None)),
            ev(20, subagent_start("a2", Some("Plan"), None)),
        ];
        let running = state(&next_turn, at(30)).subagents;
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].agent_type.as_deref(), Some("Plan"));
        assert_eq!(running[0].since, at(20));

        // A subagent running mid-turn still shows while the turn works.
        let working = [
            ev(0, turn_start(None)),
            ev(10, subagent_start("a1", Some("Explore"), None)),
            ev(20, heartbeat(Some("Bash"))),
        ];
        assert_eq!(state(&working, at(30)).subagents.len(), 1);
    }

    #[test]
    fn mismatched_late_end_does_not_ghost_a_done_agent() {
        // The real bug: Claude's SubagentStop carried a different agent_id than
        // SubagentStart AND arrived turns later, so id-based removal never
        // fired. Turn-boundary clearing must still leave a Done agent clean.
        let events = [
            ev(0, turn_start(None)),
            ev(10, subagent_start("a57", Some("Explore"), None)),
            ev(20, EventKind::TurnEnd { summary: None }),
            ev(30, turn_start(None)),
            ev(40, EventKind::TurnEnd { summary: None }),
            ev(50, subagent_end("a170")), // wrong id, three turns late
        ];
        let state = state(&events, at(60));
        assert_eq!(state.status, Status::Done);
        assert!(state.subagents.is_empty());
    }

    #[test]
    fn maps_every_event_kind_to_activity() {
        let events = [
            ev(0, EventKind::SessionFocus),
            ev(
                1,
                EventKind::SessionStart {
                    model: Some("opus".into()),
                },
            ),
            ev(2, turn_start(Some("implement activity"))),
            ev(3, heartbeat(Some("cargo test"))),
            ev(
                4,
                EventKind::Attention {
                    attention: AttentionKind::Approval,
                    summary: Some("run command".into()),
                },
            ),
            ev(
                5,
                EventKind::Attention {
                    attention: AttentionKind::Question,
                    summary: Some("which option".into()),
                },
            ),
            ev(
                6,
                EventKind::TurnEnd {
                    summary: Some("finished".into()),
                },
            ),
            ev(
                7,
                EventKind::TurnError {
                    reason: Some("rate_limit".into()),
                    summary: Some("try later".into()),
                },
            ),
            ev(
                8,
                EventKind::SubagentStart {
                    agent: "agent-1".into(),
                    agent_type: Some("Explore".into()),
                    model: Some("haiku".into()),
                    summary: Some("find tests".into()),
                },
            ),
            ev(
                9,
                EventKind::SubagentEnd {
                    agent: "agent-1".into(),
                },
            ),
            ev(10, EventKind::SessionEnd),
        ];

        let activity = state(&events, at(11)).activity;
        assert_eq!(
            activity
                .iter()
                .map(|entry| (entry.kind, entry.detail.as_str()))
                .collect::<Vec<_>>(),
            [
                (ActivityKind::Focus, "foreground"),
                (ActivityKind::Session, "opus"),
                (ActivityKind::Turn, "implement activity"),
                (ActivityKind::Tool, "cargo test"),
                (ActivityKind::Approval, "run command"),
                (ActivityKind::Question, "which option"),
                (ActivityKind::Done, "finished"),
                (ActivityKind::Error, "try later"),
                (ActivityKind::SubagentStarted, "Explore · find tests"),
                (ActivityKind::SubagentEnded, "agent-1"),
                (ActivityKind::Session, "ended"),
            ]
        );
        assert_eq!(activity[0].at, Some(at(0)));
        assert_eq!(activity[10].at, Some(at(10)));
    }

    #[test]
    fn activity_applies_text_fallbacks() {
        let events = [
            ev(
                0,
                EventKind::TurnError {
                    reason: Some("rate_limit".into()),
                    summary: None,
                },
            ),
            ev(
                1,
                EventKind::SubagentStart {
                    agent: "agent-1".into(),
                    agent_type: None,
                    model: None,
                    summary: Some("inspect".into()),
                },
            ),
            ev(2, EventKind::SessionStart { model: None }),
        ];

        let activity = state(&events, at(3)).activity;
        assert_eq!(activity[0].detail, "rate_limit");
        assert_eq!(activity[1].detail, "inspect");
        assert_eq!(activity[2].detail, "");
    }

    #[test]
    fn activity_keeps_the_latest_64_events() {
        let events: Vec<_> = (0..65)
            .map(|secs| ev(secs, heartbeat(Some(&secs.to_string()))))
            .collect();

        let activity = state(&events, at(65)).activity;
        assert_eq!(activity.len(), ACTIVITY_TAIL);
        assert_eq!(activity[0].at, Some(at(1)));
        assert_eq!(activity[0].detail, "1");
        assert_eq!(activity[63].at, Some(at(64)));
        assert_eq!(activity[63].detail, "64");
    }

    #[test]
    fn session_end_is_separate_from_status_and_a_later_start_reopens_it() {
        let ended = [ev(0, attention()), ev(10, EventKind::SessionEnd)];
        let ended_state = state(&ended, at(20));
        assert_eq!(ended_state.status, Status::Idle);
        assert_eq!(ended_state.since, Some(at(10)));
        assert!(ended_state.ended);

        let reopened = [
            ev(0, EventKind::SessionEnd),
            ev(10, EventKind::SessionStart { model: None }),
        ];
        let reopened_state = state(&reopened, at(20));
        assert_eq!(reopened_state.status, Status::Idle);
        assert!(!reopened_state.ended);
    }
}
