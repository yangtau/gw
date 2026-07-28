//! The panel's pure state machine. `PanelState` holds everything the panel
//! remembers between keystrokes — screen, view, cursor, picker, help overlay —
//! and nothing else: no store, no tmux, no env. One `on(Input, &Ctx)` method
//! folds a keystroke into the next state and returns the side effects the
//! adapter must run (jump, launch, resume, quit). Every navigation rule
//! (view-toggle resets the cursor, picker wraparound, attention-jump, cursor
//! clamping) is decided here, so it is all testable without a terminal.

use std::collections::HashSet;

use gw_core::config::PanelView;
use gw_core::discover::{Agent, Snapshot};
use gw_core::session::Status;
use gw_core::tmux::TmuxPaneTarget;

/// Which list the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Agents,
    Ended,
}

/// A semantic keystroke. Mapping physical keys to these is the adapter's job;
/// interpreting them against the current mode is `PanelState`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Down,
    Up,
    ToggleView,
    NextAttention,
    OpenPicker,
    ToggleEnded,
    Confirm,
    Help,
    Cancel,
    Quit,
}

/// Something the adapter must do against the outside world after a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Focus or attach to this exact tmux pane target.
    Jump(TmuxPaneTarget),
    /// Launch the discovered plugin at this index in a new window.
    LaunchProvider(usize),
    /// Resume the ended session at this index of the snapshot.
    ResumeEnded(usize),
    /// Leave the panel.
    Quit,
}

/// The facts a transition needs about the world, borrowed for the call. Kept
/// out of `PanelState` so the state itself stays pure and cheap to construct
/// in tests.
pub struct Ctx<'a> {
    pub snapshot: &'a Snapshot,
    pub current_tmux_session_id: Option<&'a str>,
    pub plugin_count: usize,
}

pub struct PanelState {
    screen: Screen,
    show_shortcuts: bool,
    view: PanelView,
    selected: usize,
    picker: Option<usize>,
}

