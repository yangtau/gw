//! The panel: fullscreen alt-screen TUI. One select! loop over terminal
//! input, a periodic tick, and event-log filesystem changes; every wakeup
//! re-derives the snapshot (panes + processes + logs are re-read each time,
//! there is no incremental state to get stale).

use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use gw_core::discover::{self, Agent, AgentStatus, Snapshot};
use gw_core::plugins::{self, Plugin};
use gw_core::store::Store;
use gw_core::tmux;
use notify::Watcher;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;

const STALE_AFTER_MINUTES: i64 = 30;
const RETENTION_DAYS: i64 = 7;
const PREVIEW_LINES: u32 = 50;

pub fn run() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let mut app = App::new()?;
        let mut terminal = ratatui::init();
        let result = app.run(&mut terminal).await;
        ratatui::restore();
        result
    })
}

enum View {
    Agents,
    Ended,
}

enum Flow {
    Continue,
    Quit,
}

struct App {
    store: Store,
    plugins: Vec<Plugin>,
    snapshot: Snapshot,
    view: View,
    selected: usize,
    picker: Option<usize>,
    preview: String,
    exit_after_jump: bool,
}

impl App {
    fn new() -> Result<Self> {
        let store = Store::open_default()?;
        store.sweep(Duration::days(RETENTION_DAYS))?;
        let mut app = Self {
            store,
            plugins: plugins::discover()?,
            snapshot: Snapshot { agents: vec![], ended: vec![], uninstrumented: vec![] },
            view: View::Agents,
            selected: 0,
            picker: None,
            preview: String::new(),
            exit_after_jump: tmux::inside_popup(),
        };
        app.refresh();
        Ok(app)
    }

    async fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let (fs_tx, mut fs_rx) = tokio::sync::mpsc::unbounded_channel();
        let sessions_dir = self.store.root().join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        let mut watcher = notify::recommended_watcher(move |_| {
            let _ = fs_tx.send(());
        })?;
        watcher.watch(&sessions_dir, notify::RecursiveMode::NonRecursive)?;

        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            terminal.draw(|f| self.render(f))?;
            tokio::select! {
                _ = tick.tick() => self.refresh(),
                Some(_) = fs_rx.recv() => {
                    while fs_rx.try_recv().is_ok() {}
                    self.refresh();
                }
                Some(Ok(ev)) = input.next() => {
                    if let TermEvent::Key(key) = ev {
                        if matches!(self.on_key(key)?, Flow::Quit) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    fn refresh(&mut self) {
        let now = Utc::now();
        match discover::snapshot(&self.store, &self.plugins, now, Duration::minutes(STALE_AFTER_MINUTES)) {
            Ok(snapshot) => self.snapshot = snapshot,
            Err(err) => eprintln!("snapshot failed: {err:#}"),
        }
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
        self.preview = match self.selected_agent() {
            Some(agent) => tmux::capture(&agent.pane.id, PREVIEW_LINES).unwrap_or_default(),
            None => String::new(),
        };
    }

    fn row_count(&self) -> usize {
        match self.view {
            View::Agents => self.snapshot.agents.len(),
            View::Ended => self.snapshot.ended.len(),
        }
    }

    fn selected_agent(&self) -> Option<&Agent> {
        match self.view {
            View::Agents => self.snapshot.agents.get(self.selected),
            View::Ended => None,
        }
    }

    fn plugin(&self, provider: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.manifest.id == provider)
    }

    fn on_key(&mut self, key: KeyEvent) -> Result<Flow> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(Flow::Quit);
        }
        if self.picker.is_some() {
            return self.on_picker_key(key);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(Flow::Quit),
            KeyCode::Char('j') | KeyCode::Down => self.select(1),
            KeyCode::Char('k') | KeyCode::Up => self.select(-1),
            KeyCode::Tab => self.select_next_attention(),
            KeyCode::Char('n') => self.picker = Some(0),
            KeyCode::Char('r') => {
                self.view = match self.view {
                    View::Agents => View::Ended,
                    View::Ended => View::Agents,
                };
                self.selected = 0;
                self.refresh();
            }
            KeyCode::Enter => return self.activate(),
            _ => {}
        }
        Ok(Flow::Continue)
    }

