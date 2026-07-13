//! Process facts via `ps`: the bridge between hook processes, provider
//! processes, and tmux panes. No /proc on macOS, so everything goes through
//! one `ps -axo pid,ppid,tty,args` snapshot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::protocol::Manifest;
use crate::tmux;

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub tty: Option<String>,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AgentLocation {
    pub pid: i32,
    pub pane_id: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// One `ps` snapshot of all processes.
pub fn snapshot() -> Result<Vec<Proc>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,tty=,args="])
        .output()
        .context("failed to run ps")?;
    if !output.status.success() {
        bail!(
            "ps failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_snapshot(&String::from_utf8(output.stdout).context("ps output was not UTF-8")?)
}

/// Whether the process looks like an agent of `manifest`'s provider:
/// any of the first few argv tokens has a basename matching `process.argv0`
/// (first few, not just argv[0], to survive wrappers like `node /path/claude`).
pub fn matches_provider(proc_: &Proc, manifest: &Manifest) -> bool {
    proc_.argv.iter().take(4).any(|arg| {
        Path::new(arg)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                manifest
                    .process
                    .argv0
                    .iter()
                    .any(|candidate| candidate == name)
            })
    })
}

/// Walk the ppid chain from `from_pid` (a hook process) to the nearest
/// ancestor matching any manifest; resolve its tty to a pane and its cwd.
/// Returns the matched provider id with the location.
pub fn locate_agent(
    from_pid: i32,
    manifests: &[Manifest],
) -> Result<Option<(String, AgentLocation)>> {
    let procs = snapshot()?;
    let Some((provider, proc_)) = provider_ancestor(from_pid, &procs, manifests) else {
        return Ok(None);
    };
    let pane_id = match proc_.tty.as_deref() {
        Some(tty) => tmux::pane_for_tty(tty)?,
        None => None,
    };
    Ok(Some((
        provider,
        AgentLocation {
            pid: proc_.pid,
            pane_id,
            cwd: cwd_of(proc_.pid),
        },
    )))
}

/// Provider processes running inside `pane_root_pid`'s process tree
/// (pane pid is usually a shell; the agent is a descendant).
pub fn provider_procs_under(
    pane_root_pid: i32,
    procs: &[Proc],
    manifests: &[Manifest],
) -> Vec<(String, Proc)> {
    let by_pid: HashMap<_, _> = procs.iter().map(|proc_| (proc_.pid, proc_)).collect();
    let mut children: HashMap<i32, Vec<&Proc>> = HashMap::new();
    for proc_ in procs {
        children.entry(proc_.ppid).or_default().push(proc_);
    }

    let mut found = Vec::new();
    let mut stack = vec![pane_root_pid];
    while let Some(pid) = stack.pop() {
        if let Some(proc_) = by_pid.get(&pid) {
            if let Some(manifest) = manifests
                .iter()
                .find(|manifest| matches_provider(proc_, manifest))
            {
                found.push((manifest.id.clone(), (*proc_).clone()));
                continue;
            }
        }
        if let Some(descendants) = children.get(&pid) {
            stack.extend(descendants.iter().rev().map(|proc_| proc_.pid));
        }
    }
    found
}

/// Current working directory of a pid (via `lsof -a -d cwd -p <pid> -Fn`).
pub fn cwd_of(pid: i32) -> Option<PathBuf> {
    let output = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-p", &pid.to_string(), "-Fn"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_cwd(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

fn parse_snapshot(stdout: &str) -> Result<Vec<Proc>> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields
                .next()
                .context("missing pid")?
                .parse()
                .context("invalid pid")?;
            let ppid = fields
                .next()
                .context("missing ppid")?
                .parse()
                .context("invalid ppid")?;
            let tty = fields.next().context("missing tty")?;
            Ok(Proc {
                pid,
                ppid,
                tty: (!matches!(tty, "?" | "??" | "-")).then(|| tty.into()),
                argv: fields.map(str::to_owned).collect(),
            })
        })
        .collect()
}

