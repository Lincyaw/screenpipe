// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Optional derived summaries and durable jobs. Network/model work stays outside
//! transactions; all mutations join the recording database's existing writer.

use super::explicit_memory::ExplicitMemoryError;
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HierarchySummaryConfig {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub background: bool,
    pub max_input_chars: u32,
    pub max_evidence: u32,
    pub max_children: u32,
    pub max_output_tokens: u32,
    pub request_timeout_secs: u32,
    pub jobs_per_hour: u32,
}
impl Default for HierarchySummaryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://127.0.0.1:11434".into(),
            model: String::new(),
            background: false,
            max_input_chars: 24_000,
            max_evidence: 24,
            max_children: 16,
            max_output_tokens: 1200,
            request_timeout_secs: 90,
            jobs_per_hour: 12,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryCitation {
    pub source_type: String,
    pub source_id: String,
    pub timestamp: String,
    pub uri: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryStatement {
    pub kind: String,
    pub text: String,
    pub citations: Vec<SummaryCitation>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummarySourceVersion {
    pub source_type: String,
    pub source_id: String,
    pub fingerprint: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SummaryCoverage {
    pub evidence_used: u32,
    pub evidence_total: u32,
    pub marks_used: u32,
    pub marks_total: u32,
    pub children_used: u32,
    pub children_total: u32,
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HierarchySummary {
    pub kind: String,
    pub node_id: String,
    pub status: String,
    pub text: Option<String>,
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
    pub input_fingerprint: String,
    pub node_fingerprint: String,
    pub node_revision: i64,
    pub generated_at: Option<String>,
    pub citations: Vec<SummaryCitation>,
    pub statements: Vec<SummaryStatement>,
    #[serde(skip)]
    pub source_versions: Vec<SummarySourceVersion>,
    pub coverage: SummaryCoverage,
    pub user_notes_attributed: bool,
    pub user_corrections_attributed: bool,
    pub error_code: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HierarchySummaryJob {
    pub job_id: String,
    pub node_id: String,
    pub status: String,
    pub attempts: u32,
    pub next_attempt_at: i64,
    pub error_code: Option<String>,
    pub summary: Option<HierarchySummary>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct HierarchySummaryCounts {
    pub pending_jobs: i64,
    pub running_jobs: i64,
    pub failed_jobs: i64,
    pub last_error_code: Option<String>,
}

fn invalid(message: &str) -> ExplicitMemoryError {
    ExplicitMemoryError::Invalid(message.into())
}
async fn epoch(conn: &mut sqlx::SqliteConnection) -> Result<Option<i64>, sqlx::Error> {
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
async fn load_job(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<HierarchySummaryJob>, ExplicitMemoryError> {
    let Some(row) = sqlx::query("SELECT * FROM memory_summary_jobs WHERE id=?")
        .bind(id)
        .fetch_optional(conn)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(HierarchySummaryJob {
        job_id: row.try_get("id")?,
        node_id: row.try_get("node_id")?,
        status: row.try_get("status")?,
        attempts: u32::try_from(row.try_get::<i64, _>("attempts")?).unwrap_or(0),
        next_attempt_at: row.try_get("next_attempt_at")?,
        error_code: row.try_get("error_code")?,
        summary: None,
    }))
}
async fn load_summary(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<HierarchySummary>, ExplicitMemoryError> {
    let Some(row)=sqlx::query("SELECT s.*,n.fingerprint AS current_fingerprint,n.invalidated AS invalidated,n.privacy_epoch AS node_epoch FROM memory_node_summaries s JOIN memory_nodes n ON n.id=s.node_id WHERE s.node_id=? AND n.superseded=0").bind(id).fetch_optional(&mut *conn).await? else{return Ok(None)};
    let current_epoch = epoch(conn).await?;
    let stored_epoch: Option<i64> = row.try_get("privacy_epoch")?;
    let node_epoch: Option<i64> = row.try_get("node_epoch")?;
    let fingerprint: String = row.try_get("node_fingerprint")?;
    let current_fingerprint: String = row.try_get("current_fingerprint")?;
    let source_versions: Vec<SummarySourceVersion> =
        serde_json::from_str(&row.try_get::<String, _>("source_versions_json")?)
            .map_err(|_| invalid("summary_corrupt"))?;
    let valid = stored_epoch == current_epoch
        && node_epoch == current_epoch
        && fingerprint == current_fingerprint
        && row.try_get::<i64, _>("invalidated")? == 0;
    let stored_status: String = row.try_get("status")?;
    let ready = valid && stored_status == "ready";
    Ok(Some(HierarchySummary {
        kind: "generated_summary".into(),
        node_id: id.into(),
        status: if valid { stored_status } else { "stale".into() },
        text: if ready { row.try_get("text")? } else { None },
        provider: row.try_get("provider")?,
        model: row.try_get("model")?,
        prompt_version: row.try_get("prompt_version")?,
        input_fingerprint: row.try_get("input_fingerprint")?,
        node_fingerprint: fingerprint,
        node_revision: row.try_get("node_revision")?,
        generated_at: if ready {
            row.try_get("generated_at")?
        } else {
            None
        },
        citations: if ready {
            serde_json::from_str(&row.try_get::<String, _>("citations_json")?)
                .map_err(|_| invalid("summary_corrupt"))?
        } else {
            Vec::new()
        },
        statements: if ready {
            serde_json::from_str(&row.try_get::<String, _>("statements_json")?)
                .map_err(|_| invalid("summary_corrupt"))?
        } else {
            Vec::new()
        },
        source_versions: if ready { source_versions } else { Vec::new() },
        coverage: serde_json::from_str(&row.try_get::<String, _>("coverage_json")?)
            .unwrap_or_default(),
        user_notes_attributed: true,
        user_corrections_attributed: true,
        error_code: if valid {
            row.try_get("error_code")?
        } else {
            Some("input_changed".into())
        },
    }))
}

impl DatabaseManager {
    async fn validate_hierarchy_summary_sources(
        &self,
        summary: &mut HierarchySummary,
        before: Option<i64>,
    ) -> Result<(), ExplicitMemoryError> {
        if summary.status != "ready" {
            return Ok(());
        }
        let mut valid = !summary.source_versions.is_empty() && summary.source_versions.len() <= 128;
        if valid {
            for expected in &summary.source_versions {
                match self
                    .hierarchy_summary_source_version(&expected.source_type, &expected.source_id)
                    .await
                {
                    Ok(current) if current.fingerprint == expected.fingerprint => {}
                    _ => {
                        valid = false;
                        break;
                    }
                }
            }
        }
        if self.memory_index_epoch().await? != before {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_nodes WHERE id=? AND fingerprint=? AND invalidated=0 AND privacy_epoch IS ?)").bind(&summary.node_id).bind(&summary.node_fingerprint).bind(before).fetch_one(&mut *self.acquire_read().await?).await?;
        if !valid || !current {
            summary.status = "stale".into();
            summary.text = None;
            summary.generated_at = None;
            summary.citations.clear();
            summary.statements.clear();
            summary.source_versions.clear();
            summary.error_code = Some("input_changed".into());
        }
        Ok(())
    }
    /// Exact bounded inputs, including payload policy/generation and metadata.
    /// Physical file/sealing locators deliberately do not invalidate a summary.
    pub async fn hierarchy_summary_source_version(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<SummarySourceVersion, ExplicitMemoryError> {
        if kind == "user_note" {
            let mark = self
                .get_explicit_memory_mark(id)
                .await?
                .ok_or(ExplicitMemoryError::NotFound)?;
            let bytes = serde_json::to_vec(&mark).map_err(|_| invalid("summary_source_invalid"))?;
            return Ok(SummarySourceVersion {
                source_type: kind.into(),
                source_id: id.into(),
                fingerprint: format!("{:x}", Sha256::digest(bytes)),
            });
        }
        let id: i64 = id.parse().map_err(|_| invalid("summary_source_invalid"))?;
        let evidence = self.hydrate_memory_evidence(kind, id).await?;
        self.hierarchy_summary_evidence_version(&evidence).await
    }
    pub async fn hierarchy_summary_evidence_version(
        &self,
        evidence: &super::hierarchical_memory::MemoryEvidence,
    ) -> Result<SummarySourceVersion, ExplicitMemoryError> {
        if !evidence.available {
            return Err(ExplicitMemoryError::NotFound);
        }
        let current = self
            .hydrate_memory_evidence(&evidence.source_type, evidence.source_id)
            .await?;
        if !current.available
            || serde_json::to_value(&current).ok() != serde_json::to_value(evidence).ok()
        {
            return Err(ExplicitMemoryError::Conflict);
        }
        let frame = if let Some(frame_id) = current.frame_id {
            let mut conn = self.acquire_read().await?;
            if self.storage.is_some() {
                let row=sqlx::query("SELECT f.document_path,p.generation,p.policy,p.completed_surfaces FROM frames f JOIN frame_payloads p ON p.frame_id=f.id WHERE f.id=?").bind(frame_id).fetch_optional(&mut *conn).await?.ok_or(ExplicitMemoryError::NotFound)?;
                serde_json::json!([
                    frame_id,
                    row.try_get::<Option<String>, _>("document_path")?,
                    row.try_get::<i64, _>("generation")?,
                    row.try_get::<String, _>("policy")?,
                    row.try_get::<i64, _>("completed_surfaces")?
                ])
            } else {
                let path: Option<Option<String>> =
                    sqlx::query_scalar("SELECT document_path FROM frames WHERE id=?")
                        .bind(frame_id)
                        .fetch_optional(&mut *conn)
                        .await?;
                let path = path.ok_or(ExplicitMemoryError::NotFound)?;
                serde_json::json!([frame_id, path])
            }
        } else {
            serde_json::Value::Null
        };
        let bytes = serde_json::to_vec(&serde_json::json!([current, frame]))
            .map_err(|_| invalid("summary_source_invalid"))?;
        Ok(SummarySourceVersion {
            source_type: evidence.source_type.clone(),
            source_id: evidence.source_id.to_string(),
            fingerprint: format!("{:x}", Sha256::digest(bytes)),
        })
    }
    pub async fn hierarchy_summary_config(
        &self,
    ) -> Result<HierarchySummaryConfig, ExplicitMemoryError> {
        let mut conn = self.acquire_read().await?;
        let value: Option<String> =
            sqlx::query_scalar("SELECT config_json FROM memory_summary_config WHERE id=1")
                .fetch_optional(&mut *conn)
                .await?;
        value
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| invalid("summary_config_invalid"))
            })
            .unwrap_or_else(|| Ok(HierarchySummaryConfig::default()))
    }
    pub async fn set_hierarchy_summary_config(
        &self,
        config: &HierarchySummaryConfig,
    ) -> Result<(), ExplicitMemoryError> {
        let value = serde_json::to_string(config).map_err(|_| invalid("summary_config_invalid"))?;
        let mut tx = self.begin_immediate_with_retry().await?;
        let old: Option<String> =
            sqlx::query_scalar("SELECT config_json FROM memory_summary_config WHERE id=1")
                .fetch_optional(&mut **tx.conn())
                .await?;
        if old.as_deref() != Some(value.as_str()) {
            sqlx::query("INSERT INTO memory_summary_config(id,config_json,updated_at) VALUES(1,?,unixepoch()) ON CONFLICT(id) DO UPDATE SET config_json=excluded.config_json,updated_at=excluded.updated_at").bind(value).execute(&mut **tx.conn()).await?;
            sqlx::query("UPDATE memory_node_summaries SET status='stale',text=NULL,citations_json='[]',statements_json='[]',error_code='config_changed',updated_at=unixepoch()").execute(&mut **tx.conn()).await?;
            sqlx::query("UPDATE memory_summary_jobs SET status='stale',error_code='config_changed',lease_until=NULL,updated_at=unixepoch() WHERE status IN ('pending','running')").execute(&mut **tx.conn()).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn get_hierarchy_summary(
        &self,
        node_id: &str,
    ) -> Result<Option<HierarchySummary>, ExplicitMemoryError> {
        let before = self.memory_index_epoch().await?;
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let mut result = load_summary(&mut tx, node_id).await?;
        tx.commit().await?;
        drop(conn);
        if let Some(summary) = result.as_mut() {
            self.validate_hierarchy_summary_sources(summary, before)
                .await?;
        }
        Ok(result)
    }
    pub async fn get_hierarchy_summary_job(
        &self,
        id: &str,
    ) -> Result<Option<HierarchySummaryJob>, ExplicitMemoryError> {
        let before = self.memory_index_epoch().await?;
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let mut job = load_job(&mut tx, id).await?;
        if let Some(job) = job.as_mut() {
            job.summary = load_summary(&mut tx, &job.node_id).await?;
        }
        tx.commit().await?;
        drop(conn);
        if let Some(summary) = job.as_mut().and_then(|job| job.summary.as_mut()) {
            self.validate_hierarchy_summary_sources(summary, before)
                .await?;
        }
        Ok(job)
    }
    pub async fn queue_hierarchy_summary(
        &self,
        node_id: &str,
        model: &str,
        prompt_version: &str,
    ) -> Result<HierarchySummaryJob, ExplicitMemoryError> {
        let mut tx = self.begin_immediate_with_retry().await?;
        let node: Option<(bool, i64)> =
            sqlx::query_as("SELECT provisional,invalidated FROM memory_nodes WHERE id=?")
                .bind(node_id)
                .fetch_optional(&mut **tx.conn())
                .await?;
        let Some((provisional, invalidated)) = node else {
            return Err(ExplicitMemoryError::NotFound);
        };
        if provisional {
            return Err(invalid("node_not_closed"));
        }
        if invalidated != 0 {
            return Err(invalid("node_stale"));
        }
        let existing:Option<String>=sqlx::query_scalar("SELECT id FROM memory_summary_jobs WHERE node_id=? AND status IN ('pending','running')").bind(node_id).fetch_optional(&mut **tx.conn()).await?;
        if let Some(id) = existing {
            let job = load_job(&mut **tx.conn(), &id)
                .await?
                .ok_or(ExplicitMemoryError::NotFound)?;
            tx.commit().await?;
            return Ok(job);
        }
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM memory_summary_jobs WHERE status IN ('pending','running')",
        )
        .fetch_one(&mut **tx.conn())
        .await?;
        if pending >= 256 {
            return Err(invalid("summary_queue_full"));
        }
        sqlx::query("DELETE FROM memory_summary_jobs WHERE id IN (SELECT id FROM memory_summary_jobs WHERE status NOT IN ('pending','running') ORDER BY updated_at DESC LIMIT -1 OFFSET 1023)").execute(&mut **tx.conn()).await?;
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO memory_summary_jobs(id,node_id,status,next_attempt_at,created_at,updated_at) VALUES(?,?,'pending',unixepoch(),unixepoch(),unixepoch())").bind(&id).bind(node_id).execute(&mut **tx.conn()).await?;
        sqlx::query("INSERT INTO memory_node_summaries(node_id,status,model,prompt_version,input_fingerprint,node_fingerprint,node_revision,privacy_epoch,updated_at) SELECT id,'pending',?,?,'',fingerprint,revision,privacy_epoch,unixepoch() FROM memory_nodes WHERE id=? ON CONFLICT(node_id) DO UPDATE SET status='pending',text=NULL,citations_json='[]',statements_json='[]',model=excluded.model,prompt_version=excluded.prompt_version,node_fingerprint=excluded.node_fingerprint,node_revision=excluded.node_revision,privacy_epoch=excluded.privacy_epoch,generated_at=NULL,error_code=NULL,updated_at=excluded.updated_at").bind(model).bind(prompt_version).bind(node_id).execute(&mut **tx.conn()).await?;
        let job = load_job(&mut **tx.conn(), &id)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        tx.commit().await?;
        Ok(job)
    }
    pub async fn claim_hierarchy_summary_job(
        &self,
        budget: u32,
        lease_secs: u32,
    ) -> Result<Option<HierarchySummaryJob>, ExplicitMemoryError> {
        let mut tx = self.begin_immediate_with_retry().await?;
        sqlx::query("UPDATE memory_summary_jobs SET status='pending',lease_until=NULL,error_code='interrupted',next_attempt_at=unixepoch(),updated_at=unixepoch() WHERE status='running' AND lease_until<=unixepoch()").execute(&mut **tx.conn()).await?;
        sqlx::query("DELETE FROM memory_summary_attempts WHERE started_at<unixepoch()-86400")
            .execute(&mut **tx.conn())
            .await?;
        let recent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM memory_summary_attempts WHERE started_at>unixepoch()-3600",
        )
        .fetch_one(&mut **tx.conn())
        .await?;
        if recent >= i64::from(budget) {
            tx.commit().await?;
            return Ok(None);
        }
        let current_epoch = epoch(&mut **tx.conn()).await?;
        let id:Option<String>=sqlx::query_scalar("SELECT j.id FROM memory_summary_jobs j JOIN memory_nodes n ON n.id=j.node_id WHERE j.status='pending' AND j.next_attempt_at<=unixepoch() AND n.invalidated=0 AND n.superseded=0 AND n.privacy_epoch IS ?1 ORDER BY CASE n.kind WHEN 'task' THEN 0 WHEN 'session' THEN 1 WHEN 'day' THEN 2 ELSE 3 END,j.created_at,j.rowid LIMIT 1").bind(current_epoch).fetch_optional(&mut **tx.conn()).await?;
        let Some(id) = id else {
            tx.commit().await?;
            return Ok(None);
        };
        sqlx::query("UPDATE memory_summary_jobs SET status='running',attempts=attempts+1,lease_until=unixepoch()+?,updated_at=unixepoch() WHERE id=?").bind(i64::from(lease_secs)).bind(&id).execute(&mut **tx.conn()).await?;
        sqlx::query("INSERT INTO memory_summary_attempts(started_at) VALUES(unixepoch())")
            .execute(&mut **tx.conn())
            .await?;
        let job = load_job(&mut **tx.conn(), &id).await?;
        tx.commit().await?;
        Ok(job)
    }
    pub async fn fail_hierarchy_summary_job(
        &self,
        id: &str,
        code: &str,
        retry: bool,
    ) -> Result<(), ExplicitMemoryError> {
        let mut tx = self.begin_immediate_with_retry().await?;
        let Some(job) = load_job(&mut **tx.conn(), id).await? else {
            return Err(ExplicitMemoryError::NotFound);
        };
        if job.status != "running" {
            tx.commit().await?;
            return Ok(());
        }
        let retry = retry && job.attempts < 4;
        let delay = 30_i64.saturating_mul(1_i64 << job.attempts.min(6));
        sqlx::query("UPDATE memory_summary_jobs SET status=?,error_code=?,next_attempt_at=unixepoch()+?,lease_until=NULL,updated_at=unixepoch() WHERE id=?").bind(if retry{"pending"}else{"failed"}).bind(code).bind(delay).bind(id).execute(&mut **tx.conn()).await?;
        sqlx::query("INSERT INTO memory_node_summaries(node_id,status,model,prompt_version,input_fingerprint,node_fingerprint,node_revision,privacy_epoch,error_code,updated_at) SELECT id,?,'','','',fingerprint,revision,privacy_epoch,?,unixepoch() FROM memory_nodes WHERE id=? ON CONFLICT(node_id) DO UPDATE SET status=excluded.status,text=NULL,citations_json='[]',statements_json='[]',error_code=excluded.error_code,updated_at=excluded.updated_at").bind(if retry{"pending"}else{"failed"}).bind(code).bind(&job.node_id).execute(&mut **tx.conn()).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn defer_hierarchy_summary_job(&self, id: &str) -> Result<(), ExplicitMemoryError> {
        let mut tx = self.begin_immediate_with_retry().await?;
        sqlx::query("UPDATE memory_summary_jobs SET status='pending',attempts=MAX(0,attempts-1),next_attempt_at=unixepoch()+3,lease_until=NULL,updated_at=unixepoch(),error_code='waiting_for_children' WHERE id=? AND status='running'").bind(id).execute(&mut **tx.conn()).await?;
        // Dependency orchestration is not an inference attempt.
        sqlx::query("DELETE FROM memory_summary_attempts WHERE id=(SELECT MAX(id) FROM memory_summary_attempts)").execute(&mut **tx.conn()).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn complete_hierarchy_summary_job(
        &self,
        id: &str,
        summary: &HierarchySummary,
        expected_epoch: Option<i64>,
        expected_storage_revocation: Option<i64>,
    ) -> Result<(), ExplicitMemoryError> {
        if summary
            .text
            .as_ref()
            .is_none_or(|text| text.is_empty() || text.len() > 32_000)
            || summary.citations.len() > 256
            || summary.source_versions.len() > 128
        {
            return Err(invalid("summary_output_invalid"));
        }
        let mut tx = self.begin_immediate_with_retry().await?;
        if let Some(expected) = expected_storage_revocation {
            let current: i64 =
                sqlx::query_scalar("SELECT revision FROM _storage_revocation WHERE id=1")
                    .fetch_one(&mut **tx.conn())
                    .await?;
            if expected != current {
                return Err(ExplicitMemoryError::Conflict);
            }
        }
        let Some(job) = load_job(&mut **tx.conn(), id).await? else {
            return Err(ExplicitMemoryError::NotFound);
        };
        if job.status != "running" {
            return Err(ExplicitMemoryError::Conflict);
        }
        let node:Option<(String,i64,Option<i64>,bool,i64)>=sqlx::query_as("SELECT fingerprint,revision,privacy_epoch,provisional,invalidated FROM memory_nodes WHERE id=?").bind(&job.node_id).fetch_optional(&mut **tx.conn()).await?;
        let Some((fingerprint, revision, node_epoch, provisional, invalidated)) = node else {
            return Err(ExplicitMemoryError::NotFound);
        };
        if fingerprint != summary.node_fingerprint
            || revision != summary.node_revision
            || provisional
            || invalidated != 0
        {
            return Err(ExplicitMemoryError::Conflict);
        }
        let privacy_epoch = epoch(&mut **tx.conn()).await?;
        // Raw epoch, including disabled-policy transitions, is the index's token.
        if privacy_epoch != node_epoch {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let current_effective = if let Some(raw) = privacy_epoch {
            let required: bool = sqlx::query_scalar(
                "SELECT text_required OR image_required FROM _privacy_policy WHERE id=1",
            )
            .fetch_one(&mut **tx.conn())
            .await?;
            required.then_some(raw)
        } else {
            None
        };
        if current_effective != expected_epoch {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let citations = serde_json::to_string(&summary.citations)
            .map_err(|_| invalid("summary_output_invalid"))?;
        let coverage = serde_json::to_string(&summary.coverage)
            .map_err(|_| invalid("summary_output_invalid"))?;
        let statements = serde_json::to_string(&summary.statements)
            .map_err(|_| invalid("summary_output_invalid"))?;
        let source_versions = serde_json::to_string(&summary.source_versions)
            .map_err(|_| invalid("summary_output_invalid"))?;
        sqlx::query("INSERT INTO memory_node_summaries(node_id,status,text,provider,model,prompt_version,input_fingerprint,node_fingerprint,node_revision,privacy_epoch,generated_at,citations_json,statements_json,coverage_json,source_versions_json,error_code,updated_at) VALUES(?,'ready',?,?,?,?,?,?,?,?,?,?,?,?,?,NULL,unixepoch()) ON CONFLICT(node_id) DO UPDATE SET status='ready',text=excluded.text,provider=excluded.provider,model=excluded.model,prompt_version=excluded.prompt_version,input_fingerprint=excluded.input_fingerprint,node_fingerprint=excluded.node_fingerprint,node_revision=excluded.node_revision,privacy_epoch=excluded.privacy_epoch,generated_at=excluded.generated_at,citations_json=excluded.citations_json,statements_json=excluded.statements_json,coverage_json=excluded.coverage_json,source_versions_json=excluded.source_versions_json,error_code=NULL,updated_at=excluded.updated_at")
            .bind(&job.node_id).bind(&summary.text).bind(&summary.provider).bind(&summary.model).bind(&summary.prompt_version).bind(&summary.input_fingerprint).bind(&summary.node_fingerprint).bind(summary.node_revision).bind(privacy_epoch).bind(&summary.generated_at).bind(citations).bind(statements).bind(coverage).bind(source_versions).execute(&mut **tx.conn()).await?;
        sqlx::query("UPDATE memory_summary_jobs SET status='ready',error_code=NULL,lease_until=NULL,updated_at=unixepoch() WHERE id=?").bind(id).execute(&mut **tx.conn()).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn hierarchy_summary_counts(
        &self,
    ) -> Result<HierarchySummaryCounts, ExplicitMemoryError> {
        let mut conn = self.acquire_read().await?;
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT status,COUNT(*) FROM memory_summary_jobs GROUP BY status")
                .fetch_all(&mut *conn)
                .await?;
        let mut result = HierarchySummaryCounts::default();
        for (status, count) in rows {
            match status.as_str() {
                "pending" => result.pending_jobs = count,
                "running" => result.running_jobs = count,
                "failed" => result.failed_jobs = count,
                _ => {}
            }
        }
        result.last_error_code=sqlx::query_scalar("SELECT error_code FROM memory_summary_jobs WHERE error_code IS NOT NULL ORDER BY updated_at DESC LIMIT 1").fetch_optional(&mut *conn).await?.flatten();
        Ok(result)
    }
    pub async fn hierarchy_summary_background_candidates(
        &self,
        limit: u32,
    ) -> Result<Vec<String>, ExplicitMemoryError> {
        let mut conn = self.acquire_read().await?;
        Ok(sqlx::query_scalar("SELECT n.id FROM memory_nodes n LEFT JOIN memory_node_summaries s ON s.node_id=n.id WHERE n.provisional=0 AND n.invalidated=0 AND n.superseded=0 AND (s.node_id IS NULL OR s.status='stale') AND NOT EXISTS(SELECT 1 FROM memory_summary_jobs j WHERE j.node_id=n.id AND j.status IN ('pending','running')) ORDER BY CASE n.kind WHEN 'task' THEN 0 WHEN 'session' THEN 1 ELSE 2 END,n.start_time DESC LIMIT ?").bind(i64::from(limit.min(8))).fetch_all(&mut *conn).await?)
    }
}
