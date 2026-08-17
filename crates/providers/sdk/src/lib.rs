use std::io::{Read, Write};
use std::path::Path;

use gw_plugin_protocol::{Event, EventKind, Manifest, Patch, PatchMode, PROTOCOL_VERSION};
use serde_json::{json, Map, Value};

pub fn run(
    manifest: Manifest,
    session_field: &str,
    map_kind: impl Fn(&Map<String, Value>) -> Option<EventKind>,
) {
    run_session_fields(manifest, &[session_field], map_kind)
}

/// Like [`run`], but the session id is the first present field in
/// `session_fields`. Cursor payloads use `conversation_id` on most events and
/// `session_id` on session boundaries.
pub fn run_session_fields(
    manifest: Manifest,
    session_fields: &[&str],
    map_kind: impl Fn(&Map<String, Value>) -> Option<EventKind>,
) {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();

    if args.next().is_some() {
        usage();
    }

    match command.as_deref().and_then(|value| value.to_str()) {
        Some("manifest") => print_manifest(&manifest),
        Some("normalize") => print_events(session_fields, map_kind),
        _ => usage(),
    }
}

fn usage() -> ! {
    let binary = std::env::args_os()
        .next()
        .and_then(|value| {
            Path::new(&value)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "gw-provider".into());
    eprintln!("usage: {binary} <manifest|normalize>");
    std::process::exit(2);
}

fn print_manifest(manifest: &Manifest) {
    let Ok(json) = serde_json::to_string(manifest) else {
        return;
    };
    let _ = writeln!(std::io::stdout().lock(), "{json}");
}

fn print_events(
    session_fields: &[&str],
    map_kind: impl Fn(&Map<String, Value>) -> Option<EventKind>,
) {
    let mut payload = Vec::new();
    if std::io::stdin().read_to_end(&mut payload).is_err() {
        return;
    }

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    for event in normalize_session_fields(session_fields, map_kind, &payload) {
        let Ok(json) = serde_json::to_string(&event) else {
            return;
        };
        if writeln!(stdout, "{json}").is_err() {
            return;
        }
    }
}

pub fn normalize(
    session_field: &str,
    map_kind: impl Fn(&Map<String, Value>) -> Option<EventKind>,
    raw: &[u8],
) -> Vec<Event> {
    normalize_session_fields(&[session_field], map_kind, raw)
}

pub fn normalize_session_fields(
    session_fields: &[&str],
    map_kind: impl Fn(&Map<String, Value>) -> Option<EventKind>,
    raw: &[u8],
) -> Vec<Event> {
    let Ok(payload) = serde_json::from_slice::<Value>(raw) else {
        return Vec::new();
    };
    let Some(payload) = payload.as_object() else {
        return Vec::new();
    };
    let Some(session) = session_fields.iter().find_map(|field| {
        payload
            .get(*field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    }) else {
        return Vec::new();
    };
    let Some(kind) = map_kind(payload) else {
        return Vec::new();
    };

    vec![Event {
        v: PROTOCOL_VERSION,
        ts: None,
        session: session.into(),
        // Providers using claude-style hook payloads carry the native
        // transcript path under `transcript_path`; extract it generically.
        transcript: text(payload, "transcript_path"),
        kind,
    }]
}

/// Build the command-hook patch shared by providers using the Claude-style
/// hook config shape. Providers retain ownership of which events and matchers
/// they subscribe to; the SDK owns the protocol pointer and command encoding.
pub fn command_hook_patch(provider: &str, event: &str, matcher: Option<&str>) -> Patch {
    let mut value = Map::new();
    if let Some(matcher) = matcher {
        value.insert("matcher".into(), json!(matcher));
    }
    value.insert(
        "hooks".into(),
        json!([{
            "type": "command",
            "command": format!("gw hook {provider}")
        }]),
    );
    Patch {
        pointer: format!("/hooks/{event}"),
        mode: PatchMode::Ensure,
        value: Value::Object(value),
    }
}

/// Build the command-hook patch for providers whose hook file is a flat
/// `{ command, matcher? }` array (Cursor `hooks.json`), not Claude's nested
/// `{ hooks: [{ type, command }] }` shape.
pub fn flat_command_hook_patch(provider: &str, event: &str, matcher: Option<&str>) -> Patch {
    let mut value = Map::new();
    value.insert("command".into(), json!(format!("gw hook {provider}")));
    if let Some(matcher) = matcher {
        value.insert("matcher".into(), json!(matcher));
    }
    Patch {
        pointer: format!("/hooks/{event}"),
        mode: PatchMode::Ensure,
        value: Value::Object(value),
    }
}

pub fn text(payload: &Map<String, Value>, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub fn excerpt(payload: &Map<String, Value>, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .and_then(one_liner)
}

/// Collapse whitespace and cap at ~120 chars for panel display.
pub fn one_liner(value: &str) -> Option<String> {
    let mut line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    if line.chars().count() > 120 {
        line = line.chars().take(119).collect::<String>() + "…";
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_end(_: &Map<String, Value>) -> Option<EventKind> {
        Some(EventKind::SessionEnd)
    }

    #[test]
    fn extracts_present_session_field() {
        let events = normalize("session_id", session_end, br#"{"session_id":"s1"}"#);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session, "s1");
    }

    #[test]
    fn ignores_missing_or_non_string_session_field() {
        assert!(normalize("session_id", session_end, br#"{}"#).is_empty());
        assert!(normalize("session_id", session_end, br#"{"session_id":1}"#).is_empty());
        assert!(normalize("session_id", session_end, br#"{"session_id":""}"#).is_empty());
    }

    #[test]
    fn session_fields_prefer_the_first_present_nonempty_name() {
        let events = normalize_session_fields(
            &["conversation_id", "session_id"],
            session_end,
            br#"{"session_id":"s1"}"#,
        );
        assert_eq!(events[0].session, "s1");

        let events = normalize_session_fields(
            &["conversation_id", "session_id"],
            session_end,
            br#"{"conversation_id":"c1","session_id":"s1"}"#,
        );
        assert_eq!(events[0].session, "c1");
    }

    #[test]
    fn ignores_invalid_json() {
        assert!(normalize("session_id", session_end, b"not json").is_empty());
    }

    #[test]
    fn ignores_map_kind_none() {
        let events = normalize("session_id", |_| None, br#"{"session_id":"s1"}"#);

        assert!(events.is_empty());
    }

    #[test]
    fn wraps_event_kind_with_protocol_fields() {
        let mut events = normalize("session_id", session_end, br#"{"session_id":"s1"}"#);
        let event = events.pop().unwrap();

        assert_eq!(event.v, PROTOCOL_VERSION);
        assert_eq!(event.ts, None);
        assert_eq!(event.session, "s1");
        assert_eq!(event.kind, EventKind::SessionEnd);
    }

    #[test]
    fn command_hook_patch_hides_config_and_command_encoding() {
        let patch = command_hook_patch(
            "claude",
            "Notification",
            Some("elicitation_dialog|agent_needs_input"),
        );

        assert_eq!(patch.pointer, "/hooks/Notification");
        assert_eq!(patch.mode, PatchMode::Ensure);
        assert_eq!(
            patch.value,
            json!({
                "matcher": "elicitation_dialog|agent_needs_input",
                "hooks": [{
                    "type": "command",
                    "command": "gw hook claude"
                }]
            })
        );

        let unmatched = command_hook_patch("codex", "Stop", None);
        assert!(unmatched.value.get("matcher").is_none());
        assert_eq!(unmatched.value["hooks"][0]["command"], "gw hook codex");
    }

    #[test]
    fn flat_command_hook_patch_uses_cursor_entry_shape() {
        let patch = flat_command_hook_patch("cursor", "sessionStart", None);
        assert_eq!(patch.pointer, "/hooks/sessionStart");
        assert_eq!(patch.mode, PatchMode::Ensure);
        assert_eq!(patch.value, json!({ "command": "gw hook cursor" }));

        let matched = flat_command_hook_patch("cursor", "preToolUse", Some("Shell"));
        assert_eq!(
            matched.value,
            json!({
                "command": "gw hook cursor",
                "matcher": "Shell"
            })
        );
    }

    #[test]
    fn one_liner_collapses_whitespace() {
        assert_eq!(
            one_liner("  alpha\n beta\t gamma  "),
            Some("alpha beta gamma".into())
        );
    }

    #[test]
    fn one_liner_truncates_to_120_characters() {
        let line = one_liner(&"x".repeat(121)).unwrap();

        assert_eq!(line.chars().count(), 120);
        assert!(line.ends_with('…'));
    }

    #[test]
    fn one_liner_ignores_empty_whitespace() {
        assert_eq!(one_liner(" \n\t "), None);
    }
}
