// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Owner-authenticated annotations on exact historical frames. Image preparation
//! happens outside the coordinated database transaction; neither a model nor the
//! capture callback is involved in saving or searching a mark.

use axum::{
    body::{to_bytes, Body},
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Json, Response},
};
use chrono::{DateTime, Utc};
use screenpipe_db::{
    ExplicitMemoryAnchor, ExplicitMemoryError, ExplicitMemoryImage, ExplicitMemoryMark,
    ExplicitMemoryQuery, NewExplicitMemoryMark, UpdateExplicitMemoryMark,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashSet, io::Cursor, sync::Arc};
use uuid::Uuid;

use super::frames::{get_frame_data, FrameDataQuery};
use crate::server::AppState;

type ApiError = (StatusCode, Json<Value>);
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
static IMAGE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const MAX_ANCHORS: usize = 32;
const MAX_TAGS: usize = 32;
const MAX_NOTE_BYTES: usize = 32_000;
const MAX_QUOTE_BYTES: usize = 8_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateMark {
    pub id: Option<String>,
    pub frame_id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_importance")]
    pub importance: f64,
    #[serde(default)]
    pub anchors: Vec<ExplicitMemoryAnchor>,
    #[serde(default)]
    pub retain_source_image: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateMark {
    pub revision: i64,
    pub title: String,
    pub note: String,
    pub tags: Vec<String>,
    pub importance: f64,
    pub anchors: Vec<ExplicitMemoryAnchor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListMarks {
    pub q: Option<String>,
    pub frame_id: Option<i64>,
    pub tag: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteMark {
    pub revision: i64,
}

fn canonical_id(value: &str) -> Result<String, ApiError> {
    Uuid::parse_str(value)
        .map(|id| id.to_string())
        .map_err(|_| invalid("Mark ID must be a UUID"))
}

fn same_creation_payload(mark: &ExplicitMemoryMark, body: &CreateMark) -> bool {
    let mut tags: Vec<_> = body.tags.iter().map(|tag| tag.trim().to_owned()).collect();
    tags.sort();
    tags.dedup();
    mark.revision == 1
        && mark.source_frame_id == body.frame_id
        && mark.title == body.title
        && mark.note == body.note
        && mark.tags == tags
        && mark.importance == body.importance
        && mark.anchors == body.anchors
        && mark.retained_image == body.retain_source_image
}

fn default_limit() -> u32 {
    50
}
fn default_importance() -> f64 {
    0.5
}

fn error(status: StatusCode, code: &'static str, message: &'static str) -> ApiError {
    (status, Json(json!({"code": code, "error": message})))
}

fn invalid(message: &'static str) -> ApiError {
    error(StatusCode::BAD_REQUEST, "invalid_mark", message)
}

/// These endpoints intentionally do not infer owner access from localhost,
/// `api_auth=false`, cookies, or pipe endpoint allowlists. Agent write capability
/// is a separate future feature. A missing configured key cannot grant access.
fn require_owner_key(configured_key: Option<&str>, headers: &HeaderMap) -> Result<(), ApiError> {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match (configured_key, supplied) {
        (Some(expected), Some(actual))
            if !expected.is_empty() && !actual.starts_with("sp_pipe_") && actual == expected =>
        {
            Ok(())
        }
        _ => Err(error(
            StatusCode::UNAUTHORIZED,
            "owner_auth_required",
            "This operation requires the local owner's API key",
        )),
    }
}

pub(crate) fn require_owner(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    require_owner_key(state.api_auth_key.as_deref(), headers)
}

fn validate_fields(
    title: &str,
    note: &str,
    tags: &[String],
    importance: f64,
    anchors: &[ExplicitMemoryAnchor],
) -> Result<(), ApiError> {
    if title.len() > 500 || note.len() > MAX_NOTE_BYTES {
        return Err(invalid("Title or note exceeds its size limit"));
    }
    if !importance.is_finite() || !(0.0..=1.0).contains(&importance) {
        return Err(invalid("Importance must be between zero and one"));
    }
    if tags.len() > MAX_TAGS
        || tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.len() > 100)
    {
        return Err(invalid("Tags must be nonempty and within size limits"));
    }
    if anchors.len() > MAX_ANCHORS {
        return Err(invalid("Too many anchors"));
    }
    let text_bytes = title.len()
        + note.len()
        + tags.iter().map(String::len).sum::<usize>()
        + anchors
            .iter()
            .map(|anchor| anchor.comment.len() + anchor.quote.as_ref().map_or(0, String::len))
            .sum::<usize>();
    if text_bytes > 128 * 1024 {
        return Err(invalid("Annotation text exceeds the aggregate size limit"));
    }
    let mut ids = HashSet::new();
    for anchor in anchors {
        if anchor.id.is_empty() || anchor.id.len() > 100 || !ids.insert(&anchor.id) {
            return Err(invalid("Anchor IDs must be unique and nonempty"));
        }
        if anchor.comment.len() > MAX_QUOTE_BYTES
            || anchor
                .quote
                .as_ref()
                .is_some_and(|text| text.len() > MAX_QUOTE_BYTES)
        {
            return Err(invalid("Anchor text exceeds its size limit"));
        }
        let has_rect = [anchor.x, anchor.y, anchor.width, anchor.height]
            .iter()
            .any(Option::is_some);
        if has_rect {
            let (Some(x), Some(y), Some(width), Some(height)) =
                (anchor.x, anchor.y, anchor.width, anchor.height)
            else {
                return Err(invalid("Anchor rectangles require all four coordinates"));
            };
            if ![x, y, width, height].iter().all(|value| value.is_finite())
                || x < 0.0
                || y < 0.0
                || width <= 0.0
                || height <= 0.0
                || x + width > 1.0
                || y + height > 1.0
            {
                return Err(invalid(
                    "Anchor rectangles must fit inside the original image",
                ));
            }
        }
        match anchor.anchor_type.as_str() {
            "region" if has_rect => {}
            "text_quote"
                if anchor
                    .quote
                    .as_ref()
                    .is_some_and(|text| !text.trim().is_empty()) => {}
            _ => {
                return Err(invalid(
                    "An anchor requires a region or a nonempty text quote",
                ))
            }
        }
    }
    if title.trim().is_empty() && note.trim().is_empty() && anchors.is_empty() {
        return Err(invalid("A mark must contain a title, note, or anchor"));
    }
    Ok(())
}

fn map_db_error(err: ExplicitMemoryError) -> ApiError {
    match err {
        ExplicitMemoryError::NotFound => error(
            StatusCode::NOT_FOUND,
            "mark_not_found",
            "The mark or source is unavailable",
        ),
        ExplicitMemoryError::Conflict => error(
            StatusCode::CONFLICT,
            "revision_conflict",
            "This mark changed in another window; reload before saving",
        ),
        ExplicitMemoryError::Invalid(_) => invalid("The annotation is invalid"),
        ExplicitMemoryError::PrivacyChanged => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "privacy_revalidation_required",
            "Saved or source evidence is unavailable under the current privacy policy",
        ),
        ExplicitMemoryError::Database(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "memory_storage_failed",
            "Annotation storage is unavailable",
        ),
    }
}

fn db_unavailable(_: sqlx::Error) -> ApiError {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "memory_storage_failed",
        "Annotation storage is unavailable",
    )
}

