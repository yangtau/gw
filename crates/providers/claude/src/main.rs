use gw_plugin_protocol::{
    AttentionKind, Command, EventKind, FileFormat, HookFile, Manifest, ProcessMatch,
    PROTOCOL_VERSION,
};
use gw_provider_sdk::{command_hook_patch, excerpt, one_liner, text};
use serde_json::{Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "session_id", map_kind);
}

// PermissionRequest is the approval signal; Notification(permission_prompt)
// fires for the same dialog and must not also be subscribed. idle_prompt is
// the 60s nag — turn_end already means done.
const SUBSCRIPTIONS: [(&str, Option<&str>); 13] = [
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", Some("AskUserQuestion|ExitPlanMode")),
    ("PermissionRequest", None),
    ("Notification", Some("elicitation_dialog|agent_needs_input")),
    ("PostToolUse", None),
    ("PreCompact", None),
    ("PostCompact", None),
    ("SubagentStart", None),
    ("SubagentStop", None),
    ("Stop", None),
    ("StopFailure", None),
    ("SessionEnd", None),
];

fn manifest() -> Manifest {
    let patches = SUBSCRIPTIONS
        .into_iter()
        .map(|(event, matcher)| command_hook_patch("claude", event, matcher))
        .collect();

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "claude".into(),
        label: "Claude".into(),
        color: Some("#D97757".into()),
        process: ProcessMatch {
            exclude_args: Vec::new(),
            exclude_arg_sequences: Vec::new(),
            argv0: vec!["claude".into()],
        },
        launch: Command {
            argv: vec!["claude".into()],
        },
        resume: Some(Command {
            argv: vec!["claude".into(), "--resume".into(), "{session_id}".into()],
        }),
        resume_prompt: Some(Command {
            argv: vec![
                "claude".into(),
                "--resume".into(),
                "{session_id}".into(),
                "{prompt}".into(),
            ],
        }),
        fork: Some(Command {
            argv: vec![
                "claude".into(),
                "--resume".into(),
                "{session_id}".into(),
                "--fork-session".into(),
            ],
        }),
        transcript: None,
        // Hook payloads carry transcript_path; the glob is the fallback for
        // sessions recorded before the transcript field existed.
        transcript_glob: Some("~/.claude/projects/*/{session_id}.jsonl".into()),
        hooks: vec![HookFile {
            path: "~/.claude/settings.json".into(),
            format: FileFormat::Json,
            patches,
        }],
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
        "PreToolUse" => match payload.get("tool_name").and_then(Value::as_str) {
            Some("AskUserQuestion") => Some(EventKind::Attention {
                attention: AttentionKind::Question,
                summary: first_question(payload).or_else(|| Some("question".into())),
            }),
            Some("ExitPlanMode") => Some(EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("plan ready for review".into()),
            }),
            // Only reachable if the installed matcher was widened by hand.
            Some(tool) => Some(EventKind::Heartbeat {
                activity: Some(tool.into()),
            }),
            None => None,
        },
        "PermissionRequest" => Some(EventKind::Attention {
            attention: AttentionKind::Approval,
            summary: tool_summary(payload),
        }),
        // Re-checked despite the installed matcher: a pre-matcher hook entry
        // delivers every type until `gw setup` is re-run.
        "Notification" => match payload.get("notification_type").and_then(Value::as_str) {
            Some("elicitation_dialog") | Some("agent_needs_input") => Some(EventKind::Attention {
                attention: AttentionKind::Question,
                summary: excerpt(payload, "message").or_else(|| text(payload, "notification_type")),
            }),
            _ => None,
        },
        "PostToolUse" => Some(EventKind::Heartbeat {
            activity: text(payload, "tool_name"),
        }),
        "PreCompact" | "PostCompact" => Some(EventKind::Heartbeat {
            activity: Some("compact".into()),
        }),
        // Both fire with the parent's session_id; agent_id names the subagent.
        // The payload carries only agent_id/agent_type — no model or task.
        "SubagentStart" => text(payload, "agent_id").map(|agent| EventKind::SubagentStart {
            agent,
            agent_type: text(payload, "agent_type"),
            model: None,
            summary: None,
        }),
        "SubagentStop" => text(payload, "agent_id").map(|agent| EventKind::SubagentEnd { agent }),
        "Stop" => Some(EventKind::TurnEnd {
            summary: excerpt(payload, "last_assistant_message"),
        }),
        "StopFailure" => Some(EventKind::TurnError {
            reason: text(payload, "error_type"),
            summary: excerpt(payload, "error_message"),
        }),
        "SessionEnd" => Some(EventKind::SessionEnd),
        _ => None,
    }
}

/// "Bash: rm -rf build" — tool name plus its most telling argument.
fn tool_summary(payload: &Map<String, Value>) -> Option<String> {
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    let argument = payload
        .get("tool_input")
        .and_then(Value::as_object)
        .and_then(|input| {
            ["command", "file_path", "description"]
                .iter()
                .find_map(|key| input.get(*key).and_then(Value::as_str))
        });
    match argument.and_then(one_liner) {
        Some(argument) => one_liner(&format!("{tool}: {argument}")),
        None => Some(tool.to_owned()),
    }
}

