// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Explicit user annotations. Rare user writes join the existing coordinator;
//! capture never waits for image processing, indexing, or a separate writer.

use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const EXPLICIT_MEMORY_MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MARK_COLUMNS: &str = "m.id,m.frame_id,m.source_frame_id,m.source_timestamp,m.source_app,m.source_window,m.source_url,m.title,m.note,m.importance,m.revision,m.created_at,m.updated_at,m.privacy_epoch,m.image_width,m.image_height,EXISTS(SELECT 1 FROM explicit_memory_assets b WHERE b.mark_id=m.id) AS retained_image";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, oasgen::OaSchema)]
pub struct ExplicitMemoryAnchor {
    pub id: String,
    #[serde(rename = "type")]
    pub anchor_type: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub quote: Option<String>,
    #[serde(default)]
    pub comment: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, oasgen::OaSchema)]
pub struct ExplicitMemoryMark {
    pub id: String,
    pub frame_id: Option<i64>,
    pub source_frame_id: i64,
    pub source_timestamp: String,
    pub source_app: Option<String>,
    pub source_window: Option<String>,
    pub source_url: Option<String>,
    pub title: String,
    pub note: String,
    pub tags: Vec<String>,
    pub importance: f64,
    pub anchors: Vec<ExplicitMemoryAnchor>,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    pub source_state: String,
    pub retained_image: bool,
    pub image_width: Option<i64>,
    pub image_height: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NewExplicitMemoryMark {
    pub id: String,
    pub frame_id: i64,
    pub title: String,
    pub note: String,
    pub tags: Vec<String>,
    pub importance: f64,
    pub anchors: Vec<ExplicitMemoryAnchor>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    /// Taken before source hydration. Never populated from untrusted request JSON.
    pub revocation: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct UpdateExplicitMemoryMark {
    pub revision: i64,
    pub title: String,
    pub note: String,
    pub tags: Vec<String>,
    pub importance: f64,
    pub anchors: Vec<ExplicitMemoryAnchor>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub revocation: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ExplicitMemoryImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub revocation: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct ExplicitMemoryQuery {
    pub q: Option<String>,
    pub frame_id: Option<i64>,
    pub tag: Option<String>,
    pub start_time: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug)]
pub enum ExplicitMemoryError {
    Database(sqlx::Error),
    NotFound,
    Conflict,
    Invalid(String),
    PrivacyChanged,
}
impl std::fmt::Display for ExplicitMemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Database(_) => "explicit memory database operation failed",
            Self::NotFound => "explicit memory source or mark not found",
            Self::Conflict => "explicit memory revision or request conflict",
            Self::Invalid(_) => "invalid explicit memory input",
            Self::PrivacyChanged => "explicit memory privacy processing required",
        })
    }
}
impl std::error::Error for ExplicitMemoryError {}
impl From<sqlx::Error> for ExplicitMemoryError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Copy)]
struct PrivacySnapshot {
    epoch: Option<i64>,
    effective: Option<i64>,
    text: bool,
    images: bool,
}

async fn privacy_snapshot(
    conn: &mut sqlx::SqliteConnection,
) -> Result<PrivacySnapshot, sqlx::Error> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='_privacy_policy')",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !exists {
        return Ok(PrivacySnapshot {
            epoch: None,
            effective: None,
            text: false,
            images: false,
        });
    }
    let (epoch, text, images): (i64, bool, bool) = sqlx::query_as(
        "SELECT revocation,text_required,image_required FROM _privacy_policy WHERE id=1",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(PrivacySnapshot {
        epoch: Some(epoch),
        effective: (text || images).then_some(epoch),
        text,
        images,
    })
}

async fn require_source_release(
    conn: &mut sqlx::SqliteConnection,
    frame_id: i64,
    image: bool,
    privacy: PrivacySnapshot,
) -> Result<(), ExplicitMemoryError> {
    for (kind, required) in [("frames", privacy.text), ("image", image && privacy.images)] {
        if required {
            let ready: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM _privacy_release r JOIN _privacy_policy p ON r.policy=CAST(p.receipt_key AS TEXT) WHERE r.kind=? AND r.id=CAST(? AS TEXT))")
                .bind(kind).bind(frame_id).fetch_one(&mut *conn).await?;
            if !ready {
                return Err(ExplicitMemoryError::PrivacyChanged);
            }
        }
    }
    Ok(())
}

fn invalid(message: &str) -> ExplicitMemoryError {
    ExplicitMemoryError::Invalid(message.into())
}

fn validate_content(
    title: &str,
    note: &str,
    tags: &[String],
    importance: f64,
    anchors: &[ExplicitMemoryAnchor],
) -> Result<(), ExplicitMemoryError> {
    if title.len() > 1024
        || note.len() > 65536
        || !importance.is_finite()
        || !(0.0..=1.0).contains(&importance)
        || tags.len() > 32
        || anchors.len() > 64
    {
        return Err(invalid("annotation exceeds limits"));
    }
    let total_bytes = title.len()
        + note.len()
        + tags.iter().map(String::len).sum::<usize>()
        + anchors
            .iter()
            .map(|a| a.comment.len() + a.quote.as_ref().map_or(0, String::len))
            .sum::<usize>();
    if total_bytes > 128 * 1024 {
        return Err(invalid("annotation text exceeds aggregate limit"));
    }
    let mut ids = std::collections::HashSet::new();
    for tag in tags {
        if tag.trim().is_empty() || tag.len() > 256 {
            return Err(invalid("invalid tag"));
        }
    }
    for a in anchors {
        if a.id.is_empty()
            || a.id.len() > 128
            || !ids.insert(&a.id)
            || a.comment.len() > 8192
            || a.quote.as_ref().is_some_and(|s| s.len() > 32768)
        {
            return Err(invalid("invalid anchor"));
        }
        let geometry = [a.x, a.y, a.width, a.height];
        let has_geometry = geometry.iter().any(Option::is_some);
        if has_geometry {
            let [Some(x), Some(y), Some(w), Some(h)] = geometry else {
                return Err(invalid("incomplete anchor geometry"));
            };
            if ![x, y, w, h].iter().all(|v| v.is_finite())
                || x < 0.0
                || y < 0.0
                || w <= 0.0
                || h <= 0.0
                || x + w > 1.0 + 1e-9
                || y + h > 1.0 + 1e-9
            {
                return Err(invalid("anchor outside source image"));
            }
        }
        match a.anchor_type.as_str() {
            "region" if has_geometry => {}
            "text_quote" if a.quote.as_ref().is_some_and(|q| !q.trim().is_empty()) => {}
            _ => return Err(invalid("unsupported or incomplete anchor")),
        }
    }
    if title.trim().is_empty()
        && note.trim().is_empty()
        && anchors.iter().all(|a| {
            a.comment.trim().is_empty() && a.quote.as_ref().is_none_or(|q| q.trim().is_empty())
        })
    {
        return Err(invalid("annotation is empty"));
    }
    Ok(())
}

