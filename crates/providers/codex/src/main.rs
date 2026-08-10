use gw_plugin_protocol::{
    AttentionKind, Command, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode,
    ProcessMatch, PROTOCOL_VERSION,
};
use gw_provider_sdk::{command_hook_patch, excerpt, one_liner, text};
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
    .map(|event| command_hook_patch("codex", event, None))
    .collect();

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "codex".into(),
        label: "Codex".into(),
        color: Some("#74AA9C".into()),
        process: ProcessMatch {
            exclude_args: Vec::new(),
            exclude_arg_sequences: Vec::new(),
            argv0: vec!["codex".into()],
        },
        launch: Command {
            argv: vec!["codex".into()],
        },
        resume: Some(Command {
            argv: vec!["codex".into(), "resume".into(), "{session_id}".into()],
        }),
        resume_prompt: Some(Command {
            argv: vec![
                "codex".into(),
                "resume".into(),
                "{session_id}".into(),
                "{prompt}".into(),
            ],
        }),
        fork: Some(Command {
            argv: vec!["codex".into(), "fork".into(), "{session_id}".into()],
        }),
        transcript: None,
        transcript_glob: Some("~/.codex/sessions/*/*/*/rollout-*{session_id}.jsonl".into()),
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
        managed_files: Vec::new(),
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
        // Detailed activity ("shell: cargo test") so the panel matches
        // amp/opencode/pi. Falls back to the bare tool name when tool_input
        // is absent or carries no display-worthy field.
        "PostToolUse" => Some(EventKind::Heartbeat {
            activity: tool_activity(payload),
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

/// "shell: cargo test" — heartbeat-friendly variant of `approval_summary`.
/// Unlike the approval summary, the activity flavor falls back to the bare
/// tool name (never a raw-JSON blob) so a `Bash` heartbeat never turns into
/// an unreadable one-liner. Keys mirror the amp/opencode/pi bridges.
fn tool_activity(payload: &Map<String, Value>) -> Option<String> {
    let tool_name = payload.get("tool_name").and_then(Value::as_str)?;
    let detail = payload
        .get("tool_input")
        .and_then(Value::as_object)
        .and_then(|input| {
            command_argument(input.get("command")).or_else(|| {
                ["file_path", "path", "query", "description"]
                    .iter()
                    .find_map(|key| input.get(*key).and_then(Value::as_str).map(str::to_owned))
            })
        });
    match detail.as_deref().and_then(one_liner) {
        Some(detail) => one_liner(&format!("{tool_name}: {detail}")),
        None => Some(tool_name.to_owned()),
    }
}

/// Codex's `command` is either a shell string or an argv array; both
/// approval and activity summaries need the same rendering.
fn command_argument(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(command) => Some(command.clone()),
        Value::Array(argv) => Some(
            argv.iter()
                .map(|arg| match arg {
                    Value::String(arg) => arg.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
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
                // No tool_input → fall back to the bare tool name.
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
    fn post_tool_use_carries_detailed_activity() {
        // Codex canonical shape: `command` is an argv array.
        assert_eq!(
            event(
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"shell","tool_input":{"command":["cargo","test"]}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("shell: cargo test".into()),
            }
        );

        // Some tools carry `command` as a plain shell string.
        assert_eq!(
            event(
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"shell","tool_input":{"command":"cargo build"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("shell: cargo build".into()),
            }
        );

        // Non-shell tools may carry `file_path` or `query` instead.
        assert_eq!(
            event(
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"/tmp/a.rs"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("Write: /tmp/a.rs".into()),
            }
        );

        // Unrecognized fields → fall back to the bare tool name.
        // Heartbeats never carry a raw-JSON blob — that stays exclusive to
        // `PermissionRequest` where the user needs the full context.
        assert_eq!(
            event(
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"custom","tool_input":{"weird":42}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("custom".into()),
            }
        );
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
        let manifest = manifest();
        assert!(manifest.managed_files.is_empty());
        let value = serde_json::to_value(manifest).unwrap();
        let decoded: gw_plugin_protocol::Manifest = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
}
