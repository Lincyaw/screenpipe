// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! User-invoked screenshots: persist through the engine before showing any UI.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

use crate::window::GatedWindowPlacement;

const LABEL: &str = "annotation-capture";

#[derive(Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationCapture {
    request_id: String,
    frame_id: i64,
    timestamp: String,
    app: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct CaptureResponse {
    request_id: String,
    state: String,
    code: Option<String>,
    frame_id: Option<i64>,
    timestamp: Option<String>,
    app_name: Option<String>,
    image_width: Option<u32>,
    image_height: Option<u32>,
    monitor_id: Option<u32>,
}

#[derive(Clone, Copy)]
struct Target {
    monitor_id: u32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    origin_pid: i32,
}

struct Session {
    request_id: String,
    target: Target,
    capture: Option<AnnotationCapture>,
    original_window_class: Option<usize>,
}

static SESSION: Lazy<Mutex<Option<Session>>> = Lazy::new(|| Mutex::new(None));

/// Read the display under the pointer without activating Screenpipe. AppKit
/// coordinates stay in points until we place the native window on that display.
#[cfg(target_os = "macos")]
fn target(app: &tauri::AppHandle) -> Result<Target, String> {
    use objc::{class, msg_send, sel, sel_impl};
    use tauri_nspanel::cocoa::{
        appkit::{NSEvent, NSScreen},
        base::{id, nil},
        foundation::{NSArray, NSString},
    };
    let _ = app;
    crate::window::with_autorelease_pool(|| unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let frontmost: id = msg_send![workspace, frontmostApplication];
        let origin_pid = if frontmost == nil {
            0
        } else {
            msg_send![frontmost, processIdentifier]
        };
        let pointer = NSEvent::mouseLocation(nil);
        let screens = NSScreen::screens(nil);
        let key = NSString::alloc(nil).init_str("NSScreenNumber");
        let mut result = Err("No display is available under the pointer.".into());
        for index in 0..NSArray::count(screens) {
            let screen: id = NSArray::objectAtIndex(screens, index);
            let frame = NSScreen::frame(screen);
            if pointer.x < frame.origin.x
                || pointer.x >= frame.origin.x + frame.size.width
                || pointer.y < frame.origin.y
                || pointer.y >= frame.origin.y + frame.size.height
            {
                continue;
            }
            let description: id = msg_send![screen, deviceDescription];
            let number: id = msg_send![description, objectForKey: key];
            if number != nil {
                let monitor_id: u32 = msg_send![number, unsignedIntValue];
                result = Ok(Target {
                    monitor_id,
                    x: frame.origin.x,
                    y: frame.origin.y,
                    width: frame.size.width,
                    height: frame.size.height,
                    origin_pid,
                });
            }
            break;
        }
        let _: () = msg_send![key, release];
        result
    })
}

#[cfg(not(target_os = "macos"))]
fn target(_app: &tauri::AppHandle) -> Result<Target, String> {
    // Display IDs must be resolved against the capture backend, never guessed
    // from a Tauri monitor index or an unrelated display name.
    Err("Capture and annotate is currently available on macOS.".into())
}

fn notify_failure(message: &str) {
    crate::notifications::client::send("Could not capture for annotation", message);
}

fn failure_message(code: &str) -> &'static str {
    match code {
        "capture_paused" | "monitor_paused" | "vision_disabled" | "screenshots_disabled" =>
            "Screen recording is paused or disabled. Resume it before capturing an annotation.",
        "excluded" | "target_excluded" | "privacy_blocked" | "capture_privacy_blocked" | "protected_content" =>
            "The current screen cannot be captured under your privacy settings.",
        "permission_denied" => "Allow screen recording in macOS settings, then try again.",
        "privacy_pending" | "privacy_not_ready" | "privacy_or_image_unavailable" =>
            "The new screenshot is still being protected. Try again after privacy processing finishes.",
        "timeout" | "capture_timeout" => "The screenshot did not finish in time. Try the shortcut again.",
        "capture_busy" => "Another screenshot is being saved. Try the shortcut again shortly.",
        _ => "A fresh screenshot could not be saved. Check that screen recording is running, then try again.",
    }
}

