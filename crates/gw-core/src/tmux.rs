//! Thin wrapper over the tmux CLI. Scope is always the current session.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct Pane {
    pub id: String,
    pub window_id: String,
    /// Root pid of the pane (usually the shell).
    pub pid: i32,
    pub tty: String,
    pub cwd: PathBuf,
    pub window_index: u32,
    pub window_name: String,
}

/// Panes of the current session (`list-panes -s`).
pub fn list_panes() -> Result<Vec<Pane>> {
    let stdout = run_tmux(&[
        "list-panes".into(),
        "-s".into(),
        "-F".into(),
        "#{pane_id}\t#{window_id}\t#{pane_pid}\t#{pane_tty}\t#{pane_current_path}\t#{window_index}\t#{window_name}".into(),
    ])?;
    parse_panes(&stdout)
}

/// Pane whose tty matches, if any.
pub fn pane_for_tty(tty: &str) -> Result<Option<String>> {
    let tty = normalize_tty(tty);
    Ok(list_panes()?
        .into_iter()
        .find(|pane| normalize_tty(&pane.tty) == tty)
        .map(|pane| pane.id))
}

/// Open a new window running `argv` in `cwd`; returns the new pane id.
pub fn new_window(name: &str, cwd: &Path, argv: &[String]) -> Result<String> {
    let mut args = vec![
        "new-window".into(),
        "-P".into(),
        "-F".into(),
        "#{pane_id}".into(),
        "-n".into(),
        name.into(),
        "-c".into(),
        cwd.as_os_str().to_owned(),
        "--".into(),
    ];
    args.extend(argv.iter().map(|arg| OsString::from(arg.as_str())));
    Ok(parse_pane_id(&run_tmux(&args)?))
}

/// Focus the window/pane containing `pane_id`.
pub fn focus(pane_id: &str) -> Result<()> {
    run_tmux(&["select-window".into(), "-t".into(), pane_id.into()])?;
    run_tmux(&["select-pane".into(), "-t".into(), pane_id.into()])?;
    Ok(())
}

/// Last `lines` visible lines of the pane, for the preview.
pub fn capture(pane_id: &str, lines: u32) -> Result<String> {
    run_tmux(&[
        "capture-pane".into(),
        "-p".into(),
        "-t".into(),
        pane_id.into(),
        "-S".into(),
        format!("-{lines}").into(),
    ])
}

pub fn current_session_name() -> Result<String> {
    Ok(run_tmux(&[
        "display-message".into(),
        "-p".into(),
        "#{session_name}".into(),
    ])?
    .trim()
    .to_owned())
}

pub fn preview_session_create(name: &str, group_with: &str) -> Result<()> {
    run_tmux(&[
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.into(),
        "-t".into(),
        group_with.into(),
    ])?;
    let configured = (|| {
        run_tmux(&[
            "set-option".into(),
            "-t".into(),
            name.into(),
            "destroy-unattached".into(),
            "on".into(),
        ])?;
        run_tmux(&[
            "set-option".into(),
            "-t".into(),
            name.into(),
            "status".into(),
            "off".into(),
        ])?;
        Ok(())
    })();
    if configured.is_err() {
        let _ = kill_session(name);
    }
    configured
}

pub fn preview_select_window(session: &str, window_id: &str) -> Result<()> {
    run_tmux(&[
        "select-window".into(),
        "-t".into(),
        format!("{session}:{window_id}").into(),
    ])?;
    Ok(())
}

pub fn set_window_aggressive_resize(window_id: &str, on: bool) -> Result<()> {
    let mut args = vec!["set-option".into()];
    if on {
        args.extend([
            "-w".into(),
            "-t".into(),
            window_id.into(),
            "aggressive-resize".into(),
            "on".into(),
        ]);
    } else {
        args.extend([
            "-uw".into(),
            "-t".into(),
            window_id.into(),
            "aggressive-resize".into(),
        ]);
    }
    run_tmux(&args)?;
    Ok(())
}

pub fn kill_session(name: &str) -> Result<()> {
    run_tmux(&["kill-session".into(), "-t".into(), name.into()])?;
    Ok(())
}

