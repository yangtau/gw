//! Discovery: join live tmux panes, provider processes, and the event log
//! into the panel's data model. Panes are the source of truth for existence;
//! the log is the source of truth for status.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use gw_plugin_protocol::Event;

use crate::plugins::Plugin;
use crate::procs::{self, Proc};
use crate::protocol::{AttentionKind, Manifest};
use crate::status::{self, Derived, SessionStatus, Subagent};
use crate::store::{SessionRecord, Store};
use crate::tmux::{self, Pane, TmuxSessionPane, TopologyRow};

const AGENT_EVENT_TAIL: usize = 64;

/// Variant order is priority order: the panel sorts by it, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentStatus {
    Attention(AttentionKind),
    Error,
    Stale,
    Working,
    Done,
    Idle,
    /// Agent process discovered but no hook events attributable to it.
    Unknown,
}

/// A live agent: a provider process in a pane of a tmux session.
#[derive(Debug, Clone)]
pub struct Agent {
    pub provider: String,
    pub pane: Pane,
    pub tmux_session_name: String,
    pub tmux_session_id: String,
    pub pid: i32,
    pub cwd: PathBuf,
    pub session_id: Option<String>,
    pub status: AgentStatus,
    /// When the current status was established; None for Unknown.
    pub since: Option<DateTime<Utc>>,
    /// One-line context for the status (task, activity, result, reason).
    pub detail: Option<String>,
    /// Subagents currently running inside this session, in start order.
    pub subagents: Vec<Subagent>,
    pub events: Vec<Event>,
}

