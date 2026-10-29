//! Native workflow action menus, revision previews, and retained multiline forms.
pub(crate) const ACTIONS_VIEW_ID: &str = "workflow-actions";
pub(crate) const REVISION_PREVIEW_VIEW_ID: &str = "workflow-revision-preview";
pub(crate) const INSPECTION_ID: &str = "workflow-inspection";

mod context;
mod forms;
mod inspection;
mod menu;
mod model;
mod revisions;

pub(crate) use context::context;
pub(crate) use forms::form;
pub(crate) use forms::form_with_error;
pub(crate) use forms::revision_arguments;
pub(crate) use inspection::InspectionView;
pub(crate) use menu::menu;
pub(crate) use model::*;
pub(crate) use revisions::history;
pub(crate) use revisions::preview;

#[cfg(test)]
#[path = "actions_tests.rs"]
mod tests;