fn fingerprint(
    draft: &NewExplicitMemoryMark,
    image: Option<&ExplicitMemoryImage>,
) -> Result<String, ExplicitMemoryError> {
    let mut value =
        serde_json::to_value(draft).map_err(|_| invalid("cannot serialize annotation"))?;
    value
        .as_object_mut()
        .expect("serialized struct")
        .remove("revocation");
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&value).map_err(|_| invalid("cannot serialize annotation"))?);
    if let Some(image) = image {
        hash.update(image.mime_type.as_bytes());
        hash.update(image.width.to_le_bytes());
        hash.update(image.height.to_le_bytes());
        hash.update(&image.bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}

async fn write_children(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    title: &str,
    note: &str,
    tags: &[String],
    anchors: &[ExplicitMemoryAnchor],
) -> Result<(), ExplicitMemoryError> {
    sqlx::query("DELETE FROM explicit_memory_anchors WHERE mark_id=?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM explicit_memory_mark_tags WHERE mark_id=?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    let mut text = format!("{title}\n{note}");
    for (ordinal, a) in anchors.iter().enumerate() {
        let json = serde_json::to_string(a).map_err(|_| invalid("cannot serialize anchor"))?;
        sqlx::query(
            "INSERT INTO explicit_memory_anchors(id,mark_id,ordinal,anchor_json) VALUES(?,?,?,?)",
        )
        .bind(&a.id)
        .bind(id)
        .bind(ordinal as i64)
        .bind(json)
        .execute(&mut *conn)
        .await?;
        text.push('\n');
        text.push_str(&a.comment);
        if let Some(quote) = &a.quote {
            text.push('\n');
            text.push_str(quote);
        }
    }
    for tag in tags {
        let tag = tag.trim();
        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO tags(name) VALUES(?) ON CONFLICT(name) DO NOTHING RETURNING id",
        )
        .bind(tag)
        .fetch_optional(&mut *conn)
        .await?;
        let tag_id = if let Some(id) = inserted {
            sqlx::query("INSERT INTO explicit_memory_owned_tags(tag_id) VALUES(?)")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            id
        } else {
            sqlx::query_scalar("SELECT id FROM tags WHERE name=?")
                .bind(tag)
                .fetch_one(&mut *conn)
                .await?
        };
        sqlx::query("INSERT OR IGNORE INTO explicit_memory_mark_tags(mark_id,tag_id) VALUES(?,?)")
            .bind(id)
            .bind(tag_id)
            .execute(&mut *conn)
            .await?;
        text.push('\n');
        text.push_str(tag);
    }
    sqlx::query("INSERT INTO explicit_memory_search_docs(mark_id,text) VALUES(?,?) ON CONFLICT(mark_id) DO UPDATE SET text=excluded.text")
        .bind(id).bind(text).execute(&mut *conn).await?;
    Ok(())
}

async fn load_mark(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    privacy: PrivacySnapshot,
) -> Result<Option<ExplicitMemoryMark>, ExplicitMemoryError> {
    let sql = format!("SELECT {MARK_COLUMNS} FROM explicit_memory_marks m WHERE m.id=?");
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(None);
    };
    let epoch: Option<i64> = row.try_get("privacy_epoch")?;
    if epoch != privacy.epoch {
        return Err(ExplicitMemoryError::PrivacyChanged);
    }
    let tags:Vec<String>=sqlx::query_scalar("SELECT t.name FROM explicit_memory_mark_tags mt JOIN tags t ON t.id=mt.tag_id WHERE mt.mark_id=? ORDER BY t.name")
        .bind(id).fetch_all(&mut *conn).await?;
    let anchors: Vec<String> = sqlx::query_scalar(
        "SELECT anchor_json FROM explicit_memory_anchors WHERE mark_id=? ORDER BY ordinal",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let anchors = anchors
        .into_iter()
        .map(|json| serde_json::from_str(&json).map_err(|_| invalid("invalid stored anchor")))
        .collect::<Result<Vec<_>, _>>()?;
    let frame_id: Option<i64> = row.try_get("frame_id")?;
    let retained: bool = row.try_get("retained_image")?;
    Ok(Some(ExplicitMemoryMark {
        id: row.try_get("id")?,
        frame_id,
        source_frame_id: row.try_get("source_frame_id")?,
        source_timestamp: row.try_get("source_timestamp")?,
        source_app: row.try_get("source_app")?,
        source_window: row.try_get("source_window")?,
        source_url: row.try_get("source_url")?,
        title: row.try_get("title")?,
        note: row.try_get("note")?,
        tags,
        importance: row.try_get("importance")?,
        anchors,
        revision: row.try_get("revision")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        source_state: if retained {
            "retained"
        } else if frame_id.is_some() {
            "available"
        } else {
            "unavailable"
        }
        .into(),
        retained_image: retained,
        image_width: row.try_get("image_width")?,
        image_height: row.try_get("image_height")?,
    }))
}

impl DatabaseManager {
    pub async fn create_explicit_memory_mark(
        &self,
        draft: &NewExplicitMemoryMark,
        image: Option<&ExplicitMemoryImage>,
    ) -> Result<ExplicitMemoryMark, ExplicitMemoryError> {
        validate_content(
            &draft.title,
            &draft.note,
            &draft.tags,
            draft.importance,
            &draft.anchors,
        )?;
        if uuid::Uuid::parse_str(&draft.id).is_err() || draft.frame_id <= 0 {
            return Err(invalid("invalid mark or frame identifier"));
        }
        match (draft.image_width, draft.image_height) {
            (None, None) if draft.anchors.iter().all(|a| a.x.is_none()) => {}
            (Some(w), Some(h))
                if w > 0
                    && h > 0
                    && w <= 32768
                    && h <= 32768
                    && u64::from(w) * u64::from(h) <= 100_000_000 => {}
            _ => {
                return Err(invalid(
                    "original image geometry is required for region anchors",
                ))
            }
        }
        if let Some(image) = image {
            if image.bytes.is_empty()
                || image.bytes.len() > EXPLICIT_MEMORY_MAX_IMAGE_BYTES
                || !matches!(
                    image.mime_type.as_str(),
                    "image/jpeg" | "image/png" | "image/webp"
                )
                || image.width == 0
                || image.height == 0
                || image.width > 32768
                || image.height > 32768
                || u64::from(image.width) * u64::from(image.height) > 100_000_000
            {
                return Err(invalid("invalid retained image"));
            }
            if image.revocation != draft.revocation {
                return Err(ExplicitMemoryError::PrivacyChanged);
            }
            if draft.image_width != Some(image.width) || draft.image_height != Some(image.height) {
                return Err(invalid("retained image does not match original geometry"));
            }
        }
        let fingerprint = fingerprint(draft, image)?;
        let mut tx = self.begin_immediate_with_retry().await?;
        let privacy = privacy_snapshot(&mut **tx.conn()).await?;
        if draft.revocation != privacy.effective {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let existing: Option<String> =
            sqlx::query_scalar("SELECT request_fingerprint FROM explicit_memory_marks WHERE id=?")
                .bind(&draft.id)
                .fetch_optional(&mut **tx.conn())
                .await?;
        if let Some(existing) = existing {
            if existing != fingerprint {
                return Err(ExplicitMemoryError::Conflict);
            }
            let mark = load_mark(&mut **tx.conn(), &draft.id, privacy)
                .await?
                .ok_or(ExplicitMemoryError::NotFound)?;
            tx.commit().await?;
            return Ok(mark);
        }
        require_source_release(&mut **tx.conn(), draft.frame_id, image.is_some(), privacy).await?;
        let source: Option<(
            DateTime<Utc>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT timestamp,app_name,window_name,browser_url,machine_id FROM frames WHERE id=?",
        )
        .bind(draft.frame_id)
        .fetch_optional(&mut **tx.conn())
        .await?;
        let (timestamp, app, window, url, machine_id) =
            source.ok_or(ExplicitMemoryError::NotFound)?;
        // Local capture leaves machine_id NULL (legacy local rows may use '').
        // Synced evidence has a separate device deletion lifecycle; until marks
        // participate in that lifecycle, never retain or copy remote sources.
        if machine_id.as_ref().is_some_and(|id| !id.is_empty()) {
            return Err(invalid(
                "explicit memory currently supports local frames only",
            ));
        }
        let timestamp = timestamp.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        sqlx::query("INSERT INTO explicit_memory_marks(id,frame_id,source_frame_id,source_timestamp,source_app,source_window,source_url,title,note,importance,privacy_epoch,request_fingerprint,created_at,updated_at,image_width,image_height) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&draft.id).bind(draft.frame_id).bind(draft.frame_id).bind(timestamp).bind(app).bind(window).bind(url).bind(&draft.title).bind(&draft.note).bind(draft.importance).bind(privacy.epoch).bind(fingerprint).bind(&now).bind(&now).bind(draft.image_width.map(i64::from)).bind(draft.image_height.map(i64::from)).execute(&mut **tx.conn()).await?;
        write_children(
            &mut **tx.conn(),
            &draft.id,
            &draft.title,
            &draft.note,
            &draft.tags,
            &draft.anchors,
        )
        .await?;
        if let Some(image) = image {
            sqlx::query("INSERT INTO explicit_memory_assets(mark_id,bytes,mime_type,width,height) VALUES(?,?,?,?,?)")
                .bind(&draft.id).bind(&image.bytes).bind(&image.mime_type).bind(i64::from(image.width)).bind(i64::from(image.height)).execute(&mut **tx.conn()).await?;
        }
        let mark = load_mark(&mut **tx.conn(), &draft.id, privacy)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        tx.commit().await?;
        Ok(mark)
    }

    pub async fn get_explicit_memory_mark(
        &self,
        id: &str,
    ) -> Result<Option<ExplicitMemoryMark>, ExplicitMemoryError> {
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let privacy = privacy_snapshot(&mut tx).await?;
        let mark = load_mark(&mut tx, id, privacy).await?;
        tx.commit().await?;
        Ok(mark)
    }

    pub async fn list_explicit_memory_marks(
        &self,
        query: &ExplicitMemoryQuery,
    ) -> Result<(Vec<ExplicitMemoryMark>, i64), ExplicitMemoryError> {
        let q = query.q.as_deref().unwrap_or("").trim();
        if q.len() > 1024 || query.tag.as_ref().is_some_and(|tag| tag.len() > 256) {
            return Err(invalid("search exceeds limits"));
        }
        if query
            .start_time
            .as_ref()
            .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_err())
        {
            return Err(invalid("invalid history cutoff"));
        }
        let fts = crate::sanitize_fts5_query(q);
        let fts = if fts.is_empty() {
            "\"\"".to_string()
        } else {
            fts
        };
        let limit = if query.limit == 0 {
            50
        } else {
            query.limit.min(100)
        };
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let privacy = privacy_snapshot(&mut tx).await?;
        // The literal substring fallback deliberately covers CJK fragments that
        // unicode61 does not split into independently searchable words.
        let filter="FROM explicit_memory_marks m JOIN explicit_memory_search_docs d ON d.mark_id=m.id WHERE m.privacy_epoch IS ?1 AND (?2 IS NULL OR m.source_frame_id=?2) AND (?3 IS NULL OR EXISTS(SELECT 1 FROM explicit_memory_mark_tags mt JOIN tags t ON t.id=mt.tag_id WHERE mt.mark_id=m.id AND t.name=?3)) AND (?4 IS NULL OR julianday(m.source_timestamp)>=julianday(?4)) AND (?5='' OR d.rowid IN (SELECT rowid FROM explicit_memory_fts WHERE explicit_memory_fts MATCH ?6) OR instr(lower(d.text),lower(?5))>0)";
        let count_sql = format!("SELECT COUNT(*) {filter}");
        let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count_sql))
            .bind(privacy.epoch)
            .bind(query.frame_id)
            .bind(query.tag.as_deref())
            .bind(query.start_time.as_deref())
            .bind(q)
            .bind(&fts)
            .fetch_one(&mut *tx)
            .await?;
        let list_sql =
            format!("SELECT m.id {filter} ORDER BY m.created_at DESC,m.id DESC LIMIT ?7 OFFSET ?8");
        let ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(list_sql))
            .bind(privacy.epoch)
            .bind(query.frame_id)
            .bind(query.tag.as_deref())
            .bind(query.start_time.as_deref())
            .bind(q)
            .bind(&fts)
            .bind(limit)
            .bind(query.offset)
            .fetch_all(&mut *tx)
            .await?;
        let mut marks = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(mark) = load_mark(&mut tx, &id, privacy).await? {
                marks.push(mark);
            }
        }
        tx.commit().await?;
        Ok((marks, total))
    }

    pub async fn update_explicit_memory_mark(
        &self,
        id: &str,
        update: &UpdateExplicitMemoryMark,
    ) -> Result<ExplicitMemoryMark, ExplicitMemoryError> {
        validate_content(
            &update.title,
            &update.note,
            &update.tags,
            update.importance,
            &update.anchors,
        )?;
        let mut tx = self.begin_immediate_with_retry().await?;
        let privacy = privacy_snapshot(&mut **tx.conn()).await?;
        if update.revocation != privacy.effective {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let current = load_mark(&mut **tx.conn(), id, privacy)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        if current.revision != update.revision {
            return Err(ExplicitMemoryError::Conflict);
        }
        let supplied_geometry = match (update.image_width, update.image_height) {
            (None, None) => None,
            (Some(w), Some(h))
                if w > 0
                    && h > 0
                    && w <= 32768
                    && h <= 32768
                    && u64::from(w) * u64::from(h) <= 100_000_000 =>
            {
                Some((i64::from(w), i64::from(h)))
            }
            _ => return Err(invalid("invalid original image geometry")),
        };
        let geometry = match (current.image_width, current.image_height, supplied_geometry) {
            (Some(w), Some(h), Some(supplied)) if supplied != (w, h) => {
                return Err(invalid("original image geometry is immutable"))
            }
            (Some(w), Some(h), _) => Some((w, h)),
            (None, None, Some(supplied)) => {
                let source = current.frame_id.ok_or(ExplicitMemoryError::NotFound)?;
                require_source_release(&mut **tx.conn(), source, true, privacy).await?;
                Some(supplied)
            }
            _ => None,
        };
        if geometry.is_none() && update.anchors.iter().any(|a| a.x.is_some()) {
            return Err(invalid(
                "original image geometry is required for region anchors",
            ));
        }
        let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        sqlx::query("UPDATE explicit_memory_marks SET title=?,note=?,importance=?,revision=revision+1,updated_at=?,image_width=?,image_height=? WHERE id=? AND revision=?")
            .bind(&update.title).bind(&update.note).bind(update.importance).bind(now).bind(geometry.map(|g|g.0)).bind(geometry.map(|g|g.1)).bind(id).bind(update.revision).execute(&mut **tx.conn()).await?;
        write_children(
            &mut **tx.conn(),
            id,
            &update.title,
            &update.note,
            &update.tags,
            &update.anchors,
        )
        .await?;
        let mark = load_mark(&mut **tx.conn(), id, privacy)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        tx.commit().await?;
        Ok(mark)
    }

    pub async fn delete_explicit_memory_mark(
        &self,
        id: &str,
        revision: i64,
    ) -> Result<bool, ExplicitMemoryError> {
        let mut tx = self.begin_immediate_with_retry().await?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM explicit_memory_marks WHERE id=?")
                .bind(id)
                .fetch_optional(&mut **tx.conn())
                .await?;
        let Some(current) = current else {
            return Ok(false);
        };
        if current != revision {
            return Err(ExplicitMemoryError::Conflict);
        }
        sqlx::query("DELETE FROM explicit_memory_marks WHERE id=?")
            .bind(id)
            .execute(&mut **tx.conn())
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn get_explicit_memory_image(
        &self,
        id: &str,
    ) -> Result<Option<ExplicitMemoryImage>, ExplicitMemoryError> {
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let privacy = privacy_snapshot(&mut tx).await?;
        if load_mark(&mut tx, id, privacy).await?.is_none() {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT bytes,mime_type,width,height FROM explicit_memory_assets WHERE mark_id=?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let image = row
            .map(|row| {
                Ok::<_, sqlx::Error>(ExplicitMemoryImage {
                    bytes: row.try_get("bytes")?,
                    mime_type: row.try_get("mime_type")?,
                    width: row.try_get::<i64, _>("width")? as u32,
                    height: row.try_get::<i64, _>("height")? as u32,
                    revocation: privacy.effective,
                })
            })
            .transpose()?;
        tx.commit().await?;
        Ok(image)
    }
}

/// Explicit erasure applies even after the original raw frame expired. Ambient
/// retention intentionally does not call this helper.
pub(super) async fn delete_explicit_memory_in_range(
    conn: &mut sqlx::SqliteConnection,
    start: &str,
    end: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM explicit_memory_marks WHERE julianday(source_timestamp) BETWEEN julianday(?) AND julianday(?)")
        .bind(start).bind(end).execute(conn).await?;
    Ok(())
}

/// Snapshot only inside the existing retention writer transaction. A policy
/// replacement cannot interleave with that transaction; the receipt key still
/// makes the narrow trust boundary explicit and guards future refactors.
pub(super) async fn explicit_memory_retention_epoch(
    conn: &mut sqlx::SqliteConnection,
) -> Result<Option<(i64, i64)>, sqlx::Error> {
    let privacy = privacy_snapshot(conn).await?;
    let Some(epoch) = privacy.epoch else {
        return Ok(None);
    };
    let generation: i64 = sqlx::query_scalar("SELECT receipt_key FROM _privacy_policy WHERE id=1")
        .fetch_one(conn)
        .await?;
    Ok(Some((epoch, generation)))
}

/// Ordinary expiration revokes raw-frame receipts without changing the saved
/// pixels or authored notes. Carry forward only marks that were current before
/// this trusted retention transaction. Stale marks can never become readable.
/// This is one indexed epoch-only update per retention batch, not a frame
/// trigger; it copies no payloads and adds no work to capture admission.
pub(super) async fn preserve_explicit_memory_after_retention(
    conn: &mut sqlx::SqliteConnection,
    before: Option<(i64, i64)>,
) -> Result<(), sqlx::Error> {
    let Some((before_epoch, before_generation)) = before else {
        return Ok(());
    };
    let (after_epoch, after_generation): (i64, i64) =
        sqlx::query_as("SELECT revocation,receipt_key FROM _privacy_policy WHERE id=1")
            .fetch_one(&mut *conn)
            .await?;
    if before_epoch != after_epoch && before_generation == after_generation {
        sqlx::query("UPDATE explicit_memory_marks SET privacy_epoch=? WHERE privacy_epoch=?")
            .bind(after_epoch)
            .bind(before_epoch)
            .execute(conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenpipe_config::DbConfig;

    async fn test_db() -> (DatabaseManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = DatabaseManager::new(
            dir.path().join("memory.sqlite").to_str().unwrap(),
            DbConfig::default(),
        )
        .await
        .unwrap();
        (db, dir)
    }
    fn at(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }
    async fn frame(db: &DatabaseManager, timestamp: &str) -> i64 {
        let mut tx = db.begin_immediate_with_retry().await.unwrap();
        let id=sqlx::query_scalar("INSERT INTO frames(timestamp,app_name,window_name,browser_url) VALUES(?,'Browser','Design article','https://example.test/article') RETURNING id")
            .bind(timestamp).fetch_one(&mut **tx.conn()).await.unwrap();
        tx.commit().await.unwrap();
        id
    }
    fn draft(frame_id: i64) -> NewExplicitMemoryMark {
        NewExplicitMemoryMark {
            id: uuid::Uuid::new_v4().to_string(),
            frame_id,
            title: "Indexing idea".into(),
            note: "把增量索引方案用到项目甲".into(),
            tags: vec!["project:a".into()],
            importance: 0.9,
            anchors: vec![
                ExplicitMemoryAnchor {
                    id: "region-1".into(),
                    anchor_type: "region".into(),
                    x: Some(0.2),
                    y: Some(0.3),
                    width: Some(0.4),
                    height: Some(0.1),
                    quote: None,
                    comment: "Preserve evidence".into(),
                },
                ExplicitMemoryAnchor {
                    id: "quote-1".into(),
                    anchor_type: "text_quote".into(),
                    x: None,
                    y: None,
                    width: None,
                    height: None,
                    quote: Some("bounded reconciliation".into()),
                    comment: String::new(),
                },
            ],
            image_width: Some(100),
            image_height: Some(50),
            revocation: None,
        }
    }
    fn image() -> ExplicitMemoryImage {
        ExplicitMemoryImage {
            bytes: vec![1, 2, 3, 4],
            mime_type: "image/png".into(),
            width: 100,
            height: 50,
            revocation: None,
        }
    }
    fn query(q: &str) -> ExplicitMemoryQuery {
        ExplicitMemoryQuery {
            q: Some(q.into()),
            limit: 50,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn mark_search_update_and_delete_are_atomic() {
        let (db, _dir) = test_db().await;
        let frame = frame(&db, "2026-10-24T10:00:00Z").await;
        let draft = draft(frame);
        let mark = db
            .create_explicit_memory_mark(&draft, Some(&image()))
            .await
            .unwrap();
        assert_eq!(mark.revision, 1);
        assert_eq!(mark.source_state, "retained");
        assert_eq!(mark.anchors, draft.anchors);
        for term in [
            "Indexing",
            "增量索引",
            "Preserve",
            "reconciliation",
            "project:a",
        ] {
            let (found, total) = db.list_explicit_memory_marks(&query(term)).await.unwrap();
            assert_eq!(total, 1, "term {term}");
            assert_eq!(found[0].id, mark.id);
        }
        let (found, total) = db
            .list_explicit_memory_marks(&ExplicitMemoryQuery {
                tag: Some("project:other".into()),
                ..query("")
            })
            .await
            .unwrap();
        assert!(found.is_empty());
        assert_eq!(total, 0);
        let update = UpdateExplicitMemoryMark {
            revision: 1,
            title: "New decision".into(),
            note: "新的实现方向".into(),
            tags: vec!["project:b".into()],
            importance: 0.5,
            anchors: vec![],
            image_width: None,
            image_height: None,
            revocation: None,
        };
        let updated = db
            .update_explicit_memory_mark(&mark.id, &update)
            .await
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(
            db.list_explicit_memory_marks(&query("增量索引"))
                .await
                .unwrap()
                .1,
            0
        );
        assert_eq!(
            db.list_explicit_memory_marks(&query("实现方向"))
                .await
                .unwrap()
                .1,
            1
        );
        assert!(matches!(
            db.update_explicit_memory_mark(&mark.id, &update).await,
            Err(ExplicitMemoryError::Conflict)
        ));
        assert!(matches!(
            db.delete_explicit_memory_mark(&mark.id, 1).await,
            Err(ExplicitMemoryError::Conflict)
        ));
        assert!(db.delete_explicit_memory_mark(&mark.id, 2).await.unwrap());
        assert!(db
            .get_explicit_memory_mark(&mark.id)
            .await
            .unwrap()
            .is_none());
        for table in [
            "explicit_memory_assets",
            "explicit_memory_anchors",
            "explicit_memory_search_docs",
            "explicit_memory_mark_tags",
        ] {
            let count: i64 =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                    .fetch_one(&db.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        assert_eq!(
            db.list_explicit_memory_marks(&query("实现方向"))
                .await
                .unwrap()
                .1,
            0
        );
    }

    #[tokio::test]
    async fn repeated_request_is_idempotent_but_changed_payload_conflicts() {
        let (db, _dir) = test_db().await;
        let id = frame(&db, "2026-10-24T10:00:00Z").await;
        let mut draft = draft(id);
        let image = image();
        let first = db
            .create_explicit_memory_mark(&draft, Some(&image))
            .await
            .unwrap();
        let retry = db
            .create_explicit_memory_mark(&draft, Some(&image))
            .await
            .unwrap();
        assert_eq!(first.id, retry.id);
        assert_eq!(retry.revision, 1);
        draft.note = "different request".into();
        assert!(matches!(
            db.create_explicit_memory_mark(&draft, Some(&image)).await,
            Err(ExplicitMemoryError::Conflict)
        ));
        assert_eq!(
            db.list_explicit_memory_marks(&query("")).await.unwrap().1,
            1
        );
    }

    #[tokio::test]
    async fn failed_annotations_do_not_interrupt_durable_recording() {
        let (db, _dir) = test_db().await;
        let id = frame(&db, "2026-10-24T10:00:00Z").await;
        let mut invalid = draft(id);
        invalid.anchors[0].x = Some(0.9);
        assert!(matches!(
            db.create_explicit_memory_mark(&invalid, None).await,
            Err(ExplicitMemoryError::Invalid(_))
        ));
        let missing = draft(99999);
        assert!(matches!(
            db.create_explicit_memory_mark(&missing, None).await,
            Err(ExplicitMemoryError::NotFound)
        ));
        db.insert_video_chunk("recording.mp4", "monitor")
            .await
            .unwrap();
        let next = db
            .insert_frame(
                "monitor",
                Some(at("2026-10-24T10:01:00Z")),
                None,
                Some("Editor"),
                Some("main.rs"),
                true,
                Some(0),
            )
            .await
            .unwrap();
        assert!(next > 0);
        assert!(db.get_frame_timestamp(next).await.unwrap().is_some());
        assert_eq!(
            db.list_explicit_memory_marks(&query("")).await.unwrap().1,
            0
        );
    }

    #[tokio::test]
    async fn retained_source_survives_ambient_retention_and_restart_but_explicit_erasure_revokes_it(
    ) {
        let (db, dir) = test_db().await;
        let id = frame(&db, "2026-10-20T10:00:00Z").await;
        let retained = db
            .create_explicit_memory_mark(&draft(id), Some(&image()))
            .await
            .unwrap();
        let unretained = db
            .create_explicit_memory_mark(&draft(id), None)
            .await
            .unwrap();
        assert!(!unretained.retained_image);
        assert_eq!(unretained.image_width, Some(100));
        assert_eq!(unretained.image_height, Some(50));
        let start = at("2026-10-20T09:00:00Z");
        let end = at("2026-10-20T11:00:00Z");
        db.delete_retention_time_slice(start, start, end)
            .await
            .unwrap();
        assert!(db.get_frame_timestamp(id).await.unwrap().is_none());
        let saved = db
            .get_explicit_memory_mark(&retained.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.frame_id, None);
        assert_eq!(saved.source_frame_id, id);
        assert_eq!(saved.source_state, "retained");
        assert_eq!(
            db.get_explicit_memory_mark(&unretained.id)
                .await
                .unwrap()
                .unwrap()
                .source_state,
            "unavailable"
        );
        assert_eq!(
            db.get_explicit_memory_image(&saved.id)
                .await
                .unwrap()
                .unwrap()
                .bytes,
            image().bytes
        );
        db.close().await;
        drop(db);
        let reopened = DatabaseManager::new(
            dir.path().join("memory.sqlite").to_str().unwrap(),
            DbConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            reopened
                .list_explicit_memory_marks(&query("增量索引"))
                .await
                .unwrap()
                .1,
            2
        );
        assert!(reopened
            .get_explicit_memory_image(&saved.id)
            .await
            .unwrap()
            .is_some());
        reopened.delete_time_range_local(start, end).await.unwrap();
        assert_eq!(
            reopened
                .list_explicit_memory_marks(&query(""))
                .await
                .unwrap()
                .1,
            0
        );
        let assets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM explicit_memory_assets")
            .fetch_one(&reopened.pool)
            .await
            .unwrap();
        assert_eq!(assets, 0);
    }

    #[tokio::test]
    async fn privacy_change_hides_copied_evidence_without_destroying_authored_notes() {
        let (db, _dir) = test_db().await;
        let id = frame(&db, "2026-10-24T10:00:00Z").await;
        let draft = draft(id);
        let mark = db
            .create_explicit_memory_mark(&draft, Some(&image()))
            .await
            .unwrap();
        db.configure_release(&privacy_release::ReleasePolicy {
            identity: "strict-v1".into(),
            text: true,
            images: true,
        })
        .await
        .unwrap();
        assert!(matches!(
            db.get_explicit_memory_mark(&mark.id).await,
            Err(ExplicitMemoryError::PrivacyChanged)
        ));
        assert!(matches!(
            db.get_explicit_memory_image(&mark.id).await,
            Err(ExplicitMemoryError::PrivacyChanged)
        ));
        assert_eq!(
            db.list_explicit_memory_marks(&query("增量索引"))
                .await
                .unwrap()
                .1,
            0
        );
        let note: String = sqlx::query_scalar("SELECT note FROM explicit_memory_marks WHERE id=?")
            .bind(&mark.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(note, draft.note);
        let protected = Arc::new(db).protected_reads().await.unwrap();
        assert!(protected
            .autocomplete_tags("project:a", 50, 0)
            .await
            .unwrap()
            .is_empty());
        assert!(protected
            .query_raw_sql("SELECT hex(bytes) FROM explicit_memory_assets")
            .await
            .is_err());
        assert!(protected
            .query_raw_sql("SELECT text FROM explicit_memory_search_docs")
            .await
            .is_err());
        assert!(protected
            .delete_explicit_memory_mark(&mark.id, 1)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn pending_source_privacy_and_stale_capture_tokens_cannot_create_marks() {
        let (db, _dir) = test_db().await;
        db.configure_release(&privacy_release::ReleasePolicy {
            identity: "strict-v1".into(),
            text: true,
            images: true,
        })
        .await
        .unwrap();
        let id = frame(&db, "2026-10-24T10:00:00Z").await;
        let mut draft = draft(id);
        assert!(matches!(
            db.create_explicit_memory_mark(&draft, None).await,
            Err(ExplicitMemoryError::PrivacyChanged)
        ));
        draft.revocation = db.release_revocation().await.unwrap();
        assert!(matches!(
            db.create_explicit_memory_mark(&draft, None).await,
            Err(ExplicitMemoryError::PrivacyChanged)
        ));
        assert_eq!(
            db.list_explicit_memory_marks(&query("")).await.unwrap().1,
            0
        );
    }

    #[tokio::test]
    async fn synced_sources_are_rejected_without_interrupting_local_capture() {
        let (db, _dir) = test_db().await;
        let remote = frame(&db, "2026-10-24T10:00:00Z").await;
        let local = frame(&db, "2026-10-24T10:01:00Z").await;
        let mut tx = db.begin_immediate_with_retry().await.unwrap();
        sqlx::query("UPDATE frames SET machine_id='remote-device' WHERE id=?")
            .bind(remote)
            .execute(&mut **tx.conn())
            .await
            .unwrap();
        sqlx::query("UPDATE frames SET machine_id='' WHERE id=?")
            .bind(local)
            .execute(&mut **tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut annotation = draft(remote);
        annotation.anchors.clear();
        annotation.image_width = None;
        annotation.image_height = None;
        assert!(matches!(
            db.create_explicit_memory_mark(&annotation, None).await,
            Err(ExplicitMemoryError::Invalid(_))
        ));
        annotation.id = uuid::Uuid::new_v4().to_string();
        annotation.frame_id = local;
        assert!(db
            .create_explicit_memory_mark(&annotation, None)
            .await
            .is_ok());
        db.insert_video_chunk("local-capture-after-remote-rejection.mp4", "monitor")
            .await
            .unwrap();
        let captured = db
            .insert_frame(
                "monitor",
                Some(Utc::now()),
                None,
                Some("Editor"),
                Some("main.rs"),
                true,
                Some(0),
            )
            .await
            .unwrap();
        assert!(captured > 0);
        assert!(db.get_frame_timestamp(captured).await.unwrap().is_some());
        assert_eq!(
            db.list_explicit_memory_marks(&query("")).await.unwrap().1,
            1
        );
    }

    #[tokio::test]
    async fn protected_retention_keeps_current_saved_evidence_but_never_revives_stale_marks() {
        let (db, dir) = test_db().await;
        let id = frame(&db, "2020-01-01T10:00:00Z").await;
        let stale = db
            .create_explicit_memory_mark(&draft(id), None)
            .await
            .unwrap();
        db.configure_release(&privacy_release::ReleasePolicy {
            identity: "strict-retention-v1".into(),
            text: true,
            images: true,
        })
        .await
        .unwrap();
        for job in db.release_pending("frames", 10).await.unwrap() {
            assert!(db.publish_release(&job, &job.values).await.unwrap());
        }
        for job in db.release_pending("image", 10).await.unwrap() {
            assert!(db.publish_image_release(&job).await.unwrap());
        }
        let mut source = draft(id);
        source.revocation = db.release_revocation().await.unwrap();
        let mut pixels = image();
        pixels.revocation = source.revocation;
        let saved = db
            .create_explicit_memory_mark(&source, Some(&pixels))
            .await
            .unwrap();
        let before = db.release_revocation().await.unwrap().unwrap();
        let start = at("2020-01-01T09:00:00Z");
        let end = at("2020-01-01T11:00:00Z");
        db.delete_retention_time_slice(start, start, end)
            .await
            .unwrap();
        assert!(db.release_revocation().await.unwrap().unwrap() > before);
        assert!(matches!(
            db.get_explicit_memory_mark(&stale.id).await,
            Err(ExplicitMemoryError::PrivacyChanged)
        ));
        let db = Arc::new(db);
        let protected = db.protected_reads().await.unwrap();
        assert_eq!(
            protected
                .get_explicit_memory_image(&saved.id)
                .await
                .unwrap()
                .unwrap()
                .bytes,
            pixels.bytes
        );
        assert_eq!(
            protected
                .list_explicit_memory_marks(&query("增量索引"))
                .await
                .unwrap()
                .1,
            1
        );
        // The retention/annotation writer still admits a following normal capture.
        db.insert_video_chunk("recording-after-retention.mp4", "monitor")
            .await
            .unwrap();
        let recorded = db
            .insert_frame(
                "monitor",
                Some(Utc::now()),
                None,
                Some("Editor"),
                Some("main.rs"),
                true,
                Some(0),
            )
            .await
            .unwrap();
        assert!(recorded > 0);
        assert!(db.get_frame_timestamp(recorded).await.unwrap().is_some());
        drop(protected);
        db.close().await;
        drop(db);
        let reopened = Arc::new(
            DatabaseManager::new(
                dir.path().join("memory.sqlite").to_str().unwrap(),
                DbConfig::default(),
            )
            .await
            .unwrap(),
        );
        let protected = reopened.protected_reads().await.unwrap();
        assert!(protected
            .get_explicit_memory_image(&saved.id)
            .await
            .unwrap()
            .is_some());
        assert!(matches!(
            protected.get_explicit_memory_mark(&stale.id).await,
            Err(ExplicitMemoryError::PrivacyChanged)
        ));
        reopened.delete_time_range_local(start, end).await.unwrap();
        assert!(protected
            .get_explicit_memory_mark(&saved.id)
            .await
            .unwrap()
            .is_none());
        assert!(protected
            .get_explicit_memory_image(&saved.id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn deleting_marks_cleans_only_tags_introduced_by_explicit_memory() {
        let (db, _dir) = test_db().await;
        let id = frame(&db, "2026-10-24T10:00:00Z").await;
        let mut tx = db.begin_immediate_with_retry().await.unwrap();
        sqlx::query("INSERT INTO tags(name) VALUES('existing-unused')")
            .execute(&mut **tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut source = draft(id);
        source.tags = vec![
            "explicit-only".into(),
            "shared-vision".into(),
            "existing-unused".into(),
        ];
        let mark = db.create_explicit_memory_mark(&source, None).await.unwrap();
        db.add_tags(
            id,
            crate::TagContentType::Vision,
            vec!["shared-vision".into()],
        )
        .await
        .unwrap();
        assert_eq!(
            db.autocomplete_tags("explicit-only", 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        db.delete_explicit_memory_mark(&mark.id, 1).await.unwrap();
        assert!(db
            .autocomplete_tags("explicit-only", 50, 0)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            db.autocomplete_tags("existing-unused", 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.autocomplete_tags("shared-vision", 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_note_only_mark_can_gain_trusted_geometry_without_replacing_it_later() {
        let (db, _dir) = test_db().await;
        let id = frame(&db, "2026-10-24T10:00:00Z").await;
        let mut source = draft(id);
        source.anchors.clear();
        source.image_width = None;
        source.image_height = None;
        let mark = db.create_explicit_memory_mark(&source, None).await.unwrap();
        let mut update = UpdateExplicitMemoryMark {
            revision: 1,
            title: mark.title,
            note: mark.note,
            tags: mark.tags,
            importance: mark.importance,
            anchors: draft(id).anchors,
            image_width: Some(100),
            image_height: Some(50),
            revocation: None,
        };
        let updated = db
            .update_explicit_memory_mark(&mark.id, &update)
            .await
            .unwrap();
        assert_eq!(updated.image_width, Some(100));
        assert_eq!(updated.image_height, Some(50));
        update.revision = 2;
        update.image_width = Some(200);
        assert!(matches!(
            db.update_explicit_memory_mark(&mark.id, &update).await,
            Err(ExplicitMemoryError::Invalid(_))
        ));
        update.image_width = None;
        update.image_height = None;
        update.revocation = Some(999);
        assert!(matches!(
            db.update_explicit_memory_mark(&mark.id, &update).await,
            Err(ExplicitMemoryError::PrivacyChanged)
        ));
    }

    #[tokio::test]
    async fn legacy_source_timestamps_are_normalized_to_utc_rfc3339() {
        let (db, _dir) = test_db().await;
        let id = frame(&db, "2026-10-24 18:00:00+08:00").await;
        let mark = db
            .create_explicit_memory_mark(&draft(id), None)
            .await
            .unwrap();
        assert_eq!(mark.source_timestamp, "2026-10-24T10:00:00Z");
        assert!(DateTime::parse_from_rfc3339(&mark.source_timestamp).is_ok());
    }

    #[tokio::test]
    async fn history_filter_is_applied_before_pagination_and_count() {
        let (db, _dir) = test_db().await;
        let old = frame(&db, "2026-10-20T10:00:00Z").await;
        let recent = frame(&db, "2026-10-24T10:00:00Z").await;
        db.create_explicit_memory_mark(&draft(old), None)
            .await
            .unwrap();
        let mark = db
            .create_explicit_memory_mark(&draft(recent), None)
            .await
            .unwrap();
        let (marks, total) = db
            .list_explicit_memory_marks(&ExplicitMemoryQuery {
                start_time: Some("2026-10-23T10:00:00Z".into()),
                limit: 1,
                ..query("")
            })
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(marks[0].id, mark.id);
    }
}