fn provider_ancestor(
    from_pid: i32,
    procs: &[Proc],
    manifests: &[Manifest],
) -> Option<(String, Proc)> {
    let by_pid: HashMap<_, _> = procs.iter().map(|proc_| (proc_.pid, proc_)).collect();
    let mut pid = by_pid.get(&from_pid)?.ppid;
    while let Some(proc_) = by_pid.get(&pid) {
        if let Some(manifest) = manifests
            .iter()
            .find(|manifest| matches_provider(proc_, manifest))
        {
            return Some((manifest.id.clone(), (*proc_).clone()));
        }
        pid = proc_.ppid;
    }
    None
}

fn parse_cwd(stdout: &str) -> Option<PathBuf> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix('n').map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use crate::protocol::{Command as ProviderCommand, Manifest, ProcessMatch};

    use super::*;

    fn manifest(id: &str, names: &[&str]) -> Manifest {
        Manifest {
            protocol: 1,
            id: id.into(),
            label: id.into(),
            color: None,
            process: ProcessMatch {
                argv0: names.iter().map(|name| (*name).into()).collect(),
            },
            launch: ProviderCommand {
                argv: vec![id.into()],
            },
            resume: None,
            hooks: Vec::new(),
        }
    }

    fn proc_(pid: i32, ppid: i32, argv: &[&str]) -> Proc {
        Proc {
            pid,
            ppid,
            tty: Some("ttys001".into()),
            argv: argv.iter().map(|arg| (*arg).into()).collect(),
        }
    }

    #[test]
    fn parses_ps_columns_and_preserves_argument_tokens() {
        let procs = parse_snapshot(
            "  12     1 ??       /usr/bin/helper --flag value with spaces\n\
               42    12 ttys003  node /usr/local/bin/claude --session abc\n",
        )
        .unwrap();

        assert_eq!(procs[0].pid, 12);
        assert_eq!(procs[0].ppid, 1);
        assert_eq!(procs[0].tty, None);
        assert_eq!(
            procs[0].argv,
            ["/usr/bin/helper", "--flag", "value", "with", "spaces"]
        );
        assert_eq!(procs[1].tty.as_deref(), Some("ttys003"));
    }

    #[test]
    fn matches_basename_in_first_four_tokens_only() {
        let claude = manifest("claude", &["claude"]);
        assert!(matches_provider(
            &proc_(2, 1, &["node", "/usr/local/bin/claude"]),
            &claude
        ));
        assert!(!matches_provider(
            &proc_(2, 1, &["/tmp/claude-wrapper"]),
            &claude
        ));
        assert!(!matches_provider(
            &proc_(2, 1, &["a", "b", "c", "d", "claude"]),
            &claude
        ));
    }

    #[test]
    fn finds_nearest_provider_ancestor() {
        let manifests = [manifest("outer", &["outer"]), manifest("inner", &["inner"])];
        let procs = [
            proc_(10, 1, &["outer"]),
            proc_(20, 10, &["inner"]),
            proc_(30, 20, &["gw", "hook"]),
        ];

        let (provider, proc_) = provider_ancestor(30, &procs, &manifests).unwrap();
        assert_eq!(provider, "inner");
        assert_eq!(proc_.pid, 20);
    }

    #[test]
    fn subtree_skips_descendants_of_a_matched_provider() {
        let manifests = [
            manifest("claude", &["claude"]),
            manifest("codex", &["codex"]),
        ];
        let procs = [
            proc_(100, 1, &["zsh"]),
            proc_(110, 100, &["node", "/opt/bin/claude"]),
            proc_(111, 110, &["codex"]),
            proc_(120, 100, &["/usr/local/bin/codex"]),
        ];

        let found = provider_procs_under(100, &procs, &manifests);
        assert_eq!(
            found
                .iter()
                .map(|(id, proc_)| (id.as_str(), proc_.pid))
                .collect::<Vec<_>>(),
            [("claude", 110), ("codex", 120),]
        );
    }

    #[test]
    fn parses_lsof_cwd_line() {
        assert_eq!(
            parse_cwd("p42\nfcwd\nn/Users/me/project\n"),
            Some(PathBuf::from("/Users/me/project"))
        );
    }
}
