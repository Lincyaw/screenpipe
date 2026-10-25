// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Bounded, durable receipts for intentional captures. Only explicit requests
//! enter this journal; normal capture keeps its existing sole writer unchanged.

use super::explicit_memory::ExplicitMemoryError;
use super::*;
use serde::{Deserialize, Serialize};

const PENDING_SECONDS: i64 = 30;
const TERMINAL_SECONDS: i64 = 24 * 60 * 60;
const MAX_PENDING: i64 = 16;
const MAX_TERMINAL: i64 = 2048;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarkedCaptureRequest {
    pub request_id: String,
    pub monitor_id: u32,
    pub state: String,
    pub code: Option<String>,
    pub frame_id: Option<i64>,
    pub timestamp: Option<String>,
    pub app_name: Option<String>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Clone, Debug)]
pub struct MarkedCaptureOutcome {
    pub state: String,
    pub code: Option<String>,
    pub frame_id: Option<i64>,
    pub timestamp: Option<String>,
    pub app_name: Option<String>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    /// Trusted snapshot from before exact protected-image hydration.
    pub revocation: Option<i64>,
}

fn invalid(message: &str) -> ExplicitMemoryError {
    ExplicitMemoryError::Invalid(message.to_string())
}

async fn privacy_epoch(conn: &mut sqlx::SqliteConnection) -> Result<Option<i64>, sqlx::Error> {
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

fn iso(seconds: i64) -> Result<String, ExplicitMemoryError> {
    DateTime::from_timestamp(seconds, 0)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .ok_or_else(|| invalid("invalid_capture_request_timestamp"))
}

async fn load(
    conn: &mut sqlx::SqliteConnection,
    request_id: &str,
) -> Result<Option<MarkedCaptureRequest>, ExplicitMemoryError> {
    let Some(row) = sqlx::query("SELECT * FROM marked_capture_requests WHERE request_id=?")
        .bind(request_id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(None);
    };
    let epoch: Option<i64> = row.try_get("privacy_epoch")?;
    let current_epoch = privacy_epoch(conn).await?;
    let state: String = row.try_get("state")?;
    if state == "completed" && epoch != current_epoch {
        return Err(ExplicitMemoryError::PrivacyChanged);
    }
    let monitor: i64 = row.try_get("monitor_id")?;
    let width: Option<i64> = row.try_get("image_width")?;
    let height: Option<i64> = row.try_get("image_height")?;
    Ok(Some(MarkedCaptureRequest {
        request_id: row.try_get("request_id")?,
        monitor_id: u32::try_from(monitor).map_err(|_| invalid("invalid_capture_monitor"))?,
        state,
        code: row.try_get("code")?,
        frame_id: row.try_get("frame_id")?,
        timestamp: row.try_get("timestamp")?,
        // Completion certifies one moment, not future privacy policy changes.
        // Pixel release always remains the authenticated frame API's job.
        app_name: if epoch == current_epoch {
            row.try_get("app_name")?
        } else {
            None
        },
        image_width: width
            .map(u32::try_from)
            .transpose()
            .map_err(|_| invalid("invalid_capture_geometry"))?,
        image_height: height
            .map(u32::try_from)
            .transpose()
            .map_err(|_| invalid("invalid_capture_geometry"))?,
        created_at: iso(row.try_get("created_at")?)?,
        expires_at: iso(row.try_get("expires_at")?)?,
    }))
}

async fn maintain(
    conn: &mut sqlx::SqliteConnection,
    now: i64,
    keep: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE marked_capture_requests SET state='failed',code='timeout',updated_at=?,expires_at=? WHERE state='pending' AND expires_at<=?")
        .bind(now).bind(now+TERMINAL_SECONDS).bind(now).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM marked_capture_requests WHERE state!='pending' AND expires_at<=?")
        .bind(now)
        .execute(&mut *conn)
        .await?;
    // Keep the newest terminal receipts; never evict accepted pending work.
    sqlx::query("DELETE FROM marked_capture_requests WHERE request_id IN (SELECT request_id FROM marked_capture_requests WHERE state!='pending' ORDER BY (request_id IS ?) DESC,updated_at DESC,request_id DESC LIMIT -1 OFFSET ?)")
        .bind(keep).bind(MAX_TERMINAL).execute(conn).await?;
    Ok(())
}

impl DatabaseManager {
    /// Start once per engine queue instance, before accepting requests. Native
    /// startup failure or shutdown cannot leave a durable pending success claim.
    pub async fn interrupt_pending_marked_captures(&self) -> Result<u64, ExplicitMemoryError> {
        let now = Utc::now().timestamp();
        let mut tx = self.begin_immediate_with_retry().await?;
        let result = sqlx::query("UPDATE marked_capture_requests SET state='interrupted',code='capture_interrupted',updated_at=?,expires_at=? WHERE state='pending'")
            .bind(now).bind(now+TERMINAL_SECONDS).execute(&mut **tx.conn()).await?;
        maintain(&mut **tx.conn(), now, None).await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }

    pub async fn reserve_marked_capture(
        &self,
        request_id: &str,
        selector: Option<u32>,
        monitor_id: u32,
    ) -> Result<(MarkedCaptureRequest, bool), ExplicitMemoryError> {
        if uuid::Uuid::parse_str(request_id).is_err() {
            return Err(invalid("invalid_capture_request_id"));
        }
        let now = Utc::now().timestamp();
        let mut tx = self.begin_immediate_with_retry().await?;
        maintain(&mut **tx.conn(), now, None).await?;
        let existing: Option<(Option<i64>, i64)> = sqlx::query_as(
            "SELECT selector,monitor_id FROM marked_capture_requests WHERE request_id=?",
        )
        .bind(request_id)
        .fetch_optional(&mut **tx.conn())
        .await?;
        if let Some((existing_selector, _resolved_monitor)) = existing {
            if existing_selector != selector.map(i64::from) {
                return Err(ExplicitMemoryError::Conflict);
            }
            let row = load(&mut **tx.conn(), request_id)
                .await?
                .ok_or(ExplicitMemoryError::NotFound)?;
            tx.commit().await?;
            return Ok((row, false));
        }
        if selector.is_some_and(|requested| requested != monitor_id) {
            return Err(invalid("capture_monitor_mismatch"));
        }
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM marked_capture_requests WHERE state='pending'",
        )
        .fetch_one(&mut **tx.conn())
        .await?;
        if pending >= MAX_PENDING {
            return Err(invalid("capture_request_capacity"));
        }
        sqlx::query("INSERT INTO marked_capture_requests(request_id,selector,monitor_id,state,created_at,updated_at,expires_at) VALUES(?,?,?,'pending',?,?,?)")
            .bind(request_id).bind(selector.map(i64::from)).bind(i64::from(monitor_id))
            .bind(now).bind(now).bind(now+PENDING_SECONDS).execute(&mut **tx.conn()).await?;
        let row = load(&mut **tx.conn(), request_id)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        tx.commit().await?;
        Ok((row, true))
    }

    pub async fn get_marked_capture(
        &self,
        request_id: &str,
    ) -> Result<Option<MarkedCaptureRequest>, ExplicitMemoryError> {
        let now = Utc::now().timestamp();
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let row = load(&mut tx, request_id).await?;
        tx.commit().await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let expiry = DateTime::parse_from_rfc3339(&row.expires_at)
            .map_err(|_| invalid("invalid_capture_request_timestamp"))?
            .timestamp();
        if expiry > now {
            return Ok(Some(row));
        }
        if row.state != "pending" {
            return Ok(None);
        }
        // Polling pending receipts is read-only until their first expiry.
        let expired = self
            .finish_marked_capture(
                request_id,
                &MarkedCaptureOutcome {
                    state: "failed".into(),
                    code: Some("timeout".into()),
                    frame_id: None,
                    timestamp: None,
                    app_name: None,
                    image_width: None,
                    image_height: None,
                    revocation: None,
                },
            )
            .await?;
        Ok(Some(expired))
    }

    pub async fn finish_marked_capture(
        &self,
        request_id: &str,
        outcome: &MarkedCaptureOutcome,
    ) -> Result<MarkedCaptureRequest, ExplicitMemoryError> {
        if !matches!(
            outcome.state.as_str(),
            "completed" | "failed" | "blocked" | "cancelled" | "interrupted"
        ) || outcome.code.as_ref().is_some_and(|code| {
            code.len() > 128
                || !code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }) {
            return Err(invalid("invalid_capture_request_outcome"));
        }
        let now = Utc::now().timestamp();
        let mut tx = self.begin_immediate_with_retry().await?;
        // Timeout wins over a late screenshot completion, exactly once.
        sqlx::query("UPDATE marked_capture_requests SET state='failed',code='timeout',updated_at=?,expires_at=? WHERE request_id=? AND state='pending' AND expires_at<=?")
            .bind(now).bind(now+TERMINAL_SECONDS).bind(request_id).bind(now).execute(&mut **tx.conn()).await?;
        let current = load(&mut **tx.conn(), request_id)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        if current.state != "pending" {
            tx.commit().await?;
            return Ok(current);
        }
        let mut frame_id = None;
        let mut timestamp = None;
        let mut app_name = None;
        let mut width = None;
        let mut height = None;
        let epoch = privacy_epoch(&mut **tx.conn()).await?;
        if outcome.state == "completed" {
            let current_revocation = if epoch.is_some() {
                let required: bool = sqlx::query_scalar(
                    "SELECT text_required OR image_required FROM _privacy_policy WHERE id=1",
                )
                .fetch_one(&mut **tx.conn())
                .await?;
                if required {
                    epoch
                } else {
                    None
                }
            } else {
                None
            };
            if current_revocation != outcome.revocation {
                return Err(ExplicitMemoryError::PrivacyChanged);
            }
            let id = outcome
                .frame_id
                .filter(|id| *id > 0)
                .ok_or_else(|| invalid("invalid_capture_frame"))?;
            let (Some(w), Some(h)) = (outcome.image_width, outcome.image_height) else {
                return Err(invalid("invalid_capture_geometry"));
            };
            if w == 0
                || h == 0
                || w > 32768
                || h > 32768
                || u64::from(w) * u64::from(h) > 100_000_000
            {
                return Err(invalid("invalid_capture_geometry"));
            }
            let source: Option<(DateTime<Utc>,String,Option<String>)> = sqlx::query_as("SELECT timestamp,COALESCE(snapshot_path,''),app_name FROM frames WHERE id=? AND (machine_id IS NULL OR machine_id='')")
                .bind(id).fetch_optional(&mut **tx.conn()).await?;
            let (at, path, app) = source.ok_or(ExplicitMemoryError::NotFound)?;
            if path.is_empty() {
                return Err(invalid("capture_pixels_unavailable"));
            }
            if epoch.is_some() {
                let (text, images): (bool, bool) = sqlx::query_as(
                    "SELECT text_required,image_required FROM _privacy_policy WHERE id=1",
                )
                .fetch_one(&mut **tx.conn())
                .await?;
                for (kind, required) in [("frames", text), ("image", images)] {
                    if required {
                        let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM _privacy_release r JOIN _privacy_policy p ON r.policy=CAST(p.receipt_key AS TEXT) WHERE r.kind=? AND r.id=CAST(? AS TEXT))")
                            .bind(kind).bind(id).fetch_one(&mut **tx.conn()).await?;
                        if !ready {
                            return Err(ExplicitMemoryError::PrivacyChanged);
                        }
                    }
                }
            }
            frame_id = Some(id);
            timestamp = Some(at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true));
            app_name = app.map(|name| name.chars().take(256).collect::<String>());
            width = Some(i64::from(w));
            height = Some(i64::from(h));
        }
        sqlx::query("UPDATE marked_capture_requests SET state=?,code=?,frame_id=?,timestamp=?,app_name=?,image_width=?,image_height=?,privacy_epoch=?,updated_at=?,expires_at=? WHERE request_id=? AND state='pending'")
            .bind(&outcome.state).bind(&outcome.code).bind(frame_id).bind(timestamp).bind(app_name).bind(width).bind(height).bind(epoch).bind(now).bind(now+TERMINAL_SECONDS).bind(request_id)
            .execute(&mut **tx.conn()).await?;
        let row = load(&mut **tx.conn(), request_id)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        // At most 2048 terminal rows, even if requests finish without another
        // reservation. The current (newest) result remains in the journal.
        maintain(&mut **tx.conn(), now, Some(request_id)).await?;
        tx.commit().await?;
        Ok(row)
    }
}
