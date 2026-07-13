use std::io::{Read, Write};

use gw_plugin_protocol::{
    AttentionKind, Command, Event, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode,
    ProcessMatch, PROTOCOL_VERSION,
};
use serde_json::{json, Map, Value};

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
    eprintln!("usage: gw-provider-codex <manifest|normalize>");
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
    let hook_patches = [
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "PostToolUse",
        "Stop",
    ]
    .into_iter()
    .map(hook_patch)
    .collect();

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "codex".into(),
        label: "Codex".into(),
        color: Some("#74AA9C".into()),
        process: ProcessMatch {
            argv0: vec!["codex".into()],
        },
        launch: Command {
            argv: vec!["codex".into()],
        },
        resume: Some(Command {
            argv: vec!["codex".into(), "resume".into(), "{session_id}".into()],
        }),
        hooks: vec![
            HookFile {
                path: "~/.codex/hooks.json".into(),
                format: FileFormat::Json,
                patches: hook_patches,
            },
            HookFile {
                path: "~/.codex/config.toml".into(),
                format: FileFormat::Toml,
                patches: vec![Patch {
                    pointer: "/features/hooks".into(),
                    mode: PatchMode::Set,
                    value: json!(true),
                }],
            },
        ],
    }
}

fn hook_patch(event: &str) -> Patch {
    Patch {
        pointer: format!("/hooks/{event}"),
        mode: PatchMode::Ensure,
        value: json!({
            "hooks": [{
                "type": "command",
                "command": "gw hook codex"
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
        "PermissionRequest" => {
            let Some(summary) = approval_summary(payload) else {
                return Vec::new();
            };
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some(summary),
            }
        }
        "PostToolUse" => EventKind::Heartbeat,
        "Stop" => EventKind::TurnEnd,
        _ => return Vec::new(),
    };

    vec![Event {
        v: PROTOCOL_VERSION,
        ts: None,
        session: session.into(),
        kind,
    }]
}

fn approval_summary(payload: &Map<String, Value>) -> Option<String> {
    let tool_name = payload.get("tool_name")?.as_str()?;
    let tool_input = payload.get("tool_input")?.as_object()?;

    match tool_input.get("command") {
        Some(Value::Array(command)) => Some(
            command
                .iter()
                .map(|value| match value {
                    Value::String(value) => value.clone(),
                    value => value.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
        Some(Value::String(command)) => Some(command.clone()),
        _ => {
            let compact = Value::Object(tool_input.clone()).to_string();
            let compact = compact.chars().take(200).collect::<String>();
            Some(format!("{tool_name} {compact}"))
        }
    }
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
                r#"{"session_id":"s1","hook_event_name":"PostToolUse"}"#,
                EventKind::Heartbeat,
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"Stop"}"#,
                EventKind::TurnEnd,
            ),
        ];

        for (payload, expected_kind) in cases {
            let event = event(payload);
            assert_eq!(event.session, "s1");
            assert_eq!(event.kind, expected_kind);
            assert_eq!(event.ts, None);
        }
    }

    #[test]
    fn maps_permission_request_summaries() {
        let array = event(
            r#"{"session_id":"s2","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":["git","status"]}}"#,
        );
        assert_eq!(
            array.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("git status".into()),
            }
        );

        let string = event(
            r#"{"session_id":"s2","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
        );
        assert_eq!(
            string.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("cargo test".into()),
            }
        );

        let fallback = event(
            r#"{"session_id":"s2","hook_event_name":"PermissionRequest","tool_name":"Write","tool_input":{"path":"README.md"}}"#,
        );
        assert_eq!(
            fallback.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some(r#"Write {"path":"README.md"}"#.into()),
            }
        );
    }

    #[test]
    fn truncates_permission_request_fallback() {
        let payload = json!({
            "session_id": "s3",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Write",
            "tool_input": {"text": "x".repeat(250)}
        });
        let event = normalize_payload(payload.to_string().as_bytes())
            .pop()
            .unwrap();
        let EventKind::Attention {
            summary: Some(summary),
            ..
        } = event.kind
        else {
            panic!("expected an attention event");
        };
        assert_eq!(summary.strip_prefix("Write ").unwrap().chars().count(), 200);
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