pub fn stale_preview_sessions() -> Result<Vec<String>> {
    let stdout = run_tmux(&[
        "list-sessions".into(),
        "-F".into(),
        "#{session_name}".into(),
    ])?;
    stdout
        .lines()
        .filter_map(|name| preview_session_pid(name).map(|pid| (name, pid)))
        .filter_map(|(name, pid)| match process_is_alive(pid) {
            Ok(true) => None,
            Ok(false) => Some(Ok(name.to_owned())),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

/// Whether `GW_POPUP=1` marks this process as running in a tmux popup.
/// The recommended binding is `bind g popup -E 'GW_POPUP=1 gw'`.
pub fn inside_popup() -> bool {
    matches!(std::env::var("GW_POPUP").as_deref(), Ok("1"))
}

pub const TMUX_BINARIES: [&str; 3] = ["tmux", "/opt/homebrew/bin/tmux", "/usr/local/bin/tmux"];

fn run_tmux(args: &[OsString]) -> Result<String> {
    let mut not_found = None;
    for bin in TMUX_BINARIES {
        match Command::new(bin).args(args).output() {
            Ok(output) => {
                if !output.status.success() {
                    bail!(
                        "{bin} failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                return String::from_utf8(output.stdout).context("tmux output was not UTF-8");
            }
            Err(error) if error.kind() == ErrorKind::NotFound => not_found = Some(error),
            Err(error) => return Err(error).with_context(|| format!("failed to run {bin}")),
        }
    }
    Err(not_found.expect("tmux candidates are non-empty"))
        .context("tmux was not found on PATH or in common locations")
}

fn parse_panes(stdout: &str) -> Result<Vec<Pane>> {
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.splitn(7, '\t');
            let id = fields.next().context("missing pane id")?;
            let window_id = fields.next().context("missing window id")?;
            let pid = fields
                .next()
                .context("missing pane pid")?
                .parse()
                .context("invalid pane pid")?;
            let tty = fields.next().context("missing pane tty")?;
            let cwd = fields.next().context("missing pane cwd")?;
            let window_index = fields
                .next()
                .context("missing window index")?
                .parse()
                .context("invalid window index")?;
            let window_name = fields.next().context("missing window name")?;
            Ok(Pane {
                id: id.into(),
                window_id: window_id.into(),
                pid,
                tty: tty.into(),
                cwd: cwd.into(),
                window_index,
                window_name: window_name.into(),
            })
        })
        .collect()
}

fn preview_session_pid(name: &str) -> Option<u32> {
    name.strip_prefix("gw-preview-")?.parse().ok()
}

fn process_is_alive(pid: u32) -> Result<bool> {
    let status = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .context("failed to check preview process")?
        .status;
    Ok(status.success())
}

fn normalize_tty(tty: &str) -> &str {
    tty.strip_prefix("/dev/").unwrap_or(tty)
}

fn parse_pane_id(stdout: &str) -> String {
    stdout.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_panes_output() {
        let panes = parse_panes(
            "%1\t@2\t123\t/dev/ttys001\t/Users/me/project one\t2\tagent\n\
             %2\t@3\t456\tttys002\t/tmp\t3\tsecond window\n",
        )
        .unwrap();

        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].id, "%1");
        assert_eq!(panes[0].window_id, "@2");
        assert_eq!(panes[0].pid, 123);
        assert_eq!(panes[0].tty, "/dev/ttys001");
        assert_eq!(panes[0].cwd, PathBuf::from("/Users/me/project one"));
        assert_eq!(panes[0].window_index, 2);
        assert_eq!(panes[0].window_name, "agent");
        assert_eq!(panes[1].window_name, "second window");
    }

    #[test]
    fn normalizes_tty_prefix_and_pane_id_output() {
        assert_eq!(normalize_tty("/dev/ttys012"), normalize_tty("ttys012"));
        assert_eq!(parse_pane_id("%7\n"), "%7");
    }

    #[test]
    fn parses_preview_session_pid() {
        assert_eq!(preview_session_pid("gw-preview-123"), Some(123));
        assert_eq!(preview_session_pid("gw-preview-nope"), None);
        assert_eq!(preview_session_pid("other-123"), None);
    }
}
