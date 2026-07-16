use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use gw_core::{tmux, tui_log};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc::UnboundedSender;

const PARKED_COLS: u16 = 800;
const PARKED_ROWS: u16 = 240;
const PREVIEW_LINES: u32 = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

pub enum PreviewVisibility {
    Always,
    Polled { pane_id: String },
}

impl PreviewVisibility {
    pub fn new(popup: bool, pane_id: Option<String>) -> Self {
        match (popup, pane_id) {
            (false, Some(pane_id)) => Self::Polled { pane_id },
            _ => Self::Always,
        }
    }

    fn pane_id(&self) -> Option<&str> {
        match self {
            Self::Always => None,
            Self::Polled { pane_id } => Some(pane_id),
        }
    }

    fn is_always(&self) -> bool {
        matches!(self, Self::Always)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentTopology {
    pane_id: String,
    window_id: String,
    window_panes: u32,
    window_index: u32,
    window_name: String,
    geometry: tmux::PaneGeometry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreviewTopology {
    panel: Option<tmux::PanelLocation>,
    agent: Option<AgentTopology>,
    visible: bool,
    colocated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TopologyAction {
    Clear,
    Placard(Direction),
    Live {
        agent: AgentTopology,
        visible: bool,
        reacquire: bool,
    },
}

pub struct PreviewView {
    pub title: Option<String>,
    pub content: PreviewContent,
}

pub enum PreviewContent {
    Empty,
    Placard(Direction),
    Live(Arc<Mutex<vt100::Parser>>),
    Snapshot(String),
}

pub struct Preview {
    session: String,
    state: State,
    wanted: Option<Target>,
    selected: Option<Selection>,
    visible: bool,
    notifications: UnboundedSender<()>,
    snapshot_preview: String,
    preview_direction: Option<Direction>,
    topology: Option<PreviewTopology>,
    visibility: PreviewVisibility,
}

#[derive(Clone, Eq, PartialEq)]
struct Target {
    window_id: String,
    pane_id: String,
}

impl Target {
    fn new(window_id: &str, pane_id: &str) -> Self {
        Self {
            window_id: window_id.to_owned(),
            pane_id: pane_id.to_owned(),
        }
    }
}

struct Selection {
    window_id: String,
    pane_id: String,
    zoomed_by_us: bool,
    size_pinned_by_us: bool,
}

impl Selection {
    fn new(target: &Target) -> Self {
        Self {
            window_id: target.window_id.clone(),
            pane_id: target.pane_id.clone(),
            zoomed_by_us: false,
            size_pinned_by_us: false,
        }
    }
}

enum State {
    Uninitialized,
    Live(LiveClient),
    Dead,
}

struct LiveClient {
    master: Box<dyn MasterPty + Send>,
    _child: Box<dyn Child + Send + Sync>,
    parser: Arc<Mutex<vt100::Parser>>,
    alive: Arc<AtomicBool>,
    panel_size: PtySize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SizeStep {
    Pty(u16, u16),
    PinWindow { cols: u16, rows: u16 },
}

fn resize_steps(current: (u16, u16), target: (u16, u16)) -> [SizeStep; 3] {
    let bridge = (current.0.max(target.0), current.1.max(target.1));
    [
        SizeStep::Pty(bridge.0, bridge.1),
        SizeStep::PinWindow {
            cols: target.0,
            rows: target.1,
        },
        SizeStep::Pty(target.0, target.1),
    ]
}

fn acquire_steps(panel: (u16, u16)) -> [SizeStep; 2] {
    [
        SizeStep::PinWindow {
            cols: panel.0,
            rows: panel.1,
        },
        SizeStep::Pty(panel.0, panel.1),
    ]
}

fn park_steps() -> [SizeStep; 1] {
    [SizeStep::Pty(PARKED_COLS, PARKED_ROWS)]
}

impl LiveClient {
    fn execute_size_steps<const N: usize>(
        &mut self,
        window_id: &str,
        steps: [SizeStep; N],
        mut size_pinned_by_us: Option<&mut bool>,
    ) -> Result<()> {
        for step in steps {
            match step {
                SizeStep::Pty(cols, rows) => self
                    .master
                    .resize(pty_size(cols, rows))
                    .with_context(|| format!("resize preview PTY to {cols}x{rows}"))?,
                SizeStep::PinWindow { cols, rows } => {
                    tmux::pin_window_size(window_id, cols, rows).with_context(|| {
                        format!("pin preview window {window_id} to {cols}x{rows}")
                    })?;
                    if let Some(size_pinned_by_us) = size_pinned_by_us.as_mut() {
                        **size_pinned_by_us = true;
                    }
                }
            }
        }
        Ok(())
    }
}

fn reduce_topology(
    visibility: &PreviewVisibility,
    selected_pane_id: Option<&str>,
    rows: &[tmux::TopologyRow],
    previous: Option<&PreviewTopology>,
) -> (PreviewTopology, TopologyAction) {
    let panel = visibility
        .pane_id()
        .and_then(|pane_id| tmux::locate_panel(pane_id, rows));
    let preferred_session = panel.as_ref().map(|panel| panel.session_id.as_str());
    let agent = selected_pane_id.and_then(|pane_id| {
        let mut candidates = rows.iter().filter(|row| row.pane_id == pane_id);
        let first = candidates.next()?;
        let row = preferred_session
            .and_then(|session_id| {
                std::iter::once(first)
                    .chain(candidates)
                    .find(|row| row.session_id == session_id)
            })
            .unwrap_or(first);
        Some(AgentTopology {
            pane_id: row.pane_id.clone(),
            window_id: row.window_id.clone(),
            window_panes: row.window_panes,
            window_index: row.window_index,
            window_name: row.window_name.clone(),
            geometry: row.geometry,
        })
    });
    let visible = if visibility.is_always() {
        true
    } else {
        panel.as_ref().is_some_and(|panel| panel.visible)
    };
    let colocated = !visibility.is_always()
        && panel
            .as_ref()
            .zip(agent.as_ref())
            .is_some_and(|(panel, agent)| panel.window_id == agent.window_id);
    let topology = PreviewTopology {
        panel,
        agent,
        visible,
        colocated,
    };
    let action = match topology.agent.as_ref() {
        None => TopologyAction::Clear,
        Some(agent) if topology.colocated => {
            let panel = topology.panel.as_ref().expect("colocated panel exists");
            TopologyAction::Placard(pane_direction(panel.geometry, agent.geometry))
        }
        Some(agent) => {
            let reacquire = previous.is_none_or(|previous| {
                previous.colocated
                    || previous.agent.as_ref().is_none_or(|old| {
                        old.pane_id != agent.pane_id
                            || old.window_id != agent.window_id
                            || old.window_panes != agent.window_panes
                    })
            });
            TopologyAction::Live {
                agent: agent.clone(),
                visible,
                reacquire,
            }
        }
    };
    (topology, action)
}

fn pane_direction(panel: tmux::PaneGeometry, agent: tmux::PaneGeometry) -> Direction {
    let center = |pane: tmux::PaneGeometry| {
        (
            pane.left as i64 * 2 + pane.cols as i64,
            pane.top as i64 * 2 + pane.rows as i64,
        )
    };
    let (panel_x, panel_y) = center(panel);
    let (agent_x, agent_y) = center(agent);
    let dx = agent_x - panel_x;
    let dy = agent_y - panel_y;
    if dx.abs() >= dy.abs() * 2 {
        if dx < 0 {
            Direction::Left
        } else {
            Direction::Right
        }
    } else if dy < 0 {
        Direction::Up
    } else {
        Direction::Down
    }
}

impl Preview {
    pub fn new(notifications: UnboundedSender<()>, visibility: PreviewVisibility) -> Self {
        let visible = visibility.is_always();
        Self {
            session: format!("gw-preview-{}", std::process::id()),
            state: State::Uninitialized,
            wanted: None,
            selected: None,
            visible,
            notifications,
            snapshot_preview: String::new(),
            preview_direction: None,
            topology: None,
            visibility,
        }
    }

    pub fn tick(&mut self, rows: &[tmux::TopologyRow], selected_pane_id: Option<&str>) {
        let (topology, action) = reduce_topology(
            &self.visibility,
            selected_pane_id,
            rows,
            self.topology.as_ref(),
        );
        self.topology = Some(topology);
        match action {
            TopologyAction::Clear => {
                self.deselect();
                self.snapshot_preview.clear();
                self.preview_direction = None;
            }
            TopologyAction::Placard(direction) => {
                self.deselect();
                self.snapshot_preview.clear();
                self.preview_direction = Some(direction);
            }
            TopologyAction::Live {
                agent,
                visible,
                reacquire,
            } => {
                self.preview_direction = None;
                if reacquire {
                    self.deselect();
                }
                self.set_visible(visible);
                if !self.select(&agent.window_id, &agent.pane_id) && visible {
                    self.snapshot_preview =
                        tmux::capture(&agent.pane_id, PREVIEW_LINES).unwrap_or_default();
                }
            }
        }
    }

    pub fn set_size(&mut self, cols: u16, rows: u16) {
        let was_live = self.is_live();
        let resized_live = self.resize(cols, rows);
        if was_live && !resized_live {
            self.capture_preview();
        }
    }

    pub fn sync(&mut self) {
        if self.sync_health() {
            self.capture_preview();
        }
    }

    pub fn view(&self) -> PreviewView {
        let title = self
            .topology
            .as_ref()
            .and_then(|topology| topology.agent.as_ref())
            .map(|agent| format!("{}:{}", agent.window_index, agent.window_name));
        let content = if title.is_none() {
            PreviewContent::Empty
        } else if let Some(direction) = self.preview_direction {
            PreviewContent::Placard(direction)
        } else if let Some(parser) = self.parser() {
            PreviewContent::Live(parser)
        } else {
            PreviewContent::Snapshot(self.snapshot_preview.clone())
        };
        PreviewView { title, content }
    }

    fn select(&mut self, window_id: &str, pane_id: &str) -> bool {
        self.sync_health();
        if matches!(self.state, State::Dead) {
            return false;
        }
        let target = Target::new(window_id, pane_id);
        if self.wanted.as_ref() == Some(&target) {
            return self.is_live();
        }
        self.wanted = Some(target.clone());
        if !self.visible {
            if let Err(error) = self.release_live_selection() {
                tui_log::error(&format!("live preview release failed: {error:#}"));
                self.fail();
            }
            return false;
        }
        if matches!(self.state, State::Uninitialized) {
            return false;
        }

        let result: Result<()> = (|| {
            self.release_live_selection()
                .context("release previous preview selection")?;
            let State::Live(live) = &mut self.state else {
                return Err(anyhow!("preview client is not live"));
            };
            acquire_selection(&self.session, &target, live, &mut self.selected)
        })();
        if let Err(error) = result {
            tui_log::error(&format!("live preview selection failed: {error:#}"));
            self.fail();
        }
        self.is_live()
    }

    fn deselect(&mut self) {
        self.wanted = None;
        if let Err(error) = self.release_live_selection() {
            tui_log::error(&format!("live preview deselect failed: {error:#}"));
            self.fail();
        }
    }

    fn set_visible(&mut self, visible: bool) -> bool {
        self.sync_health();
        if self.visible == visible || matches!(self.state, State::Dead) {
            return self.is_live();
        }
        self.visible = visible;
        if !visible {
            if let Err(error) = self.release_live_selection() {
                tui_log::error(&format!(
                    "live preview visibility release failed: {error:#}"
                ));
                self.fail();
            }
            return false;
        }
        let Some(target) = self.wanted.clone() else {
            return false;
        };
        let State::Live(live) = &mut self.state else {
            return false;
        };
        if let Err(error) = acquire_selection(&self.session, &target, live, &mut self.selected) {
            tui_log::error(&format!(
                "live preview visibility restore failed: {error:#}"
            ));
            self.fail();
        }
        self.is_live()
    }

    fn resize(&mut self, cols: u16, rows: u16) -> bool {
        self.sync_health();
        if cols == 0 || rows == 0 {
            self.deselect();
            return false;
        }
        if matches!(self.state, State::Uninitialized) && self.visible && self.wanted.is_some() {
            self.initialize(cols, rows);
        }

        let State::Live(live) = &mut self.state else {
            return false;
        };
        if live.panel_size.cols == cols && live.panel_size.rows == rows {
            return self.is_live();
        }
        let size = pty_size(cols, rows);
        let result: Result<()> = (|| {
            let mut parser = live
                .parser
                .lock()
                .map_err(|_| anyhow!("preview parser lock was poisoned"))?;
            parser.screen_mut().set_size(rows, cols);
            drop(parser);
            if let Some(selected) = &self.selected {
                live.execute_size_steps(
                    &selected.window_id,
                    resize_steps((live.panel_size.cols, live.panel_size.rows), (cols, rows)),
                    None,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            tui_log::error(&format!("live preview resize failed: {error:#}"));
            self.fail();
            return false;
        }
        if let State::Live(live) = &mut self.state {
            live.panel_size = size;
        }
        self.is_live()
    }

    fn sync_health(&mut self) -> bool {
        let died = matches!(
            &self.state,
            State::Live(live) if !live.alive.load(Ordering::Acquire)
        );
        if died {
            self.fail();
        }
        died
    }

    fn is_live(&self) -> bool {
        self.visible
            && self.selected.is_some()
            && matches!(
                &self.state,
                State::Live(live) if live.alive.load(Ordering::Acquire)
            )
    }

    fn parser(&self) -> Option<Arc<Mutex<vt100::Parser>>> {
        if !self.visible || self.selected.is_none() {
            return None;
        }
        match &self.state {
            State::Live(live) if live.alive.load(Ordering::Acquire) => {
                Some(Arc::clone(&live.parser))
            }
            _ => None,
        }
    }

    fn capture_preview(&mut self) {
        self.snapshot_preview = self
            .topology
            .as_ref()
            .and_then(|topology| topology.agent.as_ref())
            .and_then(|agent| tmux::capture(&agent.pane_id, PREVIEW_LINES).ok())
            .unwrap_or_default();
    }

    fn initialize(&mut self, cols: u16, rows: u16) {
        let setup: Result<()> = (|| {
            for session in tmux::stale_preview_sessions()? {
                tmux::kill_session(&session)
                    .with_context(|| format!("kill stale preview session {session}"))?;
            }
            let current = tmux::current_session_name().context("get current tmux session")?;
            tmux::preview_session_create(&self.session, &current)
                .with_context(|| format!("create preview session {}", self.session))
        })();
        if let Err(error) = setup {
            tui_log::error(&format!("live preview session setup failed: {error:#}"));
            self.wanted = None;
            self.selected = None;
            self.state = State::Dead;
            return;
        }

        let live = match self.start_client(cols, rows) {
            Ok(live) => live,
            Err(error) => {
                tui_log::error(&format!("live preview client spawn failed: {error:#}"));
                let _ = tmux::kill_session(&self.session);
                self.wanted = None;
                self.selected = None;
                self.state = State::Dead;
                return;
            }
        };
        self.state = State::Live(live);
        if let Some(target) = self.wanted.clone() {
            let State::Live(live) = &mut self.state else {
                unreachable!()
            };
            if let Err(error) = acquire_selection(&self.session, &target, live, &mut self.selected)
            {
                tui_log::error(&format!("live preview selection failed: {error:#}"));
                self.fail();
            }
        }
    }

    fn release_live_selection(&mut self) -> Result<()> {
        let Some(selected) = self.selected.as_ref() else {
            return Ok(());
        };
        let State::Live(live) = &mut self.state else {
            return Err(anyhow!("preview client is not live"));
        };
        live.execute_size_steps(&selected.window_id, park_steps(), None)
            .context("park preview PTY before selection release")?;
        release_selection(selected).context("restore previewed window")?;
        self.selected = None;
        Ok(())
    }

    fn start_client(&self, cols: u16, rows: u16) -> Result<LiveClient> {
        let panel_size = pty_size(cols, rows);
        let pair = native_pty_system()
            .openpty(parked_size())
            .context("failed to open preview PTY")?;
        let reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone preview PTY reader")?;

        let binary = tmux::binary();
        let child = pair
            .slave
            .spawn_command(attach_command(binary, &self.session))
            .with_context(|| format!("spawn {binary} preview client"))?;
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let alive = Arc::new(AtomicBool::new(true));
        spawn_reader(
            reader,
            Arc::clone(&parser),
            Arc::clone(&alive),
            self.notifications.clone(),
        );
        Ok(LiveClient {
            master: pair.master,
            _child: child,
            parser,
            alive,
            panel_size,
        })
    }

    fn fail(&mut self) {
        if let State::Live(live) = &self.state {
            live.alive.store(false, Ordering::Release);
            if let Err(error) = tmux::kill_session(&self.session) {
                tui_log::error(&format!("live preview session teardown failed: {error:#}"));
            }
        }
        if let Some(selected) = self.selected.take() {
            if let Err(error) = release_selection(&selected) {
                tui_log::error(&format!("live preview release failed: {error:#}"));
            }
        }
        self.wanted = None;
        self.state = State::Dead;
    }
}

fn attach_command(binary: &str, session: &str) -> CommandBuilder {
    let mut command = CommandBuilder::new(binary);
    command.args([
        "attach",
        "-f",
        "read-only,ignore-size",
        "-t",
        session,
        ";",
        "set-option",
        "-t",
        session,
        "destroy-unattached",
        "on",
    ]);
    command.env_remove("TMUX");
    command.env_remove("TMUX_PANE");
    command.env("TERM", "xterm-256color");
    command
}

impl Drop for Preview {
    fn drop(&mut self) {
        if let State::Live(live) = &self.state {
            live.alive.store(false, Ordering::Release);
            if let Err(error) = tmux::kill_session(&self.session) {
                tui_log::error(&format!("live preview session teardown failed: {error:#}"));
            }
        }
        if let Some(selected) = self.selected.take() {
            if let Err(error) = release_selection(&selected) {
                tui_log::error(&format!("live preview release failed: {error:#}"));
            }
        }
    }
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn parked_size() -> PtySize {
    pty_size(PARKED_COLS, PARKED_ROWS)
}

fn acquire_selection(
    session: &str,
    target: &Target,
    live: &mut LiveClient,
    selected: &mut Option<Selection>,
) -> Result<()> {
    tmux::preview_select_window(session, &target.window_id)
        .with_context(|| format!("select preview window {}", target.window_id))?;
    *selected = Some(Selection::new(target));
    let selected = selected.as_mut().expect("preview selection was set");
    let window = tmux::preview_window_state(&target.window_id)
        .with_context(|| format!("query preview window {}", target.window_id))?;
    if window.pane_count > 1 && !window.zoomed {
        tmux::toggle_pane_zoom(&target.pane_id)
            .with_context(|| format!("zoom preview pane {}", target.pane_id))?;
        selected.zoomed_by_us = true;
    }
    let parser = Arc::clone(&live.parser);
    let mut parser = parser
        .lock()
        .map_err(|_| anyhow!("preview parser lock was poisoned"))?;
    live.execute_size_steps(
        &target.window_id,
        acquire_steps((live.panel_size.cols, live.panel_size.rows)),
        Some(&mut selected.size_pinned_by_us),
    )?;
    parser.process(b"\x1bc");
    Ok(())
}

fn release_selection(selected: &Selection) -> Result<()> {
    if selected.zoomed_by_us {
        match tmux::preview_window_state(&selected.window_id) {
            Ok(window) if window.zoomed => {
                if let Err(error) = tmux::toggle_pane_zoom(&selected.pane_id) {
                    if !is_missing_target(&error) {
                        return Err(error).context("restore preview pane zoom");
                    }
                }
            }
            Ok(_) => {}
            Err(error) if is_missing_target(&error) => {}
            Err(error) => return Err(error).context("query preview zoom state during release"),
        }
    }
    if selected.size_pinned_by_us {
        if let Err(error) = tmux::release_window_size(&selected.window_id) {
            if !is_missing_target(&error) {
                return Err(error).context("release preview window size");
            }
        }
    }
    Ok(())
}

fn is_missing_target(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    [
        "can't find window",
        "can't find pane",
        "no such window",
        "no such pane",
        "window not found",
        "pane not found",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    alive: Arc<AtomicBool>,
    notifications: UnboundedSender<()>,
) {
    drop(tokio::task::spawn_blocking(move || {
        let mut buffer = [0; 8192];
        let error = loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break anyhow!("preview PTY reached EOF"),
                Err(error) => break anyhow::Error::new(error).context("read preview PTY"),
                Ok(count) => count,
            };
            let mut parser = match parser.lock() {
                Ok(parser) => parser,
                Err(_) => break anyhow!("preview parser lock was poisoned"),
            };
            parser.process(&buffer[..count]);
            drop(parser);
            let _ = notifications.send(());
        };
        if alive.swap(false, Ordering::AcqRel) {
            tui_log::error(&format!("live preview reader failed: {error:#}"));
        }
        let _ = notifications.send(());
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_size_plan(
        mut pty: (u16, u16),
        mut window: (u16, u16),
        steps: impl IntoIterator<Item = SizeStep>,
    ) -> ((u16, u16), (u16, u16)) {
        assert!(pty.0 >= window.0 && pty.1 >= window.1);
        for step in steps {
            match step {
                SizeStep::Pty(cols, rows) => pty = (cols, rows),
                SizeStep::PinWindow { cols, rows } => window = (cols, rows),
            }
            assert!(
                pty.0 >= window.0 && pty.1 >= window.1,
                "PTY {pty:?} is smaller than window {window:?} after {step:?}"
            );
        }
        (pty, window)
    }

    fn topology_row(
        session_id: &str,
        window_id: &str,
        window_active: bool,
        pane_id: &str,
        left: u32,
        top: u32,
        window_panes: u32,
    ) -> tmux::TopologyRow {
        tmux::TopologyRow {
            session_name: session_id.trim_start_matches('$').into(),
            session_id: session_id.into(),
            window_id: window_id.into(),
            window_active,
            session_attached: true,
            pane_id: pane_id.into(),
            pane_pid: 100,
            pane_tty: "ttys001".into(),
            pane_current_path: "/work".into(),
            window_index: 1,
            window_name: "agent".into(),
            geometry: tmux::PaneGeometry {
                left,
                top,
                cols: 20,
                rows: 10,
            },
            window_panes,
        }
    }

    fn live_action(action: TopologyAction) -> (String, bool, bool) {
        match action {
            TopologyAction::Live {
                agent,
                visible,
                reacquire,
            } => (agent.window_id, visible, reacquire),
            action => panic!("expected live action, got {action:?}"),
        }
    }

    #[test]
    fn reduces_topology_changes_from_fresh_rows() {
        let dashboard = PreviewVisibility::new(false, Some("%panel".into()));
        let base = vec![
            topology_row("$1", "@panel", true, "%panel", 0, 0, 1),
            topology_row("$1", "@agent", false, "%agent", 40, 0, 1),
        ];
        let (initial, action) = reduce_topology(&dashboard, Some("%agent"), &base, None);
        assert_eq!(live_action(action), ("@agent".into(), true, true));

        let (_, action) = reduce_topology(&dashboard, Some("%agent"), &base, Some(&initial));
        assert_eq!(live_action(action), ("@agent".into(), true, false));

        let hidden = vec![
            topology_row("$1", "@panel", false, "%panel", 0, 0, 1),
            base[1].clone(),
        ];
        let (_, action) = reduce_topology(&dashboard, Some("%agent"), &hidden, Some(&initial));
        assert_eq!(live_action(action), ("@agent".into(), false, false));

        let mut detached_panel = topology_row("$1", "@panel", true, "%panel", 0, 0, 1);
        detached_panel.session_attached = false;
        let detached = vec![detached_panel, base[1].clone()];
        let (_, action) = reduce_topology(&dashboard, Some("%agent"), &detached, Some(&initial));
        assert_eq!(live_action(action), ("@agent".into(), false, false));

        let (_, action) = reduce_topology(&dashboard, Some("%agent"), &base[1..], Some(&initial));
        assert_eq!(live_action(action), ("@agent".into(), false, false));

        let (gone, action) =
            reduce_topology(&dashboard, Some("%agent"), &base[..1], Some(&initial));
        assert_eq!(action, TopologyAction::Clear);
        assert!(gone.agent.is_none());

        let moved_agent = vec![
            base[0].clone(),
            topology_row("$1", "@other", false, "%agent", 40, 0, 1),
        ];
        let (_, action) = reduce_topology(&dashboard, Some("%agent"), &moved_agent, Some(&initial));
        assert_eq!(live_action(action), ("@other".into(), true, true));

        let more_panes = vec![
            base[0].clone(),
            topology_row("$1", "@agent", false, "%agent", 40, 0, 2),
        ];
        let (_, action) = reduce_topology(&dashboard, Some("%agent"), &more_panes, Some(&initial));
        assert_eq!(live_action(action), ("@agent".into(), true, true));

        let colocated = vec![
            topology_row("$1", "@shared", true, "%panel", 0, 0, 2),
            topology_row("$1", "@shared", true, "%agent", 40, 0, 2),
        ];
        let (placard, action) =
            reduce_topology(&dashboard, Some("%agent"), &colocated, Some(&initial));
        assert_eq!(action, TopologyAction::Placard(Direction::Right));

        let shifted_placard = vec![
            topology_row("$1", "@shared", true, "%panel", 0, 0, 2),
            topology_row("$1", "@shared", true, "%agent", 0, 30, 2),
        ];
        let (_, action) =
            reduce_topology(&dashboard, Some("%agent"), &shifted_placard, Some(&placard));
        assert_eq!(action, TopologyAction::Placard(Direction::Down));

        let moved_out = vec![
            topology_row("$2", "@panel2", true, "%panel", 0, 0, 1),
            topology_row("$2", "@agent2", false, "%agent", 0, 30, 1),
        ];
        let (moved, action) =
            reduce_topology(&dashboard, Some("%agent"), &moved_out, Some(&placard));
        assert_eq!(live_action(action), ("@agent2".into(), true, true));
        assert_eq!(moved.panel.as_ref().unwrap().session_id, "$2");

        let popup = PreviewVisibility::new(true, None);
        let (_, action) = reduce_topology(&popup, Some("%agent"), &colocated, None);
        assert_eq!(live_action(action), ("@shared".into(), true, true));

        let mut old_panel = topology_row("$1", "@old", true, "%panel", 0, 0, 1);
        old_panel.session_attached = false;
        let grouped_panel = vec![
            old_panel,
            topology_row("$2", "@fresh", false, "%panel", 0, 0, 1),
        ];
        let (grouped, _) = reduce_topology(&dashboard, None, &grouped_panel, None);
        assert_eq!(grouped.panel.unwrap().session_id, "$2");
    }

    #[test]
    fn points_from_panel_center_toward_agent_center() {
        let geometry = |left, top| tmux::PaneGeometry {
            left,
            top,
            cols: 10,
            rows: 10,
        };
        let panel = geometry(20, 20);

        assert_eq!(pane_direction(panel, geometry(40, 20)), Direction::Right);
        assert_eq!(pane_direction(panel, geometry(0, 20)), Direction::Left);
        assert_eq!(pane_direction(panel, geometry(20, 0)), Direction::Up);
        assert_eq!(pane_direction(panel, geometry(20, 40)), Direction::Down);
        assert_eq!(pane_direction(panel, geometry(24, 22)), Direction::Right);
        assert_eq!(pane_direction(panel, panel), Direction::Right);
    }

    #[test]
    fn resize_plans_keep_pty_at_least_as_large_as_window() {
        let cases = [
            ((80, 24), (120, 40), (120, 40)),
            ((120, 40), (80, 24), (120, 40)),
            ((80, 40), (120, 24), (120, 40)),
            ((120, 24), (80, 40), (120, 40)),
            ((80, 24), (80, 24), (80, 24)),
        ];

        for (current, target, bridge) in cases {
            let steps = resize_steps(current, target);
            assert_eq!(
                steps,
                [
                    SizeStep::Pty(bridge.0, bridge.1),
                    SizeStep::PinWindow {
                        cols: target.0,
                        rows: target.1,
                    },
                    SizeStep::Pty(target.0, target.1),
                ]
            );
            assert_eq!(apply_size_plan(current, current, steps), (target, target));
        }
    }

    #[test]
    fn acquire_plan_pins_before_shrinking_parked_pty() {
        let panel = (120, 40);
        let steps = acquire_steps(panel);

        assert_eq!(
            steps,
            [
                SizeStep::PinWindow {
                    cols: panel.0,
                    rows: panel.1,
                },
                SizeStep::Pty(panel.0, panel.1),
            ]
        );
        assert_eq!(
            apply_size_plan((PARKED_COLS, PARKED_ROWS), (200, 60), steps),
            (panel, panel)
        );
    }

    #[test]
    fn park_plan_grows_pty_without_changing_window() {
        let window = (80, 24);
        let steps = park_steps();

        assert_eq!(steps, [SizeStep::Pty(PARKED_COLS, PARKED_ROWS)]);
        assert_eq!(
            apply_size_plan(window, window, steps),
            ((PARKED_COLS, PARKED_ROWS), window)
        );
    }

    #[test]
    fn attach_is_read_only_and_ignores_client_sizing() {
        let command = attach_command("tmux", "gw-preview-42");
        let argv = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            argv,
            [
                "tmux",
                "attach",
                "-f",
                "read-only,ignore-size",
                "-t",
                "gw-preview-42",
                ";",
                "set-option",
                "-t",
                "gw-preview-42",
                "destroy-unattached",
                "on",
            ]
        );
    }

    #[test]
    fn missing_release_targets_are_tolerated() {
        assert!(is_missing_target(&anyhow!(
            "tmux failed: can't find window: @7"
        )));
        assert!(is_missing_target(&anyhow!(
            "tmux failed: pane not found: %3"
        )));
        assert!(!is_missing_target(&anyhow!(
            "tmux failed: server exited unexpectedly"
        )));
    }
}
