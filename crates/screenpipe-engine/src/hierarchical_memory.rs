// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Bounded, restartable hierarchy generation over durable captures. No model,
//! capture callback or second database writer participates in this worker.

use chrono::{DateTime, Duration, Local, Utc};
use screenpipe_db::{DatabaseManager, ExplicitMemoryError, MemoryIndexJob};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};

struct Entry {
    db: Mutex<Weak<DatabaseManager>>,
}
static WORKERS: OnceLock<Mutex<HashMap<PathBuf, Arc<Entry>>>> = OnceLock::new();
struct Registration(PathBuf, Arc<Entry>);
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut workers) = WORKERS.get_or_init(Mutex::default).lock() {
            if workers
                .get(&self.0)
                .is_some_and(|current| Arc::ptr_eq(current, &self.1))
            {
                workers.remove(&self.0);
            }
        }
    }
}

fn code(error: &ExplicitMemoryError) -> String {
    match error {
        ExplicitMemoryError::PrivacyChanged => "privacy_pending".into(),
        ExplicitMemoryError::Conflict => "index_revision_changed".into(),
        ExplicitMemoryError::NotFound => "index_reference_missing".into(),
        ExplicitMemoryError::Invalid(message)
            if message.len() <= 96
                && message
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()) =>
        {
            message.clone()
        }
        ExplicitMemoryError::Invalid(_) => "index_budget_or_input".into(),
        ExplicitMemoryError::Database(error) => match error {
            sqlx::Error::Database(db) => {
                let numeric = db
                    .code()
                    .map(|code| {
                        code.chars()
                            .filter(char::is_ascii_digit)
                            .take(10)
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                if numeric.is_empty() {
                    "index_database_error".into()
                } else {
                    format!("index_sqlite_{numeric}")
                }
            }
            sqlx::Error::RowNotFound => "index_reference_missing".into(),
            sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => {
                "index_column_decode".into()
            }
            sqlx::Error::ColumnNotFound(_) => "index_column_missing".into(),
            sqlx::Error::PoolClosed => "index_pool_closed".into(),
            sqlx::Error::PoolTimedOut => "index_pool_busy".into(),
            sqlx::Error::Protocol(_) => "index_storage_protocol".into(),
            _ => "index_storage_unavailable".into(),
        },
    }
}
fn diagnostic(stage: &'static str, error: &ExplicitMemoryError) {
    if matches!(error, ExplicitMemoryError::Database(_)) {
        // Never format database messages, bound values, paths or source text.
        tracing::warn!(
            stage,
            code = code(error),
            "memory index database operation deferred"
        );
    }
}

pub fn spawn(db: Arc<DatabaseManager>) -> Option<tokio::task::JoinHandle<()>> {
    let identity = db.pool.connect_options().get_filename().to_owned();
    let entry = {
        let mut workers = WORKERS.get_or_init(Mutex::default).lock().ok()?;
        if let Some(existing) = workers.get(&identity) {
            *existing.db.lock().ok()? = Arc::downgrade(&db);
            return None;
        }
        let entry = Arc::new(Entry {
            db: Mutex::new(Arc::downgrade(&db)),
        });
        workers.insert(identity.clone(), entry.clone());
        entry
    };
    Some(tokio::spawn(async move {
        let _registration = Registration(identity.clone(), entry.clone());
        let offset = Local::now().offset().local_minus_utc() / 60;
        let mut initialized = false;
        let mut last_adapter = 0_usize;
        let mut last_slide = tokio::time::Instant::now() - std::time::Duration::from_secs(120);
        loop {
            // Router reconstruction can replace a protected adapter before the
            // old pool finishes closing. Refresh it under the same registry
            // lock as spawn/removal so no restart loses its sole worker.
            let db = {
                let mut workers = WORKERS
                    .get_or_init(Mutex::default)
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let latest = entry
                    .db
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .upgrade()
                    .filter(|db| !db.pool.is_closed());
                match latest {
                    Some(db) => db,
                    None => {
                        if workers
                            .get(&identity)
                            .is_some_and(|current| Arc::ptr_eq(current, &entry))
                        {
                            workers.remove(&identity);
                        }
                        break;
                    }
                }
            };
            let adapter = Arc::as_ptr(&db) as usize;
            if adapter != last_adapter {
                initialized = false;
                last_adapter = adapter;
            }
            // Quit/search-only, system sleep and user suspension do not admit
            // index writes or observations. Saved tree reads remain available.
            if screenpipe_core::background_work::is_suspended() {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
            if !initialized {
                let now = Utc::now();
                match db.recover_memory_index_jobs(offset).await {
                    Ok(()) => {}
                    Err(error) => {
                        tracing::warn!(code = code(&error), "memory index recovery deferred");
                        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                        continue;
                    }
                }
                // No automatic full-history scan. Recent work is indexed first;
                // seven days of bounded jobs fill the durable historical tail.
                let _ = db
                    .schedule_memory_index(now - Duration::hours(1), now, offset, 10)
                    .await;
                if let Err(error) = db
                    .schedule_memory_index(now - Duration::days(7), now, offset, 0)
                    .await
                {
                    tracing::warn!(
                        code = code(&error),
                        "memory index initial backfill deferred"
                    );
                }
                initialized = true;
            }
            if last_slide.elapsed() >= std::time::Duration::from_secs(120) {
                if let Err(error) = db.maintain_memory_projection(offset).await {
                    tracing::debug!(
                        code = code(&error),
                        "memory projection maintenance deferred"
                    );
                }
                let now = DateTime::from_timestamp(Utc::now().timestamp() / 60 * 60, 0)
                    .unwrap_or_else(Utc::now);
                let _ = db
                    .schedule_memory_index(now - Duration::hours(1), now, offset, 10)
                    .await;
                last_slide = tokio::time::Instant::now();
            }
            match db.claim_memory_index_job().await {
                Ok(Some(job)) => {
                    let original_cursor = DateTime::parse_from_rfc3339(&job.cursor)
                        .map(|v| v.with_timezone(&Utc))
                        .unwrap_or(DateTime::UNIX_EPOCH);
                    match process(&db, &job).await {
                        Ok(cursor) => {
                            let _ = db.finish_memory_index_slice(&job.id, cursor, None).await;
                        }
                        Err(error) => {
                            let error_code = code(&error);
                            let _ = db
                                .finish_memory_index_slice(
                                    &job.id,
                                    original_cursor,
                                    Some(&error_code),
                                )
                                .await;
                            tracing::debug!(
                                code = code(&error),
                                "memory index batch deferred; recording remains independent"
                            );
                        }
                    }
                    // Yield between each observation/read/projection batch.
                    // Even a historical rebuild cannot occupy the writer in a
                    // tight loop or consume capture admission capacity.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Ok(None) => tokio::time::sleep(std::time::Duration::from_secs(5)).await,
                Err(error) => {
                    tracing::warn!(
                        code = code(&error),
                        "memory index unavailable; retrying later"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                }
            }
        }
    }))
}

async fn process(
    db: &DatabaseManager,
    job: &MemoryIndexJob,
) -> Result<DateTime<Utc>, ExplicitMemoryError> {
    let start = DateTime::parse_from_rfc3339(&job.cursor)
        .map_err(|_| ExplicitMemoryError::Invalid("invalid_index_cursor".into()))?
        .with_timezone(&Utc);
    let job_end = DateTime::parse_from_rfc3339(&job.end_time)
        .map_err(|_| ExplicitMemoryError::Invalid("invalid_index_cursor".into()))?
        .with_timezone(&Utc);
    let mut end = (start + Duration::minutes(15)).min(job_end);
    if let Some((end, complete)) = db.memory_projection_step(&job.id, start).await? {
        return Ok(if complete { end } else { start });
    }
    let policy = db.release_policy().await?;
    if (policy.text || policy.images) && !db.protected_reads_enabled() {
        return Err(ExplicitMemoryError::PrivacyChanged);
    }
    let epoch = db.memory_index_epoch().await?;
    if job.id.starts_with("mark:") {
        db.begin_memory_projection(
            &job.id,
            start,
            start + Duration::days(1),
            job.timezone_offset_minutes,
            true,
        )
        .await?;
        let (end, complete) = db
            .memory_projection_step(&job.id, start)
            .await?
            .ok_or_else(|| ExplicitMemoryError::Invalid("memory_projection_missing".into()))?;
        return Ok(if complete { end } else { start });
    }
    let observations = loop {
        if screenpipe_core::background_work::is_suspended() {
            return Err(ExplicitMemoryError::Invalid("index_suspended".into()));
        }
        if !db.activity_sources_ready(start, end).await? {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        match db.load_memory_observations(start, end).await {
            Ok(rows) => break rows,
            Err(ExplicitMemoryError::Invalid(message))
                if message == "memory_index_batch_too_dense"
                    && end - start > Duration::milliseconds(1) =>
            {
                // Cursor persistence and SQLite date math use milliseconds.
                // Keep adaptive boundaries on that same clock rather than
                // introducing half-millisecond cuts after repeated division.
                end = start + Duration::milliseconds(((end - start).num_milliseconds() / 2).max(1));
            }
            Err(error) => {
                diagnostic("read_observations", &error);
                return Err(error);
            }
        }
    };
    if db.memory_index_epoch().await? != epoch {
        return Err(ExplicitMemoryError::PrivacyChanged);
    }
    let (tasks, mut intervals) = crate::activity_ledger::build_ledger(observations, start, end);
    for interval in &mut intervals {
        // OS events can change context hundreds of microseconds apart.
        // Chrono preserves that ordering, but SQLite julianday rounds both to
        // one millisecond and rejects the resulting zero-length interval.
        // Enclose only the derived interval in milliseconds; evidence keeps
        // its immutable, nanosecond-precision source timestamp and exact ID.
        let start_ms = interval.start_at.timestamp_millis();
        let end_ms = interval.end_at.timestamp_millis()
            + i64::from(interval.end_at.timestamp_subsec_nanos() % 1_000_000 != 0);
        interval.start_at = DateTime::from_timestamp_millis(start_ms)
            .ok_or_else(|| ExplicitMemoryError::Invalid("index_interval_precision".into()))?
            .max(start);
        interval.end_at = DateTime::from_timestamp_millis(end_ms)
            .ok_or_else(|| ExplicitMemoryError::Invalid("index_interval_precision".into()))?
            .min(end);
        if interval.end_at <= interval.start_at {
            return Err(ExplicitMemoryError::Invalid(
                "index_interval_precision".into(),
            ));
        }
    }
    // Reuse the same ledger heuristics and coordinated writer as the existing
    // Activities surface. Generation changes cannot silently merge old text.
    db.reconcile_activity_ledger_checked(
        crate::activity_ledger::ACTIVITY_LEDGER_PRODUCER,
        start,
        end,
        &tasks,
        &intervals,
        epoch,
    )
    .await
    .map_err(|error| {
        let error = ExplicitMemoryError::Database(error);
        diagnostic("reconcile_ledger", &error);
        error
    })?;
    if screenpipe_core::background_work::is_suspended() {
        return Err(ExplicitMemoryError::Invalid("index_suspended".into()));
    }
    db.begin_memory_projection(&job.id, start, end, job.timezone_offset_minutes, false)
        .await?;
    let (end, complete) = db
        .memory_projection_step(&job.id, start)
        .await?
        .ok_or_else(|| ExplicitMemoryError::Invalid("memory_projection_missing".into()))?;
    Ok(if complete { end } else { start })
}
