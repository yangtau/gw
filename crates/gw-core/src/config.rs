use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use toml_edit::DocumentMut;

use crate::protocol::EventKind;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_notify")]
    notify: Vec<NotifyEvent>,
    #[serde(default)]
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NotifyEvent {
    SessionStart,
    TurnStart,
    TurnEnd,
    TurnError,
    Attention,
    Heartbeat,
    SubagentStart,
    SubagentEnd,
    SessionEnd,
}

#[derive(Debug, Default, Deserialize)]
pub struct DebugConfig {
    #[serde(default)]
    pub hooks: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notify: default_notify(),
            debug: DebugConfig::default(),
        }
    }
}

fn default_notify() -> Vec<NotifyEvent> {
    vec![NotifyEvent::Attention]
}

impl Config {
    pub fn load() -> Config {
        let Some(path) = config_path() else {
            return Config::default();
        };
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Config::default();
            }
            Err(error) => {
                eprintln!(
                    "warning: could not read gw config {}: {error}",
                    path.display()
                );
                return Config::default();
            }
        };
        let document = match contents.parse::<DocumentMut>() {
            Ok(document) => document,
            Err(error) => {
                eprintln!(
                    "warning: could not parse gw config {}: {error}",
                    path.display()
                );
                return Config::default();
            }
        };

        Config {
            notify: parse_notify(document.get("notify")),
            debug: DebugConfig {
                hooks: document
                    .get("debug")
                    .and_then(|debug| debug.get("hooks"))
                    .and_then(toml_edit::Item::as_bool)
                    .unwrap_or_default(),
            },
        }
    }

    pub fn should_notify(&self, event: &EventKind) -> bool {
        let event = match event {
            EventKind::SessionStart { .. } => NotifyEvent::SessionStart,
            EventKind::TurnStart { .. } => NotifyEvent::TurnStart,
            EventKind::TurnEnd { .. } => NotifyEvent::TurnEnd,
            EventKind::TurnError { .. } => NotifyEvent::TurnError,
            EventKind::Attention { .. } => NotifyEvent::Attention,
            EventKind::Heartbeat { .. } => NotifyEvent::Heartbeat,
            EventKind::SubagentStart { .. } => NotifyEvent::SubagentStart,
            EventKind::SubagentEnd { .. } => NotifyEvent::SubagentEnd,
            EventKind::SessionEnd => NotifyEvent::SessionEnd,
        };
        self.notify.contains(&event)
    }
}

fn parse_notify(item: Option<&toml_edit::Item>) -> Vec<NotifyEvent> {
    let Some(item) = item else {
        return default_notify();
    };
    if let Some(enabled) = item.as_bool() {
        return if enabled {
            default_notify()
        } else {
            Vec::new()
        };
    }
    let Some(events) = item.as_array() else {
        return default_notify();
    };

    events
        .iter()
        .filter_map(|value| value.as_str())
        .filter_map(|name| match name {
            "session_start" => Some(NotifyEvent::SessionStart),
            "turn_start" => Some(NotifyEvent::TurnStart),
            "turn_end" => Some(NotifyEvent::TurnEnd),
            "turn_error" => Some(NotifyEvent::TurnError),
            "attention" => Some(NotifyEvent::Attention),
            "heartbeat" => Some(NotifyEvent::Heartbeat),
            "subagent_start" => Some(NotifyEvent::SubagentStart),
            "subagent_end" => Some(NotifyEvent::SubagentEnd),
            "session_end" => Some(NotifyEvent::SessionEnd),
            _ => None,
        })
        .collect()
}

fn config_path() -> Option<PathBuf> {
    match std::env::var_os("GW_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir).join("config.toml")),
        None => Some(dirs::home_dir()?.join(".config/gw/config.toml")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AttentionKind;
    use std::ffi::OsString;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct ConfigDirGuard(Option<OsString>);

    impl ConfigDirGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("GW_CONFIG_DIR");
            std::env::set_var("GW_CONFIG_DIR", path);
            Self(previous)
        }
    }

    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("GW_CONFIG_DIR", value),
                None => std::env::remove_var("GW_CONFIG_DIR"),
            }
        }
    }

    #[test]
    fn missing_file_uses_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _env = ConfigDirGuard::set(temp.path());

        let config = Config::load();
        assert!(config.should_notify(&attention()));
        assert!(!config.debug.hooks);
    }

    #[test]
    fn notify_false_disables_notifications() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), "notify = false\n").unwrap();
        let _env = ConfigDirGuard::set(temp.path());

        assert!(!Config::load().should_notify(&attention()));
    }

    #[test]
    fn loads_notify_event_list() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("config.toml"),
            "notify = [\"turn_end\", \"turn_error\"]\n",
        )
        .unwrap();
        let _env = ConfigDirGuard::set(temp.path());

        let config = Config::load();
        assert!(config.should_notify(&EventKind::TurnEnd { summary: None }));
        assert!(config.should_notify(&EventKind::TurnError {
            reason: None,
            summary: None,
        }));
        assert!(!config.should_notify(&attention()));
    }

    #[test]
    fn loads_debug_hooks() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), "[debug]\nhooks = true\n").unwrap();
        let _env = ConfigDirGuard::set(temp.path());

        assert!(Config::load().debug.hooks);
    }

    #[test]
    fn missing_debug_key_uses_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), "[debug]\n").unwrap();
        let _env = ConfigDirGuard::set(temp.path());

        let config = Config::load();
        assert!(config.should_notify(&attention()));
        assert!(!config.debug.hooks);
    }

    #[test]
    fn malformed_file_uses_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), "[debug\nhooks = true\n").unwrap();
        let _env = ConfigDirGuard::set(temp.path());

        let config = Config::load();
        assert!(config.should_notify(&attention()));
        assert!(!config.debug.hooks);
    }

    fn attention() -> EventKind {
        EventKind::Attention {
            attention: AttentionKind::Approval,
            summary: None,
        }
    }
}