async fn request_capture(
    app: &tauri::AppHandle,
    request_id: &str,
    target: Target,
) -> Result<AnnotationCapture, String> {
    let api = crate::recording::local_api_context_from_app(app);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|_| "unavailable".to_string())?;
    let response = api
        .apply_auth(client.post(api.url("/capture/marked")))
        .json(&serde_json::json!({ "request_id": request_id, "monitor_id": target.monitor_id }))
        .send()
        .await
        .map_err(|_| "unavailable".to_string())?;
    if !response.status().is_success() {
        return Err(response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body["code"].as_str().map(str::to_string))
            .unwrap_or_else(|| "unavailable".into()));
    }
    let mut result: CaptureResponse = response
        .json()
        .await
        .map_err(|_| "invalid_response".to_string())?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if result.request_id != request_id {
            return Err("invalid_response".into());
        }
        match result.state.as_str() {
            "completed" => {
                let frame_id = result
                    .frame_id
                    .filter(|id| *id > 0 && *id <= 9_007_199_254_740_991)
                    .ok_or("invalid_frame")?;
                let width = result
                    .image_width
                    .filter(|v| *v > 0)
                    .ok_or("invalid_geometry")?;
                let height = result
                    .image_height
                    .filter(|v| *v > 0)
                    .ok_or("invalid_geometry")?;
                if result.monitor_id != Some(target.monitor_id) {
                    return Err("invalid_monitor".into());
                }
                let timestamp = result.timestamp.ok_or("invalid_timestamp")?;
                return Ok(AnnotationCapture {
                    request_id: request_id.into(),
                    frame_id,
                    timestamp,
                    app: result.app_name.unwrap_or_default(),
                    width,
                    height,
                });
            }
            "pending" => {}
            _ => return Err(result.code.unwrap_or(result.state)),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timeout".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let response = api
            .apply_auth(client.get(api.url(&format!("/capture/marked/{request_id}"))))
            .send()
            .await
            .map_err(|_| "unavailable".to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let code = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|body| body["code"].as_str().map(str::to_string))
                .unwrap_or_else(|| "unavailable".into());
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && code == "privacy_pending" {
                continue;
            }
            return Err(code);
        }
        result = response
            .json()
            .await
            .map_err(|_| "invalid_response".to_string())?;
    }
}

/// Called on the UI thread directly from the global shortcut. No window, app
/// activation, or screenshot fallback precedes the durable capture response.
pub(crate) fn start(app: &tauri::AppHandle) {
    let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if session.is_some() {
        drop(session);
        if let Some(window) = app.get_webview_window(LABEL) {
            let _ = focus(&window);
        }
        return;
    }
    let target = match target(app) {
        Ok(target) => target,
        Err(error) => {
            drop(session);
            notify_failure(&error);
            return;
        }
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    tracing::info!(request_id = %request_id, monitor_id = target.monitor_id, "intentional capture requested");
    *session = Some(Session {
        request_id: request_id.clone(),
        target,
        capture: None,
        original_window_class: None,
    });
    drop(session);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = request_capture(&app, &request_id, target).await;
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
            let Some(active) = session.as_mut().filter(|s| s.request_id == request_id) else { return; };
            match result {
                Ok(capture) => {
                    active.capture = Some(capture.clone());
                    drop(session);
                    if let Err(error) = show(&handle, target, &capture) {
                        tracing::warn!("annotation editor failed to open: {error}");
                        finish(&handle, &request_id, true);
                        notify_failure("The screenshot was saved, but the annotation editor could not open. Try again.");
                    }
                }
                Err(code) => {
                    *session = None;
                    drop(session);
                    tracing::warn!("intentional capture did not complete: {code}");
                    notify_failure(failure_message(&code));
                }
            }
        });
    });
}

fn show(app: &tauri::AppHandle, target: Target, capture: &AnnotationCapture) -> Result<(), String> {
    let window =
        tauri::WebviewWindowBuilder::new(app, LABEL, tauri::WebviewUrl::App("annotate".into()))
            .title("Capture and annotate")
            .decorations(false)
            .resizable(false)
            .shadow(false)
            .skip_taskbar(true)
            .always_on_top_gated(true)
            .visible_on_all_workspaces_gated(true)
            .focused_gated(false)
            .visible(false)
            .inner_size(target.width, target.height)
            .build()
            .map(crate::window::finalize_webview_window)
            .map_err(|e| e.to_string())?;
    let handle = app.clone();
    let request_id = capture.request_id.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            finish(&handle, &request_id, false);
        }
    });
    // The editor is intentionally never a new ambient source. Protect it even
    // when ordinary Screenpipe windows are allowed in the user's recordings.
    let _ = window.set_content_protected(true);
    #[cfg(target_os = "macos")]
    unsafe {
        use objc::{msg_send, sel, sel_impl};
        use tauri_nspanel::{
            cocoa::{
                base::{id, nil},
                foundation::{NSPoint, NSRect, NSSize},
            },
            objc_foundation::INSObject,
            raw_nspanel::object_setClass,
        };
        let native = window.ns_window().map_err(|e| e.to_string())? as id;
        // Same raw class conversion as Search: to_panel() can crash on macOS26.
        let class: id = msg_send![tauri_nspanel::raw_nspanel::RawNSPanel::class(), class];
        let original = object_setClass(native, class);
        if let Some(session) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            session.original_window_class = Some(original as usize);
        }
        let style: u64 = msg_send![native, styleMask];
        let _: () = msg_send![native, setStyleMask: style | 128_u64];
        crate::window::ns::set_level_gated(native, 1002);
        crate::window::ns::set_collection_behavior_gated(native, 257);
        let _: () = msg_send![native, setHidesOnDeactivate: false];
        let _: () = msg_send![native, setBecomesKeyOnlyIfNeeded: false];
        let _: () = msg_send![native, setSharingType: 0_u64];
        let rect = NSRect::new(
            NSPoint::new(target.x, target.y),
            NSSize::new(target.width, target.height),
        );
        let _: () = msg_send![native, setFrame: rect display: true];
        let _: () = msg_send![native, orderFront: nil];
    }
    #[cfg(not(target_os = "macos"))]
    window.show().map_err(|e| e.to_string())?;
    focus(&window)?;
    window
        .emit("annotation-capture-ready", capture)
        .map_err(|e| e.to_string())
}

