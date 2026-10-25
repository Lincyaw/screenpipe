// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! This personal fork has local capabilities, not account or organization modes.
//! Local owner API authentication and OS/privacy protections remain independent.

use crate::store::SettingsStore;

pub(crate) fn is_local_only() -> bool { true }
pub(crate) fn deployment_or_session_is_managed() -> bool { false }
pub(crate) fn for_settings(_settings: &SettingsStore) -> bool { false }
pub(crate) fn is_managed(_app: &tauri::AppHandle) -> bool { false }
