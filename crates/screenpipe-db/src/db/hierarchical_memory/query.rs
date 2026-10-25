// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

use super::*;

const DESCENDANTS: &str = "WITH RECURSIVE descendants(id,depth) AS (SELECT id,0 FROM memory_nodes WHERE id=?1 UNION ALL SELECT n.id,d.depth+1 FROM memory_nodes n JOIN descendants d ON n.parent_id=d.id WHERE d.depth<2)";
const NODE_DIRECT_MATCH:&str="?7='' OR instr(lower(n.label),lower(?7))>0 OR n.id IN (SELECT node_id FROM memory_nodes_fts WHERE memory_nodes_fts MATCH ?8) OR (n.kind='day' AND EXISTS(SELECT 1 FROM memory_nodes child WHERE child.day_key=n.day_key AND child.invalidated=0 AND child.privacy_epoch IS ?1 AND instr(lower(child.label),lower(?7))>0))";

// These builders only receive constant SQL aliases/bind positions. Values
// always remain bound parameters. Audio tags reference chunks, not transcript
// row IDs; UI and semantic sources inherit tags only from exact linked frames.
fn frame_tag(frame: &str, tag: &str) -> String {
    format!("EXISTS(SELECT 1 FROM vision_tags vt JOIN tags t ON t.id=vt.tag_id WHERE vt.vision_id={frame} AND t.name={tag})")
}
fn audio_tag(chunk: &str, tag: &str) -> String {
    format!("EXISTS(SELECT 1 FROM audio_tags at JOIN tags t ON t.id=at.tag_id WHERE at.audio_chunk_id={chunk} AND t.name={tag})")
}
fn legacy_tag(tag: &str) -> String {
    format!("(EXISTS(SELECT 1 FROM json_each(CASE WHEN json_valid(m.tags) THEN m.tags ELSE '[]' END) WHERE value={tag}) OR {})",frame_tag("f.id",tag))
}
fn source_tag(kind: &str, id: &str, tag: &str) -> String {
    match kind {
        "frame"=>format!("EXISTS(SELECT 1 FROM frames f WHERE f.id={id} AND {})",frame_tag("f.id",tag)),
        "audio"=>format!("EXISTS(SELECT 1 FROM audio_transcriptions a WHERE a.id={id} AND {})",audio_tag("a.audio_chunk_id",tag)),
        "ui_event"=>format!("EXISTS(SELECT 1 FROM ui_events u JOIN frames f ON f.id=u.frame_id WHERE u.id={id} AND {})",frame_tag("f.id",tag)),
        _=>"0".into(),
    }
}
fn node_tag(node: &str, tag: &str, epoch: &str) -> String {
    format!("EXISTS(SELECT 1 FROM memory_nodes tagged WHERE (tagged.id={node} OR tagged.parent_id={node} OR tagged.parent_id IN (SELECT middle.id FROM memory_nodes middle WHERE middle.parent_id={node})) AND tagged.invalidated=0 AND tagged.privacy_epoch IS {epoch} AND (EXISTS(SELECT 1 FROM memory_node_marks nm JOIN explicit_memory_marks m ON m.id=nm.mark_id JOIN explicit_memory_mark_tags mt ON mt.mark_id=m.id JOIN tags t ON t.id=mt.tag_id WHERE nm.node_id=tagged.id AND m.privacy_epoch IS {epoch} AND t.name={tag}) OR EXISTS(SELECT 1 FROM memory_evidence e WHERE e.node_id=tagged.id AND ((e.source_type='frame' AND {}) OR (e.source_type='audio' AND {}) OR (e.source_type='ui_event' AND {})))))",source_tag("frame","e.source_id",tag),source_tag("audio","e.source_id",tag),source_tag("ui_event","e.source_id",tag))
}
async fn node_has_tag(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    tag: &str,
    epoch: Option<i64>,
) -> Result<bool, sqlx::Error> {
    let sql=format!("SELECT EXISTS(SELECT 1 FROM memory_nodes n WHERE n.id=?1 AND n.invalidated=0 AND n.privacy_epoch IS ?3 AND {})",node_tag("n.id","?2","?3"));
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .bind(tag)
        .bind(epoch)
        .fetch_one(conn)
        .await
}
async fn evidence_has_tag(
    conn: &mut sqlx::SqliteConnection,
    e: &MemoryEvidence,
    tag: &str,
) -> Result<bool, sqlx::Error> {
    let sql=match e.source_type.as_str() {
        "frame"|"audio"|"ui_event"=>format!("SELECT {}",source_tag(&e.source_type,"?1","?2")),
        "existing_memory"=>format!("SELECT EXISTS(SELECT 1 FROM memories m LEFT JOIN frames f ON f.id=m.frame_id WHERE m.id=?1 AND (m.frame_id IS NULL OR f.id IS NOT NULL) AND {})",legacy_tag("?2")),
        "semantic_context"=>{
            let Some(frame)=e.frame_id else{return Ok(false);};
            let sql=format!("SELECT EXISTS(SELECT 1 FROM semantic_items s JOIN semantic_run_items sri ON sri.item_id=s.id JOIN frames f ON f.semantic_run_id=sri.run_id WHERE s.id=?1 AND f.id=?3 AND {})",frame_tag("f.id","?2"));
            return sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).bind(e.source_id).bind(tag).bind(frame).fetch_one(conn).await;
        },
        _=>return Ok(false),
    };
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(e.source_id)
        .bind(tag)
        .fetch_one(conn)
        .await
}

