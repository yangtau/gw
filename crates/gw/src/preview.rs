use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use gw_core::{tmux, tui_log};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc::UnboundedSender;

const PARKED_COLS: u16 = 800;
const PARKED_ROWS: u16 = 240;

pub struct Preview {
    session: String,
    state: State,
    wanted: Option<Target>,
    selected: Option<Selection>,
    visible: bool,
    notifications: UnboundedSender<()>,
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

impl Preview {
    pub fn new(notifications: UnboundedSender<()>, visible: bool) -> Self {
        Self {
            session: format!("gw-preview-{}", std::process::id()),
            state: State::Uninitialized,
            wanted: None,
            selected: None,
            visible,
            notifications,
        }
    }

    pub fn select(&mut self, window_id: &str, pane_id: &str) -> bool {
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

    pub fn deselect(&mut self) {
        self.wanted = None;
        if let Err(error) = self.release_live_selection() {
            tui_log::error(&format!("live preview deselect failed: {error:#}"));
            self.fail();
        }
    }

    pub fn set_visible(&mut self, visible: bool) -> bool {
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

    pub fn has_selection(&self) -> bool {
        self.wanted.is_some()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
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
                let bridge = pty_size(
                    live.panel_size.cols.max(cols),
                    live.panel_size.rows.max(rows),
                );
                live.master
                    .resize(bridge)
                    .context("grow preview PTY before window resize")?;
                tmux::pin_window_size(&selected.window_id, cols, rows)
                    .with_context(|| format!("resize preview window {}", selected.window_id))?;
                live.master.resize(size).context("resize preview PTY")?;
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

    pub fn sync_health(&mut self) -> bool {
        let died = matches!(
            &self.state,
            State::Live(live) if !live.alive.load(Ordering::Acquire)
        );
        if died {
            self.fail();
        }
        died
    }

    pub fn is_live(&self) -> bool {
        self.visible
            && self.selected.is_some()
            && matches!(
                &self.state,
                State::Live(live) if live.alive.load(Ordering::Acquire)
            )
    }

    pub fn parser(&self) -> Option<Arc<Mutex<vt100::Parser>>> {
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
        live.master
            .resize(parked_size())
            .context("park preview PTY before selection release")?;
        release_selection(selected);
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

        let mut last_error = None;
        let mut child = None;
        for binary in tmux::TMUX_BINARIES {
            let command = attach_command(binary, &self.session);
            match pair.slave.spawn_command(command) {
                Ok(spawned) => {
                    child = Some(spawned);
                    break;
                }
                Err(error) => {
                    last_error = Some(error.context(format!("spawn {binary} preview client")))
                }
            }
        }
        let child = child.ok_or_else(|| {
            last_error.unwrap_or_else(|| anyhow!("tmux candidates are non-empty"))
        })?;
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
            release_selection(&selected);
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
            release_selection(&selected);
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
    tmux::pin_window_size(
        &target.window_id,
        live.panel_size.cols,
        live.panel_size.rows,
    )
    .with_context(|| format!("pin preview window {} size", target.window_id))?;
    selected.size_pinned_by_us = true;
    let mut parser = live
        .parser
        .lock()
        .map_err(|_| anyhow!("preview parser lock was poisoned"))?;
    live.master
        .resize(live.panel_size)
        .context("resize preview PTY to panel size")?;
    parser.process(b"\x1bc");
    Ok(())
}

fn release_selection(selected: &Selection) {
    if selected.zoomed_by_us {
        match tmux::preview_window_state(&selected.window_id) {
            Ok(window) if window.zoomed => {
                if let Err(error) = tmux::toggle_pane_zoom(&selected.pane_id) {
                    tui_log::error(&format!("live preview zoom restore failed: {error:#}"));
                }
            }
            Ok(_) => {}
            Err(error) => {
                tui_log::error(&format!("live preview zoom state query failed: {error:#}"))
            }
        }
    }
    if selected.size_pinned_by_us {
        if let Err(error) = tmux::release_window_size(&selected.window_id) {
            tui_log::error(&format!(
                "live preview window size release failed: {error:#}"
            ));
        }
    }
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
}
