use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use toml_edit::DocumentMut;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub debug: DebugConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct DebugConfig {
    #[serde(default)]
    pub hooks: bool,
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
            debug: DebugConfig {
                hooks: document
                    .get("debug")
                    .and_then(|debug| debug.get("hooks"))
                    .and_then(toml_edit::Item::as_bool)
                    .unwrap_or_default(),
            },
        }
    }
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

        assert!(!Config::load().debug.hooks);
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

        assert!(!Config::load().debug.hooks);
    }

    #[test]
    fn malformed_file_uses_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), "[debug\nhooks = true\n").unwrap();
        let _env = ConfigDirGuard::set(temp.path());

        assert!(!Config::load().debug.hooks);
    }
}
