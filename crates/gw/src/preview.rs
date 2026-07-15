use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use gw_core::tmux;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc::UnboundedSender;

pub struct Preview {
    session: String,
    state: State,
    selected: Option<String>,
    notifications: UnboundedSender<()>,
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

    pub fn select(&mut self, window_id: &str) -> bool {
        self.sync_health();
        if matches!(self.state, State::Dead) {
            return false;
        }
        if self.selected.as_deref() == Some(window_id) {
            return self.is_live();
        }
        if matches!(self.state, State::Uninitialized) {
            self.selected = Some(window_id.to_owned());
            return false;
        }

        let result = (|| {
            if let Some(previous) = self.selected.clone() {
                tmux::set_window_aggressive_resize(&previous, false)?;
                self.selected = None;
            }
            tmux::set_window_aggressive_resize(window_id, true)?;
            self.selected = Some(window_id.to_owned());
            tmux::preview_select_window(&self.session, window_id)
        })();
        if result.is_err() {
            self.fail();
        }
        self.is_live()
    }

    pub fn deselect(&mut self) {
        let Some(window_id) = self.selected.clone() else {
            return;
        };
        if matches!(self.state, State::Live(_))
            && tmux::set_window_aggressive_resize(&window_id, false).is_err()
        {
            self.fail();
        } else {
            self.selected = None;
        }
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
        let result = live.master.resize(size).and_then(|()| {
            let mut parser = live
                .parser
                .lock()
                .map_err(|_| anyhow!("preview parser lock was poisoned"))?;
            parser.screen_mut().set_size(rows, cols);
            Ok(())
        });
        if result.is_err() {
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
        let setup = (|| {
            for session in tmux::stale_preview_sessions()? {
                tmux::kill_session(&session)?;
            }
            let current = tmux::current_session_name()?;
            tmux::preview_session_create(&self.session, &current)
        })();
        if setup.is_err() {
            self.state = State::Dead;
            return;
        }

        let live = match self.start_client(cols, rows) {
            Ok(live) => live,
            Err(_) => {
                let _ = tmux::kill_session(&self.session);
                self.state = State::Dead;
                return;
            }
        };
        self.state = State::Live(live);
        if let Some(window_id) = self.selected.take() {
            self.select(&window_id);
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
            let mut command = CommandBuilder::new(binary);
            command.args(["attach", "-r", "-t", self.session.as_str()]);
            command.env_remove("TMUX");
            command.env_remove("TMUX_PANE");
            command.env("TERM", "xterm-256color");
            match pair.slave.spawn_command(command) {
                Ok(spawned) => {
                    child = Some(spawned);
                    break;
                }
                Err(error) => last_error = Some(error),
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
        if let Some(window_id) = self.selected.take() {
            let _ = tmux::set_window_aggressive_resize(&window_id, false);
        }
        if matches!(self.state, State::Live(_)) {
            let _ = tmux::kill_session(&self.session);
        }
        self.state = State::Dead;
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        if let Some(window_id) = self.selected.take() {
            let _ = tmux::set_window_aggressive_resize(&window_id, false);
        }
        if matches!(self.state, State::Live(_)) {
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

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    alive: Arc<AtomicBool>,
    notifications: UnboundedSender<()>,
) {
    drop(tokio::task::spawn_blocking(move || {
        let mut buffer = [0; 8192];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            let Ok(mut parser) = parser.lock() else {
                break;
            };
            parser.process(&buffer[..count]);
            drop(parser);
            let _ = notifications.send(());
        }
        alive.store(false, Ordering::Release);
        let _ = notifications.send(());
    }));
}
