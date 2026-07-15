//! The panel: fullscreen alt-screen TUI. One select! loop over terminal
//! input, a periodic tick, and event-log filesystem changes; every wakeup
//! re-derives the snapshot (panes + processes + logs are re-read each time,
//! there is no incremental state to get stale).
//!
//! Rendering is a borderless list of composed lines, not a table. Each row
//! has a fixed left cluster (marker, agent, status, detail) and a dim right
//! cluster (cwd · branch · window · time). `plan_right` picks which right
//! fields fit the terminal width, degrading in a fixed order; the detail
//! text absorbs whatever space is left.

use std::path::PathBuf;
use std::time::{Duration as StdDuration, Instant};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use gw_core::discover::{self, Agent, AgentStatus, Snapshot};
use gw_core::plugins::{self, Plugin};
use gw_core::protocol::AttentionKind;
use gw_core::store::Store;
use gw_core::tmux;
use notify::Watcher;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::preview::Preview;

const STALE_AFTER_MINUTES: i64 = 30;
const RETENTION_DAYS: i64 = 7;
const PREVIEW_LINES: u32 = 50;
const PREVIEW_FRAME_INTERVAL: StdDuration = StdDuration::from_millis(33);
const MIN_PREVIEW_TERM_HEIGHT: u16 = 16;
const MARKER_W: usize = 3;
const COL_GAP: usize = 2;
const MIN_DETAIL: usize = 16;
const SEP: &str = " · ";
const CWD_MAX: usize = 32;

pub fn run() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
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
    snapshot_preview: String,
    live_preview: Preview,
    preview_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    exit_after_jump: bool,
}