async fn check_revocation(state: &AppState, before: Option<i64>) -> Result<(), ApiError> {
    if state
        .db
        .release_revocation()
        .await
        .map_err(db_unavailable)?
        != before
    {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "privacy_pending",
            "Privacy policy changed; retry after processing completes",
        ));
    }
    Ok(())
}

fn require_history(state: &AppState, mark: &ExplicitMemoryMark) -> Result<(), ApiError> {
    let timestamp = DateTime::parse_from_rfc3339(&mark.source_timestamp)
        .map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "source_unavailable",
                "The source timestamp is unavailable",
            )
        })?
        .with_timezone(&Utc);
    if !state.history_access.allows(timestamp, Utc::now()) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "history_access_limited",
            "The source is outside the available history",
        ));
    }
    Ok(())
}

fn mark_json(mark: ExplicitMemoryMark) -> Result<Json<Value>, ApiError> {
    serde_json::to_value(mark).map(Json).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "memory_storage_failed",
            "The annotation could not be read",
        )
    })
}

/// Validate image dimensions without decoding an unbounded raster. The original
/// encoded bytes are retained, so the editor's coordinate space remains exact.
fn image_geometry(bytes: &[u8]) -> Result<(String, u32, u32), ApiError> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| invalid("Source image format is unavailable"))?;
    let mime = match reader.format() {
        Some(image::ImageFormat::Jpeg) => "image/jpeg",
        Some(image::ImageFormat::Png) => "image/png",
        _ => return Err(invalid("Only JPEG and PNG source images can be retained")),
    };
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| invalid("Source image dimensions are unavailable"))?;
    if width == 0
        || height == 0
        || width > 16_384
        || height > 16_384
        || u64::from(width) * u64::from(height) > 40_000_000
    {
        return Err(invalid(
            "Source image dimensions exceed the retention limit",
        ));
    }
    Ok((mime.to_owned(), width, height))
}