fn first_question(payload: &Map<String, Value>) -> Option<String> {
    payload
        .get("tool_input")?
        .get("questions")?
        .get(0)?
        .get("question")
        .and_then(Value::as_str)
        .and_then(one_liner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_plugin_protocol::Event;

    fn normalize_payload(raw: &[u8]) -> Vec<Event> {
        gw_provider_sdk::normalize("session_id", map_kind, raw)
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
                r#"{"session_id":"s1","hook_event_name":"SessionStart","model":"claude-sonnet-5"}"#,
                EventKind::SessionStart {
                    model: Some("claude-sonnet-5".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"fix the\n tests"}"#,
                EventKind::TurnStart {
                    summary: Some("fix the tests".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
                EventKind::Heartbeat {
                    activity: Some("Bash".into()),
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
            (
                r#"{"session_id":"s1","hook_event_name":"StopFailure","error_type":"rate_limit","error_message":"try later"}"#,
                EventKind::TurnError {
                    reason: Some("rate_limit".into()),
                    summary: Some("try later".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"SessionEnd","end_reason":"logout"}"#,
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
    fn permission_request_is_approval_with_tool_summary() {
        let approval = event(
            r#"{"session_id":"s2","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#,
        );
        assert_eq!(
            approval.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("Bash: rm -rf build".into()),
            }
        );

        let bare = event(
            r#"{"session_id":"s2","hook_event_name":"PermissionRequest","tool_name":"WebSearch","tool_input":{"query_kind":"x"}}"#,
        );
        assert_eq!(
            bare.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("WebSearch".into()),
            }
        );
    }

    #[test]
    fn interactive_tools_are_questions() {
        let ask = event(
            r#"{"session_id":"s2","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which db?"}]}}"#,
        );
        assert_eq!(
            ask.kind,
            EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("Which db?".into()),
            }
        );

        let plan = event(
            r#"{"session_id":"s2","hook_event_name":"PreToolUse","tool_name":"ExitPlanMode","tool_input":{}}"#,
        );
        assert_eq!(
            plan.kind,
            EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("plan ready for review".into()),
            }
        );
    }

    #[test]
    fn notifications_are_questions_with_message_or_type() {
        let with_message = event(
            r#"{"session_id":"s2","hook_event_name":"Notification","notification_type":"elicitation_dialog","message":"Form opened"}"#,
        );
        assert_eq!(
            with_message.kind,
            EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("Form opened".into()),
            }
        );

        let type_only = event(
            r#"{"session_id":"s2","hook_event_name":"Notification","notification_type":"agent_needs_input"}"#,
        );
        assert_eq!(
            type_only.kind,
            EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("agent_needs_input".into()),
            }
        );

        // A stale pre-matcher hook entry delivers every type; the rest are noise.
        for noise in ["permission_prompt", "idle_prompt", "auth_success"] {
            let payload = format!(
                r#"{{"session_id":"s2","hook_event_name":"Notification","notification_type":"{noise}","message":"hi"}}"#
            );
            assert!(normalize_payload(payload.as_bytes()).is_empty());
        }
    }

    #[test]
    fn maps_subagent_lifecycle() {
        // Real 2.1.210 payloads: SubagentStart carries only agent_id and
        // agent_type beyond the common fields — no model, no task text.
        let start = event(
            r#"{"session_id":"s1","transcript_path":"/tmp/t.jsonl","cwd":"/work","prompt_id":"p1","hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore"}"#,
        );
        assert_eq!(
            start.kind,
            EventKind::SubagentStart {
                agent: "a1".into(),
                agent_type: Some("Explore".into()),
                model: None,
                summary: None,
            }
        );

        let stop = event(
            r#"{"session_id":"s1","transcript_path":"/tmp/t.jsonl","cwd":"/work","prompt_id":"p1","hook_event_name":"SubagentStop","stop_hook_active":false,"agent_id":"a1","agent_transcript_path":"/tmp/a1.jsonl","agent_type":"Explore","last_assistant_message":"done"}"#,
        );
        assert_eq!(stop.kind, EventKind::SubagentEnd { agent: "a1".into() });

        // Without an agent id there is nothing to correlate start/stop.
        assert!(events(r#"{"session_id":"s1","hook_event_name":"SubagentStart"}"#).is_empty());
    }

    #[test]
    fn truncates_long_summaries() {
        let long = "x".repeat(200);
        let payload = format!(
            r#"{{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"{long}"}}"#
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
        assert!(events(r#"{"session_id":"s1","hook_event_name":"SomethingElse"}"#).is_empty());
        assert!(events(r#"{"hook_event_name":"SessionStart"}"#).is_empty());
    }

    #[test]
    fn manifest_round_trips_and_subscribes_with_matchers() {
        let manifest = manifest();
        assert!(manifest.managed_files.is_empty());
        let value = serde_json::to_value(manifest).unwrap();
        let decoded: gw_plugin_protocol::Manifest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);

        let patches = value["hooks"][0]["patches"].as_array().unwrap();
        let pointer = |pointer: &str| {
            patches
                .iter()
                .find(|patch| patch["pointer"] == pointer)
                .unwrap_or_else(|| panic!("no patch for {pointer}"))
        };
        assert_eq!(
            pointer("/hooks/Notification")["value"]["matcher"],
            "elicitation_dialog|agent_needs_input"
        );
        assert_eq!(
            pointer("/hooks/PreToolUse")["value"]["matcher"],
            "AskUserQuestion|ExitPlanMode"
        );
        assert!(pointer("/hooks/PermissionRequest")["value"]
            .get("matcher")
            .is_none());
        pointer("/hooks/StopFailure");
        pointer("/hooks/SubagentStart");
        pointer("/hooks/SubagentStop");
    }
}
