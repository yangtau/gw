//! `gw hook <provider>`: the ingest path. Called by provider hook configs;
//! must be fast and must never fail the provider's hook (log errors to
//! stderr, always exit 0 once the payload was read).
//!
//! Flow: read stdin → plugin normalize → locate agent (ppid walk from our
//! own parent) → store append → desktop notification on configured events.

use std::io::{self, Read};
use std::process::Command;

use anyhow::Result;
use gw_core::protocol::{Event, EventKind};

pub fn run(provider: &str) -> Result<()> {
    let mut payload = Vec::new();
    io::stdin().read_to_end(&mut payload)?;
    if let Err(error) = ingest(provider, &payload) {
        eprintln!("gw hook {provider}: {error:#}");
    }
    Ok(())
}

fn ingest(provider: &str, payload: &[u8]) -> Result<()> {
    let cfg = gw_core::config::Config::load();
    let plugin = gw_core::plugins::find(provider)?;
    let result = gw_core::plugins::normalize(&plugin, payload);
    let store = if cfg.debug.hooks {
        Some(gw_core::store::Store::open_default()?)
    } else {
        None
    };
    let location = match gw_core::procs::locate_agent(
        std::os::unix::process::parent_id() as i32,
        std::slice::from_ref(&plugin.manifest),
    ) {
        Ok(Some((_, location))) => Some(location),
        Ok(None) => None,
        Err(error) => {
            eprintln!("gw hook {provider}: could not locate agent: {error:#}");
            None
        }
    };
    if let Some(store) = &store {
        if let Err(error) = store.append_debug(provider, payload, &result, location.as_ref()) {
            eprintln!("gw hook {provider}: could not append debug payload: {error:#}");
        }
    }
    let events = result?;
    let store = match store {
        Some(store) => store,
        None => gw_core::store::Store::open_default()?,
    };

    for event in events {
        match store.append(provider, &event, location.as_ref()) {
            Ok(()) if cfg.should_notify(&event.kind) => notify(provider, &event),
            Ok(()) => {}
            Err(error) => eprintln!("gw hook {provider}: could not append event: {error:#}"),
        }
    }
    Ok(())
}

fn notify(provider: &str, event: &Event) {
    let (kind, summary) = match &event.kind {
        EventKind::SessionFocus => unreachable!("focus events are never configured to notify"),
        EventKind::WaitStart { .. } | EventKind::WaitEnd { .. } => {
            unreachable!("wait events are never configured to notify")
        }
        EventKind::SessionStart { model } => ("session_start", model.as_deref()),
        EventKind::TurnStart { summary } => ("turn_start", summary.as_deref()),
        EventKind::TurnEnd { summary } => ("turn_end", summary.as_deref()),
        EventKind::TurnError { reason, summary } => {
            ("turn_error", summary.as_deref().or(reason.as_deref()))
        }
        EventKind::Attention { summary, .. } => ("attention", summary.as_deref()),
        EventKind::Heartbeat { activity } => ("heartbeat", activity.as_deref()),
        EventKind::SubagentStart { summary, .. } => ("subagent_start", summary.as_deref()),
        EventKind::SubagentEnd { agent } => ("subagent_end", Some(agent.as_str())),
        EventKind::SessionEnd => ("session_end", None),
    };
    let summary = summary.unwrap_or(kind);
    let script = format!(
        "display notification \"{}\" with title \"gw: {} ({})\"",
        escape_applescript(summary),
        escape_applescript(provider),
        kind
    );
    match Command::new("osascript").args(["-e", &script]).status() {
        Ok(status) if !status.success() => {
            eprintln!("gw hook {provider}: osascript exited with {status}");
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("gw hook {provider}: could not run osascript: {error}"),
    }
}

fn escape_applescript(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::escape_applescript;

    #[test]
    fn escapes_applescript_string_contents() {
        assert_eq!(
            escape_applescript(r#"say "yes" at C:\tmp"#),
            r#"say \"yes\" at C:\\tmp"#
        );
    }
}
