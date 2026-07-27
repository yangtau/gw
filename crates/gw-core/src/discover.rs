//! Discovery: join live tmux panes, provider processes, and the event log
//! into the panel's data model. Panes are the source of truth for existence;
//! the log is the source of truth for status.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::plugins::Plugin;
use crate::procs::{self, Proc};
use crate::protocol::Manifest;
use crate::session::{self, ActivityEntry, Status, Subagent};
use crate::setup;
use crate::store::{SessionRecord, Store};
use crate::tmux::{self, Pane, TmuxSessionPane, TopologyRow};

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
    pub status: Status,
    /// When the current status was established; None before the first event.
    pub since: Option<DateTime<Utc>>,
    /// One-line context for the status (task, activity, result, reason).
    pub detail: Option<String>,
    /// Subagents currently running inside this session, in start order.
    pub subagents: Vec<Subagent>,
    /// Recent display-ready Activity in Event Log order.
    pub activity: Vec<ActivityEntry>,
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
    /// Live providers whose declared hook/config integration is not installed.
    pub setup_required: Vec<String>,
}

/// One full global scan: list panes, find provider processes under each pane,
/// correlate with session logs by recorded provider pid, derive statuses.
/// Agents sort Attention first, then by window index; ended sessions sort most
/// recent first.
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
    let mut snapshot = join(
        &panes,
        &procs,
        &sessions,
        &manifests,
        now,
        stale_after,
        procs::cwd_of,
    );
    snapshot.setup_required = providers_requiring_setup(&snapshot.agents, &manifests);
    Ok(snapshot)
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

    let interpretations: Vec<_> = sessions
        .iter()
        .map(|record| session::interpret(&record.events, now, stale_after))
        .collect();
    let mut matched = HashSet::new();
    let mut agents = Vec::new();
    for candidate in &live {
        let session_index = latest_match(sessions, &matched, |session| {
            session.meta.provider == candidate.provider && session.meta.pid == Some(candidate.pid)
        });
        let (session_id, status, since, detail, subagents, activity) = match session_index {
            Some(index) => {
                matched.insert(index);
                let record = &sessions[index];
                let interpretation = &interpretations[index];
                (
                    Some(record.meta.session.clone()),
                    interpretation.status,
                    interpretation.since,
                    interpretation.detail.clone(),
                    interpretation.subagents.clone(),
                    interpretation.activity.clone(),
                )
            }
            None => (None, Status::Idle, None, None, Vec::new(), Vec::new()),
        };
        agents.push(Agent {
            provider: candidate.provider.clone(),
            pane: candidate.pane.clone(),
            tmux_session_name: candidate.tmux_session_name.clone(),
            tmux_session_id: candidate.tmux_session_id.clone(),
            pid: candidate.pid,
            cwd: candidate.cwd.clone(),
            session_id,
            status,
            since,
            detail,
            subagents,
            activity,
        });
    }

    let mut ended = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        if matched.contains(&index) || session.meta.session.is_empty() {
            continue;
        }
        let explicitly_ended = interpretations[index].ended;
        let still_live = live.iter().any(|candidate| {
            candidate.provider == session.meta.provider && session.meta.pid == Some(candidate.pid)
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

    agents.sort_by_key(|agent| (agent.status, agent.pane.window_index));
    ended.sort_by_key(|session| std::cmp::Reverse(session.ended_at));

    Snapshot {
        agents,
        ended,
        setup_required: Vec::new(),
    }
}

fn providers_requiring_setup(agents: &[Agent], manifests: &[Manifest]) -> Vec<String> {
    manifests
        .iter()
        .filter(|manifest| {
            (!manifest.hooks.is_empty() || !manifest.managed_files.is_empty())
                && agents.iter().any(|agent| agent.provider == manifest.id)
                && !setup::integration_is_installed(manifest)
        })
        .map(|manifest| manifest.id.clone())
        .collect()
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::protocol::{
        AttentionKind, Command as ProviderCommand, Event, EventKind, FileFormat, HookFile,
        ManagedFile, Manifest, ProcessMatch,
    };
    use crate::session::ActivityKind;
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
                exclude_args: Vec::new(),
            },
            launch: ProviderCommand {
                argv: vec![id.into()],
            },
            resume: None,
            resume_prompt: None,
            fork: None,
            transcript: None,
            transcript_glob: None,
            hooks: hooks
                .then(|| HookFile {
                    path: format!("~/.{id}.json"),
                    format: FileFormat::Json,
                    patches: Vec::new(),
                })
                .into_iter()
                .collect(),
            managed_files: Vec::new(),
        }
    }

    fn managed_manifest(id: &str, path: &Path) -> Manifest {
        let mut manifest = manifest(id, false);
        manifest.managed_files.push(ManagedFile {
            path: path.to_string_lossy().into_owned(),
            content: "bridge\n".into(),
            comment_prefix: "//".into(),
            comment_suffix: String::new(),
        });
        manifest
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
                transcript_path: None,
                updated_at: at(secs),
            },
            events: vec![Event {
                v: 1,
                ts: Some(at(secs)),
                session: id.into(),
                transcript: None,
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
                Some(101),
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
                Some("%3"),
                Some(301),
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
            Status::Attention(AttentionKind::Approval)
        ));
        assert_eq!(snapshot.agents[0].cwd, PathBuf::from("/real-claude"));
        assert_eq!(snapshot.agents[0].activity.len(), 1);
        assert_eq!(snapshot.agents[0].activity[0].kind, ActivityKind::Approval);
        assert_eq!(
            snapshot.agents[0].detail.as_deref(),
            Some("Bash: rm -rf build")
        );
        assert_eq!(snapshot.agents[1].status, Status::Working);
        assert_eq!(snapshot.agents[1].cwd, PathBuf::from("/pane-codex"));
        assert_eq!(snapshot.agents[1].tmux_session_name, "other");
        assert_eq!(snapshot.agents[1].tmux_session_id, "$2");
        assert_eq!(snapshot.agents[2].status, Status::Done);
        assert_eq!(snapshot.agents[2].session_id.as_deref(), Some("agy-live"));
        assert_eq!(snapshot.agents[3].status, Status::Idle);
        assert!(snapshot.agents[3].activity.is_empty());
        assert_eq!(
            snapshot
                .ended
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            ["claude-ended", "codex-old",]
        );
        assert!(snapshot.setup_required.is_empty());
    }

    #[test]
    fn idle_agent_setup_warning_follows_integration_file_not_event_history() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gw.ts");
        let manifests = [managed_manifest("amp", &path)];
        let panes = [tmux_pane("main", "$1", pane("%1", 100, 1, "/work"))];
        let procs = [proc_(100, 1, "zsh"), proc_(101, 100, "amp")];
        let snapshot = join(
            &panes,
            &procs,
            &[],
            &manifests,
            at(20),
            Duration::minutes(30),
            |_| None,
        );

        assert_eq!(snapshot.agents[0].status, Status::Idle);
        assert_eq!(
            providers_requiring_setup(&snapshot.agents, &manifests),
            ["amp"]
        );

        setup::install(&manifests).unwrap();
        assert!(providers_requiring_setup(&snapshot.agents, &manifests).is_empty());

        std::fs::write(path, "modified").unwrap();
        assert_eq!(
            providers_requiring_setup(&snapshot.agents, &manifests),
            ["amp"]
        );
    }

    #[test]
    fn blank_agent_does_not_inherit_an_old_session_from_the_same_cwd() {
        let panes = [tmux_pane("main", "$1", pane("%1", 100, 1, "/work"))];
        let procs = [proc_(100, 1, "zsh"), proc_(101, 100, "claude")];
        let manifests = [manifest("claude", true)];
        let sessions = [session(
            "claude",
            "old",
            Some("%gone"),
            Some(999),
            Some("/work"),
            10,
            EventKind::TurnEnd {
                summary: Some("old result".into()),
            },
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
        assert_eq!(snapshot.agents[0].status, Status::Idle);
        assert!(snapshot.agents[0].session_id.is_none());
        assert!(snapshot.agents[0].activity.is_empty());
        assert_eq!(snapshot.ended[0].session_id, "old");
    }

    #[test]
    fn blank_agent_does_not_inherit_an_old_session_from_a_reused_pane() {
        let panes = [tmux_pane("main", "$1", pane("%1", 100, 1, "/work"))];
        let procs = [proc_(100, 1, "zsh"), proc_(101, 100, "claude")];
        let manifests = [manifest("claude", true)];
        let sessions = [session(
            "claude",
            "old",
            Some("%1"),
            Some(999),
            Some("/work"),
            10,
            EventKind::TurnEnd {
                summary: Some("old result".into()),
            },
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
        assert_eq!(snapshot.agents[0].status, Status::Idle);
        assert!(snapshot.agents[0].session_id.is_none());
        assert!(snapshot.agents[0].activity.is_empty());
        assert_eq!(snapshot.ended[0].session_id, "old");
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

        assert_eq!(snapshot.agents[0].status, Status::Idle);
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
    fn matched_session_with_no_readable_events_is_idle() {
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

        assert_eq!(snapshot.agents[0].status, Status::Idle);
        assert_eq!(snapshot.agents[0].session_id.as_deref(), Some("old"));
    }
}