fn focus(window: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    unsafe {
        use tauri_nspanel::cocoa::base::id;
        let native = window.ns_window().map_err(|e| e.to_string())? as id;
        crate::window::ns::make_key_and_order_front_if_allowed(native);
        crate::window::make_nswindow_webview_first_responder(native);
    }
    #[cfg(not(target_os = "macos"))]
    crate::window::focus_window(window);
    Ok(())
}

#[cfg(target_os = "macos")]
fn restore_origin(pid: i32) {
    if pid <= 0 || pid == std::process::id() as i32 {
        return;
    }
    crate::window::with_autorelease_pool(|| unsafe {
        use objc::{class, msg_send, sel, sel_impl};
        use tauri_nspanel::cocoa::base::{id, nil};
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let current: id = msg_send![workspace, frontmostApplication];
        if current == nil {
            return;
        }
        let current_pid: i32 = msg_send![current, processIdentifier];
        // Do not pull focus back if the user has switched to a different app.
        if current_pid != std::process::id() as i32 {
            return;
        }
        let origin: id =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
        if origin != nil {
            let _: bool = msg_send![origin, activateWithOptions: 2_u64];
        }
    });
}

fn finish(app: &tauri::AppHandle, request_id: &str, destroy: bool) {
    let session = {
        let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if !session.as_ref().is_some_and(|s| s.request_id == request_id) {
            return;
        }
        session.take()
    };
    if destroy {
        if let Some(window) = app.get_webview_window(LABEL) {
            // AppKit must order the panel out synchronously before restoring
            // another app; a queued hide can otherwise bounce focus/Spaces.
            #[cfg(target_os = "macos")]
            if let Ok(native) = window.ns_window() {
                unsafe {
                    use objc::{msg_send, sel, sel_impl};
                    use tauri_nspanel::cocoa::base::{id, nil};
                    let _: () = msg_send![native as id, orderOut: nil];
                }
            }
            let _ = window.hide();
            #[cfg(target_os = "macos")]
            restore_window_class(
                &window,
                session.as_ref().and_then(|s| s.original_window_class),
            );
            let _ = window.destroy();
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(session) = session {
        restore_origin(session.target.origin_pid);
    }
}

/// RawNSPanel in the pinned plugin has an unsafe dealloc override. Restore the
/// original Tauri NSWindow class on every destruction path, including Cmd+W.
#[cfg(target_os = "macos")]
fn restore_window_class(window: &tauri::WebviewWindow, original: Option<usize>) {
    if let (Ok(native), Some(original)) = (window.ns_window(), original) {
        unsafe {
            use tauri_nspanel::{cocoa::base::id, raw_nspanel::object_setClass};
            object_setClass(native as id, original as id);
        }
    }
}

pub(crate) fn prepare_close(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let original = SESSION
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|session| session.original_window_class.take());
        if let Some(window) = app.get_webview_window(LABEL) {
            restore_window_class(&window, original);
        }
    }
}

/// Existing Main windows can have a global Escape grab. Give the annotation
/// editor first refusal instead of accidentally dismissing the user's Timeline.
pub(crate) fn route_escape(app: &tauri::AppHandle) -> bool {
    if let Some(window) = app
        .get_webview_window(LABEL)
        .filter(|w| w.is_visible().unwrap_or(false))
    {
        let _ = window.emit("escape-pressed", ());
        return true;
    }
    false
}

#[tauri::command]
#[specta::specta]
pub async fn get_annotation_capture(
    window: tauri::WebviewWindow,
) -> Result<Option<AnnotationCapture>, String> {
    if window.label() != LABEL {
        return Err("This command belongs to the annotation editor.".into());
    }
    Ok(SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|s| s.capture.clone()))
}

#[tauri::command]
#[specta::specta]
pub async fn close_annotation_capture(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    request_id: String,
) -> Result<(), String> {
    if window.label() != LABEL {
        return Err("This command belongs to the annotation editor.".into());
    }
    let handle = app.clone();
    app.run_on_main_thread(move || finish(&handle, &request_id, true))
        .map_err(|e| e.to_string())
}