async fn capture_retained_image(
    state: &Arc<AppState>,
    frame_id: i64,
    revocation: Option<i64>,
) -> Result<ExplicitMemoryImage, ApiError> {
    let response = get_frame_data(
        State(state.clone()),
        Path(frame_id),
        Query(FrameDataQuery { fallback: false }),
    )
    .await?;
    let bytes = to_bytes(response.into_body(), MAX_IMAGE_BYTES)
        .await
        .map_err(|_| {
            error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "source_image_too_large",
                "The source image exceeds the retention limit",
            )
        })?;
    let bytes = bytes.to_vec();
    let (mime_type, width, height) = image_geometry(&bytes)?;
    check_revocation(state, revocation).await?;
    Ok(ExplicitMemoryImage {
        bytes,
        mime_type,
        width,
        height,
        revocation,
    })
}

/// Authored notes and retained images must be re-read after deletion or privacy
/// changes; the browser must not keep an independent HTTP cache of this surface.
pub(crate) async fn no_store(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    response
}

pub(crate) async fn create_mark(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateMark>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_owner(&state, &headers)?;
    if body.frame_id <= 0 {
        return Err(invalid("A valid source frame ID is required"));
    }
    validate_fields(
        &body.title,
        &body.note,
        &body.tags,
        body.importance,
        &body.anchors,
    )?;
    let id = match body.id.as_deref() {
        Some(id) => canonical_id(id)?,
        None => Uuid::new_v4().to_string(),
    };
    // A lost response can be retried after ordinary recording retention. The
    // original retained pixels, not a new source hydration, satisfy that retry.
    let before = state
        .db
        .release_revocation()
        .await
        .map_err(db_unavailable)?;
    if let Some(existing) = state
        .db
        .get_explicit_memory_mark(&id)
        .await
        .map_err(map_db_error)?
    {
        require_history(&state, &existing)?;
        if !same_creation_payload(&existing, &body) {
            return Err(map_db_error(ExplicitMemoryError::Conflict));
        }
        check_revocation(&state, before).await?;
        return Ok((StatusCode::CREATED, mark_json(existing)?));
    }
    let timestamp = state
        .db
        .get_frame_timestamp(body.frame_id)
        .await
        .map_err(db_unavailable)?
        .ok_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "source_unavailable",
                "The selected source frame is unavailable",
            )
        })?;
    if !state.history_access.allows(timestamp, Utc::now()) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "history_access_limited",
            "The source is outside the available history",
        ));
    }
    let revocation = state
        .db
        .release_revocation()
        .await
        .map_err(db_unavailable)?;
    // Keep the permit through the coordinated write: extracted image buffers
    // must not accumulate unboundedly while waiting behind durable capture.
    let needs_image =
        body.retain_source_image || body.anchors.iter().any(|anchor| anchor.x.is_some());
    let _image_slot = if needs_image {
        Some(IMAGE_SLOTS.try_acquire().map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "image_busy",
                "Source image preparation is busy; retry shortly",
            )
        })?)
    } else {
        None
    };
    let prepared_image = if needs_image {
        Some(capture_retained_image(&state, body.frame_id, revocation).await?)
    } else {
        None
    };
    let image_width = prepared_image.as_ref().map(|image| image.width);
    let image_height = prepared_image.as_ref().map(|image| image.height);
    let image = if body.retain_source_image {
        prepared_image
    } else {
        None
    };
    let mark = state
        .db
        .create_explicit_memory_mark(
            &NewExplicitMemoryMark {
                id,
                frame_id: body.frame_id,
                title: body.title,
                note: body.note,
                tags: body.tags,
                importance: body.importance,
                anchors: body.anchors,
                image_width,
                image_height,
                revocation,
            },
            image.as_ref(),
        )
        .await
        .map_err(map_db_error)?;
    check_revocation(&state, revocation).await?;
    Ok((StatusCode::CREATED, mark_json(mark)?))
}