/// An ended but resumable session (log has a session id, pane is gone).
#[derive(Debug, Clone)]
pub struct EndedSession {
    pub provider: String,
    pub session_id: String,
    pub cwd: Option<PathBuf>,
    pub ended_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct Snapshot {
    pub agents: Vec<Agent>,
    pub ended: Vec<EndedSession>,
    /// Provider ids discovered as plugins but with no hooks installed.
    pub uninstrumented: Vec<String>,
}

/// One full global scan: list panes, find provider processes under each pane,
/// correlate with session logs (by recorded pane id / pid, falling back to
/// unique provider+cwd match), derive statuses. Agents sort Attention first,
/// then by window index; ended sessions sort most recent first.
pub fn snapshot(
    store: &Store,
    plugins: &[Plugin],
    now: DateTime<Utc>,
    stale_after: Duration,
    topology: &[TopologyRow],
) -> Result<Snapshot> {
    let panes = tmux::panes_from_topology(topology);
    let procs = procs::snapshot()?;
    let sessions = store.sessions()?;
    let manifests: Vec<_> = plugins
        .iter()
        .map(|plugin| plugin.manifest.clone())
        .collect();
    Ok(join(
        &panes,
        &procs,
        &sessions,
        &manifests,
        now,
        stale_after,
        procs::cwd_of,
    ))
}

#[derive(Debug)]
struct LiveCandidate {
    provider: String,
    pane: Pane,
    tmux_session_name: String,
    tmux_session_id: String,
    pid: i32,
    cwd: PathBuf,
}

fn join(
    panes: &[TmuxSessionPane],
    procs: &[Proc],
    sessions: &[SessionRecord],
    manifests: &[Manifest],
    now: DateTime<Utc>,
    stale_after: Duration,
    cwd_of: impl Fn(i32) -> Option<PathBuf>,
) -> Snapshot {
    let mut live = Vec::new();
    for located in panes {
        for (provider, proc_) in procs::provider_procs_under(located.pane.pid, procs, manifests) {
            live.push(LiveCandidate {
                provider,
                pane: located.pane.clone(),
                tmux_session_name: located.tmux_session_name.clone(),
                tmux_session_id: located.tmux_session_id.clone(),
                pid: proc_.pid,
                cwd: cwd_of(proc_.pid).unwrap_or_else(|| located.pane.cwd.clone()),
            });
        }
    }

    let derived: Vec<Option<Derived>> = sessions
        .iter()
        .map(|session| status::derive(&session.events, now, stale_after))
        .collect();
    let mut matched = HashSet::new();
    let mut agents = Vec::new();
    for candidate in &live {
        let session_index = match_session(candidate, sessions, &derived, &matched);
        let (session_id, agent_status, since, detail, subagents, events) = match session_index {
            // A matched session can still derive nothing: a log whose every
            // line is retired vocabulary reads back empty.
            Some(index) => {
                matched.insert(index);
                let session = &sessions[index];
                let (status, since, detail) = match derived[index].as_ref() {
                    Some(derived) => (
                        map_status(derived.status),
                        Some(derived.since),
                        derived.detail.clone(),
                    ),
                    None => (AgentStatus::Unknown, None, None),
                };
                let subagents = status::subagents(&session.events);
                let events = session.events
                    [session.events.len().saturating_sub(AGENT_EVENT_TAIL)..]
                    .to_vec();
                (
                    Some(session.meta.session.clone()),
                    status,
                    since,
                    detail,
                    subagents,
                    events,
                )
            }
            None => (
                None,
                AgentStatus::Unknown,
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
        };
        agents.push(Agent {
            provider: candidate.provider.clone(),
            pane: candidate.pane.clone(),
            tmux_session_name: candidate.tmux_session_name.clone(),
            tmux_session_id: candidate.tmux_session_id.clone(),
            pid: candidate.pid,
            cwd: candidate.cwd.clone(),
            session_id,
            status: agent_status,
            since,
            detail,
            subagents,
            events,
        });
    }

    let mut ended = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        if matched.contains(&index) || session.meta.session.is_empty() {
            continue;
        }
        let explicitly_ended = matches!(
            derived[index].as_ref().map(|value| value.status),
            Some(SessionStatus::Ended)
        );
        let still_live = live.iter().any(|candidate| {
            candidate.provider == session.meta.provider
                && (session.meta.pane_id.as_deref() == Some(candidate.pane.id.as_str())
                    || session.meta.pid == Some(candidate.pid))
        });
        if explicitly_ended || !still_live {
            let Some(ended_at) = session.events.last().and_then(|event| event.ts) else {
                continue;
            };
            ended.push(EndedSession {
                provider: session.meta.provider.clone(),
                session_id: session.meta.session.clone(),
                cwd: session.meta.cwd.clone(),
                ended_at,
            });
        }
    }

    let uninstrumented = manifests
        .iter()
        .filter(|manifest| {
            !manifest.hooks.is_empty()
                && !sessions
                    .iter()
                    .any(|session| session.meta.provider == manifest.id)
                && agents.iter().any(|agent| {
                    agent.provider == manifest.id && agent.status == AgentStatus::Unknown
                })
        })
        .map(|manifest| manifest.id.clone())
        .collect();

    agents.sort_by_key(|agent| (agent.status, agent.pane.window_index));
    ended.sort_by_key(|session| std::cmp::Reverse(session.ended_at));

    Snapshot {
        agents,
        ended,
        uninstrumented,
    }
}

fn match_session(
    candidate: &LiveCandidate,
    sessions: &[SessionRecord],
    derived: &[Option<Derived>],
    matched: &HashSet<usize>,
) -> Option<usize> {
    let pane_match = latest_match(sessions, matched, |session| {
        session.meta.provider == candidate.provider
            && session.meta.pane_id.as_deref() == Some(candidate.pane.id.as_str())
    });
    if pane_match.is_some() {
        return pane_match;
    }

    let pid_match = latest_match(sessions, matched, |session| {
        session.meta.provider == candidate.provider && session.meta.pid == Some(candidate.pid)
    });
    if pid_match.is_some() {
        return pid_match;
    }

    let mut cwd_matches = sessions
        .iter()
        .enumerate()
        .filter(|(index, session)| {
            !matched.contains(index)
                && session.meta.provider == candidate.provider
                && session.meta.cwd.as_ref() == Some(&candidate.cwd)
                && matches!(derived[*index].as_ref().map(|value| value.status), Some(status) if status != SessionStatus::Ended)
        })
        .map(|(index, _)| index);
    let first = cwd_matches.next()?;
    cwd_matches.next().is_none().then_some(first)
}

fn latest_match(
    sessions: &[SessionRecord],
    matched: &HashSet<usize>,
    predicate: impl Fn(&SessionRecord) -> bool,
) -> Option<usize> {
    sessions
        .iter()
        .enumerate()
        .filter(|(index, session)| !matched.contains(index) && predicate(session))
        .max_by(|(_, left), (_, right)| left.meta.updated_at.cmp(&right.meta.updated_at))
        .map(|(index, _)| index)
}

fn map_status(status: SessionStatus) -> AgentStatus {
    match status {
        SessionStatus::Attention(kind) => AgentStatus::Attention(kind),
        SessionStatus::Error => AgentStatus::Error,
        SessionStatus::Stale => AgentStatus::Stale,
        SessionStatus::Working => AgentStatus::Working,
        SessionStatus::Done => AgentStatus::Done,
        SessionStatus::Idle | SessionStatus::Ended => AgentStatus::Idle,
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::{
        Command as ProviderCommand, Event, EventKind, FileFormat, HookFile, Manifest, ProcessMatch,
    };
    use crate::store::SessionMeta;

    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn pane(id: &str, pid: i32, window_index: u32, cwd: &str) -> Pane {
        Pane {
            id: id.into(),
            window_id: format!("@{window_index}"),
            pid,
            tty: format!("/dev/ttys{pid}"),
            cwd: cwd.into(),
            window_index,
            window_name: format!("window-{window_index}"),
        }
    }

    fn topology_row(tmux_session_name: &str, tmux_session_id: &str, pane: Pane) -> TopologyRow {
        TopologyRow {
            tmux_session_name: tmux_session_name.into(),
            tmux_session_id: tmux_session_id.into(),
            window_id: pane.window_id.clone(),
            window_active: true,
            tmux_session_attached: true,
            pane_id: pane.id.clone(),
            pane_pid: pane.pid,
            pane_tty: pane.tty.clone(),
            pane_current_path: pane.cwd.clone(),
            window_index: pane.window_index,
            window_name: pane.window_name.clone(),
            geometry: crate::tmux::PaneGeometry {
                left: 0,
                top: 0,
                cols: 80,
                rows: 24,
            },
            window_panes: 1,
        }
    }

    fn tmux_pane(tmux_session_name: &str, tmux_session_id: &str, pane: Pane) -> TmuxSessionPane {
        TmuxSessionPane {
            pane,
            tmux_session_name: tmux_session_name.into(),
            tmux_session_id: tmux_session_id.into(),
        }
    }

    fn proc_(pid: i32, ppid: i32, command: &str) -> Proc {
        Proc {
            pid,
            ppid,
            tty: Some(format!("ttys{pid}")),
            argv: vec![command.into()],
        }
    }

    #[test]
    fn discovers_panes_across_all_tmux_sessions() {
        let rows = [
            topology_row("one", "$1", pane("%1", 100, 1, "/one")),
            topology_row("two", "$2", pane("%2", 200, 2, "/two")),
        ];

        let panes = tmux::panes_from_topology(&rows);

        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].pane.id, "%1");
        assert_eq!(panes[0].tmux_session_name, "one");
        assert_eq!(panes[1].pane.id, "%2");
        assert_eq!(panes[1].tmux_session_id, "$2");
    }

