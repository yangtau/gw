use gw_plugin_protocol::{
    Command, EventKind, ManagedFile, Manifest, ProcessMatch, PROTOCOL_VERSION,
};
use gw_provider_sdk::{excerpt, text};
use serde_json::{Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "session_id", map_kind);
}

fn manifest() -> Manifest {
    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "pi".into(),
        label: "Pi".into(),
        color: Some("#A78BFA".into()),
        process: ProcessMatch {
            argv0: vec!["pi".into()],
            // The observer intentionally targets Pi's interactive TUI only.
            exclude_args: [
                "-p",
                "--print",
                "--export",
                "--list-models",
                "--mode=json",
                "--mode=rpc",
            ]
            .map(str::to_owned)
            .to_vec(),
            // `--mode text` is still the TUI, so match the non-interactive
            // option/value pairs instead of excluding every `--mode` use.
            exclude_arg_sequences: vec![
                vec!["--mode".into(), "json".into()],
                vec!["--mode".into(), "rpc".into()],
            ],
        },
        launch: Command {
            argv: vec!["pi".into()],
        },
        resume: Some(Command {
            argv: vec!["pi".into(), "--session".into(), "{session_id}".into()],
        }),
        resume_prompt: Some(Command {
            argv: vec![
                "pi".into(),
                "--session".into(),
                "{session_id}".into(),
                "{prompt}".into(),
            ],
        }),
        fork: Some(Command {
            argv: vec!["pi".into(), "--fork".into(), "{session_id}".into()],
        }),
        transcript: None,
        // Hook payloads carry the exact path. This covers sessions observed
        // before transcript-path capture and Pi's default session directory.
        transcript_glob: Some("~/.pi/agent/sessions/*/*{session_id}.jsonl".into()),
        hooks: Vec::new(),
        managed_files: vec![ManagedFile {
            path: "~/.pi/agent/extensions/gw.ts".into(),
            content: include_str!("bridge.ts").into(),
            comment_prefix: "//".into(),
            comment_suffix: String::new(),
        }],
    }
}

fn map_kind(payload: &Map<String, Value>) -> Option<EventKind> {
    match payload.get("event").and_then(Value::as_str)? {
        "session_focus" => Some(EventKind::SessionFocus),
        "session_start" => Some(EventKind::SessionStart {
            model: text(payload, "model"),
        }),
        "turn_start" => Some(EventKind::TurnStart {
            summary: excerpt(payload, "summary"),
        }),
        "tool_start" => Some(EventKind::Heartbeat {
            activity: excerpt(payload, "activity"),
        }),
        "agent_settled" => match payload.get("status").and_then(Value::as_str) {
            Some("done") => Some(EventKind::TurnEnd {
                summary: excerpt(payload, "summary"),
            }),
            Some("error") => Some(EventKind::TurnError {
                reason: text(payload, "reason").or_else(|| Some("error".into())),
                summary: excerpt(payload, "summary"),
            }),
            _ => None,
        },
        "session_end" => Some(EventKind::SessionEnd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_plugin_protocol::Event;

    fn events(raw: &str) -> Vec<Event> {
        gw_provider_sdk::normalize("session_id", map_kind, raw.as_bytes())
    }

    fn kind(raw: &str) -> EventKind {
        events(raw).pop().unwrap().kind
    }

    #[test]
    fn manifest_declares_pi_contract() {
        let manifest = manifest();

        assert_eq!(manifest.id, "pi");
        assert_eq!(manifest.process.argv0, ["pi"]);
        assert_eq!(
            manifest.process.exclude_args,
            [
                "-p",
                "--print",
                "--export",
                "--list-models",
                "--mode=json",
                "--mode=rpc",
            ]
        );
        assert_eq!(
            manifest.process.exclude_arg_sequences,
            [
                vec!["--mode".to_owned(), "json".to_owned()],
                vec!["--mode".to_owned(), "rpc".to_owned()],
            ]
        );
        assert_eq!(manifest.launch.argv, ["pi"]);
        assert_eq!(
            manifest.resume.as_ref().unwrap().argv,
            ["pi", "--session", "{session_id}"]
        );
        assert_eq!(
            manifest.resume_prompt.as_ref().unwrap().argv,
            ["pi", "--session", "{session_id}", "{prompt}"]
        );
        assert_eq!(
            manifest.fork.as_ref().unwrap().argv,
            ["pi", "--fork", "{session_id}"]
        );
        assert_eq!(
            manifest.transcript_glob.as_deref(),
            Some("~/.pi/agent/sessions/*/*{session_id}.jsonl")
        );
        assert!(manifest.hooks.is_empty());
        assert_eq!(manifest.managed_files.len(), 1);
        assert_eq!(
            manifest.managed_files[0].path,
            "~/.pi/agent/extensions/gw.ts"
        );
        assert!(manifest.managed_files[0]
            .content
            .contains("pi.on(\"agent_settled\""));
        assert!(manifest.managed_files[0]
            .content
            .contains("ctx.mode === \"tui\""));

        let value = serde_json::to_value(&manifest).unwrap();
        let decoded: Manifest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }

    #[test]
    fn maps_session_turn_and_tool_lifecycle() {
        let cases = [
            (
                r#"{"session_id":"s1","event":"session_focus"}"#,
                EventKind::SessionFocus,
            ),
            (
                r#"{"session_id":"s1","event":"session_start","model":"gpt-5.6-sol"}"#,
                EventKind::SessionStart {
                    model: Some("gpt-5.6-sol".into()),
                },
            ),
            (
                r#"{"session_id":"s1","event":"turn_start","summary":"fix the\n tests"}"#,
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
                r#"{"session_id":"s1","event":"agent_settled","status":"done","summary":"all green"}"#,
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
    fn maps_failed_settlement_to_turn_error() {
        assert_eq!(
            kind(
                r#"{"session_id":"s1","event":"agent_settled","status":"error","reason":"error","summary":"rate limited"}"#
            ),
            EventKind::TurnError {
                reason: Some("error".into()),
                summary: Some("rate limited".into()),
            }
        );
        assert_eq!(
            kind(r#"{"session_id":"s1","event":"agent_settled","status":"error"}"#),
            EventKind::TurnError {
                reason: Some("error".into()),
                summary: None,
            }
        );
    }

    #[test]
    fn captures_transcript_path_and_ignores_unknown_payloads() {
        let event = events(
            r#"{"session_id":"s1","transcript_path":"/tmp/pi.jsonl","event":"session_focus"}"#,
        )
        .pop()
        .unwrap();
        assert_eq!(event.transcript.as_deref(), Some("/tmp/pi.jsonl"));

        assert!(events("bad").is_empty());
        assert!(events(r#"{"event":"session_focus"}"#).is_empty());
        assert!(events(r#"{"session_id":"s1","event":"unknown"}"#).is_empty());
        assert!(
            events(r#"{"session_id":"s1","event":"agent_settled","status":"retrying"}"#).is_empty()
        );
    }
}
