//! Thin wrapper over the tmux CLI.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

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
pub struct PaneGeometry {
    pub left: u32,
    pub top: u32,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyRow {
    pub tmux_session_name: String,
    pub tmux_session_id: String,
    pub window_id: String,
    pub window_active: bool,
    pub tmux_session_attached: bool,
    pub pane_id: String,
    pub pane_pid: i32,
    pub pane_tty: String,
    pub pane_current_path: PathBuf,
    pub window_index: u32,
    pub window_name: String,
    pub geometry: PaneGeometry,
    pub window_panes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelLocation {
    pub tmux_session_id: String,
    pub window_id: String,
    pub visible: bool,
    pub geometry: PaneGeometry,
}

#[derive(Debug, Clone)]
pub struct TmuxSessionPane {
    pub pane: Pane,
    pub tmux_session_name: String,
    pub tmux_session_id: String,
}

impl TopologyRow {
    pub fn pane(&self) -> Pane {
        Pane {
            id: self.pane_id.clone(),
            window_id: self.window_id.clone(),
            pid: self.pane_pid,
            tty: self.pane_tty.clone(),
            cwd: self.pane_current_path.clone(),
            window_index: self.window_index,
            window_name: self.window_name.clone(),
        }
    }
}

pub fn locate_panel(pane_id: &str, rows: &[TopologyRow]) -> Option<PanelLocation> {
    let row = rows
        .iter()
        .filter(|row| row.pane_id == pane_id)
        .max_by_key(|row| (row.tmux_session_attached, row.window_active))?;
    Some(PanelLocation {
        tmux_session_id: row.tmux_session_id.clone(),
        window_id: row.window_id.clone(),
        visible: row.window_active && row.tmux_session_attached,
        geometry: row.geometry,
    })
}

/// Panes across all tmux sessions, deduplicated by pane id. A pane shared by
/// grouped tmux sessions belongs to the attached row, preferring its active
/// window when more than one candidate has the same attachment state.
pub fn panes_from_topology(rows: &[TopologyRow]) -> Vec<TmuxSessionPane> {
    let mut indices: HashMap<String, usize> = HashMap::new();
    let mut selected: Vec<&TopologyRow> = Vec::new();
    for row in rows {
        if let Some(&index) = indices.get(&row.pane_id) {
            let current = selected[index];
            if (row.tmux_session_attached, row.window_active)
                > (current.tmux_session_attached, current.window_active)
            {
                selected[index] = row;
            }
        } else {
            indices.insert(row.pane_id.clone(), selected.len());
            selected.push(row);
        }
    }
    selected
        .into_iter()
        .map(|row| TmuxSessionPane {
            pane: row.pane(),
            tmux_session_name: row.tmux_session_name.clone(),
            tmux_session_id: row.tmux_session_id.clone(),
        })
        .collect()
}

/// Panes across all tmux sessions, deduplicated by pane id because grouped
/// tmux sessions can report the same pane more than once.
pub fn list_panes() -> Result<Vec<Pane>> {
    Ok(panes_from_topology(&observe_topology()?)
        .into_iter()
        .map(|located| located.pane)
        .collect())
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
    run_tmux(&["switch-client".into(), "-t".into(), pane_id.into()])?;
    run_tmux(&["select-window".into(), "-t".into(), pane_id.into()])?;
    run_tmux(&["select-pane".into(), "-t".into(), pane_id.into()])?;
    Ok(())
}

pub fn current_tmux_session_name() -> Result<String> {
    Ok(run_tmux(&[
        "display-message".into(),
        "-p".into(),
        "#{session_name}".into(),
    ])?
    .trim()
    .to_owned())
}

pub fn current_tmux_session_id() -> Result<String> {
    Ok(run_tmux(&[
        "display-message".into(),
        "-p".into(),
        "#{session_id}".into(),
    ])?
    .trim()
    .to_owned())
}

pub fn observe_topology() -> Result<Vec<TopologyRow>> {
    let args = observe_topology_command();
    parse_topology(&run_tmux(&args)?)
}

fn observe_topology_command() -> Vec<OsString> {
    vec![
        "list-panes".into(),
        "-a".into(),
        "-F".into(),
        "#{session_name}\t#{session_id}\t#{window_id}\t#{window_active}\t#{session_attached}\t#{pane_id}\t#{pane_pid}\t#{pane_tty}\t#{pane_current_path}\t#{window_index}\t#{window_name}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{window_panes}".into(),
    ]
}

/// Whether `GW_POPUP=1` marks this process as running in a tmux popup.
/// The recommended binding is `bind g popup -E 'GW_POPUP=1 gw'`.
pub fn inside_popup() -> bool {
    matches!(std::env::var("GW_POPUP").as_deref(), Ok("1"))
}

const TMUX_BINARIES: [&str; 3] = ["tmux", "/opt/homebrew/bin/tmux", "/usr/local/bin/tmux"];

pub fn binary() -> &'static str {
    static BINARY: OnceLock<&'static str> = OnceLock::new();

    BINARY.get_or_init(|| {
        TMUX_BINARIES
            .iter()
            .copied()
            .find(|candidate| {
                Command::new(candidate)
                    .arg("-V")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            })
            .unwrap_or("tmux")
    })
}

fn run_tmux(args: &[OsString]) -> Result<String> {
    let binary = binary();
    let output = Command::new(binary)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {binary}"))?;
    if !output.status.success() {
        bail!(
            "{binary} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("tmux output was not UTF-8")
}

fn parse_topology(stdout: &str) -> Result<Vec<TopologyRow>> {
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.splitn(16, '\t');
            let tmux_session_name = fields.next().context("missing tmux session name")?;
            let tmux_session_id = fields.next().context("missing tmux session id")?;
            let window_id = fields.next().context("missing window id")?;
            let window_active = parse_flag(
                fields.next().context("missing window active flag")?,
                "window active flag",
            )?;
            let tmux_session_attached = fields
                .next()
                .context("missing session attached count")?
                .parse::<u32>()
                .context("invalid session attached count")?
                > 0;
            let pane_id = fields.next().context("missing pane id")?;
            let pane_pid = fields
                .next()
                .context("missing pane pid")?
                .parse()
                .context("invalid pane pid")?;
            let pane_tty = fields.next().context("missing pane tty")?;
            let pane_current_path = fields.next().context("missing pane cwd")?;
            let window_index = fields
                .next()
                .context("missing window index")?
                .parse()
                .context("invalid window index")?;
            let window_name = fields.next().context("missing window name")?;
            let left = fields
                .next()
                .context("missing pane left")?
                .parse()
                .context("invalid pane left")?;
            let top = fields
                .next()
                .context("missing pane top")?
                .parse()
                .context("invalid pane top")?;
            let cols = fields
                .next()
                .context("missing pane width")?
                .parse()
                .context("invalid pane width")?;
            let rows = fields
                .next()
                .context("missing pane height")?
                .parse()
                .context("invalid pane height")?;
            let window_panes = fields
                .next()
                .context("missing window pane count")?
                .parse()
                .context("invalid window pane count")?;
            Ok(TopologyRow {
                tmux_session_name: tmux_session_name.into(),
                tmux_session_id: tmux_session_id.into(),
                window_id: window_id.into(),
                window_active,
                tmux_session_attached,
                pane_id: pane_id.into(),
                pane_pid,
                pane_tty: pane_tty.into(),
                pane_current_path: pane_current_path.into(),
                window_index,
                window_name: window_name.into(),
                geometry: PaneGeometry {
                    left,
                    top,
                    cols,
                    rows,
                },
                window_panes,
            })
        })
        .collect()
}

fn parse_flag(stdout: &str, name: &str) -> Result<bool> {
    match stdout.trim() {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => bail!("invalid {name}"),
    }
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
    fn constructs_and_parses_topology_snapshot() {
        let command = observe_topology_command()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            command,
            [
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{session_id}\t#{window_id}\t#{window_active}\t#{session_attached}\t#{pane_id}\t#{pane_pid}\t#{pane_tty}\t#{pane_current_path}\t#{window_index}\t#{window_name}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{window_panes}",
            ]
        );

        let rows = parse_topology(
            "main\t$1\t@7\t1\t2\t%3\t123\t/dev/ttys001\t/Users/me/project one\t2\tagent\t12\t4\t80\t24\t3\n\
             main\t$1\t@9\t0\t2\t%4\t456\ttys002\t/tmp\t3\tsecond window\t0\t0\t100\t30\t1\n\
             group\t$2\t@7\t1\t0\t%3\t123\ttys001\t/Users/me/project one\t2\tagent\t12\t4\t80\t24\t3\n",
        )
        .unwrap();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].tmux_session_name, "main");
        assert_eq!(rows[0].tmux_session_id, "$1");
        assert_eq!(rows[0].window_id, "@7");
        assert!(rows[0].window_active);
        assert!(rows[0].tmux_session_attached);
        assert_eq!(rows[0].pane_id, "%3");
        assert_eq!(rows[0].pane_pid, 123);
        assert_eq!(rows[0].pane_tty, "/dev/ttys001");
        assert_eq!(
            rows[0].pane_current_path,
            PathBuf::from("/Users/me/project one")
        );
        assert_eq!(rows[0].window_index, 2);
        assert_eq!(rows[0].window_name, "agent");
        assert_eq!(rows[0].geometry.left, 12);
        assert_eq!(rows[0].geometry.top, 4);
        assert_eq!(rows[0].geometry.cols, 80);
        assert_eq!(rows[0].geometry.rows, 24);
        assert_eq!(rows[0].window_panes, 3);
        assert!(!rows[1].window_active);
    }

    #[test]
    fn locates_panel_in_the_attached_active_group_row() {
        let rows = parse_topology(
            "old\t$1\t@old\t1\t0\t%panel\t123\tttys001\t/tmp\t1\told\t0\t0\t80\t24\t1\n\
             attached\t$2\t@attached\t0\t1\t%panel\t123\tttys001\t/tmp\t2\tattached\t4\t6\t90\t30\t1\n\
             visible\t$3\t@visible\t1\t1\t%panel\t123\tttys001\t/tmp\t3\tvisible\t8\t10\t100\t40\t1\n",
        )
        .unwrap();

        assert_eq!(
            locate_panel("%panel", &rows[..2])
                .as_ref()
                .map(|panel| (panel.window_id.as_str(), panel.visible)),
            Some(("@attached", false))
        );
        assert_eq!(
            locate_panel("%panel", &rows),
            Some(PanelLocation {
                tmux_session_id: "$3".into(),
                window_id: "@visible".into(),
                visible: true,
                geometry: PaneGeometry {
                    left: 8,
                    top: 10,
                    cols: 100,
                    rows: 40,
                },
            })
        );
        assert_eq!(locate_panel("%missing", &rows), None);
    }

    #[test]
    fn grouped_pane_belongs_to_the_attached_tmux_session() {
        let rows = parse_topology(
            "detached\t$1\t@7\t1\t0\t%3\t123\tttys001\t/tmp\t2\tagent\t0\t0\t80\t24\t1\n\
             attached\t$2\t@7\t0\t1\t%3\t123\tttys001\t/tmp\t2\tagent\t0\t0\t80\t24\t1\n\
             other\t$3\t@9\t1\t1\t%4\t456\tttys002\t/work\t3\tother\t0\t0\t80\t24\t1\n",
        )
        .unwrap();

        let panes = panes_from_topology(&rows);

        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].pane.id, "%3");
        assert_eq!(panes[0].tmux_session_name, "attached");
        assert_eq!(panes[0].tmux_session_id, "$2");
        assert_eq!(panes[1].pane.id, "%4");
    }

    #[test]
    fn normalizes_tty_prefix_and_pane_id_output() {
        assert_eq!(normalize_tty("/dev/ttys012"), normalize_tty("ttys012"));
        assert_eq!(parse_pane_id("%7\n"), "%7");
    }
}
