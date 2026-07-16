use gw_plugin_protocol::{
    AttentionKind, Command, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode,
    ProcessMatch, PROTOCOL_VERSION,
};
use gw_provider_sdk::{excerpt, one_liner, text};
use serde_json::{json, Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "session_id", map_kind);
}

// codex has no failure or question events; errors surface only as staleness.
fn manifest() -> Manifest {
    let hook_patches = [
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
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

fn map_kind(payload: &Map<String, Value>) -> Option<EventKind> {
    let event_name = payload.get("hook_event_name").and_then(Value::as_str)?;

    match event_name {
        "SessionStart" => Some(EventKind::SessionStart {
            model: text(payload, "model"),
        }),
        "UserPromptSubmit" => Some(EventKind::TurnStart {
            summary: excerpt(payload, "prompt"),
        }),
        "PermissionRequest" => {
            let summary = approval_summary(payload)?;
            Some(EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some(summary),
            })
        }
        "PostToolUse" => Some(EventKind::Heartbeat {
            activity: text(payload, "tool_name"),
        }),
        "PreCompact" | "PostCompact" => Some(EventKind::Heartbeat {
            activity: Some("compact".into()),
        }),
        // Both fire with the parent's session_id; codex carries no task text.
        "SubagentStart" => text(payload, "agent_id").map(|agent| EventKind::SubagentStart {
            agent,
            agent_type: text(payload, "agent_type"),
            model: text(payload, "model"),
            summary: None,
        }),
        "SubagentStop" => text(payload, "agent_id").map(|agent| EventKind::SubagentEnd { agent }),
        "Stop" => Some(EventKind::TurnEnd {
            summary: excerpt(payload, "last_assistant_message"),
        }),
        _ => None,
    }
}

fn approval_summary(payload: &Map<String, Value>) -> Option<String> {
    let tool_name = payload.get("tool_name")?.as_str()?;
    let tool_input = payload.get("tool_input")?.as_object()?;

    let raw = match tool_input.get("command") {
        Some(Value::Array(command)) => command
            .iter()
            .map(|value| match value {
                Value::String(value) => value.clone(),
                value => value.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        Some(Value::String(command)) => command.clone(),
        _ => format!("{tool_name} {}", Value::Object(tool_input.clone())),
    };
    one_liner(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_plugin_protocol::Event;

    fn normalize_payload(raw: &[u8]) -> Vec<Event> {
        gw_provider_sdk::normalize("session_id", map_kind, raw)
    }

    fn event(payload: &str) -> Event {
        normalize_payload(payload.as_bytes()).pop().unwrap()
    }

    #[test]
    fn maps_hook_payloads() {
        let cases = [
            (
                r#"{"session_id":"s1","hook_event_name":"SessionStart","model":"gpt-5.6-sol"}"#,
                EventKind::SessionStart {
                    model: Some("gpt-5.6-sol".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"ship it"}"#,
                EventKind::TurnStart {
                    summary: Some("ship it".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"shell"}"#,
                EventKind::Heartbeat {
                    activity: Some("shell".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"PreCompact","trigger":"auto"}"#,
                EventKind::Heartbeat {
                    activity: Some("compact".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"Stop","last_assistant_message":"done"}"#,
                EventKind::TurnEnd {
                    summary: Some("done".into()),
                },
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
        assert_eq!(summary.chars().count(), 120);
        assert!(summary.starts_with("Write "));
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn maps_subagent_lifecycle() {
        let start = event(
            r#"{"session_id":"s1","hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"reviewer","model":"gpt-5.6-sol"}"#,
        );
        assert_eq!(
            start.kind,
            EventKind::SubagentStart {
                agent: "a1".into(),
                agent_type: Some("reviewer".into()),
                model: Some("gpt-5.6-sol".into()),
                summary: None,
            }
        );

        let stop = event(
            r#"{"session_id":"s1","hook_event_name":"SubagentStop","agent_id":"a1","agent_type":"reviewer","stop_hook_active":false}"#,
        );
        assert_eq!(stop.kind, EventKind::SubagentEnd { agent: "a1".into() });

        assert!(
            normalize_payload(br#"{"session_id":"s1","hook_event_name":"SubagentStart"}"#)
                .is_empty()
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
