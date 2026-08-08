//! Plugin discovery. A plugin is any executable named `gw-provider-<id>` on
//! PATH or in `~/.config/gw/providers/bin/`; the panel reads its manifest to
//! recognize and launch matching agent processes.

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::protocol::{Manifest, PROTOCOL_VERSION};

#[derive(Debug, Clone)]
pub struct Plugin {
    pub bin: PathBuf,
    pub manifest: Manifest,
}

/// Find plugin binaries, run `manifest` on each, drop those whose protocol
/// version is unsupported (with a warning on stderr). The plugin dir takes
/// precedence over PATH for the same id.
pub fn discover() -> Result<Vec<Plugin>> {
    let plugin_dir = match std::env::var_os("GW_PLUGIN_DIR") {
        Some(path) => PathBuf::from(path),
        None => dirs::home_dir()
            .context("could not determine home directory")?
            .join(".config/gw/providers/bin"),
    };
    let mut search_dirs = vec![plugin_dir];
    if let Some(path) = std::env::var_os("PATH") {
        search_dirs.extend(std::env::split_paths(&path));
    }

    let mut candidates = Vec::new();
    let mut ids = HashSet::new();
    for dir in search_dirs {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("read {}", dir.display())),
        };
        let mut paths = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let Some(id) = candidate_id(&path) else {
                continue;
            };
            if ids.contains(id) || !is_executable(&path) {
                continue;
            }
            ids.insert(id.to_owned());
            candidates.push(path);
        }
    }

    let mut plugins = Vec::new();
    for bin in candidates {
        let manifest = match read_manifest(&bin) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("warning: ignoring plugin {}: {error:#}", bin.display());
                continue;
            }
        };
        if manifest.protocol != PROTOCOL_VERSION {
            eprintln!(
                "warning: ignoring plugin {} with unsupported protocol {}",
                bin.display(),
                manifest.protocol
            );
            continue;
        }
        plugins.push(Plugin { bin, manifest });
    }
    Ok(plugins)
}

fn run_manifest(bin: &Path, timeout: Duration) -> Result<Output> {
    let mut child = Command::new(bin)
        .arg("manifest")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {} manifest", bin.display()))?;
    let mut stdout = child.stdout.take().context("plugin stdout unavailable")?;
    let mut stderr = child.stderr.take().context("plugin stderr unavailable")?;

    thread::scope(|scope| -> Result<Output> {
        let stdout_reader = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr_reader = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + timeout;
        let (status, timed_out) = loop {
            if let Some(status) = child.try_wait()? {
                break (status, false);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                break (child.wait()?, true);
            }
            thread::sleep(Duration::from_millis(10));
        };
        let stdout = stdout_reader
            .join()
            .expect("plugin stdout reader panicked")?;
        let stderr = stderr_reader
            .join()
            .expect("plugin stderr reader panicked")?;
        if timed_out {
            bail!("{} manifest timed out", bin.display());
        }
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}

fn candidate_id(path: &Path) -> Option<&str> {
    let id = path.file_name()?.to_str()?.strip_prefix("gw-provider-")?;
    (!id.is_empty()).then_some(id)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn read_manifest(bin: &Path) -> Result<Manifest> {
    // Generous: a hung-plugin guard, not a perf contract — process spawn can
    // take seconds on a loaded machine (nix sandbox builds hit this at 2s).
    let output = run_manifest(bin, Duration::from_secs(10))?;
    if !output.status.success() {
        bail!(
            "{} manifest failed: {}",
            bin.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse manifest from {}", bin.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        plugin_dir: Option<OsString>,
        path: Option<OsString>,
    }

    impl EnvGuard {
        fn set(plugin_dir: &Path) -> Self {
            let guard = Self {
                plugin_dir: std::env::var_os("GW_PLUGIN_DIR"),
                path: std::env::var_os("PATH"),
            };
            std::env::set_var("GW_PLUGIN_DIR", plugin_dir);
            std::env::set_var("PATH", "");
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.plugin_dir {
                Some(value) => std::env::set_var("GW_PLUGIN_DIR", value),
                None => std::env::remove_var("GW_PLUGIN_DIR"),
            }
            match &self.path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    fn write_plugin(dir: &Path, name: &str, protocol: u32) -> PathBuf {
        let path = dir.join(format!("gw-provider-{name}"));
        let script = format!(
            r#"#!/bin/sh
case "$1" in
  manifest)
    printf '%s\n' '{{"protocol":{protocol},"id":"{name}","label":"Test","process":{{"argv0":["test"]}},"launch":{{"argv":["test"]}}}}'
    ;;
esac
"#
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn discovers_fixture_manifest() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bin = write_plugin(temp.path(), "fixture", PROTOCOL_VERSION);
        let _env = EnvGuard::set(temp.path());

        let plugins = discover().unwrap();

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].bin, bin);
        assert_eq!(plugins[0].manifest.id, "fixture");
    }

    #[test]
    fn drops_plugins_with_unsupported_protocols() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        write_plugin(temp.path(), "future", PROTOCOL_VERSION + 1);
        let _env = EnvGuard::set(temp.path());

        assert!(discover().unwrap().is_empty());
    }

    #[test]
    fn ignores_broken_plugins_without_hiding_valid_ones() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let valid = write_plugin(temp.path(), "valid", PROTOCOL_VERSION);
        let broken = temp.path().join("gw-provider-broken");
        fs::write(&broken, "#!/bin/sh\nprintf 'not json\\n'\n").unwrap();
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
        let _env = EnvGuard::set(temp.path());

        let plugins = discover().unwrap();

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].bin, valid);
    }

    #[test]
    fn plugin_directory_wins_over_path_for_duplicate_id() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let plugin_dir = temp.path().join("plugins");
        let path_dir = temp.path().join("path");
        fs::create_dir(&plugin_dir).unwrap();
        fs::create_dir(&path_dir).unwrap();
        let preferred = write_plugin(&plugin_dir, "fixture", PROTOCOL_VERSION);
        write_plugin(&path_dir, "fixture", PROTOCOL_VERSION);
        let _env = EnvGuard::set(&plugin_dir);
        std::env::set_var("PATH", &path_dir);

        let plugins = discover().unwrap();

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].bin, preferred);
    }
}
