use std::io::{Read, Write};

use gw_plugin_protocol::{
    AttentionKind, Command, Event, EventKind, FileFormat, HookFile, Manifest, Patch, PatchMode,
    ProcessMatch, PROTOCOL_VERSION,
};
use serde_json::{json, Value};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();

    if args.next().is_some() {
        usage();
    }

    match command.as_deref().and_then(|value| value.to_str()) {
        Some("manifest") => print_manifest(),
        Some("normalize") => print_events(),
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!("usage: gw-provider-agy <manifest|normalize>");
    std::process::exit(2);
}

fn print_manifest() {
    let Ok(json) = serde_json::to_string(&manifest()) else {
        return;
    };
    let _ = writeln!(std::io::stdout().lock(), "{json}");
}

fn print_events() {
    let mut payload = Vec::new();
    if std::io::stdin().read_to_end(&mut payload).is_err() {
        return;
    }

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    for event in normalize_payload(&payload) {
        let Ok(json) = serde_json::to_string(&event) else {
            return;
        };
        if writeln!(stdout, "{json}").is_err() {
            return;
        }
    }
}

fn manifest() -> Manifest {
    let patches = [
        "SessionStart",
        "UserPromptSubmit",
        "Notification",
        "PostToolUse",
        "Stop",
        "SessionEnd",
    ]
    .into_iter()
    .map(hook_patch)
    .collect();

    Manifest {
        protocol: PROTOCOL_VERSION,
        id: "agy".into(),
        label: "Antigravity".into(),
        color: Some("#4285F4".into()),
        process: ProcessMatch {
            argv0: vec!["agy".into()],
        },
        launch: Command {
            argv: vec!["agy".into()],
        },
        resume: Some(Command {
            argv: vec!["agy".into(), "--resume".into(), "{session_id}".into()],
        }),
        hooks: vec![HookFile {
            path: "~/.gemini/antigravity-cli/settings.json".into(),
            format: FileFormat::Json,
            patches,
        }],
    }
}

fn hook_patch(event: &str) -> Patch {
    Patch {
        pointer: format!("/hooks/{}", event),
        mode: PatchMode::Ensure,
        value: json!({
            "hooks": [{
                "type": "command",
                "command": "gw hook agy"
            }]
        }),
    }
}

fn normalize_payload(raw: &[u8]) -> Vec<Event> {
    let Ok(payload) = serde_json::from_slice::<Value>(raw) else {
        return Vec::new();
    };
    let Some(payload) = payload.as_object() else {
        return Vec::new();
    };
    let Some(session) = payload.get("session_id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(event_name) = payload.get("hook_event_name").and_then(Value::as_str) else {
        return Vec::new();
    };

    let kind = match event_name {
        "SessionStart" => EventKind::SessionStart,
        "UserPromptSubmit" => EventKind::TurnStart,
        "Notification" => {
            let Some(message) = payload.get("message").and_then(Value::as_str) else {
                return Vec::new();
            };
            EventKind::Attention {
                attention: AttentionKind::Notification,
                summary: Some(message.into()),
            }
        }
        "PostToolUse" => EventKind::Heartbeat,
        "Stop" => EventKind::TurnEnd,
        "SessionEnd" => EventKind::SessionEnd,
        _ => return Vec::new(),
    };

    vec![Event {
        v: PROTOCOL_VERSION,
        ts: None,
        session: session.into(),
        kind,
    }]
}
