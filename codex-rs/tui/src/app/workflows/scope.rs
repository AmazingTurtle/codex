//! Explicit scope selection keeps the current conversation default and ownership boundary visible.
use super::*;
use crate::workflows::WorkflowListScope;

impl App {
    pub(in crate::app) fn choose_workflow_scope(&mut self) {
        let selected = self
            .workflows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scope;
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Workflow scope".into()),
            items: [
                (WorkflowListScope::Current, "Current conversation"),
                (WorkflowListScope::Project, "Current project"),
                (WorkflowListScope::All, "All conversations"),
            ]
            .into_iter()
            .map(|(scope, label)| SelectionItem {
                name: label.into(),
                is_current: selected == scope,
                dismiss_on_select: true,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::Workflow(WorkflowEvent::SetScope(scope)))
                })],
                ..Default::default()
            })
            .collect(),
            ..SelectionViewParams::picker()
        });
    }
}
