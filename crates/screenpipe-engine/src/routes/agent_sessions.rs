// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Read-only local session context for human viewers and external MCP agents.
use super::search::OptionalPipePerms;
use crate::server::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use screenpipe_core::agents::chat_control::{session_context, ChatSource};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub(crate) struct SessionQuery {
    #[serde(default)]
    q: String,
    source: Option<ChatSource>,
    id: Option<String>,
    limit: Option<usize>,
}

pub(crate) async fn get(
    State(state): State<Arc<AppState>>,
    OptionalPipePerms(perms): OptionalPipePerms,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Files outside the capture database cannot satisfy app/time/content scopes
    // or the capture privacy release receipts. Never silently bypass them.
    if perms.as_ref().is_some_and(|p| p.has_data_restrictions())
        || state.history_access.cutoff(chrono::Utc::now()).is_some()
        || state.use_pii_removal
        || perms.as_ref().is_some_and(|p| p.privacy_filter)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "session files require unrestricted history access and have no privacy release adapter"
            })),
        ));
    }
    let policy = state.db.release_policy().await.map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "privacy policy is unavailable"})),
        )
    })?;
    if policy.text {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "agent session files have no text privacy release adapter"})),
        ));
    }
    session_context(query.q, query.source, query.id, query.limit.unwrap_or(20))
        .await
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({"error": error}))))
}