impl DatabaseManager {
    pub async fn list_memory_nodes(
        &self,
        query: &MemoryTreeQuery,
    ) -> Result<Vec<MemoryNode>, ExplicitMemoryError> {
        self.memory_reader_ready().await?;
        if query.q.as_ref().is_some_and(|q| q.len() > 1024)
            || query.app.as_ref().is_some_and(|a| a.len() > 256)
            || query
                .tag
                .as_ref()
                .is_some_and(|tag| tag.is_empty() || tag.len() > 256)
        {
            return Err(invalid("memory_query_too_large"));
        }
        if query
            .kind
            .as_ref()
            .is_some_and(|k| !matches!(k.as_str(), "day" | "session" | "task"))
        {
            return Err(invalid("invalid_memory_node_kind"));
        }
        for value in [&query.start_time, &query.end_time].into_iter().flatten() {
            time(value)?;
        }
        if let Some(offset) = query.timezone_offset_minutes {
            validate_offset(offset)?;
        }
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let current = epoch(&mut tx).await?;
        let q = query.q.as_deref().unwrap_or("");
        let fts = crate::sanitize_fts5_query(q);
        let fts = if fts.is_empty() { "\"\"".into() } else { fts };
        let sql=format!("SELECT n.*,({NODE_DIRECT_MATCH}) AS non_summary_match FROM memory_nodes n WHERE n.invalidated=0 AND n.privacy_epoch IS ?1 AND ((?2 IS NULL AND ?3 IS NOT NULL) OR n.parent_id IS ?2) AND (?3 IS NULL OR n.kind=?3) AND (?4 IS NULL OR julianday(n.start_time)>=julianday(?4)) AND (?5 IS NULL OR julianday(n.start_time)<julianday(?5)) AND (?6 IS NULL OR EXISTS(SELECT 1 FROM json_each(n.apps_json) WHERE value=?6)) AND (?7='' OR instr(lower(n.label),lower(?7))>0 OR n.id IN (SELECT node_id FROM memory_nodes_fts WHERE memory_nodes_fts MATCH ?8) OR n.id IN (SELECT s.node_id FROM memory_node_summaries s WHERE s.status='ready' AND s.node_fingerprint=n.fingerprint AND s.privacy_epoch IS ?1 AND (s.node_id IN (SELECT node_id FROM memory_summary_fts WHERE memory_summary_fts MATCH ?8) OR instr(lower(s.text),lower(?7))>0)) OR (n.kind='day' AND EXISTS(SELECT 1 FROM memory_nodes child WHERE child.day_key=n.day_key AND child.invalidated=0 AND child.privacy_epoch IS ?1 AND instr(lower(child.label),lower(?7))>0))) AND (?9 IS NULL OR n.timezone_offset_minutes=?9) AND (?12 IS NULL OR {}) ORDER BY n.start_time DESC,n.id LIMIT ?10 OFFSET ?11",node_tag("n.id","?12","?1"));
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(current)
            .bind(query.parent_id.as_deref())
            .bind(query.kind.as_deref())
            .bind(query.start_time.as_deref())
            .bind(query.end_time.as_deref())
            .bind(query.app.as_deref())
            .bind(q)
            .bind(fts)
            .bind(query.timezone_offset_minutes)
            .bind(if query.limit == 0 {
                50
            } else {
                query.limit.min(100)
            })
            .bind(query.offset.min(100_000))
            .bind(query.tag.as_deref())
            .fetch_all(&mut *tx)
            .await?;
        let mut result = Vec::new();
        for row in rows {
            let node = row_node(&row)?;
            if let Some(tag) = query.tag.as_deref() {
                if !node_has_tag(&mut tx, &node.id, tag, current).await? {
                    continue;
                }
            }
            result.push((node, row.try_get::<bool, _>("non_summary_match")?));
        }
        tx.commit().await?;
        drop(conn);
        let mut released = Vec::new();
        for (node, direct_match) in result {
            if !direct_match {
                // FTS is a candidate index, not a release receipt. A summary
                // can become dynamically stale after a payload-only change
                // without its old FTS terms being removed by a node rebuild.
                let Some(summary) = self.get_hierarchy_summary(&node.id).await? else {
                    continue;
                };
                if summary.status != "ready"
                    || summary.node_fingerprint != node.fingerprint
                    || !summary.text.as_deref().is_some_and(|text| {
                        crate::text_normalizer::text_matches_search_query(text, q, false)
                    })
                {
                    continue;
                }
            }
            released.push(node);
        }
        if self.memory_index_epoch().await? != current {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        Ok(released)
    }

    /// Exact hydration goes through the current protected read adapter, after
    /// candidate selection. Missing or unreleased records are explicit gaps.
    pub async fn hydrate_memory_evidence(
        &self,
        source_type: &str,
        source_id: i64,
    ) -> Result<MemoryEvidence, ExplicitMemoryError> {
        self.memory_reader_ready().await?;
        let mut conn = self.acquire_read().await?;
        let (sql,uri)=match source_type {
            "frame"=>("SELECT timestamp,app_name,window_name AS window_title,browser_url,NULL AS excerpt,id AS frame_id FROM frames WHERE id=?",Some(format!("screenpipe://frame/{source_id}"))),
            "ui_event"=>("SELECT timestamp,app_name,window_title,browser_url,substr(COALESCE(text_content,element_name),1,8000) AS excerpt,frame_id FROM ui_events WHERE id=?",Some(format!("screenpipe://event/{source_id}"))),
            "audio"=>("SELECT timestamp,NULL AS app_name,NULL AS window_title,NULL AS browser_url,substr(transcription,1,8000) AS excerpt,NULL AS frame_id FROM audio_transcriptions WHERE id=?",Some(format!("screenpipe://transcript/{source_id}"))),
            _=>return Err(invalid("invalid_memory_source")),
        };
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(source_id)
            .fetch_optional(&mut *conn)
            .await?;
        let mut evidence = MemoryEvidence {
            source_type: source_type.into(),
            source_id,
            timestamp: String::new(),
            app_name: None,
            excerpt: None,
            frame_id: None,
            mark_id: None,
            uri,
            available: row.is_some(),
            window_title: None,
            browser_url: None,
        };
        if let Some(row) = row {
            let at: DateTime<Utc> = row.try_get("timestamp")?;
            evidence.timestamp = iso(at);
            evidence.app_name = row.try_get("app_name")?;
            evidence.excerpt = row.try_get("excerpt")?;
            evidence.frame_id = row.try_get("frame_id")?;
            evidence.window_title = row.try_get("window_title")?;
            evidence.browser_url = row.try_get("browser_url")?;
        }
        drop(conn);
        if source_type == "frame" && evidence.available {
            // The typed reader owns Hybrid read budgets and version/privacy
            // admission. Hold one exact payload at a time and retain only its
            // bounded excerpt; never decode a day into the response buffer.
            let mut payloads = self
                .frame_payloads(&[source_id], crate::storage::Projection::Search)
                .await?;
            if let Some(payload) = payloads.remove(&source_id) {
                evidence.excerpt = payload
                    .full_text
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or(payload.accessibility_text.as_deref())
                    .map(|s| bounded(s, 8000));
            } else {
                evidence.available = false;
            }
        }
        Ok(evidence)
    }

    pub async fn get_memory_node(
        &self,
        id: &str,
        cutoff: Option<&str>,
        limit: u32,
    ) -> Result<Option<MemoryNodeDetail>, ExplicitMemoryError> {
        self.memory_reader_ready().await?;
        if id.len() > 200 {
            return Err(invalid("invalid_memory_node_id"));
        }
        if let Some(cutoff) = cutoff {
            time(cutoff)?;
        }
        let limit = limit.clamp(1, 100);
        let before = self.memory_index_epoch().await?;
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let Some(row)=sqlx::query("SELECT * FROM memory_nodes WHERE id=? AND invalidated=0 AND privacy_epoch IS ? AND (? IS NULL OR julianday(start_time)>=julianday(?))")
            .bind(id).bind(before).bind(cutoff).bind(cutoff).fetch_optional(&mut *tx).await? else {return Ok(None);};
        let node = row_node(&row)?;
        let sql=format!("{DESCENDANTS} SELECT e.source_type,e.source_id,min(e.occurred_at) AS occurred_at FROM memory_evidence e JOIN descendants d ON e.node_id=d.id GROUP BY e.source_type,e.source_id ORDER BY occurred_at,e.source_type,e.source_id LIMIT ?2");
        let refs = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await?;
        let count_sql=format!("{DESCENDANTS} SELECT count(*) FROM (SELECT DISTINCT e.source_type,e.source_id FROM memory_evidence e JOIN descendants d ON e.node_id=d.id)");
        let evidence_total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count_sql))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let marks_sql=format!("{DESCENDANTS} SELECT DISTINCT m.mark_id FROM memory_node_marks m JOIN descendants d ON m.node_id=d.id JOIN explicit_memory_marks mark ON mark.id=m.mark_id WHERE (?2 IS NULL OR julianday(mark.source_timestamp)>=julianday(?2)) ORDER BY m.mark_id LIMIT ?3");
        let mark_ids: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(marks_sql))
            .bind(id)
            .bind(cutoff)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await?;
        let mark_count_sql=format!("{DESCENDANTS} SELECT count(DISTINCT m.mark_id) FROM memory_node_marks m JOIN descendants d ON m.node_id=d.id JOIN explicit_memory_marks mark ON mark.id=m.mark_id WHERE (?2 IS NULL OR julianday(mark.source_timestamp)>=julianday(?2))");
        let mark_total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(mark_count_sql))
            .bind(id)
            .bind(cutoff)
            .fetch_one(&mut *tx)
            .await?;
        let children_total:i64=sqlx::query_scalar("SELECT count(*) FROM memory_nodes WHERE parent_id=? AND invalidated=0 AND privacy_epoch IS ?").bind(id).bind(before).fetch_one(&mut *tx).await?;
        let children=sqlx::query("SELECT * FROM memory_nodes WHERE parent_id=? AND invalidated=0 AND privacy_epoch IS ? ORDER BY start_time,id LIMIT ?").bind(id).bind(before).bind(limit).fetch_all(&mut *tx).await?.iter().map(row_node).collect::<Result<Vec<_>,_>>()?;
        tx.commit().await?;
        drop(conn);
        let mut evidence = Vec::new();
        for row in refs {
            let mut item = self
                .hydrate_memory_evidence(
                    &row.try_get::<String, _>("source_type")?,
                    row.try_get("source_id")?,
                )
                .await?;
            if item.timestamp.is_empty() {
                item.timestamp = row.try_get("occurred_at")?;
            }
            if cutoff.is_some_and(|cutoff| time(&item.timestamp).ok() < time(cutoff).ok()) {
                continue;
            }
            evidence.push(item);
        }
        let mut marks = Vec::new();
        for id in mark_ids {
            if let Some(mark) = self.get_explicit_memory_mark(&id).await? {
                marks.push(mark);
            }
        }
        if self.memory_index_epoch().await? != before {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let truncated = evidence_total > evidence.len() as i64
            || mark_total > marks.len() as i64
            || children_total > children.len() as i64;
        // Typed payload reads can await storage outside the metadata snapshot.
        // Reject a node erased/replaced meanwhile, even with PII disabled.
        let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_nodes WHERE id=? AND fingerprint=? AND invalidated=0)")
            .bind(&node.id).bind(&node.fingerprint).fetch_one(&mut *self.acquire_read().await?).await?;
        if !current {
            return Err(ExplicitMemoryError::Conflict);
        }
        Ok(Some(MemoryNodeDetail {
            node,
            evidence,
            marks,
            children,
            evidence_total,
            mark_total,
            children_total,
            truncated,
        }))
    }

    /// Direct mark/FTS retrieval and bounded graph context. Final source
    /// hydration is protected and rechecks the query against released text, so
    /// raw FTS membership cannot disclose a redacted term.
    pub async fn search_memory(
        &self,
        query: &MemoryTreeQuery,
    ) -> Result<Vec<MemorySearchHit>, ExplicitMemoryError> {
        self.memory_reader_ready().await?;
        let q = query.q.as_deref().unwrap_or("").trim();
        if q.is_empty() || q.len() > 1024 {
            return Err(invalid("memory_search_requires_bounded_query"));
        }
        if query
            .tag
            .as_ref()
            .is_some_and(|tag| tag.is_empty() || tag.len() > 256)
        {
            return Err(invalid("invalid_memory_tag"));
        }
        let before = self.memory_index_epoch().await?;
        let limit = if query.limit == 0 {
            50
        } else {
            query.limit.min(100)
        };
        for at in [&query.start_time, &query.end_time].into_iter().flatten() {
            time(at)?;
        }
        let fts = crate::sanitize_fts5_query(q);
        let fts = if fts.is_empty() {
            "\"\"".to_string()
        } else {
            fts
        };
        let mut conn = self.acquire_read().await?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT m.id FROM explicit_memory_marks m JOIN explicit_memory_search_docs d ON d.mark_id=m.id WHERE m.privacy_epoch IS ?1 AND (?2 IS NULL OR julianday(m.source_timestamp)>=julianday(?2)) AND (?3 IS NULL OR julianday(m.source_timestamp)<julianday(?3)) AND (?4 IS NULL OR m.source_app=?4) AND (?5 IS NULL OR EXISTS(SELECT 1 FROM explicit_memory_mark_tags mt JOIN tags t ON t.id=mt.tag_id WHERE mt.mark_id=m.id AND t.name=?5)) AND (d.rowid IN (SELECT rowid FROM explicit_memory_fts WHERE explicit_memory_fts MATCH ?6) OR instr(lower(d.text),lower(?7))>0) ORDER BY m.importance DESC,julianday(m.source_timestamp) DESC,m.id LIMIT ?8")
            .bind(before).bind(query.start_time.as_deref()).bind(query.end_time.as_deref()).bind(query.app.as_deref()).bind(query.tag.as_deref()).bind(&fts).bind(q).bind(limit).fetch_all(&mut *conn).await?;
        drop(conn);
        let mut marks = Vec::new();
        for id in ids {
            if let Some(mark) = self.get_explicit_memory_mark(&id).await? {
                marks.push(mark);
            }
        }
        let mut result = Vec::new();
        let mut seen = Set::new();
        for mark in marks {
            if query
                .tag
                .as_ref()
                .is_some_and(|tag| !mark.tags.contains(tag))
            {
                continue;
            }
            if query
                .end_time
                .as_ref()
                .is_some_and(|end| time(&mark.source_timestamp).ok() >= time(end).ok())
                || query
                    .app
                    .as_ref()
                    .is_some_and(|app| mark.source_app.as_ref() != Some(app))
            {
                continue;
            }
            seen.insert(format!("frame:{}", mark.source_frame_id));
            result.push(MemorySearchHit {
                kind: "user_note".into(),
                score: 3.0 + mark.importance,
                node: None,
                mark: Some(mark),
                evidence: None,
            });
        }
        let mut node_query = query.clone();
        node_query.parent_id = None;
        node_query.offset = 0;
        for kind in ["task", "session", "day"] {
            node_query.kind = Some(kind.into());
            for node in self
                .list_memory_nodes(&node_query)
                .await?
                .into_iter()
                .take(20)
            {
                result.push(MemorySearchHit {
                    kind: "context_node".into(),
                    score: 2.0,
                    node: Some(node),
                    mark: None,
                    evidence: None,
                });
            }
        }
        let fts = crate::sanitize_fts5_query(q);
        let fts = if fts.is_empty() { "\"\"".into() } else { fts };
        let mut conn = self.acquire_read().await?;
        let mut candidates = Vec::new();
        for (kind,sql) in [
            ("frame",format!("SELECT f.id FROM frames_fts JOIN frames f ON f.id=frames_fts.rowid WHERE frames_fts MATCH ?1 AND (?2 IS NULL OR julianday(f.timestamp)>=julianday(?2)) AND (?3 IS NULL OR julianday(f.timestamp)<julianday(?3)) AND (?4 IS NULL OR f.app_name=?4) AND (?5 IS NULL OR {}) ORDER BY julianday(f.timestamp) DESC LIMIT 50",frame_tag("f.id","?5"))),
            ("audio",format!("SELECT a.id FROM audio_transcriptions_fts JOIN audio_transcriptions a ON a.id=audio_transcriptions_fts.rowid WHERE audio_transcriptions_fts MATCH ?1 AND (?2 IS NULL OR julianday(a.timestamp)>=julianday(?2)) AND (?3 IS NULL OR julianday(a.timestamp)<julianday(?3)) AND ?4 IS NULL AND (?5 IS NULL OR {}) ORDER BY julianday(a.timestamp) DESC LIMIT 50",audio_tag("a.audio_chunk_id","?5")))
        ] {
            for id in sqlx::query_scalar::<_,i64>(sqlx::AssertSqlSafe(sql)).bind(&fts).bind(query.start_time.as_deref()).bind(query.end_time.as_deref()).bind(query.app.as_deref()).bind(query.tag.as_deref()).fetch_all(&mut *conn).await? {candidates.push((kind.to_string(),id));}
        }
        // unicode61 does not tokenize arbitrary CJK substrings. Inspect only a
        // bounded recent index sample rather than scanning all capture history.
        let cjk_sql=format!("SELECT e.source_type,e.source_id FROM memory_evidence e JOIN memory_nodes n ON n.id=e.node_id LEFT JOIN ui_events u ON e.source_type='ui_event' AND u.id=e.source_id LEFT JOIN frames f ON f.id=CASE WHEN e.source_type='frame' THEN e.source_id WHEN e.source_type='ui_event' THEN u.frame_id END WHERE n.invalidated=0 AND n.privacy_epoch IS ?1 AND (?2 IS NULL OR julianday(e.occurred_at)>=julianday(?2)) AND (?3 IS NULL OR julianday(e.occurred_at)<julianday(?3)) AND (?4 IS NULL OR COALESCE(f.app_name,u.app_name)=?4) AND (?5 IS NULL OR (e.source_type='frame' AND {}) OR (e.source_type='audio' AND {}) OR (e.source_type='ui_event' AND {})) ORDER BY e.occurred_at DESC LIMIT 100",source_tag("frame","e.source_id","?5"),source_tag("audio","e.source_id","?5"),source_tag("ui_event","e.source_id","?5"));
        let rows = sqlx::query(sqlx::AssertSqlSafe(cjk_sql))
            .bind(before)
            .bind(query.start_time.as_deref())
            .bind(query.end_time.as_deref())
            .bind(query.app.as_deref())
            .bind(query.tag.as_deref())
            .fetch_all(&mut *conn)
            .await?;
        for row in rows {
            candidates.push((row.try_get("source_type")?, row.try_get("source_id")?));
        }
        for (kind,sql) in [
            ("existing_memory", format!("SELECT m.id,m.created_at AS timestamp,substr(m.content,1,8000) AS excerpt,m.frame_id,f.app_name,f.window_name AS window_title,f.browser_url FROM memories_fts JOIN memories m ON m.id=memories_fts.rowid LEFT JOIN frames f ON f.id=m.frame_id WHERE memories_fts MATCH ?1 AND (m.frame_id IS NULL OR f.id IS NOT NULL) AND (?2 IS NULL OR julianday(m.created_at)>=julianday(?2)) AND (?3 IS NULL OR julianday(m.created_at)<julianday(?3)) AND (?4 IS NULL OR f.app_name=?4) AND (?5 IS NULL OR {}) ORDER BY julianday(m.created_at) DESC LIMIT 50",legacy_tag("?5"))),
            ("semantic_context", format!("SELECT s.id,max(julianday(f.timestamp)) AS source_time,f.timestamp AS timestamp,substr(COALESCE(s.title,'')||char(10)||COALESCE(s.body,''),1,8000) AS excerpt,f.id AS frame_id,f.app_name,f.window_name AS window_title,f.browser_url FROM semantic_items_fts JOIN semantic_items s ON s.id=semantic_items_fts.rowid JOIN semantic_run_items sri ON sri.item_id=s.id JOIN frames f ON f.semantic_run_id=sri.run_id WHERE semantic_items_fts MATCH ?1 AND (?2 IS NULL OR julianday(f.timestamp)>=julianday(?2)) AND (?3 IS NULL OR julianday(f.timestamp)<julianday(?3)) AND (?4 IS NULL OR f.app_name=?4) AND (?5 IS NULL OR {}) GROUP BY s.id ORDER BY source_time DESC LIMIT 50",frame_tag("f.id","?5")))
        ] {
            let rows=sqlx::query(sqlx::AssertSqlSafe(sql)).bind(&fts).bind(query.start_time.as_deref()).bind(query.end_time.as_deref()).bind(query.app.as_deref()).bind(query.tag.as_deref()).fetch_all(&mut *conn).await?;
            for row in rows {
                let excerpt:Option<String>=row.try_get("excerpt")?;
                if !excerpt.as_deref().unwrap_or("").to_lowercase().contains(&q.to_lowercase()) {continue;}
                let id:i64=row.try_get("id")?;
                let at:DateTime<Utc>=row.try_get("timestamp")?;
                let frame_id:Option<i64>=row.try_get("frame_id")?;
                let evidence=MemoryEvidence{source_type:kind.into(),source_id:id,timestamp:iso(at),app_name:row.try_get("app_name")?,excerpt,frame_id,mark_id:None,uri:Some(if kind=="existing_memory" {format!("screenpipe://memory/{id}")} else {format!("screenpipe://semantic/{id}")}),available:true,window_title:row.try_get("window_title")?,browser_url:row.try_get("browser_url")?};
                result.push(MemorySearchHit{kind:kind.into(),score:0.8,node:None,mark:None,evidence:Some(evidence)});
            }
        }
        drop(conn);
        for (kind, id) in candidates {
            if !seen.insert(format!("{kind}:{id}")) {
                continue;
            }
            let evidence = self.hydrate_memory_evidence(&kind, id).await?;
            if let Some(tag) = query.tag.as_deref() {
                if !evidence_has_tag(&mut *self.acquire_read().await?, &evidence, tag).await? {
                    continue;
                }
            }
            let haystack = format!(
                "{} {} {}",
                evidence.excerpt.as_deref().unwrap_or(""),
                evidence.app_name.as_deref().unwrap_or(""),
                evidence.window_title.as_deref().unwrap_or("")
            )
            .to_lowercase();
            if !evidence.available
                || !haystack.contains(&q.to_lowercase())
                || query
                    .app
                    .as_ref()
                    .is_some_and(|app| evidence.app_name.as_ref() != Some(app))
            {
                continue;
            }
            result.push(MemorySearchHit {
                kind: "captured_text".into(),
                score: 1.0,
                node: None,
                mark: None,
                evidence: Some(evidence),
            });
        }
        if let Some(tag) = query.tag.as_deref() {
            // Recheck hydrated candidates together in one current snapshot.
            // Never fall back to an untagged source when a link vanished.
            let mut conn = self.acquire_read().await?;
            let mut tx = conn.begin().await?;
            let current = epoch(&mut tx).await?;
            let mut filtered = Vec::new();
            for hit in result {
                let matches = if let Some(mark) = &hit.mark {
                    sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM explicit_memory_marks m JOIN explicit_memory_mark_tags mt ON mt.mark_id=m.id JOIN tags t ON t.id=mt.tag_id WHERE m.id=? AND m.privacy_epoch IS ? AND t.name=?)")
                        .bind(&mark.id).bind(current).bind(tag).fetch_one(&mut *tx).await?
                } else if let Some(node) = &hit.node {
                    node_has_tag(&mut tx, &node.id, tag, current).await?
                } else if let Some(evidence) = &hit.evidence {
                    evidence_has_tag(&mut tx, evidence, tag).await?
                } else {
                    false
                };
                if matches {
                    filtered.push(hit);
                }
            }
            tx.commit().await?;
            result = filtered;
        }
        if self.memory_index_epoch().await? != before {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        result.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(result
            .into_iter()
            .filter(|hit| {
                query.source.as_ref().is_none_or(|source| {
                    source == &hit.kind
                        || hit
                            .evidence
                            .as_ref()
                            .is_some_and(|e| source == &e.source_type)
                })
            })
            .skip(query.offset.min(1000) as usize)
            .take(limit as usize)
            .collect())
    }
}
