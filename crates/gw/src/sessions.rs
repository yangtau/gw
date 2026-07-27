//! Session-referencing CLI: `gw ls`, `gw show`, `gw wait`, `gw resume`.
//! Read-only queries plus new-process launch — no pane scraping, no key
//! injection, no daemon. Machine-readable output is stable for skills.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Local, Utc};
use gw_core::address::{self, Address};
use gw_core::discover::{self, Snapshot};
use gw_core::launch::expand_argv;
use gw_core::plugins::{self, Plugin};
use gw_core::protocol::{AttentionKind, Event, EventKind, Manifest, PROTOCOL_VERSION};
use gw_core::session::{self, ActivityKind, Interpretation, Status};
use gw_core::store::{SessionRecord, Store};
use gw_core::{procs, tmux};
use serde_json::json;

/// Same staleness threshold as the panel.
const STALE_AFTER_MINUTES: i64 = 30;

fn stale_after() -> Duration {
    Duration::minutes(STALE_AFTER_MINUTES)
}

fn load() -> Result<(Store, Vec<Plugin>)> {
    Ok((Store::open_default()?, plugins::discover()?))
}

/// Global snapshot; outside a tmux server the topology degrades to empty
/// (no live agents visible, every known session listed as ended).
fn snapshot(store: &Store, plugins: &[Plugin]) -> Result<Snapshot> {
    let topology = tmux::observe_topology().unwrap_or_default();
    discover::snapshot(store, plugins, Utc::now(), stale_after(), &topology)
}

fn status_word(status: Status) -> &'static str {
    match status {
        Status::Attention(AttentionKind::Approval) => "approval",
        Status::Attention(AttentionKind::Question) => "question",
        Status::Error => "error",
        Status::Stale => "stale",
        Status::Working => "working",
        Status::Done => "done",
        Status::Idle => "idle",
    }
}

fn kind_word(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Focus => "focus",
        ActivityKind::Session => "session",
        ActivityKind::Turn => "turn",
        ActivityKind::Tool => "tool",
        ActivityKind::Approval => "approval",
        ActivityKind::Question => "question",
        ActivityKind::Done => "done",
        ActivityKind::Error => "error",
        ActivityKind::SubagentStarted => "subagent_start",
        ActivityKind::SubagentEnded => "subagent_end",
        ActivityKind::WaitStarted => "wait_start",
        ActivityKind::WaitEnded => "wait_end",
    }
}

fn ago(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (now - since).num_minutes();
    match minutes {
        m if m < 1 => "now".into(),
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h{}m", m / 60, m % 60),
        m => format!("{}d", m / (24 * 60)),
    }
}

// ---------------------------------------------------------------- gw ls --

pub fn ls(json: bool) -> Result<()> {
    let (store, plugins) = load()?;
    let snapshot = snapshot(&store, &plugins)?;
    let now = Utc::now();

    if json {
        let agents: Vec<_> = snapshot
            .agents
            .iter()
            .map(|agent| {
                json!({
                    "address": agent.session_id.as_ref()
                        .map(|id| format!("{}:{id}", agent.provider)),
                    "provider": agent.provider,
                    "session": agent.session_id,
                    "status": status_word(agent.status),
                    "since": agent.since,
                    "detail": agent.detail,
                    "cwd": agent.cwd,
                    "tmux_session": agent.tmux_session_name,
                    "window": format!("{}:{}", agent.pane.window_index, agent.pane.window_name),
                    "pane": agent.pane.id,
                })
            })
            .collect();
        let sessions: Vec<_> = snapshot
            .ended
            .iter()
            .map(|session| {
                json!({
                    "address": format!("{}:{}", session.provider, session.session_id),
                    "provider": session.provider,
                    "session": session.session_id,
                    "ended_at": session.ended_at,
                    "cwd": session.cwd,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "agents": agents, "sessions": sessions }))?
        );
        return Ok(());
    }

    println!("agents:");
    if snapshot.agents.is_empty() {
        println!("  (none)");
    }
    for agent in &snapshot.agents {
        let address = agent
            .session_id
            .as_ref()
            .map(|id| format!("{}:{id}", agent.provider))
            .unwrap_or_else(|| format!("{}:-", agent.provider));
        let mut line = format!("  {address}  {}", status_word(agent.status));
        if let Some(since) = agent.since {
            line.push_str(&format!(" ({})", ago(since, now)));
        }
        if let Some(detail) = &agent.detail {
            line.push_str(&format!("  {detail}"));
        }
        line.push_str(&format!(
            "  {}  {}:{}",
            agent.cwd.display(),
            agent.tmux_session_name,
            agent.pane.window_index
        ));
        println!("{line}");
    }
    println!("sessions:");
    if snapshot.ended.is_empty() {
        println!("  (none)");
    }
    for session in &snapshot.ended {
        let cwd = session
            .cwd
            .as_deref()
            .map(|cwd| cwd.display().to_string())
            .unwrap_or_default();
        println!(
            "  {}:{}  ended {}  {cwd}",
            session.provider,
            session.session_id,
            ago(session.ended_at, now)
        );
    }
    Ok(())
}

