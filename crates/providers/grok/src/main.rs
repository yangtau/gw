use gw_plugin_protocol::{
    AttentionKind, Command, EventKind, FileFormat, HookFile, Manifest, ProcessMatch,
    PROTOCOL_VERSION,
};
use gw_provider_sdk::{command_hook_patch, excerpt, one_liner, text};
use serde_json::{Map, Value};

fn main() {
    gw_provider_sdk::run(manifest(), "sessionId", map_kind);
}

// Grok has no PermissionRequest hook. The permission dialog is observed
// via Notification(permission_prompt). Questions come from the tools that
// *are* the wait: ask_user_question and exit_plan_mode.
const SUBSCRIPTIONS: [(&str, Option<&str>); 12] = [
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    (
        "PreToolUse",
        Some("ask_user_question|exit_plan_mode|AskUserQuestion|ExitPlanMode"),
    ),
    ("Notification", Some("permission_prompt")),
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
        .map(|(event, matcher)| command_hook_patch("grok", event, matcher))
        .collect();

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "grok".into(),
        label: "Grok".into(),
        color: Some("#D946EF".into()),
        process: ProcessMatch {
            argv0: vec!["grok".into()],
            // Interactive TUI only. Headless (`-p` / prompt-file) and the
            // ACP `agent` server are long-lived but have no pane to jump to.
            exclude_args: [
                "-p",
                "--single",
                "--prompt-file",
                "--prompt-json",
                "agent",
                "completions",
                "dashboard",
                "doctor",
                "export",
                "inspect",
                "leader",
                "login",
                "logout",
                "mcp",
                "memory",
                "models",
                "plugin",
                "sessions",
                "setup",
                "trace",
                "update",
                "version",
                "worktree",
                "wrap",
            ]
            .map(str::to_owned)
            .to_vec(),
            exclude_arg_sequences: Vec::new(),
        },
        launch: Command {
            argv: vec!["grok".into()],
        },
        resume: Some(Command {
            argv: vec!["grok".into(), "--resume".into(), "{session_id}".into()],
        }),
        resume_prompt: Some(Command {
            argv: vec![
                "grok".into(),
                "--resume".into(),
                "{session_id}".into(),
                "{prompt}".into(),
            ],
        }),
        fork: Some(Command {
            argv: vec![
                "grok".into(),
                "--resume".into(),
                "{session_id}".into(),
                "--fork-session".into(),
            ],
        }),
        transcript: Some(Command {
            argv: vec!["grok".into(), "export".into(), "{session_id}".into()],
        }),
        transcript_glob: Some("~/.grok/sessions/*/{session_id}/updates.jsonl".into()),
        hooks: vec![HookFile {
            path: "~/.grok/hooks/gw.json".into(),
            format: FileFormat::Json,
            patches,
        }],
        managed_files: Vec::new(),
    }
}

