//! Saving reuses the retained name editor and standard destination picker.
use super::*;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use codex_app_server_protocol::WorkflowScope;

impl App {
    pub(in crate::app) fn open_save_workflow(&mut self, owner: ThreadId, id: String) {
        let initial = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.title.clone())
            .unwrap_or_else(|| id.clone());
        let initial = initial
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '-'
                }
            })
            .take(80)
            .collect::<String>();
        self.edit_workflow_name(owner, id, initial, /*context*/ None);
    }

    pub(in crate::app) fn edit_workflow_name(
        &mut self,
        owner: ThreadId,
        id: String,
        initial: String,
        context: Option<String>,
    ) {
        let tx = self.app_event_tx.clone();
        let mut view = CustomPromptView::new(
            "Save workflow".into(),
            "Workflow name".into(),
            initial,
            context.or_else(|| Some("1–80 letters, digits, hyphens or underscores".into())),
            Box::new(move |name| {
                tx.send(AppEvent::Workflow(WorkflowEvent::ChooseSaveScope {
                    owner,
                    id: id.clone(),
                    name,
                }))
            }),
        )
        .with_child_flow("save")
        .with_validation(Box::new(|name| {
            if name.is_empty()
                || name.len() > 80
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                Err("Use 1–80 letters, digits, hyphens or underscores".into())
            } else {
                Ok(())
            }
        }));
        view.set_keymap_bindings(&self.keymap);
        self.chat_widget.show_bottom_pane_view(Box::new(view));
    }

    pub(in crate::app) fn choose_workflow_save_scope(
        &mut self,
        owner: ThreadId,
        id: String,
        name: String,
    ) {
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some(format!("Save “{name}”")),
            items: [
                (
                    WorkflowScope::Project,
                    "This project",
                    "Available in this project",
                ),
                (
                    WorkflowScope::User,
                    "All projects",
                    "Available in your user workflow library",
                ),
            ]
            .into_iter()
            .map(|(scope, title, description)| {
                let id = id.clone();
                let name = name.clone();
                SelectionItem {
                    name: title.into(),
                    description: Some(description.into()),
                    dismiss_on_select: true,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::Workflow(WorkflowEvent::SaveNamed {
                            owner,
                            id: id.clone(),
                            name: name.clone(),
                            scope,
                        }))
                    })],
                    ..Default::default()
                }
            })
            .collect(),
            ..SelectionViewParams::picker()
        });
    }
}