pub(crate) async fn list_marks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListMarks>,
) -> Result<Json<Value>, ApiError> {
    require_owner(&state, &headers)?;
    if query.limit == 0
        || query.limit > 100
        || query.offset > 100_000
        || query.q.as_ref().is_some_and(|q| q.len() > 1_000)
        || query.tag.as_ref().is_some_and(|tag| tag.len() > 100)
        || query.frame_id.is_some_and(|frame_id| frame_id <= 0)
    {
        return Err(invalid("Search parameters exceed their limits"));
    }
    let revocation = state
        .db
        .release_revocation()
        .await
        .map_err(db_unavailable)?;
    let (data, total) = state
        .db
        .list_explicit_memory_marks(&ExplicitMemoryQuery {
            q: query.q,
            frame_id: query.frame_id,
            tag: query.tag,
            limit: query.limit,
            offset: query.offset,
            start_time: state
                .history_access
                .cutoff(Utc::now())
                .map(|time| time.to_rfc3339()),
        })
        .await
        .map_err(map_db_error)?;
    check_revocation(&state, revocation).await?;
    Ok(Json(
        json!({"data": data, "pagination": {"limit": query.limit, "offset": query.offset, "total": total}}),
    ))
}

pub(crate) async fn get_mark(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_owner(&state, &headers)?;
    let id = canonical_id(&id)?;
    let revocation = state
        .db
        .release_revocation()
        .await
        .map_err(db_unavailable)?;
    let mark = state
        .db
        .get_explicit_memory_mark(&id)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "mark_not_found",
                "The annotation is unavailable",
            )
        })?;
    require_history(&state, &mark)?;
    check_revocation(&state, revocation).await?;
    mark_json(mark)
}

pub(crate) async fn update_mark(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateMark>,
) -> Result<Json<Value>, ApiError> {
    require_owner(&state, &headers)?;
    let id = canonical_id(&id)?;
    if body.revision < 1 {
        return Err(invalid("A valid revision is required"));
    }
    validate_fields(
        &body.title,
        &body.note,
        &body.tags,
        body.importance,
        &body.anchors,
    )?;
    let before = state
        .db
        .release_revocation()
        .await
        .map_err(db_unavailable)?;
    let mark = state
        .db
        .get_explicit_memory_mark(&id)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "mark_not_found",
                "The annotation is unavailable",
            )
        })?;
    require_history(&state, &mark)?;
    if mark.revision != body.revision {
        return Err(map_db_error(ExplicitMemoryError::Conflict));
    }
    let needs_geometry =
        mark.image_width.is_none() && body.anchors.iter().any(|anchor| anchor.x.is_some());
    let _image_slot = if needs_geometry {
        Some(IMAGE_SLOTS.try_acquire().map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "image_busy",
                "Source image preparation is busy; retry shortly",
            )
        })?)
    } else {
        None
    };
    let (image_width, image_height) = if needs_geometry {
        let frame_id = mark.frame_id.ok_or_else(|| {
            error(
                StatusCode::GONE,
                "source_unavailable",
                "The original source image is no longer available; the saved note is unchanged",
            )
        })?;
        let prepared = capture_retained_image(&state, frame_id, before).await.map_err(|err| {
            if err.0 == StatusCode::NOT_FOUND {
                error(StatusCode::GONE, "source_unavailable", "The original source image is no longer available; the saved note is unchanged")
            } else { err }
        })?;
        (Some(prepared.width), Some(prepared.height))
    } else {
        (None, None)
    };
    let updated = state
        .db
        .update_explicit_memory_mark(
            &id,
            &UpdateExplicitMemoryMark {
                revision: body.revision,
                title: body.title,
                note: body.note,
                tags: body.tags,
                importance: body.importance,
                anchors: body.anchors,
                image_width,
                image_height,
                revocation: before,
            },
        )
        .await
        .map_err(map_db_error)?;
    check_revocation(&state, before).await?;
    mark_json(updated)
}

