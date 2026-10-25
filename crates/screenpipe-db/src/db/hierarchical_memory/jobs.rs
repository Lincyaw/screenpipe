// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

use super::*;

fn job(row: &sqlx::sqlite::SqliteRow) -> Result<MemoryIndexJob, sqlx::Error> {
    Ok(MemoryIndexJob {
        id: row.try_get("id")?,
        start_time: row.try_get("start_time")?,
        end_time: row.try_get("end_time")?,
        cursor: row.try_get("cursor")?,
        timezone_offset_minutes: row.try_get("timezone_offset_minutes")?,
        state: row.try_get("state")?,
        attempts: row.try_get("attempts")?,
        last_error: row.try_get("last_error")?,
    })
}
fn nonempty(value: Option<String>) -> Option<String> {
    // Match activity_ledger's existing observation adapter. Empty app metadata
    // is unknown, not a valid empty category title (the ledger rejects those).
    value.filter(|value| !value.trim().is_empty())
}

impl DatabaseManager {
    /// Bounded requests, split into local calendar days. Completed jobs remain
    /// durable and repeated submissions are idempotent for the same window.
    pub async fn schedule_memory_index(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        offset: i32,
        priority: i32,
    ) -> Result<Vec<MemoryIndexJob>, ExplicitMemoryError> {
        validate_offset(offset)?;
        if end <= start || end - start > chrono::Duration::days(31) {
            return Err(invalid("memory_backfill_range_must_be_within_31_days"));
        }
        let mut tx = self.begin_immediate_with_retry().await?;
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM memory_index_jobs WHERE state IN ('pending','running')",
        )
        .fetch_one(&mut **tx.conn())
        .await?;
        if active > 256 {
            return Err(invalid("memory_index_queue_full"));
        }
        let mut cursor = start;
        let mut ids = Vec::new();
        let now = iso(Utc::now());
        while cursor < end {
            let shifted = cursor + chrono::Duration::minutes(i64::from(offset));
            let next_day = shifted
                .date_naive()
                .succ_opt()
                .ok_or_else(|| invalid("invalid_memory_timestamp"))?
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                - chrono::Duration::minutes(i64::from(offset));
            let stop = end.min(next_day);
            let id = format!(
                "index:{}",
                hash(format!("{}:{}:{offset}", iso(cursor), iso(stop)))
            );
            sqlx::query("INSERT INTO memory_index_jobs(id,start_time,end_time,cursor,timezone_offset_minutes,state,priority,created_at,updated_at) VALUES(?,?,?,?,?,'pending',?,?,?) ON CONFLICT(id) DO NOTHING")
                .bind(&id).bind(iso(cursor)).bind(iso(stop)).bind(iso(cursor)).bind(offset).bind(priority).bind(&now).bind(&now).execute(&mut **tx.conn()).await?;
            if priority >= 100 {
                sqlx::query("UPDATE memory_index_jobs SET state='pending',cursor=start_time,attempts=0,next_attempt_at=0,last_error=NULL,priority=?,updated_at=? WHERE id=? AND state IN ('ready','failed')")
                    .bind(priority).bind(&now).bind(&id).execute(&mut **tx.conn()).await?;
            }
            ids.push(id);
            cursor = stop;
        }
        // Terminal diagnostics are bounded; authored marks/overrides are never
        // included in cleanup. Pending/running work is never evicted.
        sqlx::query("DELETE FROM memory_index_jobs WHERE id IN (SELECT id FROM memory_index_jobs WHERE state IN ('ready','failed') ORDER BY updated_at DESC LIMIT -1 OFFSET 2048)").execute(&mut **tx.conn()).await?;
        let mut result = Vec::new();
        for id in ids {
            result.push(job(&sqlx::query(
                "SELECT * FROM memory_index_jobs WHERE id=?",
            )
            .bind(id)
            .fetch_one(&mut **tx.conn())
            .await?)?);
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn recover_memory_index_jobs(&self, offset: i32) -> Result<(), ExplicitMemoryError> {
        validate_offset(offset)?;
        let mut tx = self.begin_immediate_with_retry().await?;
        sqlx::query("UPDATE memory_index_jobs SET state='pending',last_error='interrupted',updated_at=? WHERE state='running'").bind(iso(Utc::now())).execute(&mut **tx.conn()).await?;
        sqlx::query("UPDATE memory_index_state SET timezone_offset_minutes=? WHERE id=1")
            .bind(offset)
            .execute(&mut **tx.conn())
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn claim_memory_index_job(
        &self,
    ) -> Result<Option<MemoryIndexJob>, ExplicitMemoryError> {
        let mut tx = self.begin_immediate_with_retry().await?;
        let row = sqlx::query("SELECT * FROM memory_index_jobs WHERE state='pending' AND next_attempt_at<=? ORDER BY priority DESC,start_time DESC,created_at,id LIMIT 1")
            .bind(Utc::now().timestamp()).fetch_optional(&mut **tx.conn()).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let mut result = job(&row)?;
        sqlx::query("UPDATE memory_index_jobs SET state='running',updated_at=? WHERE id=? AND state='pending'").bind(iso(Utc::now())).bind(&result.id).execute(&mut **tx.conn()).await?;
        result.state = "running".into();
        tx.commit().await?;
        Ok(Some(result))
    }

    pub async fn finish_memory_index_slice(
        &self,
        id: &str,
        cursor: DateTime<Utc>,
        error: Option<&str>,
    ) -> Result<(), ExplicitMemoryError> {
        let mut tx = self.begin_immediate_with_retry().await?;
        let row = sqlx::query(
            "SELECT end_time,attempts FROM memory_index_jobs WHERE id=? AND state='running'",
        )
        .bind(id)
        .fetch_optional(&mut **tx.conn())
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(());
        };
        let attempts: i64 = row.try_get("attempts")?;
        let now = Utc::now();
        if let Some(error) = error {
            let attempts = attempts + 1;
            sqlx::query("UPDATE memory_index_jobs SET state=?,attempts=?,next_attempt_at=?,last_error=?,updated_at=? WHERE id=?")
                .bind(if attempts>=5 {"failed"} else {"pending"}).bind(attempts).bind(now.timestamp()+(5_i64*2_i64.pow(attempts.min(8) as u32)).min(900))
                .bind(bounded(error,128)).bind(iso(now)).bind(id).execute(&mut **tx.conn()).await?;
        } else {
            let end = time(&row.try_get::<String, _>("end_time")?)?;
            sqlx::query("UPDATE memory_index_jobs SET state=?,cursor=?,attempts=0,last_error=NULL,updated_at=? WHERE id=?")
                .bind(if cursor>=end {"ready"} else {"pending"}).bind(iso(cursor)).bind(iso(now)).bind(id).execute(&mut **tx.conn()).await?;
            sqlx::query("UPDATE memory_index_state SET cursor=?,last_success_at=? WHERE id=1")
                .bind(iso(cursor))
                .bind(iso(now))
                .execute(&mut **tx.conn())
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn memory_index_status(&self) -> Result<MemoryIndexStatus, ExplicitMemoryError> {
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let current = epoch(&mut tx).await?;
        let row = sqlx::query("SELECT * FROM memory_index_state WHERE id=1")
            .fetch_one(&mut *tx)
            .await?;
        let (pending,failed): (i64,i64) = sqlx::query_as("SELECT COALESCE(SUM(state IN ('pending','running')),0),COALESCE(SUM(state='failed'),0) FROM memory_index_jobs").fetch_one(&mut *tx).await?;
        let (count,provisional): (i64,i64) = sqlx::query_as("SELECT count(*),COALESCE(sum(provisional),0) FROM memory_nodes WHERE invalidated=0 AND privacy_epoch IS ?")
            .bind(current).fetch_one(&mut *tx).await?;
        // Calendar partition upper bounds are often tomorrow. Coverage reports
        // actual indexed source/mark times; it does not assert a gap-free scan.
        let (first,last):(Option<String>,Option<String>)=sqlx::query_as("SELECT strftime('%Y-%m-%dT%H:%M:%fZ',min(julianday(moment))),strftime('%Y-%m-%dT%H:%M:%fZ',max(julianday(moment))) FROM (SELECT e.occurred_at AS moment FROM memory_evidence e JOIN memory_nodes n ON n.id=e.node_id WHERE n.invalidated=0 AND n.privacy_epoch IS ?1 UNION ALL SELECT m.source_timestamp AS moment FROM memory_node_marks nm JOIN memory_nodes n ON n.id=nm.node_id JOIN explicit_memory_marks m ON m.id=nm.mark_id WHERE n.invalidated=0 AND n.privacy_epoch IS ?1 AND m.privacy_epoch IS ?1)")
            .bind(current).fetch_one(&mut *tx).await?;
        let rebuild_pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_nodes WHERE invalidated!=0 OR privacy_epoch IS NOT ?)").bind(current).fetch_one(&mut *tx).await?;
        let privacy_pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM memory_nodes WHERE privacy_epoch IS NOT ?)",
        )
        .bind(current)
        .fetch_one(&mut *tx)
        .await?;
        let running = sqlx::query("SELECT * FROM memory_index_jobs WHERE state='running' ORDER BY updated_at DESC LIMIT 1").fetch_optional(&mut *tx).await?.as_ref().map(job).transpose()?;
        let error: Option<String> = sqlx::query_scalar("SELECT last_error FROM memory_index_jobs WHERE last_error IS NOT NULL AND (state='failed' OR updated_at>COALESCE((SELECT last_success_at FROM memory_index_state WHERE id=1),'')) ORDER BY updated_at DESC LIMIT 1").fetch_optional(&mut *tx).await?.flatten();
        let result = MemoryIndexStatus {
            generation: row.try_get("generation")?,
            state: if running.is_some() {
                "indexing"
            } else if pending > 0 {
                "pending"
            } else if failed > 0 {
                "partial"
            } else {
                "ready"
            }
            .into(),
            pending_jobs: pending,
            failed_jobs: failed,
            cursor: row.try_get("cursor")?,
            last_success_at: row.try_get("last_success_at")?,
            last_error: error,
            indexed_nodes: count,
            provisional_nodes: provisional,
            coverage_start: first,
            coverage_end: last,
            privacy_pending,
            rebuild_pending,
            timezone_offset_minutes: row.try_get("timezone_offset_minutes")?,
            current_job: running,
        };
        tx.commit().await?;
        Ok(result)
    }

