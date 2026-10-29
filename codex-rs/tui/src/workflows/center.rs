//! Retained workflow selection, independent detail scrolling, and saved-workflow input.
#[path = "center_actions.rs"]
mod actions;
#[path = "center_columns.rs"]
mod columns;
#[path = "center_details.rs"]
mod details;
#[path = "center_help.rs"]
mod help;
#[path = "center_render.rs"]
mod render;
#[path = "center_rows.rs"]
mod rows;
use super::*;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::TextArea;
use crate::render::renderable::Renderable;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;

pub(crate) struct WorkflowCenter {
    state: SharedWorkflows,
    tx: AppEventSender,
    complete: bool,
    input: Option<TextArea>,
    input_saved_id: Option<String>,
    input_error: Option<String>,
    keymap: crate::keymap::ListKeymap,
    runtime_keymap: crate::keymap::RuntimeKeymap,
}

impl WorkflowCenter {
    #[cfg(test)]
    pub(crate) fn new(state: SharedWorkflows, tx: AppEventSender) -> Self {
        Self::new_with_keymap(state, tx, crate::keymap::RuntimeKeymap::defaults().list)
    }

    pub(crate) fn new_with_keymap(
        state: SharedWorkflows,
        tx: AppEventSender,
        keymap: crate::keymap::ListKeymap,
    ) -> Self {
        let mut runtime_keymap = crate::keymap::RuntimeKeymap::defaults();
        runtime_keymap.list = keymap.clone();
        Self {
            state,
            tx,
            complete: false,
            input: None,
            input_saved_id: None,
            input_error: None,
            keymap,
            runtime_keymap,
        }
    }

    pub(crate) fn with_runtime_keymap(mut self, keymap: crate::keymap::RuntimeKeymap) -> Self {
        self.runtime_keymap = keymap;
        self
    }

    fn send(&self, event: WorkflowEvent) {
        self.tx.send(AppEvent::Workflow(event));
    }

    fn accept(&mut self) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = state.center.saved;
        let chat = state.selected_worker_thread().is_some()
            || (!saved
                && state.child.is_none()
                && state.visible_run_indices().contains(&state.selected));
        drop(state);
        if saved {
            self.accept_saved();
        } else if chat {
            self.open_chat();
        }
    }

    fn navigate(&mut self, down: bool) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.center.saved {
                let visible = state.visible_saved_indices();
                let selected = visible
                    .iter()
                    .position(|index| *index == state.selected_saved_index())
                    .unwrap_or(0);
                let next = if down {
                    (selected + 1).min(visible.len().saturating_sub(1))
                } else {
                    selected.saturating_sub(1)
                };
                state.center.saved_id = visible
                    .get(next)
                    .map(|index| state.saved[*index].id.clone());
            } else {
                state.navigate_tree(down);
            }
        }
        self.request_selected_details();
    }
}

