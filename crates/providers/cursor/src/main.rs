use gw_plugin_protocol::{
    Command, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode, ProcessMatch,
    PROTOCOL_VERSION,
};
use gw_provider_sdk::{excerpt, flat_command_hook_patch, one_liner, text};
use serde_json::{json, Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "conversation_id", map_kind);
}

// Cursor has no PermissionRequest or Notification hook. The CLI permission
// prompt is not observable without treating every beforeShellExecution as
// Attention, which fires whether or not a dialog is showing. Questions have
// no provider-wide event either. Statuses other than Attention still work.
const SUBSCRIPTIONS: [&str; 8] = [
    "sessionStart",
    "beforeSubmitPrompt",
    "postToolUse",
    "preCompact",
    "subagentStart",
    "subagentStop",
    "stop",
    "sessionEnd",
];

fn manifest() -> Manifest {
    let mut patches: Vec<Patch> = vec![Patch {
        pointer: "/version".into(),
        mode: PatchMode::Set,
        value: json!(1),
    }];
    patches.extend(
        SUBSCRIPTIONS
            .into_iter()
            .map(|event| flat_command_hook_patch("cursor", event)),
    );

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "cursor".into(),
        label: "Cursor".into(),
        color: Some("#78716C".into()),
        process: ProcessMatch {
            // Official install symlinks both names to the same wrapper;
            // `exec -a "$0"` makes argv0 follow whichever was invoked.
            argv0: vec!["cursor-agent".into(), "agent".into()],
            // Interactive TUI only. Headless print mode, the ACP server,
            // the private worker, and utility subcommands have no pane to
            // jump to. `ls` / `resume` / the `agent` subcommand stay — they
            // become or already are the interactive session.
            exclude_args: [
                "-p",
                "--print",
                "--list-models",
                "acp",
                "about",
                "bedrock",
                "create-chat",
                "generate-rule",
                "help",
                "install-shell-integration",
                "login",
                "logout",
                "mcp",
                "models",
                "plugin",
                "rule",
                "status",
                "uninstall-shell-integration",
                "update",
                "whoami",
                "worker",
            ]
            .map(str::to_owned)
            .to_vec(),
            exclude_arg_sequences: Vec::new(),
        },
        launch: Command {
            argv: vec!["cursor-agent".into()],
        },
        resume: Some(Command {
            argv: vec![
                "cursor-agent".into(),
                "--resume".into(),
                "{session_id}".into(),
            ],
        }),
        resume_prompt: Some(Command {
            argv: vec![
                "cursor-agent".into(),
                "--resume".into(),
                "{session_id}".into(),
                "{prompt}".into(),
            ],
        }),
        fork: None,
        transcript: None,
        // Hook payloads carry transcript_path; the glob is the fallback for
        // sessions recorded before that field existed.
        transcript_glob: Some(
            "~/.cursor/projects/*/agent-transcripts/{session_id}/{session_id}.jsonl".into(),
        ),
        hooks: vec![HookFile {
            path: "~/.cursor/hooks.json".into(),
            format: FileFormat::Json,
            patches,
        }],
        managed_files: Vec::new(),
    }
}

fn map_kind(payload: &Map<String, Value>) -> Option<EventKind> {
    let event_name = payload.get("hook_event_name").and_then(Value::as_str)?;

    match event_name {
        "sessionStart" => Some(EventKind::SessionStart {
            model: text(payload, "model").or_else(|| text(payload, "model_id")),
        }),
        "beforeSubmitPrompt" => Some(EventKind::TurnStart {
            summary: excerpt(payload, "prompt"),
        }),
        "postToolUse" => Some(EventKind::Heartbeat {
            activity: tool_summary(payload),
        }),
        "preCompact" => Some(EventKind::Heartbeat {
            activity: Some("compact".into()),
        }),
        "subagentStart" => text(payload, "subagent_id").map(|agent| EventKind::SubagentStart {
            agent,
            agent_type: text(payload, "subagent_type"),
            model: text(payload, "subagent_model"),
            summary: excerpt(payload, "task"),
        }),
        "subagentStop" => {
            text(payload, "subagent_id").map(|agent| EventKind::SubagentEnd { agent })
        }
        "stop" => match payload.get("status").and_then(Value::as_str) {
            Some("error") => Some(EventKind::TurnError {
                reason: Some("error".into()),
                summary: None,
            }),
            _ => Some(EventKind::TurnEnd { summary: None }),
        },
        "sessionEnd" => Some(EventKind::SessionEnd),
        _ => None,
    }
}

