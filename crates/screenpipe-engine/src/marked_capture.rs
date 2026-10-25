// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Rare owner-requested captures join the existing monitor worker. The worker's
//! durable ACK is separate from the lossy UI-event linker and from image release.

use crate::{focus_aware_controller::FocusAwareController, server::AppState};
use axum::{
    body::to_bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use screenpipe_db::{
    DatabaseManager, ExplicitMemoryError, MarkedCaptureOutcome, MarkedCaptureRequest,
};
use screenpipe_screen::monitor::SafeMonitor;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Cursor,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, Notify};
use uuid::Uuid;

type ApiError = (StatusCode, Json<Value>);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(28);
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
static REQUEST_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
type WorkerKey = (PathBuf, u32);
static WORKERS: OnceLock<Mutex<HashMap<WorkerKey, Worker>>> = OnceLock::new();

#[derive(Clone)]
struct Worker {
    tx: mpsc::Sender<CaptureJob>,
    notify: Arc<Notify>,
    monitor: Arc<SafeMonitor>,
    focus: Arc<FocusAwareController>,
}

pub(crate) struct CaptureJob {
    pub request_id: String,
    pub deadline: Instant,
    completion: Option<oneshot::Sender<Result<CapturedFrame, &'static str>>>,
}

pub(crate) struct CapturedFrame {
    pub frame_id: i64,
    pub timestamp: String,
    pub app_name: Option<String>,
}

impl CaptureJob {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline || self.completion.as_ref().is_none_or(|tx| tx.is_closed())
    }

    pub fn complete(mut self, frame: CapturedFrame) {
        if let Some(tx) = self.completion.take() {
            let _ = tx.send(Ok(frame));
        }
    }

    pub fn reject(mut self, code: &'static str) {
        if let Some(tx) = self.completion.take() {
            let _ = tx.send(Err(code));
        }
    }
}

impl Drop for CaptureJob {
    fn drop(&mut self) {
        if let Some(tx) = self.completion.take() {
            let _ = tx.send(Err("capture_interrupted"));
        }
    }
}

pub(crate) struct CaptureRegistration {
    key: WorkerKey,
    tx: mpsc::Sender<CaptureJob>,
}

impl Drop for CaptureRegistration {
    fn drop(&mut self) {
        if let Ok(mut workers) = WORKERS.get_or_init(Mutex::default).lock() {
            if workers
                .get(&self.key)
                .is_some_and(|worker| worker.tx.same_channel(&self.tx))
            {
                workers.remove(&self.key);
            }
        }
    }
}

pub(crate) fn register(
    db: &Arc<DatabaseManager>,
    monitor_id: u32,
    monitor: Arc<SafeMonitor>,
    focus: Arc<FocusAwareController>,
) -> (CaptureRegistration, mpsc::Receiver<CaptureJob>, Arc<Notify>) {
    let key = (
        db.pool.connect_options().get_filename().to_owned(),
        monitor_id,
    );
    let (tx, rx) = mpsc::channel(2);
    let notify = Arc::new(Notify::new());
    WORKERS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            key.clone(),
            Worker {
                tx: tx.clone(),
                notify: notify.clone(),
                monitor,
                focus,
            },
        );
    (CaptureRegistration { key, tx }, rx, notify)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CaptureRequest {
    request_id: String,
    monitor_id: Option<u32>,
}

fn failure(status: StatusCode, code: &str) -> ApiError {
    (status, Json(json!({"code":code,"error":code})))
}

fn storage_error(error: ExplicitMemoryError) -> ApiError {
    match error {
        ExplicitMemoryError::Conflict => failure(StatusCode::CONFLICT, "capture_request_conflict"),
        ExplicitMemoryError::NotFound => {
            failure(StatusCode::NOT_FOUND, "capture_request_not_found")
        }
        ExplicitMemoryError::PrivacyChanged => {
            failure(StatusCode::SERVICE_UNAVAILABLE, "privacy_pending")
        }
        ExplicitMemoryError::Invalid(message) => failure(StatusCode::BAD_REQUEST, &message),
        _ => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "capture_storage_unavailable",
        ),
    }
}

fn request_id(value: &str) -> Result<String, ApiError> {
    Uuid::parse_str(value)
        .map(|id| id.to_string())
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "invalid_request_id"))
}