// -------------------------------------------------------------- gw show --

pub fn show(input: &str, transcript: bool, json: bool) -> Result<()> {
    let (store, plugins) = load()?;
    let sessions = store.sessions()?;
    let record = address::resolve(input, &sessions)?;
    let addr = Address {
        provider: record.meta.provider.clone(),
        session: record.meta.session.clone(),
    };
    let manifest = plugins
        .iter()
        .find(|plugin| plugin.manifest.id == addr.provider)
        .map(|plugin| &plugin.manifest);

    if transcript {
        return print_transcript(record, manifest);
    }

    let now = Utc::now();
    let state = session::interpret(&record.events, now, stale_after());

    if json {
        let activity: Vec<_> = state
            .activity
            .iter()
            .map(|entry| {
                json!({
                    "at": entry.at,
                    "kind": kind_word(entry.kind),
                    "detail": entry.detail,
                })
            })
            .collect();
        let waiting_on: Vec<_> = state
            .waiting_on
            .iter()
            .map(|wait| json!({ "target": wait.target, "since": wait.since }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "address": addr.canonical(),
                "provider": addr.provider,
                "session": addr.session,
                "status": status_word(state.status),
                "since": state.since,
                "detail": state.detail,
                "ended": state.ended,
                "cwd": record.meta.cwd,
                "transcript_path": transcript_path(record, manifest),
                "waiting_on": waiting_on,
                "activity": activity,
            }))?
        );
        return Ok(());
    }

    print_human_show(&addr, record, &state, manifest, now);
    Ok(())
}

