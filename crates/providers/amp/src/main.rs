use gw_plugin_protocol::{
    AttentionKind, Command, EventKind, ManagedFile, Manifest, ProcessMatch, PROTOCOL_VERSION,
};
use gw_provider_sdk::{excerpt, text};
use serde_json::{Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "thread_id", map_kind);
}

fn manifest() -> Manifest {
    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "amp".into(),
        label: "Amp".into(),
        color: None,
        process: ProcessMatch {
            argv0: vec!["amp".into()],
            exclude_args: vec!["--no-tui".into(), "-x".into(), "--execute".into()],
        },
        launch: Command {
            argv: vec!["amp".into()],
        },
        resume: Some(Command {
            argv: vec![
                "amp".into(),
                "threads".into(),
                "continue".into(),
                "{session_id}".into(),
            ],
        }),
        hooks: Vec::new(),
        managed_files: vec![ManagedFile {
            path: "~/.config/amp/plugins/gw.ts".into(),
            content: include_str!("bridge.ts").into(),
            comment_prefix: "//".into(),
        }],
    }
}

fn map_kind(p: &Map<String, Value>) -> Option<EventKind> {
    match p.get("event").and_then(Value::as_str)? {
        "session_focus" => Some(EventKind::SessionFocus),
        "agent_start" => Some(EventKind::TurnStart {
            summary: excerpt(p, "message"),
        }),
        "tool_result" => Some(EventKind::Heartbeat {
            activity: excerpt(p, "tool"),
        }),
        "approval" => Some(EventKind::Attention {
            attention: AttentionKind::Approval,
            summary: excerpt(p, "summary").or_else(|| excerpt(p, "tool")),
        }),
        "agent_end" => match p.get("status").and_then(Value::as_str) {
            Some("done" | "cancelled") => Some(EventKind::TurnEnd {
                summary: excerpt(p, "summary"),
            }),
            Some("error") => Some(EventKind::TurnError {
                reason: Some("error".into()),
                summary: excerpt(p, "summary"),
            }),
            _ => None,
        },
        "state_error" => Some(EventKind::TurnError {
            reason: text(p, "reason").or_else(|| Some("error".into())),
            summary: excerpt(p, "summary"),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(raw: &str) -> Vec<gw_plugin_protocol::Event> {
        gw_provider_sdk::normalize("thread_id", map_kind, raw.as_bytes())
    }

    fn kind(raw: &str) -> EventKind {
        events(raw).pop().unwrap().kind
    }

    #[test]
    fn manifest_declares_amp_contract() {
        let m = manifest();
        assert_eq!(m.id, "amp");
        assert_eq!(m.process.argv0, ["amp"]);
        assert_eq!(m.launch.argv, ["amp"]);
        assert_eq!(
            m.resume.as_ref().unwrap().argv,
            ["amp", "threads", "continue", "{session_id}"]
        );
        assert_eq!(m.managed_files[0].path, "~/.config/amp/plugins/gw.ts");
        assert_eq!(m.process.exclude_args, ["--no-tui", "-x", "--execute"]);
        assert!(m.hooks.is_empty());
        assert!(!m.managed_files[0].content.contains("amp.on(\"tool.call\""));
    }

    #[test]
    fn maps_lifecycle_and_tool_signals() {
        let cases = [
            (
                r#"{"thread_id":"T-1","event":"session_focus"}"#,
                EventKind::SessionFocus,
            ),
            (
                r#"{"thread_id":"T-1","event":"agent_start","message":"fix the\n tests"}"#,
                EventKind::TurnStart {
                    summary: Some("fix the tests".into()),
                },
            ),
            (
                r#"{"thread_id":"T-1","event":"tool_result","tool":"shell: cargo test"}"#,
                EventKind::Heartbeat {
                    activity: Some("shell: cargo test".into()),
                },
            ),
            (
                r#"{"thread_id":"T-1","event":"agent_end","status":"done","summary":"shipped"}"#,
                EventKind::TurnEnd {
                    summary: Some("shipped".into()),
                },
            ),
            (
                r#"{"thread_id":"T-1","event":"agent_end","status":"cancelled","summary":"stopped"}"#,
                EventKind::TurnEnd {
                    summary: Some("stopped".into()),
                },
            ),
            (
                r#"{"thread_id":"T-1","event":"agent_end","status":"error","summary":"offline"}"#,
                EventKind::TurnError {
                    reason: Some("error".into()),
                    summary: Some("offline".into()),
                },
            ),
            (
                r#"{"thread_id":"T-1","event":"state_error"}"#,
                EventKind::TurnError {
                    reason: Some("error".into()),
                    summary: None,
                },
            ),
        ];

        for (raw, expected) in cases {
            assert_eq!(kind(raw), expected);
        }
    }

    #[test]
    fn maps_approval_and_ignores_invalid_payloads() {
        assert_eq!(
            kind(r#"{"thread_id":"T","event":"approval","tool":"shell"}"#),
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("shell".into()),
            }
        );
        assert!(events("bad").is_empty());
        assert!(events(r#"{"event":"session_focus"}"#).is_empty());
        assert!(events(r#"{"thread_id":"T","event":"unknown"}"#).is_empty());
        assert!(events(r#"{"thread_id":"T","event":"agent_end","status":"other"}"#).is_empty());
    }
}