fn worker_for(db: &Arc<DatabaseManager>, monitor_id: Option<u32>) -> Option<(u32, Worker)> {
    let workers = WORKERS.get_or_init(Mutex::default).lock().ok()?;
    let identity = db.pool.connect_options().get_filename().to_owned();
    if let Some(id) = monitor_id {
        return workers
            .get(&(identity, id))
            .cloned()
            .map(|worker| (id, worker));
    }
    // Unknown focus must not select an arbitrary display.
    let mut focused = workers.iter().filter(|((key, _), worker)| {
        key == &identity && worker.focus.hosts_focus_for_monitor(&worker.monitor)
    });
    let ((_, id), worker) = focused.next()?;
    if focused.next().is_some() {
        return None;
    }
    Some((*id, worker.clone()))
}

pub(crate) async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CaptureRequest>,
) -> Result<(StatusCode, Json<MarkedCaptureRequest>), ApiError> {
    crate::routes::explicit_memory::require_owner(&state, &headers)?;
    let id = request_id(&body.request_id)?;
    // Reserve also checks selector equality on retries. Resolve focus only for a
    // new UUID; replay can never capture a second moment or switch displays.
    if let Some(existing) = state
        .db
        .get_marked_capture(&id)
        .await
        .map_err(storage_error)?
    {
        let (existing, _) = state
            .db
            .reserve_marked_capture(&id, body.monitor_id, existing.monitor_id)
            .await
            .map_err(storage_error)?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(readable_result(&state, existing).await?),
        ));
    }
    let permit = REQUEST_SLOTS
        .try_acquire()
        .map_err(|_| failure(StatusCode::TOO_MANY_REQUESTS, "capture_busy"))?;
    let selected = worker_for(&state.db, body.monitor_id);
    let monitor_id = selected
        .as_ref()
        .map(|(id, _)| *id)
        .or(body.monitor_id)
        .unwrap_or(0);
    let (request, created) = state
        .db
        .reserve_marked_capture(&id, body.monitor_id, monitor_id)
        .await
        .map_err(storage_error)?;
    if !created {
        return Ok((
            StatusCode::ACCEPTED,
            Json(readable_result(&state, request).await?),
        ));
    }
    let Some((_, worker)) = selected else {
        let result = state
            .db
            .finish_marked_capture(&id, &terminal("blocked", "capture_unavailable"))
            .await
            .map_err(storage_error)?;
        return Ok((StatusCode::ACCEPTED, Json(result)));
    };
    let (tx, rx) = oneshot::channel();
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let job = CaptureJob {
        request_id: id.clone(),
        deadline,
        completion: Some(tx),
    };
    if worker.tx.try_send(job).is_err() {
        let result = state
            .db
            .finish_marked_capture(&id, &terminal("blocked", "capture_busy"))
            .await
            .map_err(storage_error)?;
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    worker.notify.notify_one();
    tokio::spawn(async move {
        let _permit = permit;
        let outcome =
            match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), rx).await {
                Ok(Ok(Ok(frame))) => tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    await_release(&state, frame, deadline),
                )
                .await
                .unwrap_or_else(|_| terminal("blocked", "privacy_or_image_unavailable")),
                Ok(Ok(Err(code))) => terminal(
                    match code {
                        "capture_failed" | "capture_timeout" => "failed",
                        "capture_interrupted" => "interrupted",
                        _ => "blocked",
                    },
                    code,
                ),
                Ok(Err(_)) => terminal("interrupted", "capture_interrupted"),
                Err(_) => terminal("failed", "capture_timeout"),
            };
        if let Err(error) = state.db.finish_marked_capture(&id, &outcome).await {
            if matches!(error, ExplicitMemoryError::PrivacyChanged) {
                let _ = state
                    .db
                    .finish_marked_capture(&id, &terminal("blocked", "privacy_pending"))
                    .await;
            }
            tracing::warn!(%error, "intentional capture completion could not be journaled");
        }
    });
    Ok((StatusCode::ACCEPTED, Json(request)))
}

pub(crate) async fn get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<MarkedCaptureRequest>, ApiError> {
    crate::routes::explicit_memory::require_owner(&state, &headers)?;
    let id = request_id(&id)?;
    let request = state
        .db
        .get_marked_capture(&id)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "capture_request_not_found"))?;
    Ok(Json(readable_result(&state, request).await?))
}

fn terminal(state: &str, code: &str) -> MarkedCaptureOutcome {
    MarkedCaptureOutcome {
        state: state.to_owned(),
        code: Some(code.to_owned()),
        frame_id: None,
        timestamp: None,
        app_name: None,
        image_width: None,
        image_height: None,
        revocation: None,
    }
}

