use gw_plugin_protocol::{
    AttentionKind, Command, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode,
    ProcessMatch, PROTOCOL_VERSION,
};
use gw_provider_sdk::{command_hook_patch, excerpt, one_liner, text};
use serde_json::{json, Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "session_id", map_kind);
}

// traex (TRAE CLI) is a codex fork that ships claude's `hook_event_name`-tagged
// hook system: richer than upstream codex (Notification, PostToolUseFailure,
// SessionEnd) but with no compaction hooks and no distinct StopFailure — a turn
// error surfaces through PostToolUseFailure or, failing that, staleness.
const SUBSCRIPTIONS: [(&str, Option<&str>); 11] = [
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", Some("AskUserQuestion|ExitPlanMode")),
    ("PermissionRequest", None),
    ("Notification", Some("elicitation_dialog")),
    ("PostToolUse", None),
    ("PostToolUseFailure", None),
    ("SubagentStart", None),
    ("SubagentStop", None),
    ("Stop", None),
    ("SessionEnd", None),
];

fn manifest() -> Manifest {
    let hook_patches = SUBSCRIPTIONS
        .into_iter()
        .map(|(event, matcher)| command_hook_patch("traex", event, matcher))
        .collect();

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "traex".into(),
        label: "TRAE".into(),
        color: Some("#4AC56E".into()),
        process: ProcessMatch {
            exclude_args: Vec::new(),
            // The CLI installs as `traex` with `traecli`/`trae-cli` aliases.
            argv0: vec!["traex".into(), "traecli".into(), "trae-cli".into()],
        },
        launch: Command {
            argv: vec!["traex".into()],
        },
        resume: Some(Command {
            argv: vec!["traex".into(), "resume".into(), "{session_id}".into()],
        }),
        resume_prompt: None,
        fork: None,
        transcript: None,
        transcript_glob: None,
        hooks: vec![
            HookFile {
                path: "~/.trae/cli/hooks.json".into(),
                format: FileFormat::Json,
                patches: hook_patches,
            },
            // The hook runtime is gated behind a feature flag in the top-level
            // config (note: ~/.trae, not ~/.trae/cli), mirroring codex's toggle.
            HookFile {
                path: "~/.trae/traecli.toml".into(),
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
        // Interactive tools block mid-turn on the user, but aren't approvals.
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
        // delivers every type until `gw setup` is re-run. permission_prompt is
        // the same dialog PermissionRequest already reports; idle_prompt is the
        // idle nag — Done already means the turn finished.
        "Notification" => match payload.get("notification_type").and_then(Value::as_str) {
            Some("elicitation_dialog") => Some(EventKind::Attention {
                attention: AttentionKind::Question,
                summary: excerpt(payload, "message").or_else(|| text(payload, "notification_type")),
            }),
            _ => None,
        },
        "PostToolUse" => Some(EventKind::Heartbeat {
            activity: text(payload, "tool_name"),
        }),
        // traex has no StopFailure; a failed tool is the clearest failure signal
        // it emits. Report it as an error rather than routine heartbeat activity.
        "PostToolUseFailure" => Some(EventKind::TurnError {
            reason: text(payload, "tool_name"),
            summary: excerpt(payload, "error")
                .or_else(|| excerpt(payload, "tool_response"))
                .or_else(|| excerpt(payload, "message")),
        }),
        // Both fire with the parent's session_id; agent_id names the subagent.
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
                r#"{"session_id":"s1","hook_event_name":"SessionStart","model":"gpt-5.6-sol"}"#,
                EventKind::SessionStart {
                    model: Some("gpt-5.6-sol".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"ship\n it"}"#,
                EventKind::TurnStart {
                    summary: Some("ship it".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
                EventKind::Heartbeat {
                    activity: Some("Bash".into()),
                },
            ),
            (
                r#"{"session_id":"s1","hook_event_name":"Stop","last_assistant_message":"done"}"#,
                EventKind::TurnEnd {
                    summary: Some("done".into()),
                },
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
    fn notifications_are_questions_only_for_elicitation() {
        let dialog = event(
            r#"{"session_id":"s2","hook_event_name":"Notification","notification_type":"elicitation_dialog","message":"Form opened"}"#,
        );
        assert_eq!(
            dialog.kind,
            EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("Form opened".into()),
            }
        );

        // permission_prompt duplicates PermissionRequest; idle_prompt is the
        // idle nag. A stale pre-matcher hook entry delivers both; both are noise.
        for noise in ["permission_prompt", "idle_prompt"] {
            let payload = format!(
                r#"{{"session_id":"s2","hook_event_name":"Notification","notification_type":"{noise}","message":"hi"}}"#
            );
            assert!(normalize_payload(payload.as_bytes()).is_empty());
        }
    }

    #[test]
    fn tool_failure_is_a_turn_error() {
        let failure = event(
            r#"{"session_id":"s3","hook_event_name":"PostToolUseFailure","tool_name":"Bash","error":"exit status 1"}"#,
        );
        assert_eq!(
            failure.kind,
            EventKind::TurnError {
                reason: Some("Bash".into()),
                summary: Some("exit status 1".into()),
            }
        );
    }

    #[test]
    fn maps_subagent_lifecycle() {
        let start = event(
            r#"{"session_id":"s1","hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore","model":"gpt-5.6-sol"}"#,
        );
        assert_eq!(
            start.kind,
            EventKind::SubagentStart {
                agent: "a1".into(),
                agent_type: Some("Explore".into()),
                model: Some("gpt-5.6-sol".into()),
                summary: None,
            }
        );

        let stop = event(
            r#"{"session_id":"s1","hook_event_name":"SubagentStop","agent_id":"a1","agent_type":"Explore"}"#,
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
        assert!(events(r#"{"session_id":"s1","hook_event_name":"PreCompact"}"#).is_empty());
        assert!(events(r#"{"session_id":"s1","hook_event_name":"SomethingElse"}"#).is_empty());
        assert!(events(r#"{"hook_event_name":"SessionStart"}"#).is_empty());
    }

    #[test]
    fn manifest_round_trips_and_toggles_the_hooks_feature() {
        let value = serde_json::to_value(manifest()).unwrap();
        let decoded: gw_plugin_protocol::Manifest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);

        assert_eq!(value["id"], "traex");
        assert_eq!(value["hooks"][0]["path"], "~/.trae/cli/hooks.json");

        let hook_patches = value["hooks"][0]["patches"].as_array().unwrap();
        let pointer = |pointer: &str| {
            hook_patches
                .iter()
                .find(|patch| patch["pointer"] == pointer)
                .unwrap_or_else(|| panic!("no patch for {pointer}"))
        };
        assert_eq!(
            pointer("/hooks/PreToolUse")["value"]["matcher"],
            "AskUserQuestion|ExitPlanMode"
        );
        assert_eq!(
            pointer("/hooks/Notification")["value"]["matcher"],
            "elicitation_dialog"
        );
        assert!(pointer("/hooks/PermissionRequest")["value"]
            .get("matcher")
            .is_none());
        assert_eq!(
            pointer("/hooks/Stop")["value"]["hooks"][0]["command"],
            "gw hook traex"
        );
        pointer("/hooks/PostToolUseFailure");
        pointer("/hooks/SessionEnd");

        // The feature toggle lives in the top-level config, set (not ensured).
        let feature = &value["hooks"][1];
        assert_eq!(feature["path"], "~/.trae/traecli.toml");
        assert_eq!(feature["format"], "toml");
        assert_eq!(feature["patches"][0]["pointer"], "/features/hooks");
        assert_eq!(feature["patches"][0]["mode"], "set");
        assert_eq!(feature["patches"][0]["value"], true);
    }
}
