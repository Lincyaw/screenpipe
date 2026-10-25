// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Authenticated local tree navigation, exact evidence and bounded corrections.
//! Scheduling returns immediately; model and index work never runs in a route.

use crate::server::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};
use chrono::{DateTime, Local, Utc};
use screenpipe_db::{ExplicitMemoryError, MemoryNodeCorrection, MemoryTreeQuery};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

type ApiError = (StatusCode, Json<Value>);
fn failure(status: StatusCode, code: &str) -> ApiError {
    (status, Json(json!({"code":code,"error":code})))
}
fn storage(error: ExplicitMemoryError) -> ApiError {
    match error {
        ExplicitMemoryError::Conflict => failure(StatusCode::CONFLICT, "memory_revision_conflict"),
        ExplicitMemoryError::NotFound => failure(StatusCode::NOT_FOUND, "memory_node_unavailable"),
        ExplicitMemoryError::PrivacyChanged => {
            failure(StatusCode::SERVICE_UNAVAILABLE, "privacy_pending")
        }
        ExplicitMemoryError::Invalid(message) => failure(StatusCode::BAD_REQUEST, &message),
        _ => failure(StatusCode::SERVICE_UNAVAILABLE, "memory_index_unavailable"),
    }
}
fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    super::explicit_memory::require_owner(state, headers)
}
fn cutoff(state: &AppState) -> Option<String> {
    state
        .history_access
        .cutoff(Utc::now())
        .map(|at| at.to_rfc3339())
}
fn prepare_query(
    state: &AppState,
    mut query: MemoryTreeQuery,
) -> Result<MemoryTreeQuery, ApiError> {
    if let Some(history) = state.history_access.cutoff(Utc::now()) {
        let requested = query
            .start_time
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| failure(StatusCode::BAD_REQUEST, "invalid_memory_timestamp"))?
            .map(|at| at.with_timezone(&Utc));
        query.start_time = Some(requested.unwrap_or(history).max(history).to_rfc3339());
    }
    query.limit = if query.limit == 0 {
        50
    } else {
        query.limit.min(50)
    };
    query
        .timezone_offset_minutes
        .get_or_insert_with(|| Local::now().offset().local_minus_utc() / 60);
    Ok(query)
}

