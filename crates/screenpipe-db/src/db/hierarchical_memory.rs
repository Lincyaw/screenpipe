// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Disposable hierarchy over durable activity evidence. Bounded background
//! work uses the existing writer; the capture path has no new observer.

use super::explicit_memory::ExplicitMemoryError;
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap as Map, BTreeSet as Set};

mod jobs;
mod projection;
mod query;

pub(crate) const MAX_DAY_EVIDENCE: usize = 12_000;
pub(crate) const MAX_DAY_NODES: usize = 1_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub label: String,
    pub label_origin: String,
    pub start_time: String,
    pub end_time: String,
    pub provisional: bool,
    pub evidence_count: i64,
    pub mark_count: i64,
    pub apps: Vec<String>,
    pub revision: i64,
    pub fingerprint: String,
    #[serde(default)]
    pub children: Vec<MemoryNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryEvidence {
    pub source_type: String,
    pub source_id: i64,
    pub timestamp: String,
    pub app_name: Option<String>,
    pub excerpt: Option<String>,
    pub frame_id: Option<i64>,
    pub mark_id: Option<String>,
    pub uri: Option<String>,
    pub available: bool,
    pub window_title: Option<String>,
    pub browser_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryNodeDetail {
    pub node: MemoryNode,
    pub evidence: Vec<MemoryEvidence>,
    pub marks: Vec<ExplicitMemoryMark>,
    pub children: Vec<MemoryNode>,
    pub evidence_total: i64,
    pub mark_total: i64,
    pub children_total: i64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryTreeQuery {
    pub parent_id: Option<String>,
    #[serde(alias = "level")]
    pub kind: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub app: Option<String>,
    pub tag: Option<String>,
    pub source: Option<String>,
    pub q: Option<String>,
    pub limit: u32,
    pub offset: u32,
    pub depth: Option<u32>,
    pub timezone_offset_minutes: Option<i32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryIndexJob {
    pub id: String,
    pub start_time: String,
    pub end_time: String,
    pub cursor: String,
    pub timezone_offset_minutes: i32,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryIndexStatus {
    pub generation: i64,
    pub state: String,
    pub pending_jobs: i64,
    pub failed_jobs: i64,
    pub cursor: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub indexed_nodes: i64,
    pub provisional_nodes: i64,
    pub coverage_start: Option<String>,
    pub coverage_end: Option<String>,
    pub privacy_pending: bool,
    pub rebuild_pending: bool,
    pub timezone_offset_minutes: i32,
    pub current_job: Option<MemoryIndexJob>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemorySearchHit {
    pub kind: String,
    pub score: f64,
    pub node: Option<MemoryNode>,
    pub mark: Option<ExplicitMemoryMark>,
    pub evidence: Option<MemoryEvidence>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryNodeCorrection {
    pub revision: i64,
    pub label: Option<String>,
    #[serde(default)]
    pub node_ids: Vec<String>,
    pub at: Option<String>,
}

fn invalid(message: &str) -> ExplicitMemoryError {
    ExplicitMemoryError::Invalid(message.into())
}
fn hash(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}
fn time(value: &str) -> Result<DateTime<Utc>, ExplicitMemoryError> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| invalid("invalid_memory_timestamp"))
}
fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn bounded(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}
fn validate_offset(offset: i32) -> Result<(), ExplicitMemoryError> {
    if !(-840..=840).contains(&offset) {
        return Err(invalid("invalid_timezone_offset"));
    }
    Ok(())
}

pub(super) async fn epoch(conn: &mut sqlx::SqliteConnection) -> Result<Option<i64>, sqlx::Error> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='_privacy_policy')",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !exists {
        return Ok(None);
    }
    sqlx::query_scalar("SELECT revocation FROM _privacy_policy WHERE id=1")
        .fetch_optional(conn)
        .await
}

pub(super) fn row_node(row: &sqlx::sqlite::SqliteRow) -> Result<MemoryNode, sqlx::Error> {
    Ok(MemoryNode {
        id: row.try_get("id")?,
        parent_id: row.try_get("parent_id")?,
        kind: row.try_get("kind")?,
        label: row.try_get("label")?,
        label_origin: row.try_get("label_origin")?,
        start_time: row.try_get("start_time")?,
        end_time: row.try_get("end_time")?,
        provisional: row.try_get("provisional")?,
        evidence_count: row.try_get("evidence_count")?,
        mark_count: row.try_get("mark_count")?,
        apps: serde_json::from_str(&row.try_get::<String, _>("apps_json")?).unwrap_or_default(),
        revision: row.try_get("revision")?,
        fingerprint: row.try_get("fingerprint")?,
        children: Vec::new(),
    })
}

impl DatabaseManager {
    pub(super) async fn memory_reader_ready(&self) -> Result<(), ExplicitMemoryError> {
        let policy = self.release_policy().await?;
        if (policy.text || policy.images) && !self.protected_reads_enabled() {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        Ok(())
    }
    /// Raw epoch, including disabled policies. Derived copies cannot outlive a
    /// later policy change. Unlike release_revocation this is never optimized
    /// to None solely because consumer protection is currently disabled.
    pub async fn memory_index_epoch(&self) -> Result<Option<i64>, ExplicitMemoryError> {
        Ok(epoch(&mut *self.acquire_read().await?).await?)
    }
}
