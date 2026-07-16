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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewWindowState {
    pub pane_count: u32,
    pub zoomed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelIdentity {
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGeometry {
    pub left: u32,
    pub top: u32,
    pub cols: u32,
    pub rows: u32,
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
    let commands = preview_session_create_commands(name, group_with);
    run_tmux(&commands[0])?;
    let configured = commands[1..]
        .iter()
        .try_for_each(|command| run_tmux(command).map(|_| ()));
    if configured.is_err() {
        let _ = kill_session(name);
    }
    configured
}

fn preview_session_create_commands(name: &str, group_with: &str) -> Vec<Vec<OsString>> {
    vec![
        vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            name.into(),
            "-t".into(),
            group_with.into(),
        ],
        vec![
            "set-option".into(),
            "-t".into(),
            name.into(),
            "status".into(),
            "off".into(),
        ],
    ]
}

pub fn preview_select_window(session: &str, window_id: &str) -> Result<()> {
    run_tmux(&[
        "select-window".into(),
        "-t".into(),
        format!("{session}:{window_id}").into(),
    ])?;
    Ok(())
}

pub fn pin_window_size(window_id: &str, cols: u16, rows: u16) -> Result<()> {
    let args = pin_window_size_command(window_id, cols, rows);
    run_tmux(&args)?;
    Ok(())
}

fn pin_window_size_command(window_id: &str, cols: u16, rows: u16) -> Vec<OsString> {
    vec![
        "resize-window".into(),
        "-t".into(),
        window_id.into(),
        "-x".into(),
        cols.to_string().into(),
        "-y".into(),
        rows.to_string().into(),
    ]
}

fn unset_window_size_command(window_id: &str) -> Vec<OsString> {
    vec![
        "set-option".into(),
        "-uw".into(),
        "-t".into(),
        window_id.into(),
        "window-size".into(),
    ]
}

pub fn release_window_size(window_id: &str) -> Result<()> {
    for command in release_window_size_commands(window_id) {
        run_tmux(&command)?;
    }
    Ok(())
}

fn release_window_size_commands(window_id: &str) -> Vec<Vec<OsString>> {
    vec![
        unset_window_size_command(window_id),
        vec![
            "resize-window".into(),
            "-A".into(),
            "-t".into(),
            window_id.into(),
        ],
        unset_window_size_command(window_id),
    ]
}

pub fn preview_window_state(window_id: &str) -> Result<PreviewWindowState> {
    let args = preview_window_state_command(window_id);
    parse_preview_window_state(&run_tmux(&args)?)
}

pub fn panel_identity(pane_id: &str) -> Result<PanelIdentity> {
    let args = panel_identity_command();
    parse_panel_identity(&run_tmux(&args)?, pane_id)
}

fn panel_identity_command() -> Vec<OsString> {
    vec![
        "list-panes".into(),
        "-a".into(),
        "-F".into(),
        "#{session_name}\t#{session_id}\t#{window_id}\t#{pane_id}".into(),
    ]
}

pub fn session_current_window(session_id: &str) -> Result<String> {
    let args = session_current_window_command(session_id);
    let window_id = run_tmux(&args)?;
    let window_id = window_id.trim();
    if window_id.is_empty() {
        bail!("tmux returned an empty current window id");
    }
    Ok(window_id.to_owned())
}

fn session_current_window_command(session_id: &str) -> Vec<OsString> {
    vec![
        "display-message".into(),
        "-p".into(),
        "-t".into(),
        session_id.into(),
        "#{window_id}".into(),
    ]
}

pub fn pane_geometry(pane_id: &str) -> Result<PaneGeometry> {
    let args = pane_geometry_command(pane_id);
    parse_pane_geometry(&run_tmux(&args)?)
}

fn pane_geometry_command(pane_id: &str) -> Vec<OsString> {
    vec![
        "display-message".into(),
        "-p".into(),
        "-t".into(),
        pane_id.into(),
        "#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}".into(),
    ]
}

fn preview_window_state_command(window_id: &str) -> Vec<OsString> {
    vec![
        "display-message".into(),
        "-p".into(),
        "-t".into(),
        window_id.into(),
        "#{window_panes}\t#{window_zoomed_flag}".into(),
    ]
}

pub fn toggle_pane_zoom(pane_id: &str) -> Result<()> {
    let args = toggle_pane_zoom_command(pane_id);
    run_tmux(&args)?;
    Ok(())
}

fn toggle_pane_zoom_command(pane_id: &str) -> Vec<OsString> {
    vec![
        "resize-pane".into(),
        "-Z".into(),
        "-t".into(),
        pane_id.into(),
    ]
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

fn parse_preview_window_state(stdout: &str) -> Result<PreviewWindowState> {
    let (pane_count, zoomed) = stdout
        .trim()
        .split_once('\t')
        .context("missing preview window state fields")?;
    let pane_count = pane_count.parse().context("invalid window pane count")?;
    let zoomed = parse_flag(zoomed, "window zoomed flag")?;
    Ok(PreviewWindowState { pane_count, zoomed })
}

fn parse_panel_identity(stdout: &str, pane_id: &str) -> Result<PanelIdentity> {
    for line in stdout.lines() {
        let mut fields = line.splitn(4, '\t');
        let Some(session_name) = fields.next() else {
            continue;
        };
        let Some(session_id) = fields.next() else {
            continue;
        };
        let Some(window_id) = fields.next() else {
            continue;
        };
        let Some(row_pane_id) = fields.next() else {
            continue;
        };
        if row_pane_id == pane_id && !session_name.starts_with("gw-preview-") {
            return Ok(PanelIdentity {
                session_id: session_id.to_owned(),
                window_id: window_id.to_owned(),
                pane_id: row_pane_id.to_owned(),
            });
        }
    }
    bail!("panel pane {pane_id} was not found outside preview sessions")
}

fn parse_pane_geometry(stdout: &str) -> Result<PaneGeometry> {
    let mut fields = stdout.trim().splitn(4, '\t');
    let mut next = |name| -> Result<u32> {
        fields
            .next()
            .with_context(|| format!("missing pane {name}"))?
            .parse()
            .with_context(|| format!("invalid pane {name}"))
    };
    Ok(PaneGeometry {
        left: next("left")?,
        top: next("top")?,
        cols: next("width")?,
        rows: next("height")?,
    })
}

fn parse_flag(stdout: &str, name: &str) -> Result<bool> {
    match stdout.trim() {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => bail!("invalid {name}"),
    }
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

    #[test]
    fn preview_session_setup_does_not_destroy_before_attach() {
        let commands = preview_session_create_commands("gw-preview-42", "main");
        let commands = commands
            .iter()
            .map(|command| {
                command
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[1],
            ["set-option", "-t", "gw-preview-42", "status", "off"]
        );
    }

    #[test]
    fn constructs_zoom_state_and_toggle_commands() {
        let state = preview_window_state_command("@7")
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let toggle = toggle_pane_zoom_command("%9")
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            state,
            [
                "display-message",
                "-p",
                "-t",
                "@7",
                "#{window_panes}\t#{window_zoomed_flag}",
            ]
        );
        assert_eq!(toggle, ["resize-pane", "-Z", "-t", "%9"]);
    }

    #[test]
    fn parses_zoom_state() {
        assert_eq!(
            parse_preview_window_state("3\t1\n").unwrap(),
            PreviewWindowState {
                pane_count: 3,
                zoomed: true,
            }
        );
        assert_eq!(
            parse_preview_window_state("1\t0\n").unwrap(),
            PreviewWindowState {
                pane_count: 1,
                zoomed: false,
            }
        );
        assert!(parse_preview_window_state("1\tmaybe\n").is_err());
    }

    #[test]
    fn constructs_manual_pin_and_release_commands() {
        let pin = pin_window_size_command("@7", 120, 17)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let release = release_window_size_commands("@7")
            .iter()
            .map(|command| {
                command
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(pin, ["resize-window", "-t", "@7", "-x", "120", "-y", "17"]);
        assert_eq!(
            release,
            vec![
                vec!["set-option", "-uw", "-t", "@7", "window-size"],
                vec!["resize-window", "-A", "-t", "@7"],
                vec!["set-option", "-uw", "-t", "@7", "window-size"],
            ]
        );
    }

    #[test]
    fn resolves_panel_identity_outside_preview_sessions() {
        let list = panel_identity_command()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let current = session_current_window_command("$1")
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            list,
            [
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{session_id}\t#{window_id}\t#{pane_id}",
            ]
        );
        assert_eq!(
            current,
            ["display-message", "-p", "-t", "$1", "#{window_id}"]
        );
        assert_eq!(
            parse_panel_identity(
                "gw-preview-42\t$2\t@8\t%3\n\
                 user\t$1\t@7\t%3\n\
                 other\t$3\t@9\t%4\n",
                "%3",
            )
            .unwrap(),
            PanelIdentity {
                session_id: "$1".into(),
                window_id: "@7".into(),
                pane_id: "%3".into(),
            }
        );
        assert!(parse_panel_identity("gw-preview-42\t$2\t@8\t%3\n", "%3").is_err());
    }

    #[test]
    fn constructs_and_parses_pane_geometry() {
        let command = pane_geometry_command("%3")
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            command,
            [
                "display-message",
                "-p",
                "-t",
                "%3",
                "#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}",
            ]
        );
        assert_eq!(
            parse_pane_geometry("12\t4\t80\t24\n").unwrap(),
            PaneGeometry {
                left: 12,
                top: 4,
                cols: 80,
                rows: 24,
            }
        );
        assert!(parse_pane_geometry("12\t4\t80\n").is_err());
    }
}