pub(crate) async fn tree(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<MemoryTreeQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let token = state.db.storage_export_token().await.map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "memory_storage_unavailable",
        )
    })?;
    let query = prepare_query(&state, query)?;
    let before = state.db.memory_index_epoch().await.map_err(storage)?;
    let mut nodes = state.db.list_memory_nodes(&query).await.map_err(storage)?;
    let depth = query.depth.unwrap_or(1).clamp(1, 3);
    let mut budget = 100_usize.saturating_sub(nodes.len());
    if depth > 1 {
        for node in &mut nodes {
            if budget == 0 {
                break;
            }
            let child_query = MemoryTreeQuery {
                parent_id: Some(node.id.clone()),
                start_time: query.start_time.clone(),
                end_time: query.end_time.clone(),
                app: query.app.clone(),
                tag: query.tag.clone(),
                limit: budget.min(50) as u32,
                ..Default::default()
            };
            node.children = state
                .db
                .list_memory_nodes(&child_query)
                .await
                .map_err(storage)?;
            budget = budget.saturating_sub(node.children.len());
            if depth > 2 {
                for child in &mut node.children {
                    if budget == 0 {
                        break;
                    }
                    let grandchildren = MemoryTreeQuery {
                        parent_id: Some(child.id.clone()),
                        start_time: query.start_time.clone(),
                        end_time: query.end_time.clone(),
                        app: query.app.clone(),
                        tag: query.tag.clone(),
                        limit: budget.min(50) as u32,
                        ..Default::default()
                    };
                    child.children = state
                        .db
                        .list_memory_nodes(&grandchildren)
                        .await
                        .map_err(storage)?;
                    budget = budget.saturating_sub(child.children.len());
                }
            }
        }
    }
    let index = state.db.memory_index_status().await.map_err(storage)?;
    if state.db.memory_index_epoch().await.map_err(storage)? != before {
        return Err(storage(ExplicitMemoryError::PrivacyChanged));
    }
    crate::hierarchy_summary::admitted_response(&state,token,
        json!({"data":nodes,"index":index,"limit":query.limit,"offset":query.offset,"truncated":budget==0,"coverage":{"tag_filter":query.tag,"tag_scope":"node_or_descendant_marks_and_linked_sources","untagged_sources_excluded":query.tag.is_some(),"continuous_coverage":false}}),
    ).await
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct NodeQuery {
    pub limit: Option<u32>,
}
pub(crate) async fn node(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<NodeQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let token = state.db.storage_export_token().await.map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "memory_storage_unavailable",
        )
    })?;
    let before = state.db.memory_index_epoch().await.map_err(storage)?;
    let detail = state
        .db
        .get_memory_node(
            &id,
            cutoff(&state).as_deref(),
            query.limit.unwrap_or(50).clamp(1, 50),
        )
        .await
        .map_err(storage)?
        .ok_or_else(|| storage(ExplicitMemoryError::NotFound))?;
    let mut body = serde_json::to_value(detail).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "memory_serialization_failed",
        )
    })?;
    let summary = state.db.get_hierarchy_summary(&id).await.map_err(storage)?;
    body["summary"] = serde_json::to_value(summary).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "memory_serialization_failed",
        )
    })?;
    if state.db.memory_index_epoch().await.map_err(storage)? != before {
        return Err(storage(ExplicitMemoryError::PrivacyChanged));
    }
    crate::hierarchy_summary::admitted_response(&state, token, body).await
}
pub(crate) async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<MemoryTreeQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let token = state.db.storage_export_token().await.map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "memory_storage_unavailable",
        )
    })?;
    let before = state.db.memory_index_epoch().await.map_err(storage)?;
    let query = prepare_query(&state, query)?;
    let data = state.db.search_memory(&query).await.map_err(storage)?;
    let index = state.db.memory_index_status().await.map_err(storage)?;
    if state.db.memory_index_epoch().await.map_err(storage)? != before {
        return Err(storage(ExplicitMemoryError::PrivacyChanged));
    }
    crate::hierarchy_summary::admitted_response(&state,token,
        json!({"data":data,"index":index,"limit":query.limit,"offset":query.offset,"truncated":true,"coverage":{"bounded":true,"direct_candidates_per_source":50,"cjk_recent_candidates":100,"context_candidates_per_kind":20,"tag_filter":query.tag,"tag_scope":{"user_note":"mark_tags","context_node":"node_or_descendant_marks_and_sources","captured_text":"frame_or_audio_chunk_tags","existing_memory":"legacy_tags_or_linked_frame_tags","semantic_context":"exact_linked_frame_tags"},"untagged_sources_excluded":query.tag.is_some(),"continuous_coverage":false}}),
    ).await
}
pub(crate) async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&state, &headers)?;
    Ok(Json(
        serde_json::to_value(state.db.memory_index_status().await.map_err(storage)?).map_err(
            |_| {
                failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "memory_serialization_failed",
                )
            },
        )?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RebuildRequest {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub timezone_offset_minutes: Option<i32>,
}
pub(crate) async fn rebuild(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<RebuildRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    authorize(&state, &headers)?;
    if !state.history_access.allows(body.start_time, Utc::now()) {
        return Err(failure(StatusCode::FORBIDDEN, "history_access_limited"));
    }
    let jobs = state
        .db
        .schedule_memory_index(
            body.start_time,
            body.end_time,
            body.timezone_offset_minutes
                .unwrap_or_else(|| Local::now().offset().local_minus_utc() / 60),
            100,
        )
        .await
        .map_err(storage)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"jobs":jobs,"index":state.db.memory_index_status().await.map_err(storage)?})),
    ))
}
async fn correction(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: String,
    body: MemoryNodeCorrection,
    operation: &str,
) -> Result<Json<Value>, ApiError> {
    authorize(&state, &headers)?;
    let node = state
        .db
        .correct_memory_node(&id, operation, &body, cutoff(&state).as_deref())
        .await
        .map_err(storage)?;
    Ok(Json(
        json!({"node":node,"index":state.db.memory_index_status().await.map_err(storage)?}),
    ))
}
pub(crate) async fn correct(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<MemoryNodeCorrection>,
) -> Result<Json<Value>, ApiError> {
    correction(state, headers, id, body, "label").await
}
pub(crate) async fn merge(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<MemoryNodeCorrection>,
) -> Result<Json<Value>, ApiError> {
    correction(state, headers, id, body, "merge").await
}
pub(crate) async fn split(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<MemoryNodeCorrection>,
) -> Result<Json<Value>, ApiError> {
    correction(state, headers, id, body, "split").await
}
