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

use std::collections::HashMap;
use std::io::stdout;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use crossterm::event::{
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use futures::StreamExt;
use gw_core::config::PanelView;
use gw_core::discover::{self, Agent, Snapshot};
use gw_core::plugins::{self, Plugin};
use gw_core::protocol::AttentionKind;
use gw_core::session::{ActivityEntry, ActivityKind, Status, Subagent};
use gw_core::store::Store;
use gw_core::tmux;
use notify::Watcher;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, TitlePosition,
};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::panel::{AgentList, AgentListRow, Ctx, Effect, Input, PanelState, Screen};

const STALE_AFTER_MINUTES: i64 = 30;
const RETENTION_DAYS: i64 = 7;
const MIN_ACTIVITY_TERM_HEIGHT: u16 = 16;
const ACTIVITY_AGE_W: usize = 4;
const ACTIVITY_LABEL_W: usize = 9;
const MARKER_W: usize = 3;
const COL_GAP: usize = 2;
const MIN_DETAIL: usize = 16;
const SEP: &str = " · ";
const CWD_MAX: usize = 32;

pub fn run(initial_view: PanelView) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut app = App::new(initial_view)?;
    let mut terminal = ratatui::init();
    loop {
        let outcome = rt.block_on(app.run(&mut terminal));
        ratatui::restore();
        match outcome? {
            Loop::Quit => return Ok(()),
            Loop::Attach(target) => {
                tmux::attach(&target)?;
                resume_terminal(&mut terminal)?;
                app.refresh();
            }
            Loop::Continue => unreachable!("the panel run loop cannot exit with Continue"),
        }
    }
}

/// Re-enter the same terminal after an externally attached tmux client detaches.
/// Reusing the terminal and App keeps the panel's navigation state intact.
fn resume_terminal(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let result = (|| {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        let size = terminal.size()?;
        terminal.resize(size.into())?;
        Ok(())
    })();
    if result.is_err() {
        ratatui::restore();
    }
    result
}

#[derive(Clone, Copy)]
struct FrameLayout {
    main: Rect,
    activity: Rect,
    footer: Rect,
}

fn frame_layout(area: Rect, screen: Screen) -> FrameLayout {
    let activity_height =
        if matches!(screen, Screen::Ended) || area.height < MIN_ACTIVITY_TERM_HEIGHT {
            Constraint::Length(0)
        } else {
            Constraint::Percentage(40)
        };
    let [main, activity, footer] =
        Layout::vertical([Constraint::Min(5), activity_height, Constraint::Length(1)]).areas(area);
    FrameLayout {
        main,
        activity,
        footer,
    }
}

fn activity_viewport(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        area.height,
    )
}

struct RepoMetadata {
    project: String,
    branch: String,
}

struct App {
    store: Store,
    plugins: Vec<Plugin>,
    snapshot: Snapshot,
    repo_metadata: HashMap<PathBuf, RepoMetadata>,
    state: PanelState,
    panel_pane_id: Option<String>,
    current_tmux_session_id: Option<String>,
    fallback_tmux_session_id: Option<String>,
    exit_after_jump: bool,
}

/// What the run loop should do after handling a keystroke.
enum Loop {
    Continue,
    Quit,
    /// Restore the terminal, attach it to the target, then resume the Panel on detach.
    Attach(tmux::TmuxPaneTarget),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JumpRoute {
    FocusExistingClient,
    AttachExternalTerminal,
}

fn jump_route(inside_tmux: bool) -> JumpRoute {
    if inside_tmux {
        JumpRoute::FocusExistingClient
    } else {
        JumpRoute::AttachExternalTerminal
    }
}

impl App {
    fn new(initial_view: PanelView) -> Result<Self> {
        let store = Store::open_default()?;
        store.sweep(Duration::days(RETENTION_DAYS))?;
        let exit_after_jump = tmux::inside_popup();
        let panel_pane_id = if exit_after_jump {
            None
        } else {
            std::env::var("TMUX_PANE").ok()
        };
        let fallback_tmux_session_id = if panel_pane_id.is_none() {
            Some(tmux::current_tmux_session_id()?)
        } else {
            None
        };
        let mut app = Self {
            store,
            plugins: plugins::discover()?,
            snapshot: Snapshot {
                agents: vec![],
                ended: vec![],
            },
            repo_metadata: HashMap::new(),
            state: PanelState::new(initial_view),
            panel_pane_id,
            current_tmux_session_id: None,
            fallback_tmux_session_id,
            exit_after_jump,
        };
        app.refresh();
        Ok(app)
    }

