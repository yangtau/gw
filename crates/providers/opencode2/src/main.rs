use gw_plugin_protocol::{
    AttentionKind, Command, EventKind, ManagedFile, Manifest, ProcessMatch, PROTOCOL_VERSION,
};
use gw_provider_sdk::{excerpt, text};
use serde_json::{Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "session_id", map_kind);
}

fn manifest() -> Manifest {
    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "opencode2".into(),
        label: "OpenCode2".into(),
        color: Some("#FF9F1C".into()),
        process: ProcessMatch {
            argv0: vec!["opencode2".into()],
            exclude_args: [
                "acp",
                "attach",
                "completion",
                "debug",
                "export",
                "github",
                "import",
                "mcp",
                "models",
                "plugin",
                "pr",
                "providers",
                "run",
                "serve",
                "session",
                "stats",
                "uninstall",
                "upgrade",
                "web",
            ]
            .map(str::to_owned)
            .to_vec(),
            exclude_arg_sequences: Vec::new(),
        },
        launch: Command {
            argv: vec!["opencode2".into()],
        },
        resume: Some(Command {
            argv: vec!["opencode2".into(), "--session".into(), "{session_id}".into()],
        }),
        resume_prompt: Some(Command {
            argv: vec![
                "opencode2".into(),
                "--session".into(),
                "{session_id}".into(),
                "--prompt".into(),
                "{prompt}".into(),
            ],
        }),
        fork: Some(Command {
            argv: vec![
                "opencode2".into(),
                "--session".into(),
                "{session_id}".into(),
                "--fork".into(),
            ],
        }),
        transcript: Some(Command {
            argv: vec!["opencode2".into(), "export".into(), "{session_id}".into()],
        }),
        transcript_glob: None,
        hooks: Vec::new(),
        managed_files: vec![ManagedFile {
            path: "~/.config/opencode2/plugins/gw.ts".into(),
            content: include_str!("bridge.ts").into(),
            comment_prefix: "//".into(),
            comment_suffix: String::new(),
        }],
    }
}

fn map_kind(payload: &Map<String, Value>) -> Option<EventKind> {
    match payload.get("event").and_then(Value::as_str)? {
        "session_start" => Some(EventKind::SessionStart {
            model: text(payload, "model"),
        }),
        "session_focus" => Some(EventKind::SessionFocus),
        "turn_start" => Some(EventKind::TurnStart {
            summary: excerpt(payload, "summary"),
        }),
        "tool_start" | "permission_replied" => Some(EventKind::Heartbeat {
            activity: excerpt(payload, "activity"),
        }),
        "permission_asked" => Some(EventKind::Attention {
            attention: AttentionKind::Approval,
            summary: excerpt(payload, "summary"),
        }),
        "turn_end" => Some(EventKind::TurnEnd {
            summary: excerpt(payload, "summary"),
        }),
        "turn_error" => Some(EventKind::TurnError {
            reason: text(payload, "reason").or_else(|| Some("error".into())),
            summary: excerpt(payload, "summary"),
        }),
        "session_end" => Some(EventKind::SessionEnd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(raw: &str) -> Vec<gw_plugin_protocol::Event> {
        gw_provider_sdk::normalize("session_id", map_kind, raw.as_bytes())
    }

    fn kind(raw: &str) -> EventKind {
        events(raw).pop().unwrap().kind
    }

    #[test]
    fn manifest_declares_opencode2_contract() {
        let manifest = manifest();
        assert_eq!(manifest.id, "opencode2");
        assert_eq!(manifest.process.argv0, ["opencode2"]);
        assert!(manifest.process.exclude_args.contains(&"run".into()));
        assert!(manifest.process.exclude_args.contains(&"attach".into()));
        assert_eq!(manifest.launch.argv, ["opencode2"]);
        assert_eq!(
            manifest.resume.as_ref().unwrap().argv,
            ["opencode2", "--session", "{session_id}"]
        );
        assert_eq!(
            manifest.resume_prompt.as_ref().unwrap().argv,
            [
                "opencode2",
                "--session",
                "{session_id}",
                "--prompt",
                "{prompt}"
            ]
        );
        assert_eq!(
            manifest.fork.as_ref().unwrap().argv,
            ["opencode2", "--session", "{session_id}", "--fork"]
        );
        assert_eq!(
            manifest.transcript.as_ref().unwrap().argv,
            ["opencode2", "export", "{session_id}"]
        );
        assert!(manifest.hooks.is_empty());
        assert_eq!(manifest.managed_files.len(), 1);
        assert_eq!(
            manifest.managed_files[0].path,
            "~/.config/opencode2/plugins/gw.ts"
        );
    }

    #[test]
    fn maps_lifecycle_activity_and_attention() {
        let cases = [
            (
                r#"{"session_id":"s1","event":"session_start","model":"modelhub/gpt-5.6-sol"}"#,
                EventKind::SessionStart {
                    model: Some("modelhub/gpt-5.6-sol".into()),
                },
            ),
            (
                r#"{"session_id":"s1","event":"session_focus"}"#,
                EventKind::SessionFocus,
            ),
            (
                r#"{"session_id":"s1","event":"turn_start","summary":"fix the tests"}"#,
                EventKind::TurnStart {
                    summary: Some("fix the tests".into()),
                },
            ),
            (
                r#"{"session_id":"s1","event":"tool_start","activity":"bash: cargo test"}"#,
                EventKind::Heartbeat {
                    activity: Some("bash: cargo test".into()),
                },
            ),
            (
                r#"{"session_id":"s1","event":"permission_asked","summary":"bash: cargo test"}"#,
                EventKind::Attention {
                    attention: AttentionKind::Approval,
                    summary: Some("bash: cargo test".into()),
                },
            ),
            (
                r#"{"session_id":"s1","event":"turn_end","summary":"all green"}"#,
                EventKind::TurnEnd {
                    summary: Some("all green".into()),
                },
            ),
            (
                r#"{"session_id":"s1","event":"session_end"}"#,
                EventKind::SessionEnd,
            ),
        ];

        for (raw, expected) in cases {
            assert_eq!(kind(raw), expected);
        }
    }

    #[test]
    fn maps_errors_and_ignores_invalid_payloads() {
        assert_eq!(
            kind(
                r#"{"session_id":"s1","event":"turn_error","reason":"APIError","summary":"rate limited"}"#
            ),
            EventKind::TurnError {
                reason: Some("APIError".into()),
                summary: Some("rate limited".into()),
            }
        );
        assert_eq!(
            kind(r#"{"session_id":"s1","event":"turn_error"}"#),
            EventKind::TurnError {
                reason: Some("error".into()),
                summary: None,
            }
        );
        assert!(events("bad").is_empty());
        assert!(events(r#"{"event":"turn_end"}"#).is_empty());
        assert!(events(r#"{"session_id":"s1","event":"unknown"}"#).is_empty());
    }

    #[test]
    fn bridge_uses_observer_only_opencode2_hooks() {
        let bridge = &manifest().managed_files[0].content;
        assert!(bridge.contains("eventType === \"session.status\""));
        assert!(bridge.contains("eventType === \"permission.asked\""));
        assert!(bridge.contains("\"tool.execute.before\""));
        assert!(!bridge.contains("\"permission.ask\""));
    }
}
