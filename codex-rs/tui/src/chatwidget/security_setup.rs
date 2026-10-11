//! Presents the optional setup reminder without displacing account usage controls.
//! Temporary dismissal survives banner replacement and same-account notifications.
//! Permanent dismissal is stored per verified account/user in the local host config.
use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::bottom_pane::ActionableBanner;
use crate::bottom_pane::SelectionItem;
use crate::legacy_core::config::edit::ConfigEdit;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::security_setup::Identity;
use crate::security_setup::Notice;

impl ChatWidget {
    pub(crate) fn inherit_security_setup(&mut self, previous: &mut Self) {
        // Startup and thread navigation replace the widget while the request is in flight.
        // Preserve the occurrence, including its rendered/dismissed state.
        self.security_setup_request_id = previous.security_setup_request_id;
        self.security_setup_identity = previous.security_setup_identity.clone();
        self.security_setup_dismissed = previous.security_setup_dismissed
            || (previous.security_setup_presented
                && previous.bottom_pane.inline_banner_lifecycle().1);
        if previous.security_setup_presented && !self.has_applicable_backend_banner() {
            self.bottom_pane
                .transfer_inline_banner_from(&mut previous.bottom_pane);
            self.security_setup_presented = true;
            previous.security_setup_presented = false;
        }
    }

    pub(crate) fn invalidate_security_setup(&mut self) {
        // AccountUpdated can announce the same account after reconnect. Hide stale content
        // and reject pending results, but retain dismissal until we verify the identity.
        self.security_setup_request_id = uuid::Uuid::new_v4();
        self.clear_security_setup_banner();
    }

    pub(super) fn clear_security_setup_banner(&mut self) {
        // Capture this notice's dismissal before the shared slot is cleared or replaced.
        // Once another banner owns the slot, its lifecycle must not affect this notice.
        if self.security_setup_presented {
            self.security_setup_dismissed |= self.bottom_pane.inline_banner_lifecycle().1;
            self.bottom_pane.set_inline_banner(/*banner*/ None);
            self.security_setup_presented = false;
        }
    }

    pub(crate) fn show_security_setup(&mut self, identity: Identity, notice: Notice) {
        if !notice.valid() {
            return;
        }
        if self.security_setup_identity.as_ref() != Some(&identity) {
            self.invalidate_security_setup();
            self.security_setup_identity = Some(identity.clone());
            self.security_setup_dismissed = false;
        }
        // Reconnect fetches must preserve the current occurrence, including Esc dismissal.
        if self.security_setup_presented
            || self.security_setup_dismissed
            || identity.dismissed_in(&self.local_settings.notices)
        {
            return;
        }
        tracing::debug!(
            has_backend_banner = self.has_applicable_backend_banner(),
            "presenting security setup notice"
        );
        if self.has_applicable_backend_banner() {
            return;
        }
        self.security_setup_presented = true;
        let request_id = self.security_setup_request_id;
        self.bottom_pane.set_inline_banner(Some(ActionableBanner {
            title: notice.title,
            description: notice.description,
            actions: vec![
                SelectionItem {
                    name: notice.action.label,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::OpenUrlInBrowser {
                            url: notice.action.url.clone(),
                        })
                    })],
                    ..Default::default()
                },
                SelectionItem {
                    name: "Don't show again".into(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::DismissSecuritySetupPermanently {
                            request_id,
                            identity: identity.clone(),
                        });
                    })],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }));
    }

    pub(crate) async fn dismiss_security_setup_permanently(
        &mut self,
        request_id: uuid::Uuid,
        identity: Identity,
    ) -> bool {
        if request_id != self.security_setup_request_id
            || self.security_setup_identity.as_ref() != Some(&identity)
            || !self.security_setup_presented
        {
            return false;
        }
        let key = identity.dismissal_key();
        let result =
            ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
                .with_edits([ConfigEdit::SetPath {
                    segments: vec![
                        "notice".into(),
                        "security_setup_dismissed".into(),
                        key.clone(),
                    ],
                    value: toml_edit::value(true),
                }])
                .apply()
                .await;
        if let Err(err) = result {
            tracing::error!(error = %err, "failed to save security reminder preference");
            self.add_error_message(
                "Couldn't save your security reminder preference. Try again after checking that your config file is writable and valid.".into(),
            );
            return false;
        }
        self.local_settings
            .notices
            .security_setup_dismissed
            .insert(key, true);
        self.clear_security_setup_banner();
        self.security_setup_dismissed = true;
        true
    }
}