fn print_human_show(
    addr: &Address,
    record: &SessionRecord,
    state: &Interpretation,
    manifest: Option<&Manifest>,
    now: DateTime<Utc>,
) {
    println!("address:    {}", addr.canonical());
    let mut status = status_word(state.status).to_owned();
    if state.ended {
        status.push_str(" (ended)");
    } else if let Some(since) = state.since {
        status.push_str(&format!(" ({})", ago(since, now)));
    }
    println!("status:     {status}");
    if let Some(detail) = &state.detail {
        println!("detail:     {detail}");
    }
    if let Some(cwd) = &record.meta.cwd {
        println!("cwd:        {}", cwd.display());
    }
    if let Some(path) = transcript_path(record, manifest) {
        println!("transcript: {}", path.display());
    }
    for wait in &state.waiting_on {
        println!("waiting on: {} ({})", wait.target, ago(wait.since, now));
    }
    println!();
    if state.activity.is_empty() {
        println!("no events yet");
        return;
    }
    for entry in &state.activity {
        let at = entry
            .at
            .map(|at| at.with_timezone(&Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| " ".repeat(11));
        println!("  {at}  {:<14} {}", kind_word(entry.kind), entry.detail);
    }
}

/// Transcript file location: hook-captured path first, then the manifest
/// glob (newest match). The manifest `transcript` command is not a path.
fn transcript_path(record: &SessionRecord, manifest: Option<&Manifest>) -> Option<PathBuf> {
    if let Some(path) = &record.meta.transcript_path {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    let glob = manifest?.transcript_glob.as_ref()?;
    let pattern = expand_tilde(&glob.replace("{session_id}", &record.meta.session));
    glob_newest(&pattern)
}

/// `--transcript`: provider-native content by contract. Locator order:
/// hook-captured path, manifest transcript command, manifest glob.
fn print_transcript(record: &SessionRecord, manifest: Option<&Manifest>) -> Result<()> {
    if let Some(path) = &record.meta.transcript_path {
        let path = Path::new(path);
        if path.exists() {
            let bytes = std::fs::read(path)?;
            std::io::stdout().write_all(&bytes)?;
            return Ok(());
        }
    }
    if let Some(command) = manifest.and_then(|manifest| manifest.transcript.as_ref()) {
        let argv = expand_argv(&command.argv, &record.meta.session, None, Path::new("."));
        let (program, args) = argv.split_first().context("empty transcript command")?;
        let status = std::process::Command::new(program)
            .args(args)
            .status()
            .with_context(|| format!("run {}", argv.join(" ")))?;
        if !status.success() {
            bail!("transcript command failed: {}", argv.join(" "));
        }
        return Ok(());
    }
    if let Some(glob) = manifest.and_then(|manifest| manifest.transcript_glob.as_ref()) {
        let pattern = expand_tilde(&glob.replace("{session_id}", &record.meta.session));
        if let Some(path) = glob_newest(&pattern) {
            let bytes = std::fs::read(&path)?;
            std::io::stdout().write_all(&bytes)?;
            return Ok(());
        }
    }
    bail!(
        "no transcript found for {}:{}",
        record.meta.provider,
        record.meta.session
    );
}

fn expand_tilde(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest).to_string_lossy().into_owned(),
            None => path.to_owned(),
        },
        None => path.to_owned(),
    }
}

/// Newest existing file matching an absolute glob pattern. Only `*` within
/// path segments is supported (no `**`), which covers every provider glob.
fn glob_newest(pattern: &str) -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/")];
    for segment in pattern.split('/').filter(|segment| !segment.is_empty()) {
        if segment.contains('*') {
            let mut next = Vec::new();
            for dir in &candidates {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if segment_matches(&entry.file_name().to_string_lossy(), segment) {
                        next.push(entry.path());
                    }
                }
            }
            candidates = next;
        } else {
            for candidate in &mut candidates {
                candidate.push(segment);
            }
            candidates.retain(|candidate| candidate.exists());
        }
        if candidates.is_empty() {
            return None;
        }
    }
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        })
}

fn segment_matches(name: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        None => name == pattern,
        Some((prefix, rest)) => name.strip_prefix(prefix).is_some_and(|tail| {
            (0..=tail.len())
                .filter(|index| tail.is_char_boundary(*index))
                .any(|index| segment_matches(&tail[index..], rest))
        }),
    }
}

// -------------------------------------------------------------- gw wait --

pub fn wait(input: &str, timeout_secs: u64, json: bool) -> Result<()> {
    let (store, plugins) = load()?;
    let manifests: Vec<Manifest> = plugins
        .iter()
        .map(|plugin| plugin.manifest.clone())
        .collect();
    let sessions = store.sessions()?;
    let record = address::resolve(input, &sessions)?;
    let target = Address {
        provider: record.meta.provider.clone(),
        session: record.meta.session.clone(),
    };

    // Waiter identity via the existing ppid ancestor-chain mechanism: the
    // nearest provider process above us, then its freshest session record.
    let waiter =
        procs::locate_agent(std::os::unix::process::parent_id() as i32, &manifests).unwrap_or(None);
    let waiter_session = waiter.as_ref().and_then(|(provider, location)| {
        sessions
            .iter()
            .filter(|session| {
                session.meta.provider == *provider && session.meta.pid == Some(location.pid)
            })
            .max_by_key(|session| session.meta.updated_at)
            .map(|session| session.meta.session.clone())
    });
    if let (Some((provider, _)), Some(session)) = (&waiter, &waiter_session) {
        if *provider == target.provider && *session == target.session {
            bail!("cannot wait on the waiter's own session {}", target);
        }
    }

    let wait_id = format!("{}-{}", std::process::id(), Utc::now().timestamp_millis());
    append_wait(
        &store,
        &waiter,
        &waiter_session,
        EventKind::WaitStart {
            wait_id: wait_id.clone(),
            target: target.canonical(),
        },
    );

    let outcome = run_wait_loop(&store, &manifests, &target, timeout_secs);
    append_wait(
        &store,
        &waiter,
        &waiter_session,
        EventKind::WaitEnd {
            wait_id,
            outcome: match &outcome {
                Ok(result) => result.word.to_owned(),
                Err(_) => "error".to_owned(),
            },
        },
    );
    let result = outcome?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "target": target.canonical(),
                "result": result.word,
                "detail": result.detail,
            }))?
        );
    } else {
        match &result.detail {
            Some(detail) => println!("{}: {detail}", result.word),
            None => println!("{}", result.word),
        }
    }
    Ok(())
}