    /// At most 101 records per leg. A dense window is retried at a smaller
    /// time slice; no admission limit is imposed on authorized recordings.
    pub async fn load_memory_observations(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<ActivityLedgerObservation>, ExplicitMemoryError> {
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let mut result = Vec::new();
        for kind in ["frame", "ui_event", "audio"] {
            let sql = match kind {
                "frame" => "SELECT id,timestamp,app_name,window_name AS window_title,browser_url,document_path,NULL AS event_type,NULL AS element_role,NULL AS element_name,NULL AS device,NULL AS speaker_name,NULL AS is_input_device FROM frames WHERE julianday(timestamp)>=julianday(?) AND julianday(timestamp)<julianday(?) AND (machine_id IS NULL OR machine_id='') AND COALESCE(focused,1)=1 AND (COALESCE(app_name,'')!='' OR COALESCE(window_name,'')!='') ORDER BY julianday(timestamp),id LIMIT 101",
                "ui_event" => "SELECT id,timestamp,app_name,window_title,browser_url,NULL AS document_path,event_type,element_role,element_name,NULL AS device,NULL AS speaker_name,NULL AS is_input_device FROM ui_events WHERE julianday(timestamp)>=julianday(?) AND julianday(timestamp)<julianday(?) AND event_type IN ('click','text','clipboard','app_switch','window_focus') ORDER BY julianday(timestamp),id LIMIT 101",
                _ => "SELECT id,timestamp,NULL AS app_name,NULL AS window_title,NULL AS browser_url,NULL AS document_path,NULL AS event_type,NULL AS element_role,NULL AS element_name,device,NULL AS speaker_name,is_input_device FROM audio_transcriptions WHERE julianday(timestamp)>=julianday(?) AND julianday(timestamp)<julianday(?) AND length(trim(transcription))>=8 ORDER BY julianday(timestamp),id LIMIT 101"
            };
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(iso(start))
                .bind(iso(end))
                .fetch_all(&mut *tx)
                .await?;
            if rows.len() > 100 {
                return Err(invalid("memory_index_batch_too_dense"));
            }
            for row in rows {
                let at: DateTime<Utc> = row.try_get("timestamp")?;
                result.push(ActivityLedgerObservation {
                    source_type: kind.into(),
                    source_id: row.try_get("id")?,
                    occurred_at: at,
                    app_name: nonempty(row.try_get("app_name")?),
                    window_title: nonempty(row.try_get("window_title")?),
                    browser_url: nonempty(row.try_get("browser_url")?),
                    document_path: nonempty(row.try_get("document_path")?),
                    semantic_kind: None,
                    semantic_title: None,
                    semantic_key: None,
                    event_type: nonempty(row.try_get("event_type")?),
                    click_count: None,
                    element_role: nonempty(row.try_get("element_role")?),
                    element_name: nonempty(row.try_get("element_name")?),
                    device_name: nonempty(row.try_get("device")?),
                    speaker_name: nonempty(row.try_get("speaker_name")?),
                    is_input_device: row.try_get("is_input_device")?,
                    attention_rank: if kind == "ui_event" {
                        4
                    } else if kind == "frame" {
                        3
                    } else {
                        1
                    },
                });
            }
        }
        tx.commit().await?;
        result.sort_by(|a, b| {
            a.occurred_at
                .cmp(&b.occurred_at)
                .then(a.attention_rank.cmp(&b.attention_rank))
                .then(a.source_type.cmp(&b.source_type))
                .then(a.source_id.cmp(&b.source_id))
        });
        Ok(result)
    }
}
