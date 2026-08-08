//! `gw setup [--remove]`: install hooks for locally available agents and
//! report per-file outcomes.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result};
use gw_core::plugins::Plugin;

pub fn run(remove: bool, yes: bool) -> Result<()> {
    let plugins = gw_core::plugins::discover()?;
    let plugins = if remove {
        plugins
    } else {
        let (available, missing): (Vec<_>, Vec<_>) =
            plugins.into_iter().partition(agent_is_installed);
        if !missing.is_empty() {
            println!(
                "Skipping providers not installed locally: {}",
                missing
                    .iter()
                    .map(|plugin| plugin.manifest.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        available
    };
    let without_hooks = plugins
        .iter()
        .filter(|plugin| {
            plugin.manifest.hooks.is_empty() && plugin.manifest.managed_files.is_empty()
        })
        .map(|plugin| plugin.manifest.id.as_str())
        .collect::<Vec<_>>();
    if !without_hooks.is_empty() {
        eprintln!(
            "warning: providers with no hooks: {}",
            without_hooks.join(", ")
        );
    }

    let plugins = plugins
        .into_iter()
        .filter(|plugin| {
            !plugin.manifest.hooks.is_empty() || !plugin.manifest.managed_files.is_empty()
        })
        .collect::<Vec<_>>();
    if plugins.is_empty() {
        println!(
            "No {} provider integrations found.",
            if remove { "removable" } else { "installable" }
        );
        return Ok(());
    }

    if !remove && !yes {
        print_install_plan(&plugins);
        if !confirm(&mut io::stdin().lock(), &mut io::stdout().lock())? {
            println!("Setup cancelled; no files were changed.");
            return Ok(());
        }
    }

    let manifests = plugins
        .iter()
        .map(|plugin| plugin.manifest.clone())
        .collect::<Vec<_>>();
    let outcomes = if remove {
        gw_core::setup::remove(&manifests)?
    } else {
        gw_core::setup::install(&manifests)?
    };
    for (path, outcome) in outcomes {
        let label = match outcome {
            gw_core::setup::Outcome::Changed => "Changed",
            gw_core::setup::Outcome::AlreadyApplied => "AlreadyApplied",
        };
        println!("{}: {label}", path.display());
    }
    Ok(())
}

fn agent_is_installed(plugin: &Plugin) -> bool {
    plugin
        .manifest
        .launch
        .argv
        .first()
        .is_some_and(|command| executable_exists(command, std::env::var_os("PATH").as_deref()))
}

fn executable_exists(command: &str, path: Option<&OsStr>) -> bool {
    let command = Path::new(command);
    if command.components().count() > 1 {
        return is_executable(command);
    }
    path.into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join(command))
        .any(|candidate| is_executable(&candidate))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn print_install_plan(plugins: &[Plugin]) {
    println!("The following provider integrations will be installed:");
    for plugin in plugins {
        println!("  {} ({})", plugin.manifest.label, plugin.manifest.id);
        for path in integration_paths(plugin) {
            println!("    {path}");
        }
    }
}

fn integration_paths(plugin: &Plugin) -> Vec<&str> {
    let mut paths = Vec::new();
    for path in plugin
        .manifest
        .hooks
        .iter()
        .map(|hook| hook.path.as_str())
        .chain(
            plugin
                .manifest
                .managed_files
                .iter()
                .map(|file| file.path.as_str()),
        )
    {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

fn confirm(input: &mut impl BufRead, output: &mut impl Write) -> Result<bool> {
    write!(output, "Proceed? [y/N] ")?;
    output.flush()?;
    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .context("read setup confirmation")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::protocol::{Command, Manifest, ProcessMatch, PROTOCOL_VERSION};
    use std::path::PathBuf;

    fn plugin(command: &str) -> Plugin {
        Plugin {
            bin: PathBuf::from("gw-provider-fixture"),
            manifest: Manifest {
                protocol: PROTOCOL_VERSION,
                id: "fixture".into(),
                label: "Fixture".into(),
                color: None,
                process: ProcessMatch {
                    argv0: vec![command.into()],
                    exclude_args: Vec::new(),
                    exclude_arg_sequences: Vec::new(),
                },
                launch: Command {
                    argv: vec![command.into()],
                },
                resume: None,
                resume_prompt: None,
                fork: None,
                transcript: None,
                transcript_glob: None,
                hooks: Vec::new(),
                managed_files: Vec::new(),
            },
        }
    }

    #[test]
    fn detects_executable_agents_on_path_or_by_path() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("fixture");
        fs::write(&executable, "").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(executable_exists("fixture", Some(temp.path().as_os_str())));
        assert!(executable_exists(executable.to_str().unwrap(), None));
        assert!(!executable_exists("missing", Some(temp.path().as_os_str())));
    }

    #[test]
    fn confirmation_defaults_to_no_and_accepts_explicit_yes() {
        for answer in ["", "\n", "n\n", "anything\n"] {
            assert!(!confirm(&mut answer.as_bytes(), &mut Vec::new()).unwrap());
        }
        for answer in ["y\n", "YES\n", " yes \n"] {
            assert!(confirm(&mut answer.as_bytes(), &mut Vec::new()).unwrap());
        }
    }

    #[test]
    fn agent_detection_uses_the_manifest_launch_command() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("fixture");
        fs::write(&executable, "").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let plugin = plugin(executable.to_str().unwrap());

        assert!(agent_is_installed(&plugin));
    }
}