fn map_kind(payload: &Map<String, Value>) -> Option<EventKind> {
    let event_name = payload
        .get("hookEventName")
        .or_else(|| payload.get("hook_event_name"))
        .and_then(Value::as_str)?;

    match event_name {
        "SessionStart" | "session_start" => Some(EventKind::SessionStart {
            model: text(payload, "model"),
        }),
        "UserPromptSubmit" | "user_prompt_submit" => Some(EventKind::TurnStart {
            summary: excerpt(payload, "prompt"),
        }),
        "PreToolUse" | "pre_tool_use" => match tool_name(payload) {
            Some("ask_user_question" | "AskUserQuestion") => Some(EventKind::Attention {
                attention: AttentionKind::Question,
                summary: first_question(payload).or_else(|| Some("question".into())),
            }),
            Some("exit_plan_mode" | "ExitPlanMode") => Some(EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("plan ready for review".into()),
            }),
            // Only reachable if the installed matcher was widened by hand.
            Some(tool) => Some(EventKind::Heartbeat {
                activity: Some(tool.into()),
            }),
            None => None,
        },
        // Grok has no PermissionRequest hook today; keep the mapping so a
        // future payload (or a Claude-compat misroute) still becomes approval.
        "PermissionRequest" | "permission_request" => Some(EventKind::Attention {
            attention: AttentionKind::Approval,
            summary: tool_summary(payload),
        }),
        "Notification" | "notification" => match notification_type(payload) {
            Some("permission_prompt") => Some(EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: excerpt_field(payload, "message", "message")
                    .or_else(|| tool_summary(payload))
                    .or_else(|| Some("approval".into())),
            }),
            _ => None,
        },
        "PostToolUse" | "post_tool_use" => Some(EventKind::Heartbeat {
            activity: tool_summary(payload),
        }),
        "PreCompact" | "pre_compact" | "PostCompact" | "post_compact" => {
            Some(EventKind::Heartbeat {
                activity: Some("compact".into()),
            })
        }
        "SubagentStart" | "subagent_start" => {
            str_field(payload, "agentId", "agent_id").map(|agent| EventKind::SubagentStart {
                agent,
                agent_type: str_field(payload, "agentType", "agent_type"),
                model: text(payload, "model"),
                summary: excerpt(payload, "description").or_else(|| excerpt(payload, "prompt")),
            })
        }
        "SubagentStop" | "subagent_stop" | "SubagentEnd" | "subagent_end" => {
            str_field(payload, "agentId", "agent_id").map(|agent| EventKind::SubagentEnd { agent })
        }
        // A second Stop fires at session end (`channel_closed` / `shutdown`);
        // SessionEnd is the real boundary. Only genuine turn completions
        // (`end_turn`, or a payload with no reason) become turn_end.
        "Stop" | "stop" => match text(payload, "reason").as_deref() {
            Some("channel_closed" | "shutdown") => None,
            _ => Some(EventKind::TurnEnd {
                summary: excerpt_field(payload, "lastAssistantMessage", "last_assistant_message"),
            }),
        },
        "StopFailure" | "stop_failure" => Some(EventKind::TurnError {
            reason: str_field(payload, "error", "error_type"),
            summary: excerpt_field(payload, "errorDetails", "error_details")
                .or_else(|| {
                    excerpt_field(payload, "lastAssistantMessage", "last_assistant_message")
                })
                .or_else(|| excerpt(payload, "error_message")),
        }),
        "SessionEnd" | "session_end" => Some(EventKind::SessionEnd),
        _ => None,
    }
}

fn str_field(payload: &Map<String, Value>, camel: &str, snake: &str) -> Option<String> {
    text(payload, camel).or_else(|| text(payload, snake))
}

fn excerpt_field(payload: &Map<String, Value>, camel: &str, snake: &str) -> Option<String> {
    excerpt(payload, camel).or_else(|| excerpt(payload, snake))
}

fn tool_name(payload: &Map<String, Value>) -> Option<&str> {
    payload
        .get("toolName")
        .or_else(|| payload.get("tool_name"))
        .and_then(Value::as_str)
}

fn tool_input(payload: &Map<String, Value>) -> Option<&Map<String, Value>> {
    payload
        .get("toolInput")
        .or_else(|| payload.get("tool_input"))
        .and_then(Value::as_object)
}

fn notification_type(payload: &Map<String, Value>) -> Option<&str> {
    payload
        .get("notificationType")
        .or_else(|| payload.get("notification_type"))
        .and_then(Value::as_str)
}

/// "run_terminal_command: cargo test" — tool name plus its most telling argument.
fn tool_summary(payload: &Map<String, Value>) -> Option<String> {
    let tool = tool_name(payload)?;
    let argument = tool_input(payload).and_then(|input| {
        command_argument(input.get("command")).or_else(|| {
            [
                "file_path",
                "target_file",
                "path",
                "target_directory",
                "query",
                "pattern",
                "description",
            ]
            .iter()
            .find_map(|key| input.get(*key).and_then(Value::as_str).map(str::to_owned))
        })
    });
    match argument.as_deref().and_then(one_liner) {
        Some(argument) => one_liner(&format!("{tool}: {argument}")),
        None => Some(tool.to_owned()),
    }
}

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

