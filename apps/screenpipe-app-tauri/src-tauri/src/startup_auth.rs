// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Local application startup has no Screenpipe account or team dependency.
//! First-run capture consent, OS permissions and recorder integrity checks are
//! handled by onboarding and the capture lifecycle, independently of identity.

use serde::Serialize;
use specta::Type;
use std::sync::Mutex;
use tauri::Manager;

/// Native startup may wait for account data that a webview refreshes before
/// rendering its gate. Remember that start independently of the gate's UI.
#[derive(Default)]
pub(crate) struct DeferredAccountStart(Mutex<bool>);

impl DeferredAccountStart {
    pub(crate) fn defer(&self) {
        *self.0.lock().expect("deferred account start lock poisoned") = true;
    }

    pub(crate) fn cancel(&self) {
        *self.0.lock().expect("deferred account start lock poisoned") = false;
    }

    pub(crate) fn take_if_allowed(&self, access_allowed: bool, start: impl FnOnce()) -> bool {
        let mut pending = self.0.lock().expect("deferred account start lock poisoned");
        if !access_allowed || !*pending {
            return false;
        }
        *pending = false;
        // Publish capture intent while cancellation is excluded. A concurrent
        // explicit stop must clear intent after this, never be overwritten.
        start();
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuthenticationStatus {
    Authenticated,
    LoggedOut,
    NotRequired,
}

pub(crate) fn bootstrap(app: &tauri::AppHandle, _settings: &mut crate::store::SettingsStore) -> AuthenticationStatus {
    let status = AuthenticationStatus::NotRequired;
    std::env::set_var("SCREENPIPE_STARTUP_AUTHENTICATION_STATUS", "not_required");
    app.manage(status);
    status
}