pub(crate) async fn delete_mark(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DeleteMark>,
) -> Result<Json<Value>, ApiError> {
    require_owner(&state, &headers)?;
    let id = canonical_id(&id)?;
    if query.revision < 1 {
        return Err(invalid("A valid revision is required"));
    }
    let deleted = state
        .db
        .delete_explicit_memory_mark(&id, query.revision)
        .await
        .map_err(map_db_error)?;
    if !deleted {
        return Err(error(
            StatusCode::NOT_FOUND,
            "mark_not_found",
            "The annotation is unavailable",
        ));
    }
    Ok(Json(json!({"success": true})))
}

pub(crate) async fn get_mark_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response<Body>, ApiError> {
    require_owner(&state, &headers)?;
    let id = canonical_id(&id)?;
    let revocation = state
        .db
        .release_revocation()
        .await
        .map_err(db_unavailable)?;
    let mark = state
        .db
        .get_explicit_memory_mark(&id)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "mark_not_found",
                "The annotation is unavailable",
            )
        })?;
    require_history(&state, &mark)?;
    let _slot = IMAGE_SLOTS.try_acquire().map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "image_busy",
            "Source image preparation is busy; retry shortly",
        )
    })?;
    let mut response = if mark.retained_image {
        let image = state
            .db
            .get_explicit_memory_image(&id)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| {
                error(
                    StatusCode::GONE,
                    "source_unavailable",
                    "The saved source image is unavailable",
                )
            })?;
        Response::builder()
            .header(header::CONTENT_TYPE, image.mime_type)
            .header(header::CONTENT_LENGTH, image.bytes.len())
            .body(Body::from(image.bytes))
            .map_err(|_| {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "image_response_failed",
                    "The source image could not be read",
                )
            })?
    } else if let Some(frame_id) = mark.frame_id {
        let source = get_frame_data(
            State(state.clone()),
            Path(frame_id),
            Query(FrameDataQuery { fallback: false }),
        )
        .await?;
        let content_type = source.headers().get(header::CONTENT_TYPE).cloned();
        let bytes = to_bytes(source.into_body(), MAX_IMAGE_BYTES)
            .await
            .map_err(|_| {
                error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "source_image_too_large",
                    "The source image exceeds the retention limit",
                )
            })?;
        let mut response = Response::new(Body::from(bytes));
        if let Some(content_type) = content_type {
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type);
        }
        response
    } else {
        return Err(error(
            StatusCode::GONE,
            "source_unavailable",
            "The original source image is no longer available",
        ));
    };
    check_revocation(&state, revocation).await?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> ExplicitMemoryAnchor {
        ExplicitMemoryAnchor {
            id: "region-1".into(),
            anchor_type: "region".into(),
            x: Some(0.2),
            y: Some(0.3),
            width: Some(0.4),
            height: Some(0.2),
            quote: None,
            comment: "saved passage".into(),
        }
    }

    #[test]
    fn owner_key_is_required_even_without_global_auth() {
        let mut headers = HeaderMap::new();
        assert!(require_owner_key(None, &headers).is_err());
        assert!(require_owner_key(Some("owner"), &headers).is_err());
        headers.insert(header::AUTHORIZATION, "Bearer owner".parse().unwrap());
        assert!(require_owner_key(Some("owner"), &headers).is_ok());
        assert!(require_owner_key(None, &headers).is_err());
    }

    #[test]
    fn pipe_tokens_and_auth_cookies_do_not_grant_owner_access() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "screenpipe_auth=owner".parse().unwrap());
        assert!(require_owner_key(Some("owner"), &headers).is_err());
        headers.insert(
            header::AUTHORIZATION,
            "Bearer sp_pipe_allowed".parse().unwrap(),
        );
        assert!(require_owner_key(Some("sp_pipe_allowed"), &headers).is_err());
    }

    #[test]
    fn regions_are_bounded_in_source_coordinates() {
        let mut selected = anchor();
        assert!(validate_fields("", "", &[], 0.5, &[selected.clone()]).is_ok());
        selected.width = Some(0.9);
        assert!(validate_fields("", "", &[], 0.5, &[selected.clone()]).is_err());
        selected.width = Some(f64::NAN);
        assert!(validate_fields("", "", &[], 0.5, &[selected.clone()]).is_err());
        selected.width = None;
        assert!(validate_fields("", "", &[], 0.5, &[selected]).is_err());
    }

    #[test]
    fn quotes_need_text_and_anchor_ids_cannot_collide() {
        let mut selected = anchor();
        assert!(validate_fields("", "", &[], 0.5, &[selected.clone(), selected.clone()]).is_err());
        selected.anchor_type = "text_quote".into();
        selected.x = None;
        selected.y = None;
        selected.width = None;
        selected.height = None;
        assert!(validate_fields("", "", &[], 0.5, &[selected.clone()]).is_err());
        selected.quote = Some("中文选择".into());
        assert!(validate_fields("", "", &[], 0.5, &[selected]).is_ok());
    }

    #[test]
    fn invalid_or_empty_marks_and_oversized_inputs_are_rejected() {
        assert!(validate_fields("", "", &[], 0.5, &[]).is_err());
        assert!(validate_fields("title", "", &[], f64::INFINITY, &[]).is_err());
        assert!(validate_fields("title", "", &[" ".into()], 0.5, &[]).is_err());
        assert!(validate_fields("title", &"x".repeat(MAX_NOTE_BYTES + 1), &[], 0.5, &[]).is_err());
    }

    #[test]
    fn aggregate_text_limits_match_the_database_for_multibyte_notes() {
        let selections: Vec<_> = (0..20)
            .map(|index| ExplicitMemoryAnchor {
                id: format!("quote-{index}"),
                anchor_type: "text_quote".into(),
                x: None,
                y: None,
                width: None,
                height: None,
                quote: Some("字".repeat(2000)),
                comment: "字".repeat(500),
            })
            .collect();
        assert!(validate_fields("title", "", &[], 0.5, &selections).is_err());
        assert!(validate_fields("title", "", &[], 0.5, &selections[..10]).is_ok());
    }

    #[test]
    fn image_dimensions_preserve_original_coordinate_space() {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(320, 200)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert_eq!(
            image_geometry(bytes.get_ref()).unwrap(),
            ("image/png".into(), 320, 200)
        );
        assert!(image_geometry(b"not an image").is_err());
    }

    #[test]
    fn public_ids_are_canonical_uuids() {
        assert!(canonical_id("not-an-id").is_err());
        let id = "AB58C55C-6C2F-458F-AE34-D43D115F7A21";
        assert_eq!(canonical_id(id).unwrap(), id.to_lowercase());
    }

    #[test]
    fn malformed_json_cannot_smuggle_internal_privacy_fields() {
        let payload = json!({"frame_id": 1, "note": "note", "revocation": 1});
        assert!(serde_json::from_value::<CreateMark>(payload).is_err());
    }

    #[tokio::test]
    async fn owner_http_flow_retains_exact_image_and_enforces_revision_and_erasure() {
        use axum::{http::Request, Router};
        use screenpipe_audio::audio_manager::AudioManagerBuilder;
        use screenpipe_db::DatabaseManager;
        use tower::ServiceExt;

        async fn request(
            router: &Router,
            method: &str,
            path: &str,
            body: Option<Value>,
            key: Option<&str>,
        ) -> Response<Body> {
            let mut request = Request::builder().method(method).uri(path);
            if let Some(key) = key {
                request = request.header(header::AUTHORIZATION, format!("Bearer {key}"));
            }
            let body = if let Some(body) = body {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            } else {
                Body::empty()
            };
            router
                .clone()
                .oneshot(request.body(body).unwrap())
                .await
                .unwrap()
        }
        async fn json_body(response: Response<Body>) -> Value {
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap()
        }

        let root = tempfile::tempdir().unwrap();
        let db = Arc::new(
            DatabaseManager::new(
                root.path().join("memory.sqlite").to_str().unwrap(),
                Default::default(),
            )
            .await
            .unwrap(),
        );
        let source = root.path().join("source.png");
        image::DynamicImage::new_rgb8(320, 200)
            .save(&source)
            .unwrap();
        let source_sql = source.to_string_lossy().replace('\'', "''");
        db.execute_raw_sql_write(&format!("INSERT INTO frames(id,timestamp,snapshot_path,app_name,window_name) VALUES(1,'2026-10-24T10:00:00Z','{source_sql}','Reader','Design article')")).await.unwrap();
        let audio = Arc::new(
            AudioManagerBuilder::new()
                .is_disabled(true)
                .output_path(root.path().join("audio"))
                .build(db.clone())
                .await
                .unwrap(),
        );
        let mut server = crate::SCServer::new(
            db.clone(),
            "127.0.0.1:0".parse().unwrap(),
            root.path().to_owned(),
            true,
            true,
            audio,
            false,
            "balanced".into(),
        );
        server.timeline_disabled = true;
        server.advertise_mdns = false;
        server.api_auth = true;
        server.api_auth_key = Some("explicit-memory-owner".into());
        let router = server.try_create_router().await.unwrap();
        let id = Uuid::new_v4().to_string();
        let body = json!({"id": id, "frame_id":1,"title":"索引想法","note":"项目甲的增量索引策略","importance":0.8,"tags":["project:a"],"anchors":[anchor()],"retain_source_image":true});
        // The global API-auth middleware rejects missing credentials before
        // the stricter owner-only handler runs; its existing status is 403.
        assert_eq!(
            request(&router, "POST", "/memory/marks", Some(body.clone()), None)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let created = request(
            &router,
            "POST",
            "/memory/marks",
            Some(body.clone()),
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
        let mark = json_body(created).await;
        assert_eq!(mark["source_frame_id"], 1);
        assert_eq!(mark["retained_image"], true);
        assert_eq!(mark["image_width"], 320);
        assert!(mark.get("bytes").is_none());
        let retry = request(
            &router,
            "POST",
            "/memory/marks",
            Some(body.clone()),
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(retry.status(), StatusCode::CREATED);
        assert_eq!(json_body(retry).await["id"], id);
        let list = request(
            &router,
            "GET",
            "/memory/marks?q=%E5%A2%9E%E9%87%8F",
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(list.status(), StatusCode::OK);
        assert_eq!(json_body(list).await["pagination"]["total"], 1);
        let unretained_id = Uuid::new_v4().to_string();
        let unretained = request(&router,"POST","/memory/marks",Some(json!({"id":unretained_id,"frame_id":1,"note":"Geometry without retention","anchors":[anchor()],"retain_source_image":false})),Some("explicit-memory-owner")).await;
        assert_eq!(unretained.status(), StatusCode::CREATED);
        let unretained = json_body(unretained).await;
        assert_eq!(unretained["retained_image"], false);
        assert_eq!(unretained["image_width"], 320);
        assert_eq!(unretained["image_height"], 200);
        let upgrade_id = Uuid::new_v4().to_string();
        let expired_note_id = Uuid::new_v4().to_string();
        for id in [&upgrade_id, &expired_note_id] {
            let note = request(&router,"POST","/memory/marks",Some(json!({"id":id,"frame_id":1,"note":"Original note","anchors":[],"retain_source_image":false})),Some("explicit-memory-owner")).await;
            assert_eq!(note.status(), StatusCode::CREATED);
            assert_eq!(json_body(note).await["image_width"], Value::Null);
        }
        let upgrade = request(&router,"PATCH",&format!("/memory/marks/{upgrade_id}"),Some(json!({"revision":1,"title":"","note":"Original note","tags":[],"importance":0.5,"anchors":[anchor()]})),Some("explicit-memory-owner")).await;
        assert_eq!(upgrade.status(), StatusCode::OK);
        let upgraded = json_body(upgrade).await;
        assert_eq!(upgraded["image_width"], 320);
        assert_eq!(upgraded["image_height"], 200);
        assert_eq!(upgraded["retained_image"], false);
        let start: DateTime<Utc> = "2026-10-24T09:00:00Z".parse().unwrap();
        let end: DateTime<Utc> = "2026-10-24T11:00:00Z".parse().unwrap();
        db.delete_retention_time_slice(start, start, end)
            .await
            .unwrap();
        tokio::fs::remove_file(source).await.unwrap();
        let missing = request(
            &router,
            "GET",
            &format!("/memory/marks/{unretained_id}/image"),
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::GONE);
        let unretained = request(
            &router,
            "GET",
            &format!("/memory/marks/{unretained_id}"),
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        let unretained = json_body(unretained).await;
        assert_eq!(unretained["source_state"], "unavailable");
        assert_eq!(unretained["image_width"], 320);
        let expired_update = request(&router,"PATCH",&format!("/memory/marks/{expired_note_id}"),Some(json!({"revision":1,"title":"","note":"Should not overwrite","tags":[],"importance":0.5,"anchors":[anchor()]})),Some("explicit-memory-owner")).await;
        assert_eq!(expired_update.status(), StatusCode::GONE);
        let unchanged = request(
            &router,
            "GET",
            &format!("/memory/marks/{expired_note_id}"),
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        let unchanged = json_body(unchanged).await;
        assert_eq!(unchanged["note"], "Original note");
        assert_eq!(unchanged["revision"], 1);
        let replay = request(
            &router,
            "POST",
            "/memory/marks",
            Some(body.clone()),
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CREATED);
        let replay = json_body(replay).await;
        assert_eq!(replay["id"], id);
        assert_eq!(replay["frame_id"], Value::Null);
        assert_eq!(replay["source_state"], "retained");
        let update = json!({"revision":1,"title":"Revised","note":"中文备注","tags":["project:a"],"importance":0.9,"anchors":[anchor()]});
        assert_eq!(
            request(
                &router,
                "PATCH",
                &format!("/memory/marks/{id}"),
                Some(update.clone()),
                Some("explicit-memory-owner")
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(
                &router,
                "PATCH",
                &format!("/memory/marks/{id}"),
                Some(update),
                Some("explicit-memory-owner")
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &router,
                "DELETE",
                &format!("/memory/marks/{id}?revision=1"),
                None,
                Some("explicit-memory-owner")
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );

        let retained = request(
            &router,
            "GET",
            &format!("/memory/marks/{id}/image"),
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(retained.status(), StatusCode::OK);
        assert_eq!(
            retained.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        let bytes = to_bytes(retained.into_body(), MAX_IMAGE_BYTES)
            .await
            .unwrap();
        assert_eq!(
            image_geometry(&bytes).unwrap(),
            ("image/png".into(), 320, 200)
        );
        db.delete_time_range_local(start, end).await.unwrap();
        assert_eq!(
            request(
                &router,
                "GET",
                &format!("/memory/marks/{id}/image"),
                None,
                Some("explicit-memory-owner")
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        let list = request(
            &router,
            "GET",
            "/memory/marks",
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(json_body(list).await["pagination"]["total"], 0);

        // Revoked copies are hidden across every read surface, while the owner
        // can still erase them without waiting for a nonexistent revalidator.
        db.execute_raw_sql_write(
            "INSERT INTO frames(id,timestamp,app_name) VALUES(2,'2026-10-24T12:00:00Z','Reader')",
        )
        .await
        .unwrap();
        let revoked_id = Uuid::new_v4().to_string();
        let saved = request(
            &router,
            "POST",
            "/memory/marks",
            Some(json!({"id":revoked_id,"frame_id":2,"note":"Private note","anchors":[]})),
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(saved.status(), StatusCode::CREATED);
        db.configure_release(&screenpipe_db::privacy_release::ReleasePolicy {
            identity: "explicit-memory-new-policy".into(),
            text: true,
            images: true,
        })
        .await
        .unwrap();
        let blocked = request(
            &router,
            "GET",
            &format!("/memory/marks/{revoked_id}"),
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json_body(blocked).await["code"],
            "privacy_revalidation_required"
        );
        let list = request(
            &router,
            "GET",
            "/memory/marks",
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(json_body(list).await["pagination"]["total"], 0);
        let erased = request(
            &router,
            "DELETE",
            &format!("/memory/marks/{revoked_id}?revision=1"),
            None,
            Some("explicit-memory-owner"),
        )
        .await;
        assert_eq!(erased.status(), StatusCode::OK);
    }
}