    fn manifest(id: &str, hooks: bool) -> Manifest {
        Manifest {
            protocol: 1,
            id: id.into(),
            label: id.into(),
            color: None,
            process: ProcessMatch {
                argv0: vec![id.into()],
            },
            launch: ProviderCommand {
                argv: vec![id.into()],
            },
            resume: None,
            hooks: hooks
                .then(|| HookFile {
                    path: format!("~/.{id}.json"),
                    format: FileFormat::Json,
                    patches: Vec::new(),
                })
                .into_iter()
                .collect(),
        }
    }

    fn session(
        provider: &str,
        id: &str,
        pane_id: Option<&str>,
        pid: Option<i32>,
        cwd: Option<&str>,
        secs: i64,
        kind: EventKind,
    ) -> SessionRecord {
        SessionRecord {
            meta: SessionMeta {
                provider: provider.into(),
                session: id.into(),
                pane_id: pane_id.map(str::to_owned),
                pid,
                cwd: cwd.map(PathBuf::from),
                updated_at: at(secs),
            },
            events: vec![Event {
                v: 1,
                ts: Some(at(secs)),
                session: id.into(),
                kind,
            }],
        }
    }

    #[test]
    fn joins_live_agents_sessions_and_ended_records() {
        let panes = [
            tmux_pane("main", "$1", pane("%1", 100, 2, "/pane-claude")),
            tmux_pane("other", "$2", pane("%2", 200, 1, "/pane-codex")),
            tmux_pane("main", "$1", pane("%3", 300, 3, "/shared")),
            tmux_pane("main", "$1", pane("%4", 400, 0, "/unknown")),
        ];
        let procs = [
            proc_(100, 1, "zsh"),
            proc_(101, 100, "claude"),
            proc_(200, 1, "zsh"),
            proc_(201, 200, "codex"),
            proc_(300, 1, "zsh"),
            proc_(301, 300, "agy"),
            proc_(400, 1, "zsh"),
            proc_(401, 400, "other"),
        ];
        let manifests = [
            manifest("claude", true),
            manifest("codex", true),
            manifest("agy", true),
            manifest("other", true),
        ];
        let sessions = [
            session(
                "claude",
                "claude-live",
                Some("%1"),
                None,
                None,
                10,
                EventKind::Attention {
                    attention: AttentionKind::Approval,
                    summary: Some("Bash: rm -rf build".into()),
                },
            ),
            session(
                "codex",
                "codex-live",
                None,
                Some(201),
                None,
                20,
                EventKind::TurnStart { summary: None },
            ),
            session(
                "agy",
                "agy-live",
                None,
                None,
                Some("/shared"),
                30,
                EventKind::TurnEnd { summary: None },
            ),
            session(
                "codex",
                "codex-old",
                Some("%gone"),
                Some(999),
                Some("/old"),
                40,
                EventKind::TurnStart { summary: None },
            ),
            session(
                "claude",
                "claude-ended",
                Some("%gone"),
                None,
                None,
                50,
                EventKind::SessionEnd,
            ),
        ];

        let snapshot = join(
            &panes,
            &procs,
            &sessions,
            &manifests,
            at(60),
            Duration::minutes(30),
            |pid| (pid == 101).then(|| PathBuf::from("/real-claude")),
        );

        assert_eq!(
            snapshot
                .agents
                .iter()
                .map(|agent| agent.provider.as_str())
                .collect::<Vec<_>>(),
            ["claude", "codex", "agy", "other",]
        );
        assert!(matches!(
            snapshot.agents[0].status,
            AgentStatus::Attention(AttentionKind::Approval)
        ));
        assert_eq!(snapshot.agents[0].cwd, PathBuf::from("/real-claude"));
        assert_eq!(snapshot.agents[0].events, sessions[0].events);
        assert_eq!(
            snapshot.agents[0].detail.as_deref(),
            Some("Bash: rm -rf build")
        );
        assert_eq!(snapshot.agents[1].status, AgentStatus::Working);
        assert_eq!(snapshot.agents[1].cwd, PathBuf::from("/pane-codex"));
        assert_eq!(snapshot.agents[1].tmux_session_name, "other");
        assert_eq!(snapshot.agents[1].tmux_session_id, "$2");
        assert_eq!(snapshot.agents[2].status, AgentStatus::Done);
        assert_eq!(snapshot.agents[2].session_id.as_deref(), Some("agy-live"));
        assert_eq!(snapshot.agents[3].status, AgentStatus::Unknown);
        assert!(snapshot.agents[3].events.is_empty());
        assert_eq!(
            snapshot
                .ended
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            ["claude-ended", "codex-old",]
        );
        assert_eq!(snapshot.uninstrumented, ["other"]);
    }