struct WaitResult {
    word: &'static str,
    detail: Option<String>,
}

/// Level-triggered: evaluate immediately, block only while the target is
/// alive and Working. 1s event-log poll plus a process-liveness check —
/// a provider can die silently leaving no event.
fn run_wait_loop(
    store: &Store,
    manifests: &[Manifest],
    target: &Address,
    timeout_secs: u64,
) -> Result<WaitResult> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        let sessions = store.sessions()?;
        let record = sessions
            .iter()
            .find(|session| {
                session.meta.provider == target.provider && session.meta.session == target.session
            })
            .with_context(|| format!("session {target} disappeared from the store"))?;
        let state = session::interpret(&record.events, Utc::now(), stale_after());

        let settled = match state.status {
            _ if state.ended => Some(WaitResult {
                word: "ended",
                detail: None,
            }),
            Status::Done => Some(WaitResult {
                word: "done",
                detail: state.detail.clone(),
            }),
            Status::Attention(_) => Some(WaitResult {
                word: "attention",
                detail: state.detail.clone(),
            }),
            Status::Error => Some(WaitResult {
                word: "error",
                detail: state.detail.clone(),
            }),
            Status::Stale => Some(WaitResult {
                word: "stale",
                detail: state.detail.clone(),
            }),
            Status::Idle => Some(WaitResult {
                word: "idle",
                detail: None,
            }),
            Status::Working => {
                // Working with a dead process is an ended session the
                // provider never reported (silent death).
                if !target_alive(record, manifests) {
                    Some(WaitResult {
                        word: "ended",
                        detail: Some("process exited".into()),
                    })
                } else {
                    None
                }
            }
        };
        if let Some(result) = settled {
            return Ok(result);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(WaitResult {
                word: "timeout",
                detail: state.detail.clone(),
            });
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Whether the target's recorded provider process is still running (and is
/// still a provider process — pid reuse must not count as alive).
fn target_alive(record: &SessionRecord, manifests: &[Manifest]) -> bool {
    let Some(pid) = record.meta.pid else {
        // Never located: liveness is unknowable; rely on events and timeout.
        return true;
    };
    let Ok(procs) = procs::snapshot() else {
        return true;
    };
    procs.iter().any(|proc_| {
        proc_.pid == pid
            && manifests
                .iter()
                .any(|manifest| procs::matches_provider(proc_, manifest))
    })
}

/// Append a status-neutral wait annotation to the waiter's Event Log. Best
/// effort: an unidentifiable waiter (run from a plain shell) records nothing.
fn append_wait(
    store: &Store,
    waiter: &Option<(String, procs::AgentLocation)>,
    waiter_session: &Option<String>,
    kind: EventKind,
) {
    let (Some((provider, location)), Some(session)) = (waiter, waiter_session) else {
        return;
    };
    let event = Event {
        v: PROTOCOL_VERSION,
        ts: None,
        session: session.clone(),
        transcript: None,
        kind,
    };
    if let Err(error) = store.append(provider, &event, Some(location)) {
        eprintln!("gw wait: could not record wait annotation: {error:#}");
    }
}

// ------------------------------------------------------------ gw resume --

pub fn resume(input: &str, prompt: Option<&str>, fork: bool) -> Result<()> {
    if fork && prompt.is_some() {
        bail!("--fork takes no prompt (v1)");
    }
    let (store, plugins) = load()?;
    let sessions = store.sessions()?;
    let record = address::resolve(input, &sessions)?;
    let addr = Address {
        provider: record.meta.provider.clone(),
        session: record.meta.session.clone(),
    };
    let manifest = &plugins
        .iter()
        .find(|plugin| plugin.manifest.id == addr.provider)
        .with_context(|| format!("provider plugin {:?} not found", addr.provider))?
        .manifest;

    let snapshot = snapshot(&store, &plugins)?;
    let live = snapshot.agents.iter().any(|agent| {
        agent.provider == addr.provider && agent.session_id.as_deref() == Some(&addr.session)
    });

    let (command, action) = if fork {
        let command = manifest
            .fork
            .as_ref()
            .with_context(|| format!("provider {} does not support fork", addr.provider))?;
        (command, "forked")
    } else {
        if live {
            match manifest.fork {
                Some(_) => bail!(
                    "session {addr} is live; plain resume would fight the running agent — use --fork"
                ),
                None => bail!("session {addr} is live and {} has no fork", addr.provider),
            }
        }
        let command = match prompt {
            Some(_) => manifest.resume_prompt.as_ref().with_context(|| {
                format!(
                    "provider {} does not support resume with a prompt",
                    addr.provider
                )
            })?,
            None => manifest
                .resume
                .as_ref()
                .with_context(|| format!("provider {} does not support resume", addr.provider))?,
        };
        (command, "resumed")
    };

    let cwd = record
        .meta
        .cwd
        .clone()
        .map_or_else(std::env::current_dir, Ok)?;
    let argv = expand_argv(&command.argv, &addr.session, prompt, &cwd);
    let pane = tmux::new_window(&addr.provider, &cwd, &argv)
        .context("open tmux window (resume requires a running tmux server)")?;
    println!("{action} {addr} in tmux pane {pane} ({})", argv.join(" "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(transcript_path: Option<String>) -> SessionRecord {
        SessionRecord {
            meta: gw_core::store::SessionMeta {
                provider: "claude".into(),
                session: "smoke-1234-abcd".into(),
                pane_id: None,
                pid: None,
                cwd: None,
                transcript_path,
                updated_at: Utc::now(),
            },
            events: Vec::new(),
        }
    }

    #[test]
    fn transcript_path_returns_stored_path_when_file_exists() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("smoke.jsonl");
        std::fs::write(&file, "{}").unwrap();

        let record = record(Some(file.display().to_string()));
        assert_eq!(transcript_path(&record, None), Some(file));
    }

    #[test]
    fn transcript_path_none_when_stored_path_missing_and_no_manifest() {
        let missing = record(Some("/nonexistent/smoke.jsonl".into()));
        assert_eq!(transcript_path(&missing, None), None);
        assert_eq!(transcript_path(&record(None), None), None);
    }

    #[test]
    fn segment_matching_handles_stars() {
        assert!(segment_matches(
            "rollout-2026-abc.jsonl",
            "rollout-*abc.jsonl"
        ));
        assert!(segment_matches("anything", "*"));
        assert!(segment_matches("a-b-c", "a-*-c"));
        assert!(!segment_matches("rollout.jsonl", "rollout-*abc.jsonl"));
        assert!(!segment_matches("abc", "abcd"));
        assert!(segment_matches("abc", "abc"));
    }

    #[test]
    fn glob_finds_newest_match() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("projects/one")).unwrap();
        std::fs::create_dir_all(root.join("projects/two")).unwrap();
        std::fs::write(root.join("projects/one/s1.jsonl"), "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("projects/two/s1.jsonl"), "new").unwrap();

        let pattern = format!("{}/projects/*/s1.jsonl", root.display());
        let found = glob_newest(&pattern).unwrap();
        assert_eq!(found, root.join("projects/two/s1.jsonl"));

        assert!(glob_newest(&format!("{}/projects/*/nope.jsonl", root.display())).is_none());
    }
}