impl PanelState {
    pub fn new(view: PanelView) -> Self {
        Self {
            screen: Screen::Agents,
            show_shortcuts: false,
            view,
            selected: 0,
            picker: None,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn view(&self) -> PanelView {
        self.view
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn picker(&self) -> Option<usize> {
        self.picker
    }

    pub fn show_shortcuts(&self) -> bool {
        self.show_shortcuts
    }

    /// Fold one keystroke into the next state, returning effects to run.
    /// Mode precedence mirrors the original key dispatch: quit wins over
    /// everything, the help overlay swallows all other keys, help opens even
    /// over the picker, then the picker, then the list.
    pub fn on(&mut self, input: Input, ctx: &Ctx) -> Vec<Effect> {
        if input == Input::Quit {
            return vec![Effect::Quit];
        }
        if self.show_shortcuts {
            if matches!(input, Input::Help | Input::Cancel) {
                self.show_shortcuts = false;
            }
            return vec![];
        }
        if input == Input::Help {
            self.show_shortcuts = true;
            return vec![];
        }
        if let Some(picked) = self.picker {
            return self.on_picker(input, picked, ctx);
        }
        match input {
            Input::Cancel => return vec![Effect::Quit],
            Input::Down => self.select(1, ctx),
            Input::Up => self.select(-1, ctx),
            Input::ToggleView => {
                self.view = self.view.toggled();
                if matches!(self.screen, Screen::Agents) {
                    self.selected = 0;
                }
            }
            Input::NextAttention => self.select_next_attention(ctx),
            Input::OpenPicker => self.picker = Some(0),
            Input::ToggleEnded => {
                self.screen = match self.screen {
                    Screen::Agents => Screen::Ended,
                    Screen::Ended => Screen::Agents,
                };
                self.selected = 0;
            }
            Input::Confirm => return self.activate(ctx),
            Input::Help | Input::Quit => {}
        }
        vec![]
    }

    fn on_picker(&mut self, input: Input, picked: usize, ctx: &Ctx) -> Vec<Effect> {
        match input {
            Input::Cancel => self.picker = None,
            Input::Down => self.picker = Some((picked + 1) % ctx.plugin_count.max(1)),
            Input::Up => {
                self.picker = Some(
                    picked
                        .checked_sub(1)
                        .unwrap_or(ctx.plugin_count.saturating_sub(1)),
                );
            }
            Input::Confirm => {
                self.picker = None;
                if picked < ctx.plugin_count {
                    return vec![Effect::LaunchProvider(picked)];
                }
            }
            _ => {}
        }
        vec![]
    }

    fn activate(&self, ctx: &Ctx) -> Vec<Effect> {
        match self.screen {
            Screen::Agents => match self.selected_agent_index(ctx) {
                Some(index) => {
                    let agent = &ctx.snapshot.agents[index];
                    vec![Effect::Jump(TmuxPaneTarget {
                        tmux_session_id: agent.tmux_session_id.clone(),
                        window_id: agent.pane.window_id.clone(),
                        pane_id: agent.pane.id.clone(),
                    })]
                }
                None => vec![],
            },
            Screen::Ended => {
                if ctx.snapshot.ended.get(self.selected).is_some() {
                    vec![Effect::ResumeEnded(self.selected)]
                } else {
                    vec![]
                }
            }
        }
    }

    fn select(&mut self, delta: i64, ctx: &Ctx) {
        self.selected = move_selection(self.selected, delta, self.row_count(ctx));
    }

    fn select_next_attention(&mut self, ctx: &Ctx) {
        if !matches!(self.screen, Screen::Agents) {
            return;
        }
        let list = self.agent_list(ctx);
        let count = list.selectable_count();
        let next = (1..=count)
            .map(|offset| (self.selected + offset) % count.max(1))
            .find(|&selection| {
                list.agent_index(selection)
                    .and_then(|index| ctx.snapshot.agents.get(index))
                    .is_some_and(|agent| matches!(agent.status, Status::Attention(_)))
            });
        if let Some(selection) = next {
            self.selected = selection;
        }
    }

    /// Clamp the cursor into range after the snapshot changed under it.
    pub fn clamp_selection(&mut self, ctx: &Ctx) {
        let count = self.row_count(ctx);
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    pub fn row_count(&self, ctx: &Ctx) -> usize {
        match self.screen {
            Screen::Agents => self.agent_list(ctx).selectable_count(),
            Screen::Ended => ctx.snapshot.ended.len(),
        }
    }

    /// Index into `snapshot.agents` of the selected row, if a selectable agent
    /// is under the cursor.
    pub fn selected_agent_index(&self, ctx: &Ctx) -> Option<usize> {
        match self.screen {
            Screen::Agents => self.agent_list(ctx).agent_index(self.selected),
            Screen::Ended => None,
        }
    }

    pub fn agent_list(&self, ctx: &Ctx) -> AgentList {
        AgentList::new(&ctx.snapshot.agents, self.view, ctx.current_tmux_session_id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentListRow {
    Header { name: String, current: bool },
    Agent(usize),
}

/// The agent list flattened for display: header rows (global view only)
/// interleaved with agent rows, plus the indices of the selectable ones.
pub struct AgentList {
    pub rows: Vec<AgentListRow>,
    pub selectable_rows: Vec<usize>,
}

impl AgentList {
    pub fn new(agents: &[Agent], view: PanelView, current_tmux_session_id: Option<&str>) -> Self {
        let mut list = Self {
            rows: Vec::new(),
            selectable_rows: Vec::new(),
        };
        match view {
            PanelView::Current => {
                for (index, agent) in agents.iter().enumerate() {
                    if Some(agent.tmux_session_id.as_str()) == current_tmux_session_id {
                        list.push_agent(index);
                    }
                }
            }
            PanelView::Global => {
                let mut seen = HashSet::new();
                let mut groups = agents
                    .iter()
                    .filter_map(|agent| {
                        seen.insert(agent.tmux_session_id.as_str()).then_some((
                            agent.tmux_session_name.as_str(),
                            agent.tmux_session_id.as_str(),
                        ))
                    })
                    .collect::<Vec<_>>();
                groups.sort_by(|left, right| {
                    let left_current = Some(left.1) == current_tmux_session_id;
                    let right_current = Some(right.1) == current_tmux_session_id;
                    right_current
                        .cmp(&left_current)
                        .then_with(|| left.0.cmp(right.0))
                        .then_with(|| left.1.cmp(right.1))
                });
                for (name, tmux_session_id) in groups {
                    list.rows.push(AgentListRow::Header {
                        name: name.to_owned(),
                        current: Some(tmux_session_id) == current_tmux_session_id,
                    });
                    for (index, agent) in agents.iter().enumerate() {
                        if agent.tmux_session_id == tmux_session_id {
                            list.push_agent(index);
                        }
                    }
                }
            }
        }
        list
    }

    fn push_agent(&mut self, agent_index: usize) {
        self.selectable_rows.push(self.rows.len());
        self.rows.push(AgentListRow::Agent(agent_index));
    }

    pub fn selectable_count(&self) -> usize {
        self.selectable_rows.len()
    }

    pub fn agent_index(&self, selection: usize) -> Option<usize> {
        let row = *self.selectable_rows.get(selection)?;
        match self.rows.get(row)? {
            AgentListRow::Agent(index) => Some(*index),
            AgentListRow::Header { .. } => None,
        }
    }
}

pub fn move_selection(selected: usize, delta: i64, count: usize) -> usize {
    if count == 0 {
        0
    } else {
        (selected as i64 + delta).rem_euclid(count as i64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::protocol::AttentionKind;

    fn agent(
        tmux_session_name: &str,
        tmux_session_id: &str,
        pane_id: &str,
        status: Status,
    ) -> Agent {
        Agent {
            provider: "claude".into(),
            pane: gw_core::tmux::Pane {
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

    fn snapshot(agents: Vec<Agent>) -> Snapshot {
        Snapshot {
            agents,
            ended: vec![],
        }
    }

    fn ctx<'a>(snapshot: &'a Snapshot, current: Option<&'a str>, plugin_count: usize) -> Ctx<'a> {
        Ctx {
            snapshot,
            current_tmux_session_id: current,
            plugin_count,
        }
    }

    #[test]
    fn global_groups_put_current_first_then_sort_by_name_and_omit_empty_groups() {
        let agents = vec![
            agent(
                "zeta",
                "$3",
                "%3",
                Status::Attention(AttentionKind::Approval),
            ),
            agent("current", "$2", "%2", Status::Working),
            agent("alpha", "$1", "%1", Status::Done),
            agent("zeta", "$3", "%4", Status::Idle),
        ];

        let list = AgentList::new(&agents, PanelView::Global, Some("$2"));

        assert_eq!(
            list.rows,
            [
                AgentListRow::Header {
                    name: "current".into(),
                    current: true,
                },
                AgentListRow::Agent(1),
                AgentListRow::Header {
                    name: "alpha".into(),
                    current: false,
                },
                AgentListRow::Agent(2),
                AgentListRow::Header {
                    name: "zeta".into(),
                    current: false,
                },
                AgentListRow::Agent(0),
                AgentListRow::Agent(3),
            ]
        );
    }

    #[test]
    fn cursor_movement_skips_tmux_session_headers() {
        let agents = vec![
            agent("current", "$1", "%1", Status::Working),
            agent("other", "$2", "%2", Status::Done),
        ];
        let list = AgentList::new(&agents, PanelView::Global, Some("$1"));

        assert_eq!(list.selectable_rows, [1, 3]);
        let selection = move_selection(0, 1, list.selectable_count());
        assert_eq!(list.selectable_rows[selection], 3);
        assert_eq!(list.agent_index(selection), Some(1));
    }

    #[test]
    fn quit_wins_over_every_mode() {
        let snap = snapshot(vec![]);
        let mut state = PanelState::new(PanelView::Current);
        state.show_shortcuts = true;
        assert_eq!(state.on(Input::Quit, &ctx(&snap, None, 0)), [Effect::Quit]);
    }

    #[test]
    fn help_overlay_swallows_navigation_and_toggles_closed() {
        let snap = snapshot(vec![agent("s", "$1", "%1", Status::Working)]);
        let c = ctx(&snap, Some("$1"), 0);
        let mut state = PanelState::new(PanelView::Current);

        assert!(state.on(Input::Help, &c).is_empty());
        assert!(state.show_shortcuts());
        // A movement key does nothing while the overlay is up.
        assert!(state.on(Input::Down, &c).is_empty());
        assert_eq!(state.selected(), 0);
        // Esc closes it.
        assert!(state.on(Input::Cancel, &c).is_empty());
        assert!(!state.show_shortcuts());
    }

    #[test]
    fn esc_quits_from_the_list() {
        let snap = snapshot(vec![]);
        let mut state = PanelState::new(PanelView::Current);
        assert_eq!(
            state.on(Input::Cancel, &ctx(&snap, None, 0)),
            [Effect::Quit]
        );
    }

    #[test]
    fn toggling_view_resets_the_cursor_on_the_agents_screen() {
        let agents = vec![
            agent("current", "$1", "%1", Status::Working),
            agent("current", "$1", "%2", Status::Done),
        ];
        let snap = snapshot(agents);
        let c = ctx(&snap, Some("$1"), 0);
        let mut state = PanelState::new(PanelView::Current);

        state.on(Input::Down, &c);
        assert_eq!(state.selected(), 1);
        state.on(Input::ToggleView, &c);
        assert_eq!(state.view(), PanelView::Global);
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn confirm_on_agents_jumps_to_the_selected_pane() {
        let snap = snapshot(vec![agent("current", "$1", "%7", Status::Working)]);
        let c = ctx(&snap, Some("$1"), 0);
        let mut state = PanelState::new(PanelView::Current);
        assert_eq!(
            state.on(Input::Confirm, &c),
            [Effect::Jump(gw_core::tmux::TmuxPaneTarget {
                tmux_session_id: "$1".into(),
                window_id: "@%7".into(),
                pane_id: "%7".into(),
            })]
        );
    }

    #[test]
    fn confirm_on_empty_list_does_nothing() {
        let snap = snapshot(vec![]);
        let c = ctx(&snap, Some("$1"), 0);
        let mut state = PanelState::new(PanelView::Current);
        assert!(state.on(Input::Confirm, &c).is_empty());
    }

    #[test]
    fn attention_jump_targets_the_next_blocked_agent() {
        let agents = vec![
            agent("current", "$1", "%1", Status::Working),
            agent(
                "current",
                "$1",
                "%2",
                Status::Attention(AttentionKind::Approval),
            ),
        ];
        let snap = snapshot(agents);
        let c = ctx(&snap, Some("$1"), 0);
        let mut state = PanelState::new(PanelView::Current);
        state.on(Input::NextAttention, &c);
        assert_eq!(state.selected(), 1);
    }

    #[test]
    fn picker_wraps_around_and_confirm_launches() {
        let snap = snapshot(vec![]);
        let c = ctx(&snap, None, 3);
        let mut state = PanelState::new(PanelView::Current);

        state.on(Input::OpenPicker, &c);
        assert_eq!(state.picker(), Some(0));
        // Up from the top wraps to the last plugin.
        state.on(Input::Up, &c);
        assert_eq!(state.picker(), Some(2));
        // Down from the last wraps back to the top.
        state.on(Input::Down, &c);
        assert_eq!(state.picker(), Some(0));

        assert_eq!(state.on(Input::Confirm, &c), [Effect::LaunchProvider(0)]);
        assert_eq!(state.picker(), None);
    }

    #[test]
    fn picker_confirm_with_no_plugins_is_a_noop() {
        let snap = snapshot(vec![]);
        let c = ctx(&snap, None, 0);
        let mut state = PanelState::new(PanelView::Current);
        state.on(Input::OpenPicker, &c);
        assert!(state.on(Input::Confirm, &c).is_empty());
        assert_eq!(state.picker(), None);
    }

    #[test]
    fn toggling_to_ended_and_confirm_resumes() {
        let mut snap = snapshot(vec![]);
        snap.ended.push(gw_core::discover::EndedSession {
            provider: "claude".into(),
            session_id: "old".into(),
            cwd: None,
            ended_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        });
        let c = ctx(&snap, None, 0);
        let mut state = PanelState::new(PanelView::Current);

        state.on(Input::ToggleEnded, &c);
        assert_eq!(state.screen(), Screen::Ended);
        assert_eq!(state.on(Input::Confirm, &c), [Effect::ResumeEnded(0)]);
    }

    #[test]
    fn clamp_pulls_the_cursor_back_when_the_list_shrinks() {
        let agents = vec![
            agent("current", "$1", "%1", Status::Working),
            agent("current", "$1", "%2", Status::Done),
        ];
        let snap = snapshot(agents);
        let mut state = PanelState::new(PanelView::Current);
        state.on(Input::Down, &ctx(&snap, Some("$1"), 0));
        assert_eq!(state.selected(), 1);

        let shrunk = snapshot(vec![agent("current", "$1", "%1", Status::Working)]);
        state.clamp_selection(&ctx(&shrunk, Some("$1"), 0));
        assert_eq!(state.selected(), 0);
    }
}