    #[test]
    fn matched_agent_keeps_the_latest_event_tail() {
        let panes = [tmux_pane("main", "$1", pane("%1", 100, 1, "/work"))];
        let procs = [proc_(100, 1, "zsh"), proc_(101, 100, "claude")];
        let manifests = [manifest("claude", true)];
        let mut sessions = [session(
            "claude",
            "live",
            Some("%1"),
            Some(101),
            Some("/work"),
            0,
            EventKind::SessionStart { model: None },
        )];
        sessions[0].events = (0..65)
            .map(|secs| Event {
                v: 1,
                ts: Some(at(secs)),
                session: "live".into(),
                kind: EventKind::Heartbeat {
                    activity: Some(secs.to_string()),
                },
            })
            .collect();

        let snapshot = join(
            &panes,
            &procs,
            &sessions,
            &manifests,
            at(65),
            Duration::minutes(30),
            |_| None,
        );

        assert_eq!(snapshot.agents[0].events.len(), AGENT_EVENT_TAIL);
        assert_eq!(snapshot.agents[0].events[0].ts, Some(at(1)));
        assert_eq!(snapshot.agents[0].events[63].ts, Some(at(64)));
    }

    #[test]
    fn live_process_outliving_ended_session_is_idle_not_resumable() {
        let panes = [tmux_pane("main", "$1", pane("%1", 100, 1, "/work"))];
        let procs = [proc_(100, 1, "zsh"), proc_(101, 100, "claude")];
        let manifests = [manifest("claude", true)];
        let sessions = [session(
            "claude",
            "done",
            Some("%1"),
            Some(101),
            Some("/work"),
            10,
            EventKind::SessionEnd,
        )];

        let snapshot = join(
            &panes,
            &procs,
            &sessions,
            &manifests,
            at(20),
            Duration::minutes(30),
            |_| None,
        );

        assert_eq!(snapshot.agents[0].status, AgentStatus::Idle);
        assert_eq!(snapshot.agents[0].session_id.as_deref(), Some("done"));
        assert!(snapshot.ended.is_empty());
    }