fn first_question(payload: &Map<String, Value>) -> Option<String> {
    let first = tool_input(payload)?
        .get("questions")?
        .as_array()?
        .first()?
        .as_object()?;
    ["question", "header", "prompt", "text"]
        .iter()
        .find_map(|key| first.get(*key).and_then(Value::as_str))
        .and_then(one_liner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_plugin_protocol::Event;

    fn normalize_payload(raw: &[u8]) -> Vec<Event> {
        gw_provider_sdk::normalize("sessionId", map_kind, raw)
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
                r#"{"sessionId":"s1","hookEventName":"session_start","model":"grok-4"}"#,
                EventKind::SessionStart {
                    model: Some("grok-4".into()),
                },
            ),
            (
                r#"{"sessionId":"s1","hookEventName":"user_prompt_submit","prompt":"fix the\n tests"}"#,
                EventKind::TurnStart {
                    summary: Some("fix the tests".into()),
                },
            ),
            (
                r#"{"sessionId":"s1","hookEventName":"post_tool_use","toolName":"run_terminal_command"}"#,
                EventKind::Heartbeat {
                    activity: Some("run_terminal_command".into()),
                },
            ),
            (
                r#"{"sessionId":"s1","hookEventName":"pre_compact","trigger":"auto"}"#,
                EventKind::Heartbeat {
                    activity: Some("compact".into()),
                },
            ),
            (
                r#"{"sessionId":"s1","hookEventName":"stop","reason":"end_turn","lastAssistantMessage":"done"}"#,
                EventKind::TurnEnd {
                    summary: Some("done".into()),
                },
            ),
            (
                r#"{"sessionId":"s1","hookEventName":"stop_failure","error":"rate_limit","errorDetails":"try later"}"#,
                EventKind::TurnError {
                    reason: Some("rate_limit".into()),
                    summary: Some("try later".into()),
                },
            ),
            (
                r#"{"sessionId":"s1","hookEventName":"session_end","reason":"quit"}"#,
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
    fn accepts_pascal_case_event_names() {
        let start =
            event(r#"{"sessionId":"s1","hookEventName":"UserPromptSubmit","prompt":"hello"}"#);
        assert_eq!(
            start.kind,
            EventKind::TurnStart {
                summary: Some("hello".into()),
            }
        );
    }

    #[test]
    fn session_end_stop_is_not_a_turn_end() {
        assert!(events(
            r#"{"sessionId":"s1","hookEventName":"stop","reason":"channel_closed","lastAssistantMessage":"bye"}"#
        )
        .is_empty());
        assert!(
            events(r#"{"sessionId":"s1","hookEventName":"stop","reason":"shutdown"}"#).is_empty()
        );
    }

    #[test]
    fn stop_without_reason_is_a_turn_end() {
        assert_eq!(
            event(r#"{"sessionId":"s1","hookEventName":"stop","lastAssistantMessage":"ok"}"#).kind,
            EventKind::TurnEnd {
                summary: Some("ok".into()),
            }
        );
    }

    #[test]
    fn post_tool_use_carries_detailed_activity() {
        assert_eq!(
            event(
                r#"{"sessionId":"s1","hookEventName":"post_tool_use","toolName":"run_terminal_command","toolInput":{"command":"cargo test"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("run_terminal_command: cargo test".into()),
            }
        );

        assert_eq!(
            event(
                r#"{"sessionId":"s1","hookEventName":"post_tool_use","toolName":"read_file","toolInput":{"target_file":"/tmp/a.rs"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("read_file: /tmp/a.rs".into()),
            }
        );

        assert_eq!(
            event(
                r#"{"sessionId":"s1","hookEventName":"post_tool_use","toolName":"web_search","toolInput":{"query":"rust lifetimes"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("web_search: rust lifetimes".into()),
            }
        );

        assert_eq!(
            event(
                r#"{"sessionId":"s1","hookEventName":"post_tool_use","toolName":"web_fetch","toolInput":{"url":"https://example.com"}}"#
            )
            .kind,
            EventKind::Heartbeat {
                activity: Some("web_fetch".into()),
            }
        );
    }

    #[test]
    fn permission_prompt_is_approval() {
        let with_message = event(
            r#"{"sessionId":"s2","hookEventName":"notification","notificationType":"permission_prompt","message":"Allow cargo test"}"#,
        );
        assert_eq!(
            with_message.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("Allow cargo test".into()),
            }
        );

        let with_tool = event(
            r#"{"sessionId":"s2","hookEventName":"notification","notificationType":"permission_prompt","toolName":"run_terminal_command","toolInput":{"command":"rm -rf build"}}"#,
        );
        assert_eq!(
            with_tool.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("run_terminal_command: rm -rf build".into()),
            }
        );

        let bare = event(
            r#"{"sessionId":"s2","hookEventName":"notification","notificationType":"permission_prompt"}"#,
        );
        assert_eq!(
            bare.kind,
            EventKind::Attention {
                attention: AttentionKind::Approval,
                summary: Some("approval".into()),
            }
        );

        for noise in ["tool_execution", "unknown", "idle_prompt"] {
            let payload = format!(
                r#"{{"sessionId":"s2","hookEventName":"notification","notificationType":"{noise}","message":"hi"}}"#
            );
            assert!(normalize_payload(payload.as_bytes()).is_empty());
        }
    }

    #[test]
    fn interactive_tools_are_questions() {
        let ask = event(
            r#"{"sessionId":"s2","hookEventName":"pre_tool_use","toolName":"ask_user_question","toolInput":{"questions":[{"question":"Which db?"}]}}"#,
        );
        assert_eq!(
            ask.kind,
            EventKind::Attention {
                attention: AttentionKind::Question,
                summary: Some("Which db?".into()),
            }
        );

        let plan = event(
            r#"{"sessionId":"s2","hookEventName":"pre_tool_use","toolName":"exit_plan_mode","toolInput":{}}"#,
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
    fn maps_subagent_lifecycle() {
        let start = event(
            r#"{"sessionId":"s1","hookEventName":"subagent_start","agentId":"a1","agentType":"explore","model":"grok-4","description":"map the tree"}"#,
        );
        assert_eq!(
            start.kind,
            EventKind::SubagentStart {
                agent: "a1".into(),
                agent_type: Some("explore".into()),
                model: Some("grok-4".into()),
                summary: Some("map the tree".into()),
            }
        );

        let stop = event(
            r#"{"sessionId":"s1","hookEventName":"subagent_stop","agentId":"a1","agentType":"explore"}"#,
        );
        assert_eq!(stop.kind, EventKind::SubagentEnd { agent: "a1".into() });

        assert!(events(r#"{"sessionId":"s1","hookEventName":"subagent_start"}"#).is_empty());
    }

    #[test]
    fn truncates_long_summaries() {
        let long = "x".repeat(200);
        let payload = format!(
            r#"{{"sessionId":"s1","hookEventName":"user_prompt_submit","prompt":"{long}"}}"#
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
        assert!(events(r#"{"sessionId":"s1","hookEventName":"SomethingElse"}"#).is_empty());
        assert!(events(r#"{"hookEventName":"session_start"}"#).is_empty());
        assert!(events(r#"{"session_id":"s1","hookEventName":"session_start"}"#).is_empty());
    }

    #[test]
    fn manifest_round_trips_and_subscribes_with_matchers() {
        let manifest = manifest();
        assert_eq!(manifest.id, "grok");
        assert_eq!(manifest.label, "Grok");
        assert_eq!(manifest.launch.argv, ["grok"]);
        assert_eq!(
            manifest.resume.as_ref().unwrap().argv,
            ["grok", "--resume", "{session_id}"]
        );
        assert_eq!(
            manifest.fork.as_ref().unwrap().argv,
            ["grok", "--resume", "{session_id}", "--fork-session"]
        );
        assert_eq!(
            manifest.transcript.as_ref().unwrap().argv,
            ["grok", "export", "{session_id}"]
        );
        assert_eq!(
            manifest.transcript_glob.as_deref(),
            Some("~/.grok/sessions/*/{session_id}/updates.jsonl")
        );
        assert!(manifest.process.exclude_args.contains(&"-p".into()));
        assert!(manifest.process.exclude_args.contains(&"agent".into()));
        assert!(manifest.process.exclude_args.contains(&"dashboard".into()));
        assert!(manifest.managed_files.is_empty());
        assert_eq!(manifest.hooks[0].path, "~/.grok/hooks/gw.json");

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
        assert_eq!(
            pointer("/hooks/Notification")["value"]["matcher"],
            "permission_prompt"
        );
        assert_eq!(
            pointer("/hooks/PreToolUse")["value"]["matcher"],
            "ask_user_question|exit_plan_mode|AskUserQuestion|ExitPlanMode"
        );
        assert!(pointer("/hooks/Stop")["value"].get("matcher").is_none());
        pointer("/hooks/StopFailure");
        pointer("/hooks/SubagentStart");
        pointer("/hooks/SubagentStop");
        assert!(patches
            .iter()
            .all(|patch| patch["pointer"] != "/hooks/PermissionRequest"));
    }
}
