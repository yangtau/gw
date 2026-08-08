//! Plugin discovery and invocation. A plugin is any executable named
//! `gw-provider-<id>` on PATH or in `~/.config/gw/providers/bin/`.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::protocol::{Event, Manifest, PROTOCOL_VERSION};

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

pub fn find(id: &str) -> Result<Plugin> {
    discover()?
        .into_iter()
        .find(|plugin| plugin.manifest.id == id)
        .with_context(|| format!("provider plugin {id:?} not found"))
}

/// Pipe `payload` to the plugin's `normalize`; parse one event per stdout
/// line. A failing or garbage-printing plugin yields an error, never a panic.
pub fn normalize(plugin: &Plugin, payload: &[u8]) -> Result<Vec<Event>> {
    let output = run_with_timeout(
        &plugin.bin,
        "normalize",
        Some(payload),
        Duration::from_secs(10),
    )?;
    if !output.status.success() {
        bail!(
            "{} normalize failed: {}",
            plugin.bin.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // Lines that don't parse are skipped, not fatal: same evolution policy
    // as log replay — a plugin speaking a newer vocabulary keeps working.
    Ok(output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect())
}

fn run_with_timeout(
    bin: &Path,
    argument: &str,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    let mut child = Command::new(bin)
        .arg(argument)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {} {argument}", bin.display()))?;
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().context("plugin stdout unavailable")?;
    let mut stderr = child.stderr.take().context("plugin stderr unavailable")?;

    thread::scope(|scope| -> Result<Output> {
        let writer = scope.spawn(move || -> std::io::Result<()> {
            if let (Some(stdin), Some(input)) = (&mut stdin, input) {
                stdin.write_all(input)?;
            }
            Ok(())
        });
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
        let write_result = writer.join().expect("plugin stdin writer panicked");
        let stdout = stdout_reader
            .join()
            .expect("plugin stdout reader panicked")?;
        let stderr = stderr_reader
            .join()
            .expect("plugin stderr reader panicked")?;
        if timed_out {
            bail!("{} {argument} timed out", bin.display());
        }
        write_result?;
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
    let output = run_with_timeout(bin, "manifest", None, Duration::from_secs(10))?;
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
    use crate::protocol::EventKind;
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
  normalize)
    printf '%s\n' '{{"v":1,"session":"fixture-session","kind":"turn_start"}}'
    ;;
esac
"#
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn discovers_fixture_and_normalizes_events() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bin = write_plugin(temp.path(), "fixture", PROTOCOL_VERSION);
        let _env = EnvGuard::set(temp.path());

        let plugins = discover().unwrap();

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].bin, bin);
        assert_eq!(plugins[0].manifest.id, "fixture");
        let events = normalize(&plugins[0], br#"{"raw":true}"#).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session, "fixture-session");
        assert!(matches!(
            events[0].kind,
            EventKind::TurnStart { summary: None }
        ));
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

    #[test]
    fn normalize_skips_unparseable_lines_and_rejects_nonzero_exit() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bin = write_plugin(temp.path(), "fixture", PROTOCOL_VERSION);
        let _env = EnvGuard::set(temp.path());
        let plugin = discover().unwrap().remove(0);
        fs::write(
            &bin,
            concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' 'not json'\n",
                r#"printf '%s\n' '{"v":1,"session":"s","kind":"turn_start"}'"#,
                "\n",
            ),
        )
        .unwrap();
        let events = normalize(&plugin, b"payload").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session, "s");

        fs::write(&bin, "#!/bin/sh\nexit 7\n").unwrap();
        assert!(normalize(&plugin, b"payload").is_err());
    }
}
