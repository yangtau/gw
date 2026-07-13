use std::io::{Read, Write};

use gw_plugin_protocol::{
    AttentionKind, Command, Event, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode,
    ProcessMatch, PROTOCOL_VERSION,
};
use serde_json::{json, Value};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();

    if args.next().is_some() {
        usage();
    }

    match command.as_deref().and_then(|value| value.to_str()) {
        Some("manifest") => print_manifest(),
        Some("normalize") => print_events(),
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!("usage: gw-provider-claude <manifest|normalize>");
    std::process::exit(2);
}

fn print_manifest() {
    let Ok(json) = serde_json::to_string(&manifest()) else {
        return;
    };
    let _ = writeln!(std::io::stdout().lock(), "{json}");
}

fn print_events() {
    let mut payload = Vec::new();
    if std::io::stdin().read_to_end(&mut payload).is_err() {
        return;
    }

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    for event in normalize_payload(&payload) {
        let Ok(json) = serde_json::to_string(&event) else {
            return;
        };
        if writeln!(stdout, "{json}").is_err() {
            return;
        }
    }
}

fn manifest() -> Manifest {
    let patches = [
        "SessionStart",
        "UserPromptSubmit",
        "Notification",
        "PostToolUse",
        "Stop",
        "SessionEnd",
    ]
    .into_iter()
    .map(hook_patch)
    .collect();

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "claude".into(),
        label: "Claude".into(),
        color: Some("#D97757".into()),
        process: ProcessMatch {
            argv0: vec!["claude".into()],
        },
        launch: Command {
            argv: vec!["claude".into()],
        },
        resume: Some(Command {
            argv: vec!["claude".into(), "--resume".into(), "{session_id}".into()],
        }),
        hooks: vec![HookFile {
            path: "~/.claude/settings.json".into(),
            format: FileFormat::Json,
            patches,
        }],
    }
}

fn hook_patch(event: &str) -> Patch {
    Patch {
        pointer: format!("/hooks/{event}"),
        mode: PatchMode::Ensure,
        value: json!({
            "hooks": [{
                "type": "command",
                "command": "gw hook claude"
            }]
        }),
    }
}

fn normalize_payload(raw: &[u8]) -> Vec<Event> {
    let Ok(payload) = serde_json::from_slice::<Value>(raw) else {
        return Vec::new();
    };
    let Some(payload) = payload.as_object() else {
        return Vec::new();
    };
    let Some(session) = payload.get("session_id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(event_name) = payload.get("hook_event_name").and_then(Value::as_str) else {
        return Vec::new();
    };

    let kind = match event_name {
        "SessionStart" => EventKind::SessionStart,
        "UserPromptSubmit" => EventKind::TurnStart,
        "Notification" => {
            let Some(message) = payload.get("message").and_then(Value::as_str) else {
                return Vec::new();
            };
            EventKind::Attention {
                attention: AttentionKind::Notification,
                summary: Some(message.into()),
            }
        }
        "PostToolUse" => EventKind::Heartbeat,
        "Stop" => EventKind::TurnEnd,
        "SessionEnd" => EventKind::SessionEnd,
        _ => return Vec::new(),
    };

    vec![Event {
        v: PROTOCOL_VERSION,
        ts: None,
        session: session.into(),
        kind,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(payload: &str) -> Event {
        normalize_payload(payload.as_bytes()).pop().unwrap()
    }

    #[test]
    fn maps_hook_payloads() {
        let cases = [
            (
                r#"{"session_id":"s1","hook_event_name":"SessionStart"}"#,
                EventKind::SessionStart,
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit"}"#,
                EventKind::TurnStart,
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
                EventKind::Heartbeat,
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"Stop","last_assistant_message":"done"}"#,
                EventKind::TurnEnd,
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"SessionEnd","reason":"logout"}"#,
                EventKind::SessionEnd,
            ),
        ];

        for (payload, expected_kind) in cases {
            let event = event(payload);
            assert_eq!(event.session, "s1");
            assert_eq!(event.kind, expected_kind);
            assert_eq!(event.ts, None);
        }

        let notification = event(
            r#"{"session_id":"s2","hook_event_name":"Notification","message":"Needs input","notification_type":"info"}"#,
        );
        assert_eq!(notification.session, "s2");
        assert_eq!(
            notification.kind,
            EventKind::Attention {
                attention: AttentionKind::Notification,
                summary: Some("Needs input".into()),
            }
        );
    }

    #[test]
    fn ignores_invalid_payloads() {
        assert!(normalize_payload(b"not json").is_empty());
        assert!(
            normalize_payload(br#"{"session_id":"s1","hook_event_name":"SomethingElse"}"#)
                .is_empty()
        );
        assert!(normalize_payload(br#"{"hook_event_name":"SessionStart"}"#).is_empty());
    }

    #[test]
    fn manifest_round_trips() {
        let value = serde_json::to_value(manifest()).unwrap();
        let decoded: gw_plugin_protocol::Manifest = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
}
