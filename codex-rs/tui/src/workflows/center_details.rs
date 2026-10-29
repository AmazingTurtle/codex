//! Detail selection and on-demand reads preserve separate run and saved-library views.
use super::*;

impl WorkflowCenter {
    pub(super) fn request_selected_details(&self) {
        self.request_transcript(/*older*/ false, /*refresh*/ false);
        self.request_saved_source(/*refresh*/ false);
    }

    pub(super) fn request_transcript(&self, older: bool, refresh: bool) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.center.detail_tab == DetailTab::Transcript
            && !state.center.saved
            && let Some(target) = state.selected_transcript_target()
            && (refresh
                || state
                    .transcripts
                    .get(&target.cache_key())
                    .is_none_or(|transcript| transcript.dirty))
        {
            self.send(WorkflowEvent::ReadTranscript { target, older });
        }
    }

    pub(super) fn request_saved_source(&self, refresh: bool) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.center.saved
            && state.center.saved_detail_tab == DetailTab::Source
            && state
                .visible_saved_indices()
                .contains(&state.selected_saved_index())
            && let Some(saved) = state.saved.get(state.selected_saved_index())
            && (refresh || !state.saved_sources.contains_key(&saved.id))
        {
            self.send(WorkflowEvent::ReadSavedSource {
                id: saved.id.clone(),
                refresh,
            });
        }
    }

    pub(super) fn set_detail_tab(&self, tab: DetailTab) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.center.saved {
                state.center.saved_detail_tab = tab;
            } else {
                state.center.detail_tab = tab;
            }
            if state.center.narrow {
                state.center.details_focused = true;
            }
        }
        self.request_selected_details();
    }
}