async fn readable_result(
    state: &Arc<AppState>,
    mut request: MarkedCaptureRequest,
) -> Result<MarkedCaptureRequest, ApiError> {
    if request.state == "completed" {
        let id = request
            .frame_id
            .ok_or_else(|| failure(StatusCode::GONE, "source_unavailable"))?;
        crate::routes::frames::require_frame_history_access(state, id).await?;
        crate::routes::frames::require_image_release(state, id).await?;
        let revocation = state
            .db
            .release_revocation()
            .await
            .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "privacy_pending"))?;
        let readable = match crate::routes::frames::get_frame_data(
            State(state.clone()),
            Path(id),
            Query(crate::routes::frames::FrameDataQuery { fallback: false }),
        )
        .await
        {
            Ok(response) => match to_bytes(response.into_body(), MAX_IMAGE_BYTES).await {
                Ok(bytes) => image::ImageReader::new(Cursor::new(bytes))
                    .with_guessed_format()
                    .ok()
                    .and_then(|reader| reader.into_dimensions().ok())
                    .is_some_and(|(width, height)| width > 1 && height > 1),
                Err(_) => false,
            },
            Err((status, body))
                if status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::FORBIDDEN =>
            {
                return Err((status, body))
            }
            Err(_) => false,
        };
        if revocation
            != state
                .db
                .release_revocation()
                .await
                .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "privacy_pending"))?
        {
            return Err(failure(StatusCode::SERVICE_UNAVAILABLE, "privacy_pending"));
        }
        if !readable {
            request.state = "blocked".into();
            request.code = Some("source_unavailable".into());
            request.frame_id = None;
            request.app_name = None;
        }
    }
    Ok(request)
}

async fn await_release(
    state: &Arc<AppState>,
    frame: CapturedFrame,
    deadline: Instant,
) -> MarkedCaptureOutcome {
    loop {
        if Instant::now() >= deadline {
            return terminal("blocked", "privacy_or_image_unavailable");
        }
        if screenpipe_core::background_work::is_suspended()
            || crate::sleep_monitor::screen_is_locked()
            || crate::drm_detector::drm_content_paused()
            || crate::schedule_monitor::schedule_paused()
        {
            return terminal("blocked", "capture_paused");
        }
        if state.db.protected_reads_enabled() {
            let policy = match state.db.release_policy().await {
                Ok(policy) => policy,
                Err(_) => return terminal("blocked", "privacy_pending"),
            };
            if policy.text
                && !state
                    .db
                    .release_ready("frames", &frame.frame_id.to_string())
                    .await
                    .unwrap_or(false)
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        }
        let revocation = match state.db.release_revocation().await {
            Ok(revocation) => revocation,
            Err(_) => return terminal("blocked", "privacy_pending"),
        };
        let response = crate::routes::frames::get_frame_data(
            State(state.clone()),
            Path(frame.frame_id),
            Query(crate::routes::frames::FrameDataQuery { fallback: false }),
        )
        .await;
        match response {
            Ok(response) => {
                let bytes = match to_bytes(response.into_body(), MAX_IMAGE_BYTES).await {
                    Ok(bytes) => bytes,
                    Err(_) => return terminal("failed", "capture_image_too_large"),
                };
                let geometry = image::ImageReader::new(Cursor::new(bytes))
                    .with_guessed_format()
                    .ok()
                    .and_then(|reader| reader.into_dimensions().ok());
                match state.db.release_revocation().await {
                    Ok(current) if current == revocation => {}
                    Ok(_) => continue,
                    Err(_) => return terminal("blocked", "privacy_pending"),
                }
                let Some((width, height)) =
                    geometry.filter(|(width, height)| *width > 1 && *height > 1)
                else {
                    return terminal("failed", "capture_image_unavailable");
                };
                return MarkedCaptureOutcome {
                    state: "completed".into(),
                    code: None,
                    frame_id: Some(frame.frame_id),
                    timestamp: Some(frame.timestamp),
                    app_name: frame.app_name,
                    image_width: Some(width),
                    image_height: Some(height),
                    revocation,
                };
            }
            Err((status, _)) if status == StatusCode::SERVICE_UNAVAILABLE => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err((status, _)) => {
                return terminal(
                    "blocked",
                    if status == StatusCode::FORBIDDEN {
                        "capture_privacy_blocked"
                    } else {
                        "source_unavailable"
                    },
                )
            }
        }
    }
}