    async fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<Loop> {
        let (fs_tx, mut fs_rx) = tokio::sync::mpsc::channel(1);
        let sessions_dir = self.store.root().join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        let mut watcher = notify::recommended_watcher(move |_| {
            let _ = fs_tx.try_send(());
        })?;
        watcher.watch(&sessions_dir, notify::RecursiveMode::NonRecursive)?;

        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            terminal.draw(|f| self.render(f))?;
            tokio::select! {
                _ = tick.tick() => self.refresh(),
                Some(_) = fs_rx.recv() => self.refresh(),
                Some(Ok(ev)) = input.next() => {
                    if let TermEvent::Key(key) = ev {
                        if let Some(input) = map_key(key) {
                            match self.handle(input)? {
                                Loop::Continue => {}
                                outcome => return Ok(outcome),
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fold one semantic input through the pure state machine, then run the
    /// effects it emits against tmux/store/env. This is the only place the
    /// panel's decisions meet the outside world.
    fn handle(&mut self, input: Input) -> Result<Loop> {
        let effects = {
            let ctx = Ctx {
                snapshot: &self.snapshot,
                current_tmux_session_id: self.current_tmux_session_id.as_deref(),
                plugin_count: self.plugins.len(),
            };
            self.state.on(input, &ctx)
        };
        for effect in effects {
            match effect {
                Effect::Quit => return Ok(Loop::Quit),
                Effect::Jump(target) => match self.jump(target)? {
                    Loop::Continue => {}
                    outcome => return Ok(outcome),
                },
                Effect::LaunchProvider(index) => {
                    if let Some(plugin) = self.plugins.get(index) {
                        let cwd = std::env::current_dir()?;
                        let target = tmux::new_window(
                            &plugin.manifest.id,
                            &cwd,
                            &plugin.manifest.launch.argv,
                        )?;
                        match self.jump(target)? {
                            Loop::Continue => {}
                            outcome => return Ok(outcome),
                        }
                    }
                }
                Effect::ResumeEnded(index) => match self.resume_ended(index)? {
                    Loop::Continue => {}
                    outcome => return Ok(outcome),
                },
                Effect::ForkAgent(index) => match self.fork_agent(index)? {
                    Loop::Continue => {}
                    outcome => return Ok(outcome),
                },
            }
        }
        Ok(Loop::Continue)
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            snapshot: &self.snapshot,
            current_tmux_session_id: self.current_tmux_session_id.as_deref(),
            plugin_count: self.plugins.len(),
        }
    }

    fn refresh(&mut self) {
        let topology = match tmux::observe_topology() {
            Ok(topology) => topology,
            Err(error) => {
                gw_core::tui_log::error(&format!("topology snapshot failed: {error:#}"));
                return;
            }
        };
        let now = Utc::now();
        self.current_tmux_session_id = self
            .panel_pane_id
            .as_deref()
            .and_then(|pane_id| tmux::locate_panel(pane_id, &topology))
            .map(|panel| panel.tmux_session_id)
            .or_else(|| self.fallback_tmux_session_id.clone());
        match discover::snapshot(
            &self.store,
            &self.plugins,
            now,
            Duration::minutes(STALE_AFTER_MINUTES),
            &topology,
        ) {
            Ok(snapshot) => {
                self.repo_metadata = repo_metadata(&snapshot);
                self.snapshot = snapshot;
            }
            Err(err) => gw_core::tui_log::error(&format!("snapshot failed: {err:#}")),
        }
        let ctx = Ctx {
            snapshot: &self.snapshot,
            current_tmux_session_id: self.current_tmux_session_id.as_deref(),
            plugin_count: self.plugins.len(),
        };
        self.state.clamp_selection(&ctx);
    }

    fn selected_agent(&self) -> Option<&Agent> {
        let index = self.state.selected_agent_index(&self.ctx())?;
        self.snapshot.agents.get(index)
    }

    fn agent_list(&self) -> AgentList {
        self.state.agent_list(&self.ctx())
    }

    fn plugin(&self, provider: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.manifest.id == provider)
    }

    fn resume_ended(&mut self, index: usize) -> Result<Loop> {
        let Some(session) = self.snapshot.ended.get(index) else {
            return Ok(Loop::Continue);
        };
        let Some(resume) = self
            .plugin(&session.provider)
            .and_then(|p| p.manifest.resume.clone())
        else {
            return Ok(Loop::Continue);
        };
        let cwd = session.cwd.clone().unwrap_or(std::env::current_dir()?);
        let argv = gw_core::launch::expand_argv(&resume.argv, &session.session_id, None, &cwd);
        let target = tmux::new_window(&session.provider, &cwd, &argv)?;
        self.jump(target)
    }

    /// Fork the live agent at `index` into a new tmux window using the
    /// provider's `fork` capability template. The source pane is never
    /// touched: the fork lives entirely in the new window, and the branch
    /// itself is created inside the provider CLI. Missing capability or a
    /// missing session id are non-fatal — log and stay put.
    fn fork_agent(&mut self, index: usize) -> Result<Loop> {
        let Some(agent) = self.snapshot.agents.get(index) else {
            return Ok(Loop::Continue);
        };
        let Some(session_id) = agent.session_id.clone() else {
            gw_core::tui_log::error(&format!(
                "fork: agent has no session id yet ({} / {}) — wait for a hook",
                agent.provider, agent.pane.id
            ));
            return Ok(Loop::Continue);
        };
        let Some(plugin) = self.plugin(&agent.provider) else {
            gw_core::tui_log::error(&format!(
                "fork: no plugin loaded for provider {}",
                agent.provider
            ));
            return Ok(Loop::Continue);
        };
        let Some(fork) = plugin.manifest.fork.clone() else {
            gw_core::tui_log::error(&format!(
                "fork: provider {} does not support fork",
                agent.provider
            ));
            return Ok(Loop::Continue);
        };
        let cwd = agent.cwd.clone();
        let argv = gw_core::launch::expand_argv(&fork.argv, &session_id, None, &cwd);
        let target = tmux::new_window(&agent.provider, &cwd, &argv)?;
        self.jump(target)
    }

    fn jump(&mut self, target: tmux::TmuxPaneTarget) -> Result<Loop> {
        if jump_route(tmux::inside_tmux()) == JumpRoute::AttachExternalTerminal {
            return Ok(Loop::Attach(target));
        }
        tmux::focus(&target)?;
        if self.exit_after_jump {
            return Ok(Loop::Quit);
        }
        self.refresh();
        Ok(Loop::Continue)
    }

    fn render(&self, frame: &mut Frame) {
        if self.state.show_shortcuts() {
            self.render_shortcuts(frame);
            return;
        }
        let layout = frame_layout(frame.area(), self.state.screen());

        match self.state.screen() {
            Screen::Agents => self.render_agents(frame, layout.main),
            Screen::Ended => self.render_ended(frame, layout.main),
        }
        self.render_activity(frame, layout.activity);
        self.render_footer(frame, layout.footer);
        if self.state.picker().is_some() {
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
                        Status::Attention(_) | Status::Error | Status::Stale
                    ),
                    detail: agent.detail.clone().unwrap_or_default(),
                    cwd_full: shorten(&agent.cwd, CWD_MAX),
                    cwd_short: basename(&agent.cwd),
                    branch: self
                        .repo_metadata
                        .get(&agent.cwd)
                        .map(|metadata| metadata.branch.clone())
                        .unwrap_or_default(),
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
        let list = self.agent_list();
        let selected = self.state.selected();
        let mut lines = vec![Line::default()];
        // Physical line span [top, bottom) of the selected agent + its
        // subagent sub-lines; subagent rows can push it past the viewport.
        let mut selected_span = (0, 0);
        if list.selectable_count() == 0 {
            let empty = match self.state.view() {
                PanelView::Current => "   no agents in this tmux session — n launches one",
                PanelView::Global => "   no agents in any tmux session — n launches one",
            };
            lines.push(Line::styled(empty, Style::new().dim()));
        } else {
            let width = area.width as usize;
            let visible_cells = list
                .selectable_rows
                .iter()
                .filter_map(|row| match &list.rows[*row] {
                    AgentListRow::Agent(index) => cells.get(*index),
                    AgentListRow::Header { .. } => None,
                })
                .collect::<Vec<_>>();
            let agent_w = visible_cells
                .iter()
                .map(|c| c.label.width())
                .max()
                .unwrap_or(0);
            let status_w = visible_cells
                .iter()
                .map(|c| c.dot.width() + 1 + c.word.width())
                .max()
                .unwrap_or(0);
            let plan = plan_right_refs(&visible_cells, width, agent_w, status_w);
            let mut selection = 0;
            for row in &list.rows {
                match row {
                    AgentListRow::Header { name, current } => {
                        if lines.len() > 1 {
                            lines.push(Line::default());
                        }
                        let suffix = if *current { " (current)" } else { "" };
                        lines.push(Line::styled(
                            format!("   {name}{suffix}"),
                            Style::new().bold().dim(),
                        ));
                    }
                    AgentListRow::Agent(index) => {
                        let c = &cells[*index];
                        if selection == selected {
                            selected_span = (lines.len(), lines.len() + 1 + c.subagents.len());
                        }
                        lines.push(agent_line(
                            c,
                            selection == selected,
                            agent_w,
                            status_w,
                            &plan,
                            width,
                        ));
                        lines.extend(c.subagents.iter().map(|s| subagent_line(s, width)));
                        selection += 1;
                    }
                }
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
                    let marker = if i == self.state.selected() {
                        " ❯ "
                    } else {
                        "   "
                    };
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
        let scroll = ended_scroll_offset(self.state.selected(), area.height as usize);
        frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), area);
    }

    fn render_activity(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        let Some(agent) = self.selected_agent() else {
            return;
        };
        let viewport = activity_viewport(area);
        let frame_style = Style::new().fg(Color::DarkGray).dim();
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(frame_style)
            .title_style(frame_style)
            .title_alignment(Alignment::Left)
            .title_position(TitlePosition::Top)
            .title(format!(
                " {}: {} ",
                self.repo_metadata
                    .get(&agent.cwd)
                    .map(|metadata| metadata.project.as_str())
                    .unwrap_or_else(|| agent
                        .cwd
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("")),
                agent.provider
            ));
        let inner = block.inner(viewport);
        frame.render_widget(block, viewport);
        if inner.is_empty() {
            return;
        }

        let rows = activity_rows(&agent.activity, Utc::now());
        if rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled("no events yet", Style::new().dim())),
                inner,
            );
            return;
        }

        let visible = inner.height as usize;
        let rows = &rows[rows.len().saturating_sub(visible)..];
        let mut lines = vec![Line::default(); visible.saturating_sub(rows.len())];
        lines.extend(
            rows.iter()
                .map(|row| activity_line(row, inner.width as usize)),
        );
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let mut hints = " ?".to_owned();
        if matches!(self.state.screen(), Screen::Agents) && self.state.view() == PanelView::Current
        {
            if let Some(elsewhere) = elsewhere_hint(
                &self.snapshot.agents,
                self.current_tmux_session_id.as_deref(),
            ) {
                hints = format!(" {elsewhere}{SEP}{}", hints.trim_start());
            }
        }
        frame.render_widget(Paragraph::new(hints).dim(), area);
    }

    fn render_shortcuts(&self, frame: &mut Frame) {
        let [main, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
        let section = |title| Line::styled(format!("   {title}"), Style::new().bold().dim());
        let shortcut = |key, action| {
            Line::from(vec![
                Span::raw("   "),
                Span::styled(pad(key, 14), Style::new().bold()),
                Span::styled(action, Style::new().dim()),
            ])
        };
        let lines = vec![
            Line::default(),
            Line::styled("   keyboard shortcuts", Style::new().bold()),
            Line::default(),
            section("navigation"),
            shortcut("↑ / k", "select previous item"),
            shortcut("↓ / j", "select next item"),
            shortcut("enter", "jump, resume, or launch the selected item"),
            shortcut("a", "select the next agent needing attention"),
            Line::default(),
            section("views"),
            shortcut("tab", "toggle current / all tmux sessions"),
            shortcut("r", "toggle agents / recently ended sessions"),
            Line::default(),
            section("actions"),
            shortcut("n", "open the new-agent picker"),
            shortcut("f", "fork the selected agent into a new window"),
            shortcut("?", "open or close this page"),
            shortcut("esc", "go back, cancel the picker, or quit the panel"),
            shortcut("ctrl-c", "quit from anywhere"),
        ];
        frame.render_widget(Paragraph::new(lines), main);
        frame.render_widget(Paragraph::new(" ? / esc back").dim(), footer);
    }

    fn render_picker(&self, frame: &mut Frame) {
        let picked = self.state.picker().unwrap();
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

/// Translate a physical keystroke into a semantic panel input, or `None` for
/// keys the panel ignores. Mode-dependent meaning is resolved by `PanelState`,
/// not here — this map is context-free.
fn map_key(key: KeyEvent) -> Option<Input> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Input::Quit);
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Input::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Input::Up),
        KeyCode::Tab => Some(Input::ToggleView),
        KeyCode::Char('a') => Some(Input::NextAttention),
        KeyCode::Char('n') => Some(Input::OpenPicker),
        KeyCode::Char('r') => Some(Input::ToggleEnded),
        KeyCode::Char('f') => Some(Input::ForkSelected),
        KeyCode::Char('?') => Some(Input::Help),
        KeyCode::Enter => Some(Input::Confirm),
        KeyCode::Esc => Some(Input::Cancel),
        _ => None,
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
fn subagent_text(subagent: &Subagent, now: DateTime<Utc>) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let agent_type = subagent.agent_type.as_deref().unwrap_or("subagent");
    parts.push(agent_type);
    parts.extend(subagent.model.as_deref());
    parts.extend(subagent.summary.as_deref());
    let time = ago(subagent.since, now);
    parts.push(&time);
    parts.join(SEP)
}

fn ended_scroll_offset(selected: usize, height: usize) -> usize {
    scroll_offset(selected + 3, selected + 4, height)
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
#[cfg(test)]
fn plan_right(cells: &[AgentCells], width: usize, agent_w: usize, status_w: usize) -> RightPlan {
    let cells = cells.iter().collect::<Vec<_>>();
    plan_right_refs(&cells, width, agent_w, status_w)
}

fn plan_right_refs(
    cells: &[&AgentCells],
    width: usize,
    agent_w: usize,
    status_w: usize,
) -> RightPlan {
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

fn elsewhere_hint(agents: &[Agent], current_tmux_session_id: Option<&str>) -> Option<String> {
    let current_tmux_session_id = current_tmux_session_id?;
    let elsewhere = agents
        .iter()
        .filter(|agent| agent.tmux_session_id != current_tmux_session_id)
        .collect::<Vec<_>>();
    if elsewhere.is_empty() {
        return None;
    }
    let attention = elsewhere
        .iter()
        .filter(|agent| matches!(agent.status, Status::Attention(_)))
        .count();
    let errors = elsewhere
        .iter()
        .filter(|agent| agent.status == Status::Error)
        .count();
    let mut parts = vec![format!("{} elsewhere", elsewhere.len())];
    if attention > 0 {
        parts.push(format!("{attention} attention"));
    }
    if errors > 0 {
        let label = if errors == 1 { "error" } else { "errors" };
        parts.push(format!("{errors} {label}"));
    }
    Some(parts.join(SEP))
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

fn status_cell(status: Status) -> (&'static str, &'static str, Color) {
    match status {
        Status::Attention(AttentionKind::Approval) => ("●", "approval", Color::Red),
        Status::Attention(AttentionKind::Question) => ("●", "question", Color::Red),
        Status::Error => ("✗", "error", Color::Magenta),
        Status::Stale => ("!", "stale", Color::Yellow),
        Status::Working => ("●", "working", Color::Green),
        Status::Done => ("●", "done", Color::Cyan),
        Status::Idle => ("○", "idle", Color::Blue),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivityRow {
    age: String,
    label: &'static str,
    color: Color,
    text: String,
}

fn activity_rows(activity: &[ActivityEntry], now: DateTime<Utc>) -> Vec<ActivityRow> {
    activity
        .iter()
        .map(|entry| {
            let age = entry.at.map(|at| ago(at, now)).unwrap_or_default();
            let (label, color) = activity_cell(entry.kind);
            ActivityRow {
                age,
                label,
                color,
                text: entry.detail.clone(),
            }
        })
        .collect()
}

fn activity_cell(kind: ActivityKind) -> (&'static str, Color) {
    match kind {
        ActivityKind::Focus => ("focus", Color::DarkGray),
        ActivityKind::Session => ("session", Color::DarkGray),
        ActivityKind::Turn => ("turn", status_cell(Status::Working).2),
        ActivityKind::Tool => ("tool", Color::DarkGray),
        ActivityKind::Approval => (
            "approval",
            status_cell(Status::Attention(AttentionKind::Approval)).2,
        ),
        ActivityKind::Question => (
            "question",
            status_cell(Status::Attention(AttentionKind::Question)).2,
        ),
        ActivityKind::Done => ("done", status_cell(Status::Done).2),
        ActivityKind::Error => ("error", status_cell(Status::Error).2),
        ActivityKind::SubagentStarted => ("subagent+", Color::DarkGray),
        ActivityKind::SubagentEnded => ("subagent-", Color::DarkGray),
        ActivityKind::WaitStarted => ("wait+", Color::DarkGray),
        ActivityKind::WaitEnded => ("wait-", Color::DarkGray),
    }
}

fn activity_line(row: &ActivityRow, width: usize) -> Line<'static> {
    let age = pad_left(&tail_truncate(&row.age, ACTIVITY_AGE_W), ACTIVITY_AGE_W);
    let label = pad(row.label, ACTIVITY_LABEL_W);
    let prefix_width = ACTIVITY_AGE_W + 1 + ACTIVITY_LABEL_W + 2;
    let mut row_style = Style::new().fg(row.color);
    if row.color == Color::DarkGray {
        row_style = row_style.dim();
    }
    if width < prefix_width {
        return Line::styled(truncate(&format!("{age} {label}  "), width), row_style);
    }
    Line::from(vec![
        Span::styled(age, Style::new().fg(Color::DarkGray).dim()),
        Span::raw(" "),
        Span::styled(label, row_style),
        Span::raw("  "),
        Span::styled(truncate(&row.text, width - prefix_width), row_style),
    ])
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

fn repo_metadata(snapshot: &Snapshot) -> HashMap<PathBuf, RepoMetadata> {
    let mut metadata = HashMap::new();
    for agent in &snapshot.agents {
        metadata
            .entry(agent.cwd.clone())
            .or_insert_with(|| RepoMetadata {
                project: project_name(&agent.cwd),
                branch: git_branch(&agent.cwd).unwrap_or_default(),
            });
    }
    metadata
}

fn project_name(cwd: &std::path::Path) -> String {
    let root = cwd
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd);
    basename(root)
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

    #[test]
    fn jump_outside_tmux_defers_attach_until_after_the_panel_exits() {
        assert_eq!(jump_route(false), JumpRoute::AttachExternalTerminal);
        assert_eq!(jump_route(true), JumpRoute::FocusExistingClient);
    }

    fn agent(
        tmux_session_name: &str,
        tmux_session_id: &str,
        pane_id: &str,
        status: Status,
    ) -> Agent {
        Agent {
            provider: "claude".into(),
            pane: tmux::Pane {
                id: pane_id.into(),
                window_id: format!("@{pane_id}"),
                pid: 1,
                tty: "ttys001".into(),
                cwd: "/work".into(),
                window_index: 1,
                window_name: "agent".into(),
            },
            tmux_session_name: tmux_session_name.into(),
            tmux_session_id: tmux_session_id.into(),
            pid: 2,
            cwd: "/work".into(),
            session_id: Some(format!("session-{pane_id}")),
            status,
            since: None,
            detail: None,
            subagents: vec![],
            activity: vec![],
        }
    }

    fn activity(at: Option<DateTime<Utc>>, kind: ActivityKind, detail: &str) -> ActivityEntry {
        ActivityEntry {
            at,
            kind,
            detail: detail.into(),
        }
    }

    #[test]
    fn maps_every_activity_kind_to_rows() {
        let now = DateTime::from_timestamp(7_200, 0).unwrap();
        let activity = vec![
            activity(
                Some(now - Duration::minutes(2)),
                ActivityKind::Focus,
                "foreground",
            ),
            activity(
                Some(now - Duration::minutes(1)),
                ActivityKind::Session,
                "opus",
            ),
            activity(
                Some(now - Duration::hours(1)),
                ActivityKind::Turn,
                "implement activity",
            ),
            activity(None, ActivityKind::Tool, "cargo test"),
            activity(Some(now), ActivityKind::Approval, "run command"),
            activity(Some(now), ActivityKind::Question, "which option"),
            activity(Some(now), ActivityKind::Done, "finished"),
            activity(Some(now), ActivityKind::Error, "try later"),
            activity(
                Some(now),
                ActivityKind::SubagentStarted,
                "Explore · find tests",
            ),
            activity(Some(now), ActivityKind::SubagentEnded, "agent-1"),
            activity(Some(now), ActivityKind::Session, "ended"),
        ];

        let rows = activity_rows(&activity, now);
        assert_eq!(
            rows.iter()
                .map(|row| (row.age.as_str(), row.label, row.color, row.text.as_str()))
                .collect::<Vec<_>>(),
            [
                ("2m", "focus", Color::DarkGray, "foreground"),
                ("1m", "session", Color::DarkGray, "opus"),
                ("1h0m", "turn", Color::Green, "implement activity"),
                ("", "tool", Color::DarkGray, "cargo test"),
                ("now", "approval", Color::Red, "run command"),
                ("now", "question", Color::Red, "which option"),
                ("now", "done", Color::Cyan, "finished"),
                ("now", "error", Color::Magenta, "try later"),
                ("now", "subagent+", Color::DarkGray, "Explore · find tests"),
                ("now", "subagent-", Color::DarkGray, "agent-1"),
                ("now", "session", Color::DarkGray, "ended"),
            ]
        );
    }

    #[test]
    fn elsewhere_hint_reports_attention_and_error_counts() {
        let agents = vec![
            agent("current", "$1", "%1", Status::Working),
            agent(
                "other",
                "$2",
                "%2",
                Status::Attention(AttentionKind::Question),
            ),
            agent("other", "$2", "%3", Status::Error),
        ];

        assert_eq!(
            elsewhere_hint(&agents, Some("$1")).as_deref(),
            Some("2 elsewhere · 1 attention · 1 error")
        );
        assert_eq!(elsewhere_hint(&agents[..1], Some("$1")), None);
    }

    #[test]
    fn activity_rows_preserve_detail_and_render_truncates() {
        let now = DateTime::from_timestamp(60, 0).unwrap();
        let activity = [
            activity(Some(now), ActivityKind::Error, "rate_limit"),
            activity(Some(now), ActivityKind::SubagentStarted, "inspect"),
            activity(None, ActivityKind::Session, ""),
        ];
        let rows = activity_rows(&activity, now);
        assert_eq!(rows[0].text, "rate_limit");
        assert_eq!(rows[1].text, "inspect");
        assert_eq!(rows[2].text, "");
        assert_eq!(rows[2].age, "");

        let line = activity_line(
            &ActivityRow {
                age: "now".into(),
                label: "turn",
                color: Color::Green,
                text: "a long activity summary".into(),
            },
            24,
        );
        assert_eq!(line.width(), 24);
        assert_eq!(line.spans.last().unwrap().content, "a long …");
    }

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
    fn key_mapping_ignores_release_and_keeps_repeat() {
        let key = |code, modifiers, kind| KeyEvent::new_with_kind(code, modifiers, kind);

        assert_eq!(
            map_key(key(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Press)),
            Some(Input::Down)
        );
        assert_eq!(
            map_key(key(KeyCode::Down, KeyModifiers::NONE, KeyEventKind::Repeat)),
            Some(Input::Down)
        );
        assert_eq!(
            map_key(key(
                KeyCode::Down,
                KeyModifiers::NONE,
                KeyEventKind::Release
            )),
            None
        );
        assert_eq!(
            map_key(key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            )),
            Some(Input::Quit)
        );
        assert_eq!(
            map_key(key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                KeyEventKind::Release,
            )),
            None
        );
    }

    #[test]
    fn ended_scroll_keeps_selection_visible() {
        assert_eq!(ended_scroll_offset(0, 10), 0);
        assert_eq!(ended_scroll_offset(7, 10), 1);
        assert_eq!(ended_scroll_offset(7, 0), 10);
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
        let full = Subagent {
            agent_type: Some("Explore".into()),
            model: Some("haiku".into()),
            summary: Some("find hooks".into()),
            since: DateTime::from_timestamp(0, 0).unwrap(),
        };
        assert_eq!(
            subagent_text(&full, now),
            "Explore · haiku · find hooks · 10m"
        );

        let bare = Subagent {
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
    fn project_name_uses_git_root_and_falls_back_to_cwd() {
        let sandbox = std::env::temp_dir().join(format!("gw-tui-{}", std::process::id()));
        let project = sandbox.join("gw");
        let nested = project.join("crates/gw");
        let standalone = sandbox.join("standalone");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&standalone).unwrap();

        assert_eq!(project_name(&nested), "gw");
        assert_eq!(project_name(&standalone), "standalone");

        std::fs::remove_dir_all(sandbox).unwrap();
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
