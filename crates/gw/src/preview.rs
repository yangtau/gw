use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use gw_core::{tmux, tui_log};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc::UnboundedSender;

pub struct Preview {
    session: String,
    state: State,
    selected: Option<Selection>,
    notifications: UnboundedSender<()>,
}

#[derive(Clone)]
struct Selection {
    window_id: String,
    pane_id: String,
    zoomed_by_us: bool,
}

impl Selection {
    fn new(window_id: &str, pane_id: &str) -> Self {
        Self {
            window_id: window_id.to_owned(),
            pane_id: pane_id.to_owned(),
            zoomed_by_us: false,
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
    size: PtySize,
}

impl Preview {
    pub fn new(notifications: UnboundedSender<()>) -> Self {
        Self {
            session: format!("gw-preview-{}", std::process::id()),
            state: State::Uninitialized,
            selected: None,
            notifications,
        }
    }

    pub fn select(&mut self, window_id: &str, pane_id: &str) -> bool {
        self.sync_health();
        if matches!(self.state, State::Dead) {
            return false;
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.window_id == window_id && selected.pane_id == pane_id)
        {
            return self.is_live();
        }
        if matches!(self.state, State::Uninitialized) {
            self.selected = Some(Selection::new(window_id, pane_id));
            return false;
        }

        let result: Result<()> = (|| {
            if let Some(previous) = self.selected.clone() {
                release_selection(&previous).with_context(|| {
                    format!("release previous preview window {}", previous.window_id)
                })?;
                self.selected = None;
            }
            tmux::set_window_aggressive_resize(window_id, true)
                .with_context(|| format!("enable preview resize for window {window_id}"))?;
            self.selected = Some(Selection::new(window_id, pane_id));
            tmux::preview_select_window(&self.session, window_id)
                .with_context(|| format!("select preview window {window_id}"))?;
            let window = tmux::preview_window_state(window_id)
                .with_context(|| format!("query preview window {window_id}"))?;
            if window.pane_count > 1 && !window.zoomed {
                tmux::toggle_pane_zoom(pane_id)
                    .with_context(|| format!("zoom preview pane {pane_id}"))?;
                self.selected
                    .as_mut()
                    .expect("preview selection was set")
                    .zoomed_by_us = true;
            }
            Ok(())
        })();
        if let Err(error) = result {
            tui_log::error(&format!("live preview selection failed: {error:#}"));
            self.fail();
        }
        self.is_live()
    }

    pub fn deselect(&mut self) {
        let Some(selected) = self.selected.clone() else {
            return;
        };
        if matches!(self.state, State::Live(_)) {
            if let Err(error) = release_selection(&selected) {
                tui_log::error(&format!("live preview deselect failed: {error:#}"));
                self.fail();
                return;
            }
        }
        self.selected = None;
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        self.sync_health();
        if cols == 0 || rows == 0 {
            self.deselect();
            return false;
        }
        if matches!(self.state, State::Uninitialized) && self.selected.is_some() {
            self.initialize(cols, rows);
        }

        let State::Live(live) = &mut self.state else {
            return false;
        };
        if live.size.cols == cols && live.size.rows == rows {
            return true;
        }
        let size = pty_size(cols, rows);
        let result: Result<()> = (|| {
            live.master.resize(size).context("resize preview PTY")?;
            let mut parser = live
                .parser
                .lock()
                .map_err(|_| anyhow!("preview parser lock was poisoned"))?;
            parser.screen_mut().set_size(rows, cols);
            Ok(())
        })();
        if let Err(error) = result {
            tui_log::error(&format!("live preview resize failed: {error:#}"));
            self.fail();
            return false;
        }
        if let State::Live(live) = &mut self.state {
            live.size = size;
        }
        true
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
        matches!(
            &self.state,
            State::Live(live) if live.alive.load(Ordering::Acquire)
        )
    }

    pub fn parser(&self) -> Option<Arc<Mutex<vt100::Parser>>> {
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
            self.selected = None;
            self.state = State::Dead;
            return;
        }

        let live = match self.start_client(cols, rows) {
            Ok(live) => live,
            Err(error) => {
                tui_log::error(&format!("live preview client spawn failed: {error:#}"));
                let _ = tmux::kill_session(&self.session);
                self.selected = None;
                self.state = State::Dead;
                return;
            }
        };
        self.state = State::Live(live);
        if let Some(selected) = self.selected.take() {
            self.select(&selected.window_id, &selected.pane_id);
        }
    }

    fn start_client(&self, cols: u16, rows: u16) -> Result<LiveClient> {
        let size = pty_size(cols, rows);
        let pair = native_pty_system()
            .openpty(size)
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
            size,
        })
    }

    fn fail(&mut self) {
        if let Some(selected) = self.selected.take() {
            if let Err(error) = release_selection(&selected) {
                tui_log::error(&format!("live preview release failed: {error:#}"));
            }
        }
        if let State::Live(live) = &self.state {
            live.alive.store(false, Ordering::Release);
            let _ = tmux::kill_session(&self.session);
        }
        self.state = State::Dead;
    }
}

fn attach_command(binary: &str, session: &str) -> CommandBuilder {
    let mut command = CommandBuilder::new(binary);
    command.args([
        "attach",
        "-f",
        "read-only",
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
            if let Some(selected) = self.selected.take() {
                if let Err(error) = release_selection(&selected) {
                    tui_log::error(&format!("live preview release failed: {error:#}"));
                }
            }
            live.alive.store(false, Ordering::Release);
            let _ = tmux::kill_session(&self.session);
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

fn release_selection(selected: &Selection) -> Result<()> {
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
    tmux::set_window_aggressive_resize(&selected.window_id, false)?;
    if let Err(error) = tmux::resize_window_to_available(&selected.window_id) {
        tui_log::error(&format!("live preview window restore failed: {error:#}"));
    }
    Ok(())
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
    fn attach_is_read_only_but_participates_in_window_sizing() {
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
                "read-only",
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