    fn on_picker_key(&mut self, key: KeyEvent) -> Result<Flow> {
        let picked = self.picker.unwrap();
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('j') | KeyCode::Down => {
                self.picker = Some((picked + 1) % self.plugins.len().max(1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.picker = Some(picked.checked_sub(1).unwrap_or(self.plugins.len().saturating_sub(1)));
            }
            KeyCode::Enter => {
                self.picker = None;
                if let Some(plugin) = self.plugins.get(picked) {
                    let cwd = std::env::current_dir()?;
                    let pane = tmux::new_window(&plugin.manifest.id, &cwd, &plugin.manifest.launch.argv)?;
                    return self.jump(&pane);
                }
            }
            _ => {}
        }
        Ok(Flow::Continue)
    }

    fn select(&mut self, delta: i64) {
        let count = self.row_count() as i64;
        if count > 0 {
            self.selected = (self.selected as i64 + delta).rem_euclid(count) as usize;
            self.refresh_preview();
        }
    }

    fn select_next_attention(&mut self) {
        let agents = &self.snapshot.agents;
        let next = (1..=agents.len())
            .map(|offset| (self.selected + offset) % agents.len().max(1))
            .find(|&i| matches!(agents[i].status, AgentStatus::Attention(_)));
        if let Some(i) = next {
            self.selected = i;
            self.refresh_preview();
        }
    }

    fn refresh_preview(&mut self) {
        self.preview = match self.selected_agent() {
            Some(agent) => tmux::capture(&agent.pane.id, PREVIEW_LINES).unwrap_or_default(),
            None => String::new(),
        };
    }

    fn activate(&mut self) -> Result<Flow> {
        match self.view {
            View::Agents => match self.snapshot.agents.get(self.selected) {
                Some(agent) => {
                    let pane = agent.pane.id.clone();
                    self.jump(&pane)
                }
                None => Ok(Flow::Continue),
            },
            View::Ended => {
                let Some(session) = self.snapshot.ended.get(self.selected) else {
                    return Ok(Flow::Continue);
                };
                let Some(resume) = self.plugin(&session.provider).and_then(|p| p.manifest.resume.clone()) else {
                    return Ok(Flow::Continue);
                };
                let argv: Vec<String> =
                    resume.argv.iter().map(|a| a.replace("{session_id}", &session.session_id)).collect();
                let cwd = session.cwd.clone().unwrap_or(std::env::current_dir()?);
                let pane = tmux::new_window(&session.provider, &cwd, &argv)?;
                self.jump(&pane)
            }
        }
    }

    fn jump(&mut self, pane_id: &str) -> Result<Flow> {
        tmux::focus(pane_id)?;
        if self.exit_after_jump {
            return Ok(Flow::Quit);
        }
        self.refresh();
        Ok(Flow::Continue)
    }

    fn render(&self, frame: &mut Frame) {
        let banner_height = if self.snapshot.uninstrumented.is_empty() { 0 } else { 1 };
        let [banner, main, preview, footer] = Layout::vertical([
            Constraint::Length(banner_height),
            Constraint::Min(5),
            Constraint::Percentage(40),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        if banner_height > 0 {
            let msg = format!(
                " hooks not installed for {} — run `gw setup`",
                self.snapshot.uninstrumented.join(", ")
            );
            frame.render_widget(Paragraph::new(msg).style(Style::new().fg(Color::Black).bg(Color::Yellow)), banner);
        }

        match self.view {
            View::Agents => self.render_agents(frame, main),
            View::Ended => self.render_ended(frame, main),
        }
        self.render_preview(frame, preview);
        self.render_footer(frame, footer);
        if self.picker.is_some() {
            self.render_picker(frame);
        }
    }

    fn render_agents(&self, frame: &mut Frame, area: Rect) {
        let now = Utc::now();
        let rows = self.snapshot.agents.iter().enumerate().map(|(i, agent)| {
            let (dot, word, color) = status_cell(agent.status);
            let label = self
                .plugin(&agent.provider)
                .map(|p| p.manifest.label.clone())
                .unwrap_or_else(|| agent.provider.clone());
            let provider_color =
                self.plugin(&agent.provider).and_then(|p| p.manifest.color.as_deref()).and_then(hex_color);
            let row = Row::new(vec![
                Span::styled(format!("{dot} {word}"), Style::new().fg(color)).into(),
                Span::styled(label, Style::new().fg(provider_color.unwrap_or(Color::Reset))).into(),
                Line::from(format!("{}:{}", agent.pane.window_index, agent.pane.window_name)),
                Line::from(shorten(&agent.cwd, 32)),
                Line::from(agent.since.map(|t| ago(t, now)).unwrap_or_default()),
            ]);
            if i == self.selected {
                row.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Min(12),
                Constraint::Length(32),
                Constraint::Length(8),
            ],
        )
        .header(Row::new(["status", "agent", "window", "cwd", "for"]).style(Style::new().dim()))
        .block(Block::new().borders(Borders::ALL).title(" agents "));
        frame.render_widget(table, area);
    }

    fn render_ended(&self, frame: &mut Frame, area: Rect) {
        let now = Utc::now();
        let rows = self.snapshot.ended.iter().enumerate().map(|(i, s)| {
            let row = Row::new(vec![
                Line::from(s.provider.clone()),
                Line::from(s.session_id.chars().take(12).collect::<String>()),
                Line::from(s.cwd.as_deref().map(|c| shorten(c, 40)).unwrap_or_default()),
                Line::from(ago(s.ended_at, now)),
            ]);
            if i == self.selected {
                row.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        });
        let table = Table::new(
            rows,
            [Constraint::Length(10), Constraint::Length(14), Constraint::Min(20), Constraint::Length(8)],
        )
        .header(Row::new(["agent", "session", "cwd", "ended"]).style(Style::new().dim()))
        .block(Block::new().borders(Borders::ALL).title(" recently ended — enter resumes "));
        frame.render_widget(table, area);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let title = self.selected_agent().map(|a| format!(" {} ", a.pane.window_name)).unwrap_or_default();
        let visible = area.height.saturating_sub(2) as usize;
        let lines: Vec<&str> = self.preview.lines().collect();
        let tail: Vec<Line> =
            lines[lines.len().saturating_sub(visible)..].iter().copied().map(Line::from).collect();
        frame.render_widget(
            Paragraph::new(tail).block(Block::new().borders(Borders::ALL).title(title).dim()),
            area,
        );
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let hints = match self.view {
            View::Agents => " enter jump · n new · r ended · tab attention · q quit",
            View::Ended => " enter resume · r agents · q quit",
        };
        frame.render_widget(Paragraph::new(hints).dim(), area);
    }

    fn render_picker(&self, frame: &mut Frame) {
        let picked = self.picker.unwrap();
        let width = 30u16;
        let height = self.plugins.len() as u16 + 2;
        let area = center(frame.area(), width, height);
        let items = self.plugins.iter().enumerate().map(|(i, p)| {
            let item = ListItem::new(format!(" {} ", p.manifest.label));
            if i == picked {
                item.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        });
        frame.render_widget(Clear, area);
        frame.render_widget(
            List::new(items).block(Block::new().borders(Borders::ALL).title(" new agent ")),
            area,
        );
    }
}

fn status_cell(status: AgentStatus) -> (&'static str, &'static str, Color) {
    match status {
        AgentStatus::Attention(_) => ("●", "attention", Color::Red),
        AgentStatus::Working => ("●", "working", Color::Green),
        AgentStatus::Idle => ("○", "idle", Color::Blue),
        AgentStatus::Stale => ("!", "stale", Color::Yellow),
        AgentStatus::Unknown => ("?", "unknown", Color::DarkGray),
    }
}

fn hex_color(hex: &str) -> Option<Color> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn shorten(path: &std::path::Path, max: usize) -> String {
    let home = dirs_home();
    let s = match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    if s.len() <= max {
        s
    } else {
        format!("…{}", &s[s.len() - max + 1..])
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn ago(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (now - since).num_minutes();
    match minutes {
        m if m < 1 => "now".into(),
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h{}m", m / 60, m % 60),
        m => format!("{}d", m / (24 * 60)),
    }
}

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let [_, mid, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width.min(area.width)),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, mid, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .areas(mid);
    mid
}
