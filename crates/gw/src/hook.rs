//! `gw hook <provider>`: the ingest path. Called by provider hook configs;
//! must be fast and must never fail the provider's hook (log errors to
//! stderr, always exit 0 once the payload was read).
//!
//! Flow: read stdin → plugin normalize → locate agent (ppid walk from our
//! own parent) → store append → desktop notification on attention events.

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
    let plugin = gw_core::plugins::find(provider)?;
    let events = gw_core::plugins::normalize(&plugin, payload)?;
    let store = gw_core::store::Store::open_default()?;

    for event in events {
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
        if let Err(error) = store.append(provider, &event, location.as_ref()) {
            eprintln!("gw hook {provider}: could not append event: {error:#}");
        }
        notify_attention(provider, &event);
    }
    Ok(())
}

fn notify_attention(provider: &str, event: &Event) {
    let EventKind::Attention { summary, .. } = &event.kind else {
        return;
    };
    let summary = summary.as_deref().unwrap_or("");
    let script = format!(
        "display notification \"{}\" with title \"gw: {}\"",
        escape_applescript(summary),
        escape_applescript(provider)
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