/// "Shell: cargo test" — tool name plus its most telling argument.
fn tool_summary(payload: &Map<String, Value>) -> Option<String> {
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    let argument = payload
        .get("tool_input")
        .and_then(Value::as_object)
        .and_then(|input| {
            ["command", "file_path", "path", "query", "pattern"]
                .iter()
                .find_map(|key| input.get(*key).and_then(Value::as_str))
        });
    match argument.and_then(one_liner) {
        Some(argument) => one_liner(&format!("{tool}: {argument}")),
        None => Some(tool.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_plugin_protocol::Event;

    fn normalize_payload(raw: &[u8]) -> Vec<Event> {
        gw_provider_sdk::normalize("conversation_id", map_kind, raw)
    }

    fn events(payload: &str) -> Vec<Event> {
        normalize_payload(payload.as_bytes())
    }

    fn event(payload: &str) -> Event {
        events(payload).pop().unwrap()
    }

    #[test]
    fn maps_lifecycle_payloads() {
        let cases = [
            (
                r#"{"conversation_id":"s1","hook_event_name":"sessionStart","model":"composer-2.5"}"#,
                EventKind::SessionStart {
                    model: Some("composer-2.5".into()),
                },
            ),
            (
                r#"{"conversation_id":"s1","hook_event_name":"beforeSubmitPrompt","prompt":"fix the\n tests"}"#,
                EventKind::TurnStart {
                    summary: Some("fix the tests".into()),
                },
            ),
            (
                r#"{"conversation_id":"s1","hook_event_name":"postToolUse","tool_name":"Shell"}"#,
                EventKind::Heartbeat {
                    activity: Some("Shell".into()),
                },
            ),
            (
                r#"{"conversation_id":"s1","hook_event_name":"preCompact","trigger":"auto"}"#,
                EventKind::Heartbeat {
                    activity: Some("compact".into()),
                },
            ),
            (
                r#"{"conversation_id":"s1","hook_event_name":"stop","status":"completed"}"#,
                EventKind::TurnEnd { summary: None },
            ),
            (
                r#"{"conversation_id":"s1","hook_event_name":"stop","status":"error"}"#,
                EventKind::TurnError {
                    reason: Some("error".into()),
                    summary: None,
                },
            ),
            (
                r#"{"conversation_id":"s1","hook_event_name":"sessionEnd","reason":"user_close"}"#,
                EventKind::SessionEnd,
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
    fn session_start_falls_back_to_model_id() {
        let start = event(
            r#"{"conversation_id":"s1","hook_event_name":"sessionStart","model_id":"composer-2.5"}"#,
        );
        assert_eq!(
            start.kind,
            EventKind::SessionStart {
                model: Some("composer-2.5".into()),
            }
        );
    }

    #[test]
    fn ignores_session_id_alias() {
        assert!(events(
            r#"{"session_id":"s1","hook_event_name":"sessionStart","model":"composer-2.5"}"#
        )
        .is_empty());
    }

    #[test]
    fn aborted_stop_is_a_turn_end() {
        assert_eq!(
            event(r#"{"conversation_id":"s1","hook_event_name":"stop","status":"aborted"}"#).kind,
            EventKind::TurnEnd { summary: None }
        );
    }

    #[test]
    fn stop_without_status_is_a_turn_end() {
        assert_eq!(
            event(r#"{"conversation_id":"s1","hook_event_name":"stop"}"#).kind,
            EventKind::TurnEnd { summary: None }
        );
    }

    #[test]
    fn post_tool_use_carries_detailed_activity() {
        assert_eq!(
            event(
                r#"{"conversation_id":"s1","hook_event_name":"postToolUse","tool_name":"Shell","tool_input":{"command":"cargo test"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("Shell: cargo test".into()),
            }
        );

        assert_eq!(
            event(
                r#"{"conversation_id":"s1","hook_event_name":"postToolUse","tool_name":"Write","tool_input":{"file_path":"/tmp/a.rs"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("Write: /tmp/a.rs".into()),
            }
        );

        assert_eq!(
            event(
                r#"{"conversation_id":"s1","hook_event_name":"postToolUse","tool_name":"Grep","tool_input":{"pattern":"TODO"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("Grep: TODO".into()),
            }
        );

        assert_eq!(
            event(
                r#"{"conversation_id":"s1","hook_event_name":"postToolUse","tool_name":"WebFetch","tool_input":{"url":"https://example.com"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("WebFetch".into()),
            }
        );
    }

    #[test]
    fn maps_subagent_lifecycle() {
        let start = event(
            r#"{"conversation_id":"s1","hook_event_name":"subagentStart","subagent_id":"a1","subagent_type":"explore","subagent_model":"composer-2.5","task":"map the tree"}"#,
        );
        assert_eq!(
            start.kind,
            EventKind::SubagentStart {
                agent: "a1".into(),
                agent_type: Some("explore".into()),
                model: Some("composer-2.5".into()),
                summary: Some("map the tree".into()),
            }
        );

        let stop = event(
            r#"{"conversation_id":"s1","hook_event_name":"subagentStop","subagent_id":"a1","subagent_type":"explore"}"#,
        );
        assert_eq!(stop.kind, EventKind::SubagentEnd { agent: "a1".into() });

        // Documented subagentStop payloads omit subagent_id; without an id
        // there is nothing to correlate.
        assert!(events(
            r#"{"conversation_id":"s1","hook_event_name":"subagentStop","subagent_type":"explore"}"#
        )
        .is_empty());
        assert!(events(r#"{"conversation_id":"s1","hook_event_name":"subagentStart"}"#).is_empty());
    }

    #[test]
    fn extracts_transcript_path() {
        let event = event(
            r#"{"conversation_id":"s1","hook_event_name":"sessionStart","transcript_path":"/tmp/t.jsonl"}"#,
        );
        assert_eq!(event.transcript.as_deref(), Some("/tmp/t.jsonl"));
    }

    #[test]
    fn truncates_long_summaries() {
        let long = "x".repeat(200);
        let payload = format!(
            r#"{{"conversation_id":"s1","hook_event_name":"beforeSubmitPrompt","prompt":"{long}"}}"#
        );
        let EventKind::TurnStart {
            summary: Some(summary),
        } = event(&payload).kind
        else {
            panic!("expected turn_start with summary");
        };
        assert_eq!(summary.chars().count(), 120);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn ignores_invalid_payloads() {
        assert!(events("not json").is_empty());
        assert!(
            events(r#"{"conversation_id":"s1","hook_event_name":"afterAgentThought"}"#).is_empty()
        );
        assert!(events(r#"{"hook_event_name":"sessionStart"}"#).is_empty());
    }

    #[test]
    fn ignores_unsubscribed_permission_shaped_events() {
        assert!(events(
            r#"{"conversation_id":"s1","hook_event_name":"beforeShellExecution","command":"rm -rf build"}"#
        )
        .is_empty());
        assert!(events(
            r#"{"conversation_id":"s1","hook_event_name":"preToolUse","tool_name":"Shell"}"#
        )
        .is_empty());
    }

    #[test]
    fn manifest_round_trips_and_subscribes_flat_hooks() {
        let manifest = manifest();
        assert_eq!(manifest.id, "cursor");
        assert_eq!(manifest.label, "Cursor");
        assert_eq!(manifest.launch.argv, ["cursor-agent"]);
        assert_eq!(
            manifest.resume.as_ref().unwrap().argv,
            ["cursor-agent", "--resume", "{session_id}"]
        );
        assert_eq!(
            manifest.resume_prompt.as_ref().unwrap().argv,
            ["cursor-agent", "--resume", "{session_id}", "{prompt}"]
        );
        assert!(manifest.fork.is_none());
        assert_eq!(
            manifest.transcript_glob.as_deref(),
            Some("~/.cursor/projects/*/agent-transcripts/{session_id}/{session_id}.jsonl")
        );
        assert!(manifest.process.argv0.contains(&"cursor-agent".into()));
        assert!(manifest.process.argv0.contains(&"agent".into()));
        assert!(manifest.process.exclude_args.contains(&"-p".into()));
        assert!(manifest.process.exclude_args.contains(&"acp".into()));
        assert!(manifest.process.exclude_args.contains(&"worker".into()));
        assert!(!manifest.process.exclude_args.contains(&"resume".into()));
        assert!(manifest.managed_files.is_empty());
        assert_eq!(manifest.hooks[0].path, "~/.cursor/hooks.json");

        let value = serde_json::to_value(&manifest).unwrap();
        let decoded: gw_plugin_protocol::Manifest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);

        let patches = value["hooks"][0]["patches"].as_array().unwrap();
        let pointer = |pointer: &str| {
            patches
                .iter()
                .find(|patch| patch["pointer"] == pointer)
                .unwrap_or_else(|| panic!("no patch for {pointer}"))
        };
        assert_eq!(pointer("/version")["mode"], "set");
        assert_eq!(pointer("/version")["value"], 1);
        assert_eq!(
            pointer("/hooks/sessionStart")["value"]["command"],
            "gw hook cursor"
        );
        assert!(pointer("/hooks/beforeSubmitPrompt")["value"]
            .get("matcher")
            .is_none());
        pointer("/hooks/postToolUse");
        pointer("/hooks/stop");
        pointer("/hooks/subagentStart");
        pointer("/hooks/subagentStop");
        pointer("/hooks/sessionEnd");
        assert!(patches
            .iter()
            .all(|patch| patch["pointer"] != "/hooks/beforeShellExecution"));
        assert!(patches
            .iter()
            .all(|patch| patch["pointer"] != "/hooks/preToolUse"));
    }
}