    #[test]
    fn live_agent_in_another_tmux_session_is_not_ended() {
        let panes = [
            tmux_pane("current", "$1", pane("%1", 100, 1, "/current")),
            tmux_pane("elsewhere", "$2", pane("%2", 200, 1, "/work")),
        ];
        let procs = [
            proc_(100, 1, "zsh"),
            proc_(200, 1, "zsh"),
            proc_(201, 200, "claude"),
        ];
        let manifests = [manifest("claude", true)];
        let sessions = [session(
            "claude",
            "live-elsewhere",
            Some("%2"),
            Some(201),
            Some("/work"),
            10,
            EventKind::TurnStart { summary: None },
        )];

        let snapshot = join(
            &panes,
            &procs,
            &sessions,
            &manifests,
            at(20),
            Duration::minutes(30),
            |_| None,
        );

        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].tmux_session_id, "$2");
        assert_eq!(
            snapshot.agents[0].session_id.as_deref(),
            Some("live-elsewhere")
        );
        assert!(snapshot.ended.is_empty());
    }

    #[test]
    fn matched_session_with_no_readable_events_is_unknown() {
        let panes = [tmux_pane("main", "$1", pane("%1", 100, 1, "/work"))];
        let procs = [proc_(100, 1, "zsh"), proc_(101, 100, "claude")];
        let manifests = [manifest("claude", true)];
        // A log whose every line is retired vocabulary reads back empty.
        let mut retired = session(
            "claude",
            "old",
            Some("%1"),
            Some(101),
            Some("/work"),
            10,
            EventKind::SessionEnd,
        );
        retired.events.clear();
        let sessions = [retired];

        let snapshot = join(
            &panes,
            &procs,
            &sessions,
            &manifests,
            at(20),
            Duration::minutes(30),
            |_| None,
        );

        assert_eq!(snapshot.agents[0].status, AgentStatus::Unknown);
        assert_eq!(snapshot.agents[0].session_id.as_deref(), Some("old"));
    }
}