impl App {
    fn new() -> Result<Self> {
        let store = Store::open_default()?;
        store.sweep(Duration::days(RETENTION_DAYS))?;
        let (preview_tx, preview_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = Self {
            store,
            plugins: plugins::discover()?,
            snapshot: Snapshot {
                agents: vec![],
                ended: vec![],
                uninstrumented: vec![],
            },
            view: View::Agents,
            selected: 0,
            picker: None,
            snapshot_preview: String::new(),
            live_preview: Preview::new(preview_tx),
            preview_rx,
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
            let last_draw = Instant::now();
            tokio::select! {
                _ = tick.tick() => self.refresh(),
                Some(_) = fs_rx.recv() => {
                    while fs_rx.try_recv().is_ok() {}
                    self.refresh();
                }
                Some(_) = self.preview_rx.recv() => {
                    while self.preview_rx.try_recv().is_ok() {}
                    if self.live_preview.sync_health() {
                        self.capture_preview();
                    }
                    let wait = PREVIEW_FRAME_INTERVAL.saturating_sub(last_draw.elapsed());
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                    }
                    while self.preview_rx.try_recv().is_ok() {}
                    if self.live_preview.sync_health() {
                        self.capture_preview();
                    }
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
        match discover::snapshot(
            &self.store,
            &self.plugins,
            now,
            Duration::minutes(STALE_AFTER_MINUTES),
        ) {
            Ok(snapshot) => self.snapshot = snapshot,
            Err(err) => eprintln!("snapshot failed: {err:#}"),
        }
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
        self.refresh_preview();
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
                self.picker = Some(
                    picked
                        .checked_sub(1)
                        .unwrap_or(self.plugins.len().saturating_sub(1)),
                );
            }
            KeyCode::Enter => {
                self.picker = None;
                if let Some(plugin) = self.plugins.get(picked) {
                    let cwd = std::env::current_dir()?;
                    let pane =
                        tmux::new_window(&plugin.manifest.id, &cwd, &plugin.manifest.launch.argv)?;
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
        let selected = self
            .selected_agent()
            .map(|agent| (agent.pane.id.clone(), agent.pane.window_id.clone()));
        match selected {
            Some((pane_id, window_id)) => {
                if !self.live_preview.select(&window_id) {
                    self.snapshot_preview =
                        tmux::capture(&pane_id, PREVIEW_LINES).unwrap_or_default();
                }
            }
            None => {
                self.live_preview.deselect();
                self.snapshot_preview.clear();
            }
        }
    }

    fn capture_preview(&mut self) {
        self.snapshot_preview = self
            .selected_agent()
            .and_then(|agent| tmux::capture(&agent.pane.id, PREVIEW_LINES).ok())
            .unwrap_or_default();
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
                let Some(resume) = self
                    .plugin(&session.provider)
                    .and_then(|p| p.manifest.resume.clone())
                else {
                    return Ok(Flow::Continue);
                };
                let argv: Vec<String> = resume
                    .argv
                    .iter()
                    .map(|a| a.replace("{session_id}", &session.session_id))
                    .collect();
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

    fn render(&mut self, frame: &mut Frame) {
        let banner_height = if self.snapshot.uninstrumented.is_empty() {
            0
        } else {
            1
        };
        let preview_height =
            if matches!(self.view, View::Ended) || frame.area().height < MIN_PREVIEW_TERM_HEIGHT {
                Constraint::Length(0)
            } else {
                Constraint::Percentage(40)
            };
        let [banner, main, preview, footer] = Layout::vertical([
            Constraint::Length(banner_height),
            Constraint::Min(5),
            preview_height,
            Constraint::Length(1),
        ])
        .areas(frame.area());

        if banner_height > 0 {
            let msg = format!(
                " hooks not installed for {} — run `gw setup`",
                self.snapshot.uninstrumented.join(", ")
            );
            frame.render_widget(
                Paragraph::new(msg).style(Style::new().fg(Color::Black).bg(Color::Yellow)),
                banner,
            );
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

    fn agent_cells(&self, now: DateTime<Utc>) -> Vec<AgentCells> {
        self.snapshot
            .agents
            .iter()
            .map(|agent| {
                let (dot, word, color) = status_cell(agent.status);
                let plugin = self.plugin(&agent.provider);
                let label = plugin
                    .map(|p| p.manifest.label.clone())
                    .unwrap_or_else(|| agent.provider.clone());
                let label_color = plugin
                    .and_then(|p| p.manifest.color.as_deref())
                    .and_then(hex_color)
                    .unwrap_or(Color::Reset);
                AgentCells {
                    label,
                    label_color,
                    dot,
                    word,
                    color,
                    urgent: matches!(
                        agent.status,
                        AgentStatus::Attention(_) | AgentStatus::Error | AgentStatus::Stale
                    ),
                    detail: agent.detail.clone().unwrap_or_default(),
                    cwd_full: shorten(&agent.cwd, CWD_MAX),
                    cwd_short: basename(&agent.cwd),
                    branch: git_branch(&agent.cwd).unwrap_or_default(),
                    window: format!("{}:{}", agent.pane.window_index, agent.pane.window_name),
                    time: agent.since.map(|t| ago(t, now)).unwrap_or_default(),
                    subagents: agent
                        .subagents
                        .iter()
                        .map(|s| subagent_text(s, now))
                        .collect(),
                }
            })
            .collect()
    }

    fn render_agents(&self, frame: &mut Frame, area: Rect) {
        let cells = self.agent_cells(Utc::now());
        let mut lines = vec![Line::default()];
        // Physical line span [top, bottom) of the selected agent + its
        // subagent sub-lines; subagent rows can push it past the viewport.
        let mut selected_span = (0, 0);
        if cells.is_empty() {
            lines.push(Line::styled(
                "   no agents in this session — n launches one",
                Style::new().dim(),
            ));
        } else {
            let width = area.width as usize;
            let agent_w = cells.iter().map(|c| c.label.width()).max().unwrap_or(0);
            let status_w = cells
                .iter()
                .map(|c| c.dot.width() + 1 + c.word.width())
                .max()
                .unwrap_or(0);
            let plan = plan_right(&cells, width, agent_w, status_w);
            for (i, c) in cells.iter().enumerate() {
                if i == self.selected {
                    selected_span = (lines.len(), lines.len() + 1 + c.subagents.len());
                }
                lines.push(agent_line(
                    c,
                    i == self.selected,
                    agent_w,
                    status_w,
                    &plan,
                    width,
                ));
                lines.extend(c.subagents.iter().map(|s| subagent_line(s, width)));
            }
        }
        let scroll = scroll_offset(selected_span.0, selected_span.1, area.height as usize);
        frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), area);
    }

    fn render_ended(&self, frame: &mut Frame, area: Rect) {
        let now = Utc::now();
        let width = area.width as usize;
        let mut lines = vec![
            Line::default(),
            Line::styled("   recently ended — enter resumes", Style::new().dim()),
            Line::default(),
        ];
        if self.snapshot.ended.is_empty() {
            lines.push(Line::styled(
                "   nothing ended recently",
                Style::new().dim(),
            ));
            frame.render_widget(Paragraph::new(lines), area);
            return;
        }
        let cells: Vec<(String, Color, String, String, String)> = self
            .snapshot
            .ended
            .iter()
            .map(|s| {
                let plugin = self.plugin(&s.provider);
                let label = plugin
                    .map(|p| p.manifest.label.clone())
                    .unwrap_or_else(|| s.provider.clone());
                let color = plugin
                    .and_then(|p| p.manifest.color.as_deref())
                    .and_then(hex_color)
                    .unwrap_or(Color::Reset);
                let session: String = s.session_id.chars().take(12).collect();
                let cwd = s.cwd.as_deref().map(|c| shorten(c, 40)).unwrap_or_default();
                (label, color, session, cwd, ago(s.ended_at, now))
            })
            .collect();
        let label_w = cells.iter().map(|c| c.0.width()).max().unwrap_or(0);
        let session_w = cells.iter().map(|c| c.2.width()).max().unwrap_or(0);
        let cwd_w = cells.iter().map(|c| c.3.width()).max().unwrap_or(0);
        let time_w = cells.iter().map(|c| c.4.width()).max().unwrap_or(0);
        let dim = Style::new().dim();
        lines.extend(
            cells
                .iter()
                .enumerate()
                .map(|(i, (label, color, session, cwd, time))| {
                    let marker = if i == self.selected { " ❯ " } else { "   " };
                    let prefix = MARKER_W + label_w + COL_GAP + session_w + COL_GAP;
                    let mut right = pad(cwd, cwd_w);
                    right.push_str(SEP);
                    right.push_str(&pad_left(time, time_w));
                    let fill = width.saturating_sub(1 + prefix + right.width());
                    Line::from(vec![
                        Span::styled(marker, Style::new().bold()),
                        Span::styled(pad(label, label_w + COL_GAP), Style::new().fg(*color)),
                        Span::styled(pad(session, session_w + COL_GAP), dim),
                        Span::raw(" ".repeat(fill)),
                        Span::styled(right, dim),
                    ])
                }),
        );
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_preview(&mut self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            self.live_preview.deselect();
            return;
        }
        let width = area.width as usize;
        let selected = self.selected_agent().map(|agent| {
            (
                agent.pane.window_id.clone(),
                agent.pane.window_index,
                agent.pane.window_name.clone(),
            )
        });
        let Some((window_id, window_index, window_name)) = selected else {
            self.live_preview.deselect();
            return;
        };
        let head = format!("─── {window_index}:{window_name} ");
        let rule = format!("{head}{}", "─".repeat(width.saturating_sub(head.width())));
        let header = Rect::new(area.x, area.y, area.width, 1);
        let terminal = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
        frame.render_widget(
            Paragraph::new(Line::styled(rule, Style::new().dim())),
            header,
        );

        if self.live_preview.sync_health() {
            self.capture_preview();
        }
        let was_live = self.live_preview.is_live();
        let selected_live = self.live_preview.select(&window_id);
        let resized_live = self.live_preview.resize(terminal.width, terminal.height);
        if was_live && !(selected_live && resized_live) {
            self.capture_preview();
        }
        if let Some(parser) = self.live_preview.parser() {
            let parser = parser.lock().unwrap_or_else(|error| error.into_inner());
            frame.render_widget(PseudoTerminal::new(parser.screen()), terminal);
            return;
        }

        let all: Vec<&str> = self.snapshot_preview.lines().collect();
        let visible = terminal.height as usize;
        let lines = all[all.len().saturating_sub(visible)..]
            .iter()
            .map(|line| Line::styled(*line, Style::new().dim()))
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), terminal);
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
            let marker = if i == picked { "❯" } else { " " };
            let item = ListItem::new(format!(" {marker} {}", p.manifest.label));
            if i == picked {
                item.style(Style::new().bold())
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

struct AgentCells {
    label: String,
    label_color: Color,
    dot: &'static str,
    word: &'static str,
    color: Color,
    urgent: bool,
    detail: String,
    cwd_full: String,
    cwd_short: String,
    branch: String,
    window: String,
    time: String,
    /// Pre-composed one-liners for running subagents, rendered dim below the row.
    subagents: Vec<String>,
}

/// "agent_type · model · task · 3m" — omitting what the provider didn't report.
fn subagent_text(subagent: &gw_core::status::Subagent, now: DateTime<Utc>) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let agent_type = subagent.agent_type.as_deref().unwrap_or("subagent");
    parts.push(agent_type);
    parts.extend(subagent.model.as_deref());
    parts.extend(subagent.summary.as_deref());
    let time = ago(subagent.since, now);
    parts.push(&time);
    parts.join(SEP)
}

/// Scroll offset keeping the selected block [top, bottom) visible: 0 while
/// it fits the viewport, else just enough to bring its bottom into view; a
/// block taller than the viewport pins its top line instead.
fn scroll_offset(top: usize, bottom: usize, height: usize) -> usize {
    bottom.min(top + height).saturating_sub(height)
}

fn subagent_line(text: &str, width: usize) -> Line<'static> {
    let indent = " ".repeat(MARKER_W);
    let composed = truncate(text, width.saturating_sub(1 + MARKER_W + 2));
    Line::styled(format!("{indent}↳ {composed}"), Style::new().dim())
}

/// Column widths for the right cluster; 0 means the field is dropped.
struct RightPlan {
    cwd: usize,
    cwd_full: bool,
    branch: usize,
    window: usize,
    time: usize,
}

impl RightPlan {
    fn total(&self) -> usize {
        self.time
            + [self.cwd, self.branch, self.window]
                .iter()
                .filter(|&&w| w > 0)
                .map(|w| w + SEP.width())
                .sum::<usize>()
    }
}

/// Pick which right-cluster fields fit. Degradation order: drop window,
/// drop branch, shrink cwd to its basename, drop cwd. Time always stays;
/// detail keeps at least MIN_DETAIL columns before fields start dropping.
fn plan_right(cells: &[AgentCells], width: usize, agent_w: usize, status_w: usize) -> RightPlan {
    let maxw =
        |f: fn(&AgentCells) -> &String| cells.iter().map(|c| f(c).width()).max().unwrap_or(0);
    let time = maxw(|c| &c.time);
    let cwd_full = maxw(|c| &c.cwd_full);
    let cwd_short = maxw(|c| &c.cwd_short);
    let branch = maxw(|c| &c.branch);
    let window = maxw(|c| &c.window);
    let min_detail = if cells.iter().any(|c| !c.detail.is_empty()) {
        MIN_DETAIL
    } else {
        0
    };
    let prefix = MARKER_W + agent_w + COL_GAP + status_w + COL_GAP;
    let avail = width.saturating_sub(1 + prefix + min_detail + COL_GAP);
    let candidates = [
        (cwd_full, true, branch, window),
        (cwd_full, true, branch, 0),
        (cwd_full, true, 0, 0),
        (cwd_short, false, 0, 0),
        (0, false, 0, 0),
    ];
    for (cwd, full, branch, window) in candidates {
        let plan = RightPlan {
            cwd,
            cwd_full: full,
            branch,
            window,
            time,
        };
        if plan.total() <= avail {
            return plan;
        }
    }
    RightPlan {
        cwd: 0,
        cwd_full: false,
        branch: 0,
        window: 0,
        time,
    }
}

fn agent_line(
    c: &AgentCells,
    selected: bool,
    agent_w: usize,
    status_w: usize,
    plan: &RightPlan,
    width: usize,
) -> Line<'static> {
    let marker = if selected { " ❯ " } else { "   " };
    let mut spans = vec![
        Span::styled(marker, Style::new().fg(c.color).bold()),
        Span::styled(
            pad(&c.label, agent_w + COL_GAP),
            Style::new().fg(c.label_color),
        ),
        Span::styled(
            pad(&format!("{} {}", c.dot, c.word), status_w + COL_GAP),
            Style::new().fg(c.color),
        ),
    ];
    let prefix = MARKER_W + agent_w + COL_GAP + status_w + COL_GAP;
    let right_total = plan.total();
    let detail_avail = width.saturating_sub(1 + prefix + right_total + COL_GAP);
    let detail = truncate(&c.detail, detail_avail);
    spans.push(Span::styled(
        detail.clone(),
        if c.urgent {
            Style::new().fg(c.color)
        } else {
            Style::new()
        },
    ));
    let fill = width.saturating_sub(1 + prefix + detail.width() + right_total);
    spans.push(Span::raw(" ".repeat(fill)));

    let dim = Style::new().dim();
    let cwd = if plan.cwd_full {
        &c.cwd_full
    } else {
        &c.cwd_short
    };
    let fields = [
        (cwd.as_str(), plan.cwd, false),
        (c.branch.as_str(), plan.branch, false),
        (c.window.as_str(), plan.window, false),
        (c.time.as_str(), plan.time, true),
    ];
    let mut first = true;
    for (value, col, right_align) in fields {
        if col == 0 {
            continue;
        }
        if !first {
            let sep = if value.is_empty() { "   " } else { SEP };
            spans.push(Span::styled(sep, dim));
        }
        first = false;
        let text = truncate(value, col);
        let text = if right_align {
            pad_left(&text, col)
        } else {
            pad(&text, col)
        };
        spans.push(Span::styled(text, dim));
    }
    Line::from(spans)
}

fn status_cell(status: AgentStatus) -> (&'static str, &'static str, Color) {
    match status {
        AgentStatus::Attention(AttentionKind::Approval) => ("●", "approval", Color::Red),
        AgentStatus::Attention(AttentionKind::Question) => ("●", "question", Color::Red),
        AgentStatus::Error => ("✗", "error", Color::Magenta),
        AgentStatus::Stale => ("!", "stale", Color::Yellow),
        AgentStatus::Working => ("●", "working", Color::Green),
        AgentStatus::Done => ("●", "done", Color::Cyan),
        AgentStatus::Idle => ("○", "idle", Color::Blue),
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

fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(s.width())))
}

fn pad_left(s: &str, w: usize) -> String {
    format!("{}{s}", " ".repeat(w.saturating_sub(s.width())))
}

/// Truncate to `max` display columns, ending with … when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

/// Keep the tail of `s` within `max` display columns, prefixing … when cut.
fn tail_truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut tail = String::new();
    let mut w = 0;
    for ch in s.chars().rev() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        tail.insert(0, ch);
        w += cw;
    }
    if max > 0 {
        tail.insert(0, '…');
    }
    tail
}