impl BottomPaneView for WorkflowCenter {
    fn keymap_contexts(&self) -> crate::keymap::KeymapContextSet {
        crate::keymap::KeymapContextSet::new(if self.input.is_some() {
            crate::keymap::KeymapContext::Editor
        } else {
            crate::keymap::KeymapContext::List
        })
    }
    fn view_id(&self) -> Option<&'static str> {
        Some(CENTER_ID)
    }
    fn selected_index(&self) -> Option<usize> {
        Some(
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .selected,
        )
    }
    fn is_complete(&self) -> bool {
        self.complete
    }
    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }
    fn handle_paste(&mut self, pasted: String) -> bool {
        if let Some(input) = &mut self.input {
            input.insert_str_at(input.cursor(), &pasted);
            true
        } else {
            false
        }
    }
    fn pre_draw_tick(&mut self, _now: std::time::Instant) -> bool {
        let result = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .operation_result
            .take();
        match result {
            Some(Ok(())) => {
                self.input = None;
                self.input_saved_id = None;
                self.input_error = None;
                if !self.complete {
                    self.request_selected_details();
                }
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .center
                    .saved = false;
                true
            }
            Some(Err(error)) => {
                self.input_error = Some(format!("Could not start workflow: {error}"));
                true
            }
            None => false,
        }
    }
    fn handle_key_event(&mut self, mut key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.input.is_some() && !matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            if let Some(input) = &mut self.input {
                input.input(key);
            }
            return;
        }
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .center
            .help
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match self.keymap.action_for(key) {
                Some(crate::keymap::ListAction::Cancel) => state.center.help = false,
                Some(crate::keymap::ListAction::MoveDown) => {
                    state.center.help_scroll = state.center.help_scroll.saturating_add(1)
                }
                Some(crate::keymap::ListAction::MoveUp) => {
                    state.center.help_scroll = state.center.help_scroll.saturating_sub(1)
                }
                Some(crate::keymap::ListAction::PageDown) => {
                    state.center.help_scroll = state.center.help_scroll.saturating_add(10)
                }
                Some(crate::keymap::ListAction::PageUp) => {
                    state.center.help_scroll = state.center.help_scroll.saturating_sub(10)
                }
                _ if key.code == KeyCode::Char('?') => state.center.help = false,
                _ => {}
            }
            return;
        }
        let owns_fixed_shortcut = matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Char(
                    '1' | '2'
                        | '3'
                        | 'l'
                        | 'g'
                        | 'f'
                        | 'R'
                        | '?'
                        | 'o'
                        | 's'
                        | 'r'
                        | 'c'
                        | 'w'
                        | 'a'
                        | 'p'
                        | 'j'
                        | 'k'
                        | '['
                        | ']'
                        | ' '
                )
        );
        if self.input.is_none()
            && !owns_fixed_shortcut
            && key.code != KeyCode::Enter
            && key.code != KeyCode::Esc
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.center.saved && state.active_detail_tab() == DetailTab::Transcript {
                let key_id = state.detail_key();
                if state
                    .center
                    .detail_viewports
                    .get_mut(&key_id)
                    .is_some_and(|viewport| {
                        viewport.handle_disclosure_key(key, &self.runtime_keymap)
                    })
                {
                    return;
                }
            }
        }
        if self.input.is_none()
            && !owns_fixed_shortcut
            && let Some(action) = self.keymap.action_for(key)
        {
            use crate::keymap::ListAction;
            key.code = match action {
                ListAction::MoveUp => KeyCode::Up,
                ListAction::MoveDown => KeyCode::Down,
                ListAction::MoveLeft => KeyCode::Left,
                ListAction::MoveRight => KeyCode::Right,
                ListAction::PageUp => KeyCode::PageUp,
                ListAction::PageDown => KeyCode::PageDown,
                ListAction::Accept => KeyCode::Enter,
                ListAction::Cancel => KeyCode::Esc,
                ListAction::JumpTop => KeyCode::Home,
                ListAction::JumpBottom => KeyCode::End,
            };
            key.modifiers = KeyModifiers::NONE;
        }
        if !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                if self.input.take().is_none() {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.center.narrow && state.center.details_focused {
                        state.center.details_focused = false;
                    } else if state.center.saved {
                        state.center.saved = false;
                    } else {
                        self.complete = true;
                    }
                }
                self.input_saved_id = None;
                self.input_error = None;
            }
            KeyCode::Enter => self.accept(),
            KeyCode::Tab | KeyCode::BackTab => {
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.cycle_filter(
                        key.code == KeyCode::Tab && key.modifiers != KeyModifiers::SHIFT,
                    );
                }
                self.request_selected_details();
            }
            KeyCode::Down => self.navigate(/*down*/ true),
            KeyCode::Up => self.navigate(/*down*/ false),
            KeyCode::Home | KeyCode::End => {
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let last = key.code == KeyCode::End;
                    if state.center.saved {
                        let indices = state.visible_saved_indices();
                        let selected = if last {
                            indices.last()
                        } else {
                            indices.first()
                        };
                        state.center.saved_id =
                            selected.map(|index| state.saved[*index].id.clone());
                    } else {
                        let nodes = state.nodes();
                        if let Some(node) = if last { nodes.last() } else { nodes.first() } {
                            state.selected = node.row;
                            state.child = node.child;
                        }
                    }
                }
                self.request_selected_details();
            }
            KeyCode::PageDown | KeyCode::PageUp | KeyCode::Char('j' | 'k') => {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let scroll_key = state.detail_key();
                let down = matches!(key.code, KeyCode::PageDown | KeyCode::Char('k'));
                let boundary = matches!(key.code, KeyCode::PageDown | KeyCode::PageUp);
                let height = state.center.detail_page_height;
                if let Some(viewport) = state.center.detail_viewports.get_mut(&scroll_key) {
                    if boundary {
                        viewport.jump_to_boundary(down);
                    } else {
                        viewport.scroll(down, height);
                    }
                }
            }
            KeyCode::Left | KeyCode::Right => {
                let tab = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .active_detail_tab()
                    .step(key.code == KeyCode::Right);
                self.set_detail_tab(tab);
            }
            KeyCode::Char('o') => self.open_chat(),
            KeyCode::Char(' ') if key.kind == KeyEventKind::Press => self.open_actions(),
            KeyCode::Char('1' | '2' | '3') => {
                self.set_detail_tab(match key.code {
                    KeyCode::Char('2') => DetailTab::Transcript,
                    KeyCode::Char('3') => DetailTab::Source,
                    _ => DetailTab::Overview,
                });
            }
            KeyCode::Char('l') => {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.center.saved = !state.center.saved;
                state.center.details_focused = false;
                state.reconcile_saved_selection();
                drop(state);
                self.request_selected_details();
            }
            KeyCode::Char('?') => {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .center
                    .help = true
            }
            KeyCode::Char('f') => self.send(WorkflowEvent::ChooseScope),
            KeyCode::Char('g') => {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.center.grouping = state.center.grouping.next();
            }
            KeyCode::Char('a') => {
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.center.saved {
                        return;
                    }
                    state.center.show_attempt_history = !state.center.show_attempt_history;
                    state.reconcile_attempt_history_selection();
                }
                self.request_selected_details();
            }
            KeyCode::Char('p') => {
                self.request_transcript(/*older*/ true, /*refresh*/ true)
            }
            KeyCode::Char('R') => {
                self.send(WorkflowEvent::Refresh);
                self.request_transcript(/*older*/ false, /*refresh*/ true);
                self.request_saved_source(/*refresh*/ true);
                self.send(WorkflowEvent::RefreshSavedCatalog);
            }
            KeyCode::Char('s') => self.selected_action(WorkflowAction::Stop),
            KeyCode::Char('r') => self.selected_action(WorkflowAction::Retry),
            KeyCode::Char('c') => self.selected_action(WorkflowAction::Recover),
            KeyCode::Char('w') => self.selected_action(WorkflowAction::Save),
            KeyCode::Char('[' | ']') => {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                self.send(if key.code == KeyCode::Char(']') {
                    WorkflowEvent::NextPage {
                        saved: state.center.saved,
                    }
                } else {
                    WorkflowEvent::PreviousPage {
                        saved: state.center.saved,
                    }
                });
            }
            _ => {}
        }
    }
}
