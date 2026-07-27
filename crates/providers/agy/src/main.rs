use gw_plugin_protocol::{
    Command, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode, ProcessMatch,
    PROTOCOL_VERSION,
};
use serde_json::{json, Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "conversationId", map_kind);
}

fn manifest() -> Manifest {
    let patches = vec![
        Patch {
            pointer: "/gw/PreInvocation".into(),
            mode: PatchMode::Ensure,
            value: json!({
                "type": "command",
                "command": "gw hook agy; printf '{}'"
            }),
        },
        Patch {
            pointer: "/gw/PostToolUse".into(),
            mode: PatchMode::Ensure,
            value: json!({
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "gw hook agy; printf '{}'"
                }]
            }),
        },
        Patch {
            pointer: "/gw/Stop".into(),
            mode: PatchMode::Ensure,
            value: json!({
                "type": "command",
                "command": "gw hook agy; printf '{\"decision\":\"\"}'"
            }),
        },
    ];

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "agy".into(),
        label: "Antigravity".into(),
        color: Some("#4285F4".into()),
        process: ProcessMatch {
            exclude_args: Vec::new(),
            argv0: vec!["agy".into()],
        },
        launch: Command {
            argv: vec!["agy".into()],
        },
        resume: Some(Command {
            argv: vec!["agy".into(), "--conversation".into(), "{session_id}".into()],
        }),
        resume_prompt: None,
        fork: None,
        transcript: None,
        transcript_glob: None,
        hooks: vec![HookFile {
            path: "~/.gemini/config/hooks.json".into(),
            format: FileFormat::Json,
            patches,
        }],
        managed_files: Vec::new(),
    }
}

fn map_kind(payload: &Map<String, Value>) -> Option<EventKind> {
    if payload
        .get("executionNum")
        .and_then(Value::as_u64)
        .is_some()
    {
        match payload.get("fullyIdle").and_then(Value::as_bool) {
            Some(true) => Some(EventKind::TurnEnd { summary: None }),
            Some(false) => Some(EventKind::Heartbeat { activity: None }),
            None => None,
        }
    } else if payload.get("stepIdx").and_then(Value::as_u64).is_some() {
        Some(EventKind::Heartbeat { activity: None })
    } else if payload
        .get("invocationNum")
        .and_then(Value::as_u64)
        .is_some()
    {
        Some(EventKind::TurnStart { summary: None })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_plugin_protocol::Event;

    fn normalize_payload(raw: &[u8]) -> Vec<Event> {
        gw_provider_sdk::normalize("conversationId", map_kind, raw)
    }

    fn event(payload: &str) -> Event {
        normalize_payload(payload.as_bytes()).pop().unwrap()
    }

    #[test]
    fn manifest_uses_agy_cli_and_hook_contracts() {
        assert_eq!(
            serde_json::to_value(manifest()).unwrap(),
            json!({
                "protocol": PROTOCOL_VERSION,
                "id": "agy",
                "label": "Antigravity",
                "color": "#4285F4",
                "process": { "argv0": ["agy"] },
                "launch": { "argv": ["agy"] },
                "resume": { "argv": ["agy", "--conversation", "{session_id}"] },
                "hooks": [{
                    "path": "~/.gemini/config/hooks.json",
                    "format": "json",
                    "patches": [
                        {
                            "pointer": "/gw/PreInvocation",
                            "mode": "ensure",
                            "value": {
                                "type": "command",
                                "command": "gw hook agy; printf '{}'"
                            }
                        },
                        {
                            "pointer": "/gw/PostToolUse",
                            "mode": "ensure",
                            "value": {
                                "matcher": "*",
                                "hooks": [{
                                    "type": "command",
                                    "command": "gw hook agy; printf '{}'"
                                }]
                            }
                        },
                        {
                            "pointer": "/gw/Stop",
                            "mode": "ensure",
                            "value": {
                                "type": "command",
                                "command": "gw hook agy; printf '{\"decision\":\"\"}'"
                            }
                        }
                    ]
                }]
            })
        );
    }

    #[test]
    fn maps_agy_hook_payloads() {
        let cases = [
            (
                r#"{"conversationId":"s1","invocationNum":1,"initialNumSteps":0}"#,
                EventKind::TurnStart { summary: None },
            ),
            (
                r#"{"conversationId":"s1","stepIdx":5,"error":""}"#,
                EventKind::Heartbeat { activity: None },
            ),
            (
                r#"{"conversationId":"s1","executionNum":1,"terminationReason":"model_stop","fullyIdle":true}"#,
                EventKind::TurnEnd { summary: None },
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
    fn keeps_working_while_stop_reports_background_tasks() {
        let event = event(
            r#"{"conversationId":"s1","executionNum":1,"terminationReason":"model_stop","fullyIdle":false}"#,
        );

        assert_eq!(event.kind, EventKind::Heartbeat { activity: None });
    }

    #[test]
    fn ignores_invalid_and_unrecognized_payloads() {
        assert!(normalize_payload(b"not json").is_empty());
        assert!(normalize_payload(br#"{"conversationId":"s1"}"#).is_empty());
        assert!(normalize_payload(br#"{"invocationNum":1}"#).is_empty());
    }
}
