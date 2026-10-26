// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Explicit owner snapshots of the existing index, not a second retrieval store.
//! Shared DB export admission serializes publication with deletion/privacy purge.

use super::{authorize, cutoff, failure, storage, ApiError};
use crate::server::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use screenpipe_db::{ExplicitMemoryError, HierarchySummary, MemoryNode, MemoryTreeQuery};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path as FilePath, PathBuf},
    sync::{Arc, LazyLock},
};
use tokio::{io::AsyncWriteExt, sync::Semaphore};

const MAX_NODES: usize = 4096;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const SOURCE_LIMIT: u32 = 32;
static EXPORT_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(1));

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportRequest {
    date: String,
    #[serde(default = "task_view")]
    view: String,
    timezone_offset_minutes: i32,
}
fn task_view() -> String {
    "task".into()
}
#[derive(Clone, Serialize, Deserialize)]
struct ExportNode {
    node: MemoryNode,
    summary: Option<HierarchySummary>,
    sources: Vec<Value>,
    coverage: Value,
}
#[derive(Clone, Serialize, Deserialize)]
struct Document {
    schema_version: u32,
    export_id: String,
    revision: String,
    created_at: String,
    date: String,
    view: String,
    timezone_offset_minutes: i32,
    privacy_epoch: Option<i64>,
    coverage: Value,
    nodes: Vec<ExportNode>,
}
fn changed() -> ApiError {
    failure(StatusCode::CONFLICT, "memory_export_changed")
}
fn io_error() -> ApiError {
    failure(StatusCode::SERVICE_UNAVAILABLE, "memory_export_io_failed")
}
fn read_error(phase: &'static str, error: ExplicitMemoryError) -> ApiError {
    // SQLx Display can contain SQL or source values. Log only fixed stages,
    // error categories and numeric SQLite codes, never the underlying message.
    let (error_class, sqlite_code) = match &error {
        ExplicitMemoryError::Database(sqlx::Error::Database(db)) => (
            "database",
            db.code().and_then(|code| code.parse::<i32>().ok()),
        ),
        ExplicitMemoryError::Database(sqlx::Error::ColumnDecode { .. }) => ("column_decode", None),
        ExplicitMemoryError::Database(sqlx::Error::ColumnNotFound(_)) => ("column_missing", None),
        ExplicitMemoryError::Database(sqlx::Error::Decode(_)) => ("decode", None),
        ExplicitMemoryError::Database(sqlx::Error::Protocol(_)) => ("protocol", None),
        ExplicitMemoryError::PrivacyChanged => ("privacy_pending", None),
        ExplicitMemoryError::Conflict => ("revision_changed", None),
        ExplicitMemoryError::NotFound => ("missing", None),
        ExplicitMemoryError::Invalid(_) => ("invalid", None),
        _ => ("unavailable", None),
    };
    tracing::warn!(
        phase,
        error_class,
        sqlite_code,
        "memory export read deferred"
    );
    storage(error)
}
fn serialized<T: Serialize>(value: &T) -> Result<Vec<u8>, ApiError> {
    serde_json::to_vec_pretty(value).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "memory_serialization_failed",
        )
    })
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn day_range(date: &str, minutes: i32) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
    if date.len() != 10 || !(-840..=840).contains(&minutes) {
        return Err(failure(StatusCode::BAD_REQUEST, "invalid_memory_date"));
    }
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "invalid_memory_date"))?;
    let offset = FixedOffset::east_opt(minutes * 60)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid_memory_timezone"))?;
    let start = offset
        .from_local_datetime(
            &date
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid_memory_date"))?,
        )
        .single()
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid_memory_date"))?
        .with_timezone(&Utc);
    Ok((start, start + Duration::days(1)))
}
fn root(state: &AppState) -> Result<PathBuf, ApiError> {
    state.db.memory_export_directory().map_err(|_| io_error())
}
async fn directory(path: &FilePath, create: bool) -> Result<(), ApiError> {
    if create {
        match tokio::fs::create_dir(path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(io_error()),
        }
    }
    let meta = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| io_error())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io_error());
    }
    #[cfg(unix)]
    if create {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| io_error())?;
    }
    Ok(())
}
async fn export_root(state: &AppState, create: bool) -> Result<PathBuf, ApiError> {
    let root = root(state)?;
    directory(root.parent().ok_or_else(io_error)?, create).await?;
    directory(&root, create).await?;
    Ok(root)
}
async fn write_file(path: &FilePath, bytes: &[u8]) -> Result<(), ApiError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await.map_err(|_| io_error())?;
    file.write_all(bytes).await.map_err(|_| io_error())?;
    file.sync_all().await.map_err(|_| io_error())
}
fn markdown_text(text: &str) -> String {
    // Render source prose as text, never active HTML, images or arbitrary links.
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '!'
            | '|' => {
                out.push('\\');
                out.push(ch);
            }
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}
fn source_navigation(uri: Option<&str>, timestamp: &str) -> String {
    if let Some(uri) = uri.filter(|uri| {
        uri.strip_prefix("screenpipe://frame/")
            .is_some_and(|id| id.parse::<i64>().is_ok_and(|id| id > 0))
    }) {
        return format!(" · [exact frame]({uri})");
    }
    // The existing timeline deep-link handler accepts an RFC3339 timestamp.
    // This opens temporal context, never claims exact screenshot identity.
    if DateTime::parse_from_rfc3339(timestamp).is_ok() {
        if let Ok(mut link) = url::Url::parse("screenpipe://timeline") {
            link.query_pairs_mut().append_pair("timestamp", timestamp);
            return format!(" · [source time]({link})");
        }
    }
    String::new()
}
fn markdown(doc: &Document) -> String {
    let mut out=format!("# Screenpipe index — {}\n\nView: {}  \nRevision: `{}`  \nGenerated: {}\n\nCoverage: `{}`\n\nLabels and summaries are derived organization, not verified intent. User interpretations remain attributed. Raw captured excerpts are not included.\n\n",doc.date,doc.view,doc.revision,doc.created_at,doc.coverage);
    let parents: HashMap<_, _> = doc
        .nodes
        .iter()
        .map(|entry| (entry.node.id.as_str(), entry.node.parent_id.as_deref()))
        .collect();
    out.push_str("## Contents\n\n");
    for entry in &doc.nodes {
        let mut depth = 0;
        let mut parent = entry.node.parent_id.as_deref();
        while let Some(id) = parent {
            if depth == 3 || !parents.contains_key(id) {
                break;
            }
            depth += 1;
            parent = parents.get(id).copied().flatten();
        }
        let anchor = digest(entry.node.id.as_bytes());
        out.push_str(&format!(
            "{}- [{} — {}](#node-{})\n",
            "  ".repeat(depth),
            entry.node.kind,
            markdown_text(&entry.node.label),
            anchor
        ));
    }
    out.push('\n');
    for entry in &doc.nodes {
        out.push_str(&format!(
            "<a id=\"node-{}\"></a>\n\n",
            digest(entry.node.id.as_bytes())
        ));
        out.push_str(&format!("## {} — {}\n\nNode: `{}`  \nParent: `{}`  \nSource revision: {}  \nFingerprint: `{}`\n\n",entry.node.kind,markdown_text(&entry.node.label),entry.node.id,entry.node.parent_id.as_deref().unwrap_or(""),entry.node.revision,entry.node.fingerprint));
        if let Some(summary) = &entry.summary {
            out.push_str(&format!(
                "Summary: {} · {} · {}\n\n",
                summary.status,
                markdown_text(&summary.model),
                markdown_text(&summary.prompt_version)
            ));
            if summary.status == "ready" {
                if let Some(text) = &summary.text {
                    for line in text.lines() {
                        out.push_str(&format!("> {}\n", markdown_text(line)));
                    }
                    out.push('\n');
                }
                for cite in &summary.citations {
                    out.push_str(&format!(
                        "- {}:{} · {}{}\n",
                        markdown_text(&cite.source_type),
                        markdown_text(&cite.source_id),
                        markdown_text(&cite.uri),
                        source_navigation(Some(&cite.uri), &cite.timestamp)
                    ));
                }
                out.push('\n');
            }
        }
        out.push_str(&format!("Coverage: `{}`\n\n", entry.coverage));
        for source in &entry.sources {
            let kind = source["source_type"].as_str().unwrap_or("source");
            let id = source["source_id"].to_string();
            out.push_str(&format!(
                "- {} {} · {}",
                markdown_text(kind),
                markdown_text(&id),
                markdown_text(source["timestamp"].as_str().unwrap_or(""))
            ));
            if let Some(uri) = source["uri"].as_str() {
                out.push_str(&format!(" · {}", markdown_text(uri)));
            }
            if source["available"] == false {
                out.push_str(" · unavailable");
            } else {
                out.push_str(&source_navigation(
                    source["uri"].as_str(),
                    source["timestamp"].as_str().unwrap_or(""),
                ));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}
async fn collect(
    state: &Arc<AppState>,
    request: &ExportRequest,
    export_id: String,
) -> Result<Document, ApiError> {
    let (start, end) = day_range(&request.date, request.timezone_offset_minutes)?;
    if !state.history_access.allows(start, Utc::now()) {
        return Err(failure(StatusCode::FORBIDDEN, "history_access_limited"));
    }
    if request.view != "task" && request.view != "app" {
        return Err(failure(StatusCode::BAD_REQUEST, "invalid_memory_view"));
    }
    let before = state
        .db
        .memory_index_epoch()
        .await
        .map_err(|error| read_error("index_epoch", error))?;
    let mut queue = VecDeque::from([(None, 0_u32)]);
    let mut nodes = Vec::new();
    let mut truncated = false;
    let mut time_budget_exceeded = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12);
    'pages: while let Some((parent, offset)) = queue.pop_front() {
        let query = MemoryTreeQuery {
            view: Some(request.view.clone()),
            parent_id: parent.clone(),
            kind: if parent.is_none() {
                Some("day".into())
            } else {
                None
            },
            start_time: Some(start.to_rfc3339()),
            end_time: Some(end.to_rfc3339()),
            timezone_offset_minutes: Some(request.timezone_offset_minutes),
            limit: 50,
            offset,
            ..Default::default()
        };
        let page = match tokio::time::timeout_at(deadline, state.db.list_memory_nodes(&query)).await
        {
            Ok(result) => result.map_err(|error| read_error("list_nodes", error))?,
            Err(_) => {
                truncated = true;
                time_budget_exceeded = true;
                break;
            }
        };
        if page.len() == 50 {
            queue.push_back((parent, offset + 50));
        }
        for mut node in page {
            if nodes.len() == MAX_NODES {
                truncated = true;
                break;
            }
            node.children.clear();
            let input = async {
                let sources = state
                    .db
                    .memory_node_source_refs(&node.id, cutoff(state).as_deref(), SOURCE_LIMIT)
                    .await
                    .map_err(|error| read_error("source_refs", error))?;
                let summary = state
                    .db
                    .get_hierarchy_summary(&node.id)
                    .await
                    .map_err(|error| read_error("read_summary", error))?;
                Ok::<_, ApiError>((sources, summary))
            };
            let (sources, summary) = match tokio::time::timeout_at(deadline, input).await {
                Ok(result) => result?,
                Err(_) => {
                    truncated = true;
                    time_budget_exceeded = true;
                    break 'pages;
                }
            };
            let evidence_returned = sources
                .iter()
                .filter(|source| source["source_type"] != "user_note")
                .count();
            let marks_returned = sources.len() - evidence_returned;
            let coverage = json!({"evidence_total":node.evidence_count,"marks_total":node.mark_count,"source_refs_returned":sources.len(),"truncated":(evidence_returned as i64)<node.evidence_count||(marks_returned as i64)<node.mark_count,"continuous_coverage":false});
            if node.kind != "task" && node.kind != "location" {
                queue.push_back((Some(node.id.clone()), 0));
            }
            nodes.push(ExportNode {
                node,
                summary,
                sources,
                coverage,
            });
        }
        if nodes.len() == MAX_NODES {
            truncated |= !queue.is_empty();
            break;
        }
    }
    if nodes.is_empty() {
        return Err(failure(StatusCode::NOT_FOUND, "memory_export_no_index"));
    }
    let refs_truncated = nodes
        .iter()
        .any(|entry| entry.coverage["truncated"] == true);
    let coverage = json!({"nodes_exported":nodes.len(),"max_nodes":MAX_NODES,"source_refs_per_section":SOURCE_LIMIT,"nodes_truncated":truncated,"source_refs_truncated":refs_truncated,"time_budget_exceeded":time_budget_exceeded,"truncated":truncated||refs_truncated,"raw_excerpts_included":false,"continuous_coverage":false});
    let revision = digest(&serialized(
        &json!({"view":request.view,"date":request.date,"offset":request.timezone_offset_minutes,"nodes":nodes,"privacy_epoch":before}),
    )?);
    let doc = Document {
        schema_version: 1,
        export_id,
        revision,
        created_at: Utc::now().to_rfc3339(),
        date: request.date.clone(),
        view: request.view.clone(),
        timezone_offset_minutes: request.timezone_offset_minutes,
        privacy_epoch: before,
        coverage,
        nodes,
    };
    validate(state, &doc, false).await?;
    Ok(doc)
}
async fn validate(state: &AppState, doc: &Document, summaries: bool) -> Result<(), ApiError> {
    if state.db.memory_index_epoch().await.map_err(storage)? != doc.privacy_epoch {
        return Err(changed());
    }
    let receipts: Vec<Value> = doc
        .nodes
        .iter()
        .map(
            |e| json!({"id":e.node.id,"fingerprint":e.node.fingerprint,"revision":e.node.revision}),
        )
        .collect();
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM json_each(?) r JOIN memory_nodes n ON n.id=json_extract(r.value,'$.id') AND n.fingerprint=json_extract(r.value,'$.fingerprint') AND n.revision=json_extract(r.value,'$.revision') WHERE n.invalidated=0 AND n.superseded=0 AND n.privacy_epoch IS ?")
        .bind(serde_json::to_string(&receipts).map_err(|_|changed())?).bind(doc.privacy_epoch).fetch_one(&state.db.pool).await.map_err(|_|changed())?;
    if count != doc.nodes.len() as i64 {
        return Err(changed());
    }
    if summaries {
        for entry in &doc.nodes {
            if let Some(summary) = entry.summary.as_ref().filter(|s| s.status == "ready") {
                let current = state
                    .db
                    .get_hierarchy_summary(&entry.node.id)
                    .await
                    .map_err(storage)?
                    .ok_or_else(changed)?;
                if current.status != "ready"
                    || current.input_fingerprint != summary.input_fingerprint
                    || current.text != summary.text
                {
                    return Err(changed());
                }
            }
        }
    }
    Ok(())
}
fn descriptor(doc: &Document, files: Value) -> Value {
    json!({"schema_version":doc.schema_version,"export_id":doc.export_id,"revision":doc.revision,"created_at":doc.created_at,"date":doc.date,"view":doc.view,"timezone_offset_minutes":doc.timezone_offset_minutes,"coverage":doc.coverage,"files":files,"status":"ready","freshness":"current","retention":"owner_snapshot_revoked_on_source_deletion_or_privacy_change"})
}

pub(crate) async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ExportRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    if state.vault.is_set_up() {
        return Err(failure(StatusCode::CONFLICT, "memory_export_vault_enabled"));
    }
    let _slot = EXPORT_SLOTS
        .try_acquire()
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "memory_export_busy"))?;
    let _export_gate = state
        .db
        .memory_export_gate()
        .await
        .map_err(|_| io_error())?;
    let token = state
        .db
        .storage_export_token()
        .await
        .map_err(|_| changed())?;
    let uuid = uuid::Uuid::new_v4().to_string();
    let id = format!("{}_{}", request.date, uuid);
    // Typed readers own short SQLx transactions. Their versions are checked
    // together below and again before publication under the export gate;
    // wrapping them in a raw-BEGIN storage snapshot would nest transactions.
    let mut doc = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        collect(&state, &request, id),
    )
    .await
    .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "memory_export_timeout"))??;
    // Budget whole records, keeping JSON and Markdown on the same prefix and
    // revision. A large day remains inspectable with explicit partial coverage.
    let (data, md) = loop {
        let data = serialized(&doc)?;
        let md = markdown(&doc).into_bytes();
        if data.len() <= MAX_BYTES && md.len() <= MAX_BYTES {
            break (data, md);
        }
        if doc.nodes.len() <= 1 {
            return Err(failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "memory_export_too_large",
            ));
        }
        doc.nodes.truncate((doc.nodes.len() * 3 / 4).max(1));
        doc.coverage["nodes_exported"] = json!(doc.nodes.len());
        doc.coverage["nodes_truncated"] = json!(true);
        doc.coverage["truncated"] = json!(true);
        doc.revision = digest(&serialized(
            &json!({"date":doc.date,"view":doc.view,"offset":doc.timezone_offset_minutes,"coverage":doc.coverage,"nodes":doc.nodes,"privacy_epoch":doc.privacy_epoch}),
        )?);
    };
    let files = json!([{"format":"json","name":"index.json","bytes":data.len(),"sha256":digest(&data)},{"format":"markdown","name":"index.md","bytes":md.len(),"sha256":digest(&md)}]);
    let manifest = descriptor(&doc, files);
    let mut stored_manifest = manifest.clone();
    stored_manifest["privacy_epoch"] = json!(doc.privacy_epoch);
    stored_manifest["node_versions"]=json!(doc.nodes.iter().map(|entry|json!({"id":entry.node.id,"fingerprint":entry.node.fingerprint,"revision":entry.node.revision})).collect::<Vec<_>>());
    let manifest_bytes = serialized(&stored_manifest)?;
    let root = export_root(&state, true).await?;
    let date = root.join(&doc.date);
    directory(&date, true).await?;
    let stage = date.join(format!(".staging-{uuid}"));
    directory(&stage, true).await?;
    let destination = date.join(&uuid);
    let result = async {
        write_file(&stage.join("index.json"), &data).await?;
        write_file(&stage.join("index.md"), &md).await?;
        write_file(&stage.join("manifest.json"), &manifest_bytes).await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            validate(&state, &doc, true),
        )
        .await
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "memory_export_timeout"))??;
        if state.vault.is_set_up() {
            return Err(failure(StatusCode::CONFLICT, "memory_export_vault_enabled"));
        }
        let _admission = token.admit(&state.db.pool).await.map_err(|_| changed())?;
        #[cfg(unix)]
        tokio::fs::File::open(&stage)
            .await
            .map_err(|_| io_error())?
            .sync_all()
            .await
            .map_err(|_| io_error())?;
        tokio::fs::rename(&stage, &destination)
            .await
            .map_err(|_| io_error())?;
        #[cfg(unix)]
        tokio::fs::File::open(&date)
            .await
            .map_err(|_| io_error())?
            .sync_all()
            .await
            .map_err(|_| io_error())?;
        let mut result = manifest;
        result["directory"] = json!(destination);
        result["status"] = json!("ready");
        Ok((StatusCode::CREATED, Json(result)).into_response())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&stage).await;
    }
    result
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Page {
    limit: Option<usize>,
    offset: Option<usize>,
}
fn page(query: &Page) -> (usize, usize) {
    (
        query.limit.unwrap_or(20).clamp(1, 50),
        query.offset.unwrap_or(0).min(100_000),
    )
}
fn export_path(root: &FilePath, id: &str) -> Result<PathBuf, ApiError> {
    let (date, uuid) = id
        .split_once('_')
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid_memory_export_id"))?;
    day_range(date, 0)?;
    let uuid = uuid::Uuid::parse_str(uuid)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "invalid_memory_export_id"))?;
    Ok(root.join(date).join(uuid.to_string()))
}
async fn read_json(path: &FilePath) -> Result<Value, ApiError> {
    let meta = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| failure(StatusCode::NOT_FOUND, "memory_export_unavailable"))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_BYTES as u64 {
        return Err(io_error());
    }
    let bytes = tokio::fs::read(path).await.map_err(|_| io_error())?;
    if bytes.len() > MAX_BYTES {
        return Err(io_error());
    }
    serde_json::from_slice(&bytes).map_err(|_| io_error())
}
async fn load(state: &AppState, id: &str) -> Result<(Document, Value), ApiError> {
    let root = export_root(state, false).await?;
    let path = export_path(&root, id)?;
    directory(path.parent().ok_or_else(io_error)?, false).await?;
    directory(&path, false).await?;
    let doc: Document = serde_json::from_value(read_json(&path.join("index.json")).await?)
        .map_err(|_| io_error())?;
    if doc.export_id != id || doc.nodes.len() > MAX_NODES {
        return Err(io_error());
    }
    let (start, _) = day_range(&doc.date, doc.timezone_offset_minutes)?;
    if !state.history_access.allows(start, Utc::now()) {
        return Err(failure(StatusCode::FORBIDDEN, "history_access_limited"));
    }
    validate(state, &doc, false).await?;
    let manifest = read_json(&path.join("manifest.json")).await?;
    if manifest["export_id"] != doc.export_id || manifest["revision"] != doc.revision {
        return Err(io_error());
    }
    Ok((doc, public_manifest(&manifest)))
}
fn public_manifest(manifest: &Value) -> Value {
    let mut result = json!({});
    for key in [
        "schema_version",
        "export_id",
        "revision",
        "created_at",
        "date",
        "view",
        "timezone_offset_minutes",
        "coverage",
        "files",
        "retention",
        "status",
        "freshness",
        "requires_regeneration",
    ] {
        result[key] = manifest[key].clone();
    }
    result
}
async fn load_descriptor(state: &AppState, id: &str) -> Result<Value, ApiError> {
    let root = export_root(state, false).await?;
    let path = export_path(&root, id)?;
    directory(path.parent().ok_or_else(io_error)?, false).await?;
    directory(&path, false).await?;
    let manifest = read_json(&path.join("manifest.json")).await?;
    if manifest["export_id"] != id {
        return Err(io_error());
    }
    let epoch: Option<i64> =
        serde_json::from_value(manifest["privacy_epoch"].clone()).map_err(|_| io_error())?;
    if state.db.memory_index_epoch().await.map_err(storage)? != epoch {
        return Err(changed());
    }
    let versions = manifest["node_versions"]
        .as_array()
        .filter(|versions| versions.len() <= MAX_NODES)
        .ok_or_else(io_error)?;
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM json_each(?) r JOIN memory_nodes n ON n.id=json_extract(r.value,'$.id') AND n.fingerprint=json_extract(r.value,'$.fingerprint') AND n.revision=json_extract(r.value,'$.revision') WHERE n.invalidated=0 AND n.superseded=0 AND n.privacy_epoch IS ?")
        .bind(serde_json::to_string(versions).map_err(|_|io_error())?).bind(epoch).fetch_one(&state.db.pool).await.map_err(|_|changed())?;
    let current = count == versions.len() as i64;
    let date = manifest["date"].as_str().ok_or_else(io_error)?;
    let offset = manifest["timezone_offset_minutes"]
        .as_i64()
        .and_then(|offset| i32::try_from(offset).ok())
        .ok_or_else(io_error)?;
    if !state
        .history_access
        .allows(day_range(date, offset)?.0, Utc::now())
    {
        return Err(failure(StatusCode::FORBIDDEN, "history_access_limited"));
    }
    let mut descriptor = public_manifest(&manifest);
    descriptor["status"] = json!(if current { "ready" } else { "stale" });
    descriptor["freshness"] = json!(if current { "current" } else { "index_changed" });
    descriptor["requires_regeneration"] = json!(!current);
    Ok(descriptor)
}
pub(crate) async fn manifest(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<Page>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    if state.vault.is_set_up() {
        return Err(failure(StatusCode::CONFLICT, "memory_export_vault_enabled"));
    }
    let _export_gate = state
        .db
        .memory_export_gate()
        .await
        .map_err(|_| io_error())?;
    let token = state
        .db
        .storage_export_token()
        .await
        .map_err(|_| changed())?;
    let (doc, mut manifest) = load(&state, &id).await?;
    let (limit, offset) = page(&query);
    let scoped = Document {
        nodes: doc.nodes.iter().skip(offset).take(limit).cloned().collect(),
        ..doc.clone()
    };
    validate(&state, &scoped, true).await?;
    manifest["nodes"] = serde_json::to_value(
        doc.nodes
            .iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>(),
    )
    .map_err(|_| io_error())?;
    manifest["pagination"] = json!({"offset":offset,"limit":limit,"total":doc.nodes.len(),"returned":doc.nodes.len().saturating_sub(offset).min(limit),"next_offset":if offset+limit<doc.nodes.len(){Some(offset+limit)}else{None}});
    crate::hierarchy_summary::admitted_response(&state, token, manifest).await
}
pub(crate) async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<Page>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    if state.vault.is_set_up() {
        return Err(failure(StatusCode::CONFLICT, "memory_export_vault_enabled"));
    }
    let _export_gate = state
        .db
        .memory_export_gate()
        .await
        .map_err(|_| io_error())?;
    let token = state
        .db
        .storage_export_token()
        .await
        .map_err(|_| changed())?;
    let root = root(&state)?;
    let (limit, offset) = page(&query);
    let mut ids = Vec::new();
    let mut scanned = 0;
    if tokio::fs::try_exists(&root).await.map_err(|_| io_error())? {
        let root = export_root(&state, false).await?;
        let mut dates = tokio::fs::read_dir(&root).await.map_err(|_| io_error())?;
        while let Some(date) = dates.next_entry().await.map_err(|_| io_error())? {
            scanned += 1;
            if scanned > 512 {
                break;
            }
            let name = date.file_name().to_string_lossy().to_string();
            if day_range(&name, 0).is_err() {
                continue;
            }
            if date.file_type().await.map_err(|_| io_error())?.is_symlink() {
                continue;
            }
            let mut children = tokio::fs::read_dir(date.path())
                .await
                .map_err(|_| io_error())?;
            while let Some(child) = children.next_entry().await.map_err(|_| io_error())? {
                if ids.len() == 512 {
                    break;
                }
                if uuid::Uuid::parse_str(&child.file_name().to_string_lossy()).is_ok() {
                    ids.push(format!("{}_{}", name, child.file_name().to_string_lossy()));
                }
            }
        }
    }
    ids.sort();
    ids.reverse();
    let mut entries = Vec::new();
    for id in ids.iter().skip(offset).take(limit) {
        match load_descriptor(&state, id).await {
            Ok(manifest) => entries.push(manifest),
            Err(_) => { /* Revoked/stale bundles are not read results. */ }
        }
    }
    crate::hierarchy_summary::admitted_response(&state,token,json!({"data":entries,"limit":limit,"offset":offset,"truncated":ids.len()>=512||scanned>512,"pagination":{"offset":offset,"limit":limit,"next_offset":if offset+limit<ids.len(){Some(offset+limit)}else{None}}})).await
}