fn shorten(path: &std::path::Path, max: usize) -> String {
    let home = dirs_home();
    let s = match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    tail_truncate(&s, max)
}

fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let head = cwd
        .ancestors()
        .map(|dir| dir.join(".git/HEAD"))
        .find(|p| p.exists())?;
    let head = std::fs::read_to_string(head).ok()?;
    match head.trim().strip_prefix("ref: refs/heads/") {
        Some(branch) => Some(branch.to_string()),
        None => Some(head.trim().chars().take(8).collect()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(detail: &str, cwd: &str, branch: &str, window: &str, time: &str) -> AgentCells {
        AgentCells {
            label: "Claude".into(),
            label_color: Color::Reset,
            dot: "●",
            word: "working",
            color: Color::Green,
            urgent: false,
            detail: detail.into(),
            cwd_full: cwd.into(),
            cwd_short: cwd.rsplit('/').next().unwrap_or(cwd).into(),
            branch: branch.into(),
            window: window.into(),
            time: time.into(),
            subagents: vec![],
        }
    }

    fn sample() -> Vec<AgentCells> {
        vec![
            cell(
                "Bash · $mattpocock-select",
                "~/Workspaces/gw2",
                "main",
                "2:gw",
                "3m",
            ),
            cell("", "~/Workspaces/gw2", "main", "1:.claude-wrap", "12m"),
        ]
    }

    #[test]
    fn scroll_keeps_selected_block_visible() {
        // Fits entirely: no scroll.
        assert_eq!(scroll_offset(1, 3, 10), 0);
        // Selected block ends below the viewport: bottom-aligned.
        assert_eq!(scroll_offset(8, 12, 10), 2);
        // Block taller than the viewport: pin its top line.
        assert_eq!(scroll_offset(5, 20, 10), 5);
        assert_eq!(scroll_offset(0, 1, 0), 0);

        // Regression: subagent sub-lines push later agents past the viewport;
        // every selectable row must still scroll into view (it was clipped
        // while j/k/Enter kept operating on it).
        let subagent_counts = [2usize, 3, 0];
        let height = 5;
        for selected in 0..subagent_counts.len() {
            let mut row = 1; // leading blank line
            let mut span = (0, 0);
            for (i, n) in subagent_counts.iter().enumerate() {
                if i == selected {
                    span = (row, row + 1 + n);
                }
                row += 1 + n;
            }
            let scroll = scroll_offset(span.0, span.1, height);
            assert!(
                (scroll..scroll + height).contains(&span.0),
                "selected agent row {} not visible at scroll {scroll}",
                span.0
            );
        }
    }

    #[test]
    fn subagent_text_joins_known_fields() {
        let now = DateTime::from_timestamp(600, 0).unwrap();
        let full = gw_core::status::Subagent {
            agent_type: Some("Explore".into()),
            model: Some("haiku".into()),
            summary: Some("find hooks".into()),
            since: DateTime::from_timestamp(0, 0).unwrap(),
        };
        assert_eq!(
            subagent_text(&full, now),
            "Explore · haiku · find hooks · 10m"
        );

        let bare = gw_core::status::Subagent {
            agent_type: None,
            model: None,
            summary: None,
            since: now,
        };
        assert_eq!(subagent_text(&bare, now), "subagent · now");
    }

    #[test]
    fn subagent_lines_stay_within_width() {
        for width in 10..80 {
            let line = subagent_line("Explore · haiku · a very long task description", width);
            assert!(line.width() <= width, "overflow at width {width}");
        }
    }

    #[test]
    fn truncate_respects_display_width() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello!", 5), "hell…");
        assert_eq!(truncate("中文路径", 5), "中文…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn tail_truncate_keeps_tail() {
        assert_eq!(tail_truncate("~/Workspaces/gw2", 8), "…ces/gw2");
        assert_eq!(tail_truncate("short", 8), "short");
    }

    #[test]
    fn plan_degrades_in_order() {
        let cells = sample();
        let (agent_w, status_w) = (6, 9);
        let all = plan_right(&cells, 120, agent_w, status_w);
        assert!(all.cwd_full && all.branch > 0 && all.window > 0);
        let no_window = plan_right(&cells, 80, agent_w, status_w);
        assert!(no_window.cwd_full && no_window.branch > 0 && no_window.window == 0);
        let no_branch = plan_right(&cells, 66, agent_w, status_w);
        assert!(no_branch.cwd_full && no_branch.branch == 0);
        let short_cwd = plan_right(&cells, 55, agent_w, status_w);
        assert!(!short_cwd.cwd_full && short_cwd.cwd > 0 && short_cwd.branch == 0);
        let time_only = plan_right(&cells, 42, agent_w, status_w);
        assert!(time_only.cwd == 0 && time_only.time > 0);
    }

    #[test]
    fn lines_never_overflow() {
        let cells = sample();
        let agent_w = cells.iter().map(|c| c.label.width()).max().unwrap();
        let status_w = cells
            .iter()
            .map(|c| c.dot.width() + 1 + c.word.width())
            .max()
            .unwrap();
        // Below ~26 columns the fixed prefix itself no longer fits and
        // Paragraph clipping takes over; assert the invariant above that.
        for width in 26..140 {
            let plan = plan_right(&cells, width, agent_w, status_w);
            for (i, c) in cells.iter().enumerate() {
                let line = agent_line(c, i == 0, agent_w, status_w, &plan, width);
                assert!(
                    line.width() <= width,
                    "line overflows at width {width}: {} > {width}",
                    line.width()
                );
            }
        }
    }
}
