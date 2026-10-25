// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

use super::*;
use serde_json::json;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SourceRef {
    kind: String,
    id: i64,
    at: String,
    digest: String,
    app: Option<String>,
}
#[derive(Clone)]
struct Built {
    node: MemoryNode,
    refs: Vec<SourceRef>,
    marks: Vec<(String, i64)>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Anchor {
    id: String,
    kind: String,
    source: Option<(String, i64)>,
    mark: Option<String>,
    #[serde(default)]
    start_time: String,
    #[serde(default)]
    end_time: String,
    #[serde(default)]
    canonical_label: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Override {
    targets: Vec<Anchor>,
    label: Option<String>,
    at: Option<String>,
}

fn empty_node(
    id: String,
    parent: Option<String>,
    kind: &str,
    label: String,
    start: String,
    end: String,
) -> Built {
    Built {
        node: MemoryNode {
            id,
            parent_id: parent,
            kind: kind.into(),
            label,
            label_origin: "deterministic".into(),
            start_time: start,
            end_time: end,
            provisional: true,
            evidence_count: 0,
            mark_count: 0,
            apps: Vec::new(),
            revision: 1,
            fingerprint: String::new(),
            children: Vec::new(),
        },
        refs: Vec::new(),
        marks: Vec::new(),
    }
}

/// Project/document identity takes precedence over app identity. This links a
/// repository opened in browser, editor and terminal into the same task. When
/// deterministic identity is weak, the honest label remains user-correctable.
fn topic(title: &str, url: Option<&str>, path: Option<&str>) -> String {
    if let Some(url) = url {
        if let Some(rest) = url.split("github.com/").nth(1) {
            if let Some(repo) = rest.split('/').nth(1).filter(|s| !s.is_empty()) {
                return repo.split(['?', '#']).next().unwrap_or(repo).to_lowercase();
            }
        }
    }
    if let Some(path) = path {
        for root in ["/workspace/", "/projects/", "/repos/", "/work/"] {
            if let Some(project) = path
                .split(root)
                .nth(1)
                .and_then(|p| p.split('/').next())
                .filter(|p| !p.is_empty())
            {
                return project.to_lowercase();
            }
        }
    }
    // Browsers and editors can append profiles/workspaces after their app
    // chrome ("document - Google Chrome - profile"). Cut at that boundary so
    // the same document remains one purpose across applications.
    let boundary = [
        " - Google Chrome",
        " — Google Chrome",
        " - Chromium",
        " - Brave",
        " - Microsoft Edge",
        " — Mozilla Firefox",
        " - Mozilla Firefox",
        " - Visual Studio Code",
        " — Visual Studio Code",
    ]
    .into_iter()
    .filter_map(|suffix| title.find(suffix))
    .min()
    .unwrap_or(title.len());
    let value = title[..boundary].trim();
    // Browser document titles often carry the same repository name as editor
    // window titles. Strip activity verbs rather than introducing an App layer.
    let value = value
        .split(" · ")
        .next()
        .unwrap_or(value)
        .split(" | ")
        .next()
        .unwrap_or(value)
        .trim();
    bounded(
        if value.is_empty() {
            "Unclassified activity"
        } else {
            value
        },
        180,
    )
    .to_lowercase()
}

fn recompute(
    nodes: &mut Map<String, Built>,
    epoch: Option<i64>,
) -> Result<(), ExplicitMemoryError> {
    for kind in ["task", "session", "day"] {
        let ids: Vec<_> = nodes
            .values()
            .filter(|n| n.node.kind == kind)
            .map(|n| n.node.id.clone())
            .collect();
        for id in ids {
            let children: Vec<_> = nodes
                .values()
                .filter(|n| n.node.parent_id.as_deref() == Some(&id))
                .cloned()
                .collect();
            let n = nodes.get_mut(&id).unwrap();
            if !children.is_empty() {
                n.refs = children.iter().flat_map(|c| c.refs.clone()).collect();
                n.marks = children.iter().flat_map(|c| c.marks.clone()).collect();
                if kind != "day" {
                    n.node.start_time = children
                        .iter()
                        .map(|c| c.node.start_time.clone())
                        .min()
                        .unwrap();
                    n.node.end_time = children
                        .iter()
                        .map(|c| c.node.end_time.clone())
                        .max()
                        .unwrap();
                }
            }
            n.refs.sort_by(|a, b| {
                a.at.cmp(&b.at)
                    .then(a.kind.cmp(&b.kind))
                    .then(a.id.cmp(&b.id))
            });
            let mut seen = Set::new();
            n.refs.retain(|r| {
                seen.insert((
                    r.kind.clone(),
                    r.id,
                    if r.kind == "mark" {
                        r.digest.clone()
                    } else {
                        String::new()
                    },
                ))
            });
            n.marks.sort();
            n.marks.dedup();
            n.node.evidence_count = n.refs.iter().filter(|r| r.kind != "mark").count() as i64;
            n.node.mark_count = n.marks.len() as i64;
            n.node.apps = n
                .refs
                .iter()
                .filter_map(|r| r.app.clone())
                .collect::<Set<_>>()
                .into_iter()
                .collect();
            n.node.provisional =
                time(&n.node.end_time)? > Utc::now() - chrono::Duration::minutes(5);
            let inputs = json!([
                n.node.id,
                n.node.parent_id,
                n.node.kind,
                n.node.label,
                n.node.label_origin,
                n.node.start_time,
                n.node.end_time,
                n.node.provisional,
                n.refs,
                n.marks,
                children
                    .iter()
                    .map(|c| &c.node.fingerprint)
                    .collect::<Vec<_>>(),
                epoch
            ]);
            n.node.fingerprint = hash(inputs.to_string());
        }
    }
    Ok(())
}

fn resolve(nodes: &Map<String, Built>, anchor: &Anchor) -> Option<String> {
    if nodes.contains_key(&anchor.id) {
        return Some(anchor.id.clone());
    }
    let by_source = nodes
        .values()
        .find(|n| {
            n.node.kind == anchor.kind
                && (anchor.source.as_ref().is_some_and(|(kind, id)| {
                    n.refs.iter().any(|r| &r.kind == kind && r.id == *id)
                }) || anchor
                    .mark
                    .as_ref()
                    .is_some_and(|id| n.marks.iter().any(|m| &m.0 == id)))
        })
        .map(|n| n.node.id.clone());
    if by_source.is_some() {
        return by_source;
    }
    // Ledger reconciliation can change which first/last observations it keeps.
    // Authored actions therefore also carry their purpose/time identity, not
    // merely a sampled source ID. Prefer the greatest coherent overlap.
    let start = time(&anchor.start_time).ok()?;
    let end = time(&anchor.end_time).ok()?;
    nodes
        .values()
        .filter(|n| {
            n.node.kind == anchor.kind
                && (anchor.kind != "task" || hash(&n.node.label) == anchor.canonical_label)
        })
        .filter_map(|n| {
            let overlap =
                time(&n.node.end_time).ok()?.min(end) - time(&n.node.start_time).ok()?.max(start);
            (overlap > chrono::Duration::zero()).then_some((overlap, n.node.id.clone()))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
        .map(|(_, id)| id)
}
fn apply_override(
    nodes: &mut Map<String, Built>,
    id: &str,
    op: &str,
    payload: &Override,
) -> Result<(), ExplicitMemoryError> {
    let mut ids: Vec<_> = payload
        .targets
        .iter()
        .filter_map(|a| resolve(nodes, a))
        .collect();
    let mut seen = Set::new();
    ids.retain(|id| seen.insert(id.clone()));
    let Some(target) = ids.first().cloned() else {
        return Ok(());
    };
    match op {
        "label" => {
            if let Some(label) = &payload.label {
                let n = nodes.get_mut(&target).unwrap();
                n.node.label = label.clone();
                n.node.label_origin = "user".into();
            }
        }
        "merge" => {
            if ids.len() < 2 {
                return Ok(());
            }
            let first = nodes.get(&target).unwrap().clone();
            let mut merged = first.clone();
            merged.node.id = format!("u:{}", hash(id));
            merged.node.label = payload.label.clone().unwrap_or(first.node.label);
            merged.node.label_origin = "user".into();
            for other in ids.iter().skip(1) {
                if let Some(n) = nodes.get(other) {
                    merged.refs.extend(n.refs.clone());
                    merged.marks.extend(n.marks.clone());
                    merged.node.start_time = merged.node.start_time.min(n.node.start_time.clone());
                    merged.node.end_time = merged.node.end_time.max(n.node.end_time.clone());
                }
            }
            for n in nodes.values_mut() {
                if n.node.parent_id.as_ref().is_some_and(|p| ids.contains(p)) {
                    n.node.parent_id = Some(merged.node.id.clone());
                }
            }
            for id in ids {
                nodes.remove(&id);
            }
            nodes.insert(merged.node.id.clone(), merged);
        }
        "split" => {
            let at = payload
                .at
                .as_ref()
                .ok_or_else(|| invalid("split_time_required"))?;
            let original = nodes.get(&target).unwrap().clone();
            if original.node.kind != "task" {
                return Err(invalid("split_requires_task"));
            }
            let pivot = time(at)?;
            if pivot <= time(&original.node.start_time)? || pivot >= time(&original.node.end_time)?
            {
                // Keep the authored action, but do not invent inverted ranges
                // when retention or late evidence narrows its original task.
                return Ok(());
            }
            let mut left = original.clone();
            let mut right = original.clone();
            left.node.id = format!("u:{}", hash(format!("{id}:left")));
            right.node.id = format!("u:{}", hash(format!("{id}:right")));
            left.node.end_time = iso(pivot);
            right.node.start_time = iso(pivot);
            left.node.label_origin = "user".into();
            right.node.label_origin = "user".into();
            if let Some(label) = &payload.label {
                right.node.label = label.clone();
            }
            left.refs.retain(|r| time(&r.at).is_ok_and(|at| at < pivot));
            right
                .refs
                .retain(|r| time(&r.at).is_ok_and(|at| at >= pivot));
            // Mark timestamps are resolved before applying actions and retained
            // in the per-node timestamp map below by source pseudo references.
            let right_marks: Set<_> = right
                .refs
                .iter()
                .filter(|r| r.kind == "mark")
                .map(|r| r.digest.clone())
                .collect();
            let left_marks: Set<_> = left
                .refs
                .iter()
                .filter(|r| r.kind == "mark")
                .map(|r| r.digest.clone())
                .collect();
            left.marks.retain(|(id, _)| left_marks.contains(id));
            right.marks.retain(|(id, _)| right_marks.contains(id));
            nodes.remove(&target);
            nodes.insert(left.node.id.clone(), left);
            nodes.insert(right.node.id.clone(), right);
        }
        _ => return Err(invalid("invalid_memory_correction")),
    }
    Ok(())
}

async fn persist(
    db: &DatabaseManager,
    day: &str,
    offset: i32,
    nodes: &Map<String, Built>,
    expected: Option<i64>,
    day_revision: Option<i64>,
    correction: Option<(&str, &str, &str, &Override)>,
) -> Result<(), ExplicitMemoryError> {
    if nodes.len() > MAX_DAY_NODES {
        return Err(invalid("memory_day_node_budget_exceeded"));
    }
    // Prepare and diff outside the writer. The day revision comparison below
    // makes this snapshot safe against concurrent edits or another projection.
    let rows = sqlx::query("SELECT id,fingerprint,invalidated FROM memory_nodes WHERE day_key=?")
        .bind(day)
        .fetch_all(&mut *db.acquire_read().await?)
        .await?;
    let old: Map<String, (String, bool)> = rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get("id")?,
                (row.try_get("fingerprint")?, row.try_get("invalidated")?),
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?;
    let changed: Set<String> = nodes
        .values()
        .filter(|n| {
            old.get(&n.node.id)
                .is_none_or(|(fingerprint, invalidated)| {
                    *invalidated || fingerprint != &n.node.fingerprint
                })
        })
        .map(|n| n.node.id.clone())
        .collect();
    let changed_tasks: Vec<_> = nodes
        .values()
        .filter(|n| n.node.kind == "task" && changed.contains(&n.node.id))
        .collect();
    let changed_task_ids =
        serde_json::to_string(&changed_tasks.iter().map(|n| &n.node.id).collect::<Vec<_>>())
            .map_err(|_| invalid("invalid_memory_nodes"))?;
    let refs: Vec<_> = changed_tasks
        .iter()
        .flat_map(|n| {
            n.refs
                .iter()
                .filter(|r| r.kind != "mark")
                .map(move |r| (&n.node.id, r))
        })
        .collect();
    let marks: Vec<_> = changed_tasks
        .iter()
        .flat_map(|n| n.marks.iter().map(move |m| (&n.node.id, m)))
        .collect();
    if refs.len() > MAX_DAY_EVIDENCE || marks.len() > 1000 {
        return Err(invalid("memory_day_evidence_budget_exceeded"));
    }
    let apps: Map<_, _> = nodes
        .values()
        .filter(|n| changed.contains(&n.node.id))
        .map(|n| {
            (
                n.node.id.clone(),
                serde_json::to_string(&n.node.apps).unwrap_or_else(|_| "[]".into()),
            )
        })
        .collect();
    let keep = serde_json::to_string(&nodes.keys().collect::<Vec<_>>())
        .map_err(|_| invalid("invalid_memory_nodes"))?;
    let correction_json = correction
        .map(|(_, _, _, payload)| serde_json::to_string(payload))
        .transpose()
        .map_err(|_| invalid("invalid_memory_correction"))?;
    let now = iso(Utc::now());
    let mut tx = db.begin_immediate_with_retry().await?;
    let writer_started = std::time::Instant::now();
    let budget = || {
        if writer_started.elapsed() > std::time::Duration::from_millis(250) {
            Err(invalid("memory_index_writer_budget"))
        } else {
            Ok(())
        }
    };
    if epoch(&mut **tx.conn()).await? != expected {
        return Err(ExplicitMemoryError::PrivacyChanged);
    }
    let current_revision: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM memory_nodes WHERE id=?")
            .bind(day)
            .fetch_optional(&mut **tx.conn())
            .await?;
    let generation: i64 =
        sqlx::query_scalar("SELECT generation FROM memory_index_state WHERE id=1")
            .fetch_one(&mut **tx.conn())
            .await?;
    if current_revision != day_revision {
        return Err(ExplicitMemoryError::Conflict);
    }
    if let Some((override_id, operation, target, _payload)) = correction {
        sqlx::query("INSERT INTO memory_overrides(id,day_key,operation,target_id,payload_json,created_at) VALUES(?,?,?,?,?,?)")
            .bind(override_id).bind(day).bind(operation).bind(target).bind(correction_json.as_ref()).bind(&now).execute(&mut **tx.conn()).await?;
    }
    // At most 850 bindings per node statement, 750 per evidence/mark statement.
    // The worst allowed day needs ~110 statements, never 12k await handoffs.
    // A slow index transaction rolls back and retries later; capture keeps its
    // admission and writer priority instead of paying an unbounded rebuild.
    for kind in ["day", "session", "task"] {
        let items: Vec<_> = nodes
            .values()
            .filter(|n| n.node.kind == kind && changed.contains(&n.node.id))
            .collect();
        for chunk in items.chunks(50) {
            budget()?;
            let mut sql=sqlx::QueryBuilder::<Sqlite>::new("INSERT INTO memory_nodes(id,parent_id,day_key,timezone_offset_minutes,kind,label,label_origin,start_time,end_time,provisional,evidence_count,mark_count,apps_json,fingerprint,privacy_epoch,updated_at,generation) ");
            sql.push_values(chunk.iter().copied(), |mut row, n| {
                let n = &n.node;
                row.push_bind(&n.id)
                    .push_bind(&n.parent_id)
                    .push_bind(day)
                    .push_bind(offset)
                    .push_bind(&n.kind)
                    .push_bind(&n.label)
                    .push_bind(&n.label_origin)
                    .push_bind(&n.start_time)
                    .push_bind(&n.end_time)
                    .push_bind(n.provisional)
                    .push_bind(n.evidence_count)
                    .push_bind(n.mark_count)
                    .push_bind(&apps[&n.id])
                    .push_bind(&n.fingerprint)
                    .push_bind(expected)
                    .push_bind(&now)
                    .push_bind(generation);
            });
            sql.push(" ON CONFLICT(id) DO UPDATE SET parent_id=excluded.parent_id,label=excluded.label,label_origin=excluded.label_origin,start_time=excluded.start_time,end_time=excluded.end_time,provisional=excluded.provisional,evidence_count=excluded.evidence_count,mark_count=excluded.mark_count,apps_json=excluded.apps_json,revision=memory_nodes.revision+1,fingerprint=excluded.fingerprint,privacy_epoch=excluded.privacy_epoch,invalidated=0,updated_at=excluded.updated_at,generation=excluded.generation");
            sql.build().execute(&mut **tx.conn()).await?;
        }
    }
    if !changed_tasks.is_empty() {
        budget()?;
        sqlx::query(
            "DELETE FROM memory_evidence WHERE node_id IN (SELECT value FROM json_each(?))",
        )
        .bind(&changed_task_ids)
        .execute(&mut **tx.conn())
        .await?;
        sqlx::query(
            "DELETE FROM memory_node_marks WHERE node_id IN (SELECT value FROM json_each(?))",
        )
        .bind(&changed_task_ids)
        .execute(&mut **tx.conn())
        .await?;
    }
    for chunk in refs.chunks(150) {
        budget()?;
        let mut sql=sqlx::QueryBuilder::<Sqlite>::new("INSERT OR IGNORE INTO memory_evidence(node_id,source_type,source_id,occurred_at,content_fingerprint) ");
        sql.push_values(chunk.iter().copied(), |mut row, (id, r)| {
            row.push_bind(id)
                .push_bind(&r.kind)
                .push_bind(r.id)
                .push_bind(&r.at)
                .push_bind(&r.digest);
        });
        sql.build().execute(&mut **tx.conn()).await?;
    }
    for chunk in marks.chunks(250) {
        budget()?;
        let mut sql = sqlx::QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO memory_node_marks(node_id,mark_id,mark_revision) ",
        );
        sql.push_values(chunk.iter().copied(), |mut row, (id, (mark, revision))| {
            row.push_bind(id).push_bind(mark).push_bind(revision);
        });
        sql.build().execute(&mut **tx.conn()).await?;
    }
    budget()?;
    sqlx::query(
        "DELETE FROM memory_nodes WHERE day_key=? AND id NOT IN (SELECT value FROM json_each(?))",
    )
    .bind(day)
    .bind(keep)
    .execute(&mut **tx.conn())
    .await?;
    budget()?;
    tx.commit().await?;
    Ok(())
}

impl DatabaseManager {
    /// Only the bounded background worker calls this. A whole local day is
    /// reconciled from already-persisted ledger references, never raw history
    /// scans during tree navigation. Excerpts are hashed then discarded.
    pub async fn rebuild_memory_day(
        &self,
        day_start: DateTime<Utc>,
        offset: i32,
    ) -> Result<(), ExplicitMemoryError> {
        validate_offset(offset)?;
        let expected = self.memory_index_epoch().await?;
        let local = day_start + chrono::Duration::minutes(i64::from(offset));
        let date = local.date_naive();
        let start = date.and_hms_opt(0, 0, 0).unwrap().and_utc()
            - chrono::Duration::minutes(i64::from(offset));
        let end = start + chrono::Duration::days(1);
        let day = format!("day:{offset}:{date}");
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let day_revision: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM memory_nodes WHERE id=?")
                .bind(&day)
                .fetch_optional(&mut *tx)
                .await?;
        let intervals=sqlx::query("SELECT i.id,i.start_at,i.end_at,t.title FROM activity_intervals i JOIN activity_tasks t ON t.id=i.task_id WHERE julianday(i.end_at)>julianday(?) AND julianday(i.start_at)<julianday(?) ORDER BY julianday(i.start_at),i.id LIMIT 12001")
            .bind(iso(start)).bind(iso(end)).fetch_all(&mut *tx).await?;
        if intervals.len() > MAX_DAY_EVIDENCE {
            return Err(invalid("memory_day_interval_budget_exceeded"));
        }
        // Sealed frame bodies are not SQL expressions. Fingerprint their
        // metadata generation here; hydrate exact text via the typed reader.
        let (payload_join, payload_generation) = if self.storage.is_some() {
            (
                "LEFT JOIN frame_payloads fp ON fp.frame_id=f.id",
                "fp.generation",
            )
        } else {
            ("", "NULL")
        };
        let refs_sql=format!("SELECT e.interval_id,e.source_type,e.source_id,e.occurred_at,COALESCE(f.app_name,u.app_name) AS app_name,COALESCE(f.window_name,u.window_title) AS window_title,COALESCE(f.browser_url,u.browser_url) AS browser_url,f.document_path,f.content_hash AS frame_content_hash,{payload_generation} AS frame_payload_generation,substr(COALESCE(u.text_content,u.element_name,a.transcription,''),1,8000) AS excerpt,CASE WHEN e.source_type='frame' THEN f.id WHEN e.source_type='ui_event' THEN u.id ELSE a.id END AS available FROM activity_evidence e JOIN activity_intervals i ON i.id=e.interval_id LEFT JOIN frames f ON e.source_type='frame' AND f.id=e.source_id {payload_join} LEFT JOIN ui_events u ON e.source_type='ui_event' AND u.id=e.source_id LEFT JOIN audio_transcriptions a ON e.source_type='audio' AND a.id=e.source_id WHERE julianday(e.occurred_at)>=julianday(?) AND julianday(e.occurred_at)<julianday(?) ORDER BY julianday(e.occurred_at),e.id LIMIT 12001");
        let refs = sqlx::query(sqlx::AssertSqlSafe(refs_sql))
            .bind(iso(start))
            .bind(iso(end))
            .fetch_all(&mut *tx)
            .await?;
        if refs.len() > MAX_DAY_EVIDENCE {
            return Err(invalid("memory_day_evidence_budget_exceeded"));
        }
        let marks=sqlx::query("SELECT id,source_timestamp,source_frame_id,source_app,title,revision FROM explicit_memory_marks WHERE julianday(source_timestamp)>=julianday(?) AND julianday(source_timestamp)<julianday(?) AND privacy_epoch IS ? ORDER BY julianday(source_timestamp),id LIMIT 1001")
            .bind(iso(start)).bind(iso(end)).bind(expected).fetch_all(&mut *tx).await?;
        if marks.len() > 1000 {
            return Err(invalid("memory_day_mark_budget_exceeded"));
        }
        let overrides=sqlx::query("SELECT id,operation,payload_json FROM memory_overrides WHERE day_key=? ORDER BY created_at,id LIMIT 1001").bind(&day).fetch_all(&mut *tx).await?;
        if overrides.len() > 1000 {
            return Err(invalid("memory_correction_budget_exceeded"));
        }
        tx.commit().await?;
        drop(conn);
        let mut grouped: Map<i64, Vec<(SourceRef, String)>> = Map::new();
        for row in refs {
            if row.try_get::<Option<i64>, _>("available")?.is_none() {
                continue;
            }
            let title: Option<String> = row.try_get("window_title")?;
            let url: Option<String> = row.try_get("browser_url")?;
            let path: Option<String> = row.try_get("document_path")?;
            let app: Option<String> = row.try_get("app_name")?;
            let excerpt: String = row.try_get("excerpt")?;
            let content_hash: Option<i64> = row.try_get("frame_content_hash")?;
            let payload_generation: Option<i64> = row.try_get("frame_payload_generation")?;
            let at: DateTime<Utc> = row.try_get("occurred_at")?;
            let kind: String = row.try_get("source_type")?;
            let id: i64 = row.try_get("source_id")?;
            let digest = hash(
                json!([
                    kind,
                    id,
                    iso(at),
                    title,
                    url,
                    path,
                    app,
                    excerpt,
                    content_hash,
                    payload_generation
                ])
                .to_string(),
            );
            let key = topic(
                title.as_deref().unwrap_or(""),
                url.as_deref(),
                path.as_deref(),
            );
            grouped
                .entry(row.try_get("interval_id")?)
                .or_default()
                .push((
                    SourceRef {
                        kind,
                        id,
                        at: iso(at),
                        digest,
                        app,
                    },
                    key,
                ));
        }
        let mut nodes = Map::new();
        nodes.insert(
            day.clone(),
            empty_node(
                day.clone(),
                None,
                "day",
                date.to_string(),
                iso(start),
                iso(end),
            ),
        );
        let mut session: Option<String> = None;
        let mut last_end = start;
        let mut session_start = start;
        for interval in intervals {
            let Some(evidence) = grouped.remove(&interval.try_get::<i64, _>("id")?) else {
                continue;
            };
            if evidence.is_empty() {
                continue;
            }
            let at = interval.try_get::<DateTime<Utc>, _>("start_at")?.max(start);
            let stop = interval.try_get::<DateTime<Utc>, _>("end_at")?.min(end);
            if session.is_none()
                || at - last_end > chrono::Duration::minutes(20)
                || at - session_start > chrono::Duration::hours(4)
            {
                let anchor = &evidence[0].0;
                let id = format!("s:{}", hash(format!("{day}:{}:{}", anchor.kind, anchor.id)));
                nodes.entry(id.clone()).or_insert_with(|| {
                    empty_node(
                        id.clone(),
                        Some(day.clone()),
                        "session",
                        format!(
                            "Work session {}",
                            (at + chrono::Duration::minutes(i64::from(offset))).format("%H:%M")
                        ),
                        iso(at),
                        iso(stop),
                    )
                });
                session = Some(id);
                session_start = at;
            }
            let parent = session.as_ref().unwrap().clone();
            for (source, mut key) in evidence {
                if key == "unclassified activity" {
                    key = bounded(&interval.try_get::<String, _>("title")?, 180).to_lowercase();
                }
                let task = format!("t:{}", hash(format!("{parent}:{key}")));
                let n = nodes.entry(task.clone()).or_insert_with(|| {
                    empty_node(task, Some(parent.clone()), "task", key, iso(at), iso(stop))
                });
                n.node.start_time = n.node.start_time.clone().min(iso(at));
                n.node.end_time = n.node.end_time.clone().max(iso(stop));
                n.refs.push(source);
            }
            last_end = last_end.max(stop);
        }
        // A saved annotation is attached by immutable source time, even after
        // its raw frame has expired. Empty intervals get an honest saved-moment
        // node rather than inventing captured activity.
        for row in marks {
            let at: DateTime<Utc> = row.try_get("source_timestamp")?;
            let mark: String = row.try_get("id")?;
            let revision: i64 = row.try_get("revision")?;
            let source_frame: i64 = row.try_get("source_frame_id")?;
            let source_app: Option<String> = row.try_get("source_app")?;
            let task = nodes
                .values()
                .filter(|n| {
                    n.node.kind == "task"
                        && time(&n.node.start_time).is_ok_and(|start| start <= at)
                        && time(&n.node.end_time).is_ok_and(|end| at < end)
                })
                .max_by_key(|n| {
                    let exact = n
                        .refs
                        .iter()
                        .any(|r| r.kind == "frame" && r.id == source_frame);
                    let previous = n
                        .refs
                        .iter()
                        .filter(|r| r.kind != "mark" && time(&r.at).is_ok_and(|t| t <= at))
                        .map(|r| r.at.clone())
                        .max()
                        .unwrap_or_default();
                    let same_app = source_app
                        .as_ref()
                        .is_some_and(|app| n.refs.iter().any(|r| r.app.as_ref() == Some(app)));
                    (exact, previous, same_app)
                })
                .map(|n| n.node.id.clone());
            let task = task.unwrap_or_else(|| {
                let session = format!(
                    "s:{}",
                    hash(format!("{day}:saved:{}", at.timestamp() / 1200))
                );
                nodes.entry(session.clone()).or_insert_with(|| {
                    empty_node(
                        session.clone(),
                        Some(day.clone()),
                        "session",
                        "Saved moments".into(),
                        iso(at),
                        iso(at + chrono::Duration::seconds(1)),
                    )
                });
                let task = format!("t:{}", hash(format!("{session}:saved")));
                nodes.entry(task.clone()).or_insert_with(|| {
                    empty_node(
                        task.clone(),
                        Some(session),
                        "task",
                        "Saved moment".into(),
                        iso(at),
                        iso(at + chrono::Duration::seconds(1)),
                    )
                });
                task
            });
            let n = nodes.get_mut(&task).unwrap();
            n.marks.push((mark.clone(), revision));
            n.refs.push(SourceRef {
                kind: "mark".into(),
                id: 0,
                at: iso(at),
                digest: mark,
                app: source_app,
            });
        }
        recompute(&mut nodes, expected)?;
        for row in overrides {
            let payload: Override =
                serde_json::from_str(&row.try_get::<String, _>("payload_json")?)
                    .map_err(|_| invalid("invalid_saved_memory_override"))?;
            apply_override(
                &mut nodes,
                &row.try_get::<String, _>("id")?,
                &row.try_get::<String, _>("operation")?,
                &payload,
            )?;
            recompute(&mut nodes, expected)?;
        }
        persist(self, &day, offset, &nodes, expected, day_revision, None).await
    }

    pub async fn correct_memory_node(
        &self,
        id: &str,
        operation: &str,
        body: &MemoryNodeCorrection,
        cutoff: Option<&str>,
    ) -> Result<MemoryNode, ExplicitMemoryError> {
        if body
            .label
            .as_ref()
            .is_some_and(|label| label.trim().is_empty() || label.len() > 500)
            || body.node_ids.len() > 16
        {
            return Err(invalid("invalid_memory_correction"));
        }
        let expected = self.memory_index_epoch().await?;
        let mut conn = self.acquire_read().await?;
        let mut tx = conn.begin().await?;
        let row = sqlx::query(
            "SELECT * FROM memory_nodes WHERE id=? AND invalidated=0 AND privacy_epoch IS ?",
        )
        .bind(id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ExplicitMemoryError::NotFound)?;
        let node = row_node(&row)?;
        if node.revision != body.revision {
            return Err(ExplicitMemoryError::Conflict);
        }
        if cutoff.is_some_and(|cutoff| time(&node.start_time).ok() < time(cutoff).ok()) {
            return Err(ExplicitMemoryError::NotFound);
        }
        if operation == "label" && body.label.is_none() {
            return Err(invalid("memory_label_required"));
        }
        if operation == "merge" && (body.node_ids.is_empty() || node.kind == "day") {
            return Err(invalid("merge_requires_sibling_nodes"));
        }
        if operation == "split" {
            let at = time(
                body.at
                    .as_deref()
                    .ok_or_else(|| invalid("split_time_required"))?,
            )?;
            if node.kind != "task" || at <= time(&node.start_time)? || at >= time(&node.end_time)? {
                return Err(invalid("invalid_split_boundary"));
            }
        }
        let day: String = row.try_get("day_key")?;
        let offset: i32 = row.try_get("timezone_offset_minutes")?;
        let rows=sqlx::query("SELECT * FROM memory_nodes WHERE day_key=? AND invalidated=0 AND privacy_epoch IS ? LIMIT 1001").bind(&day).bind(expected).fetch_all(&mut *tx).await?;
        if rows.len() > MAX_DAY_NODES {
            return Err(invalid("memory_day_node_budget_exceeded"));
        }
        let mut nodes = Map::new();
        for row in rows {
            let node = row_node(&row)?;
            nodes.insert(
                node.id.clone(),
                Built {
                    node,
                    refs: Vec::new(),
                    marks: Vec::new(),
                },
            );
        }
        let day_revision = nodes.get(&day).map(|n| n.node.revision);
        let refs=sqlx::query("SELECT e.*,COALESCE(f.app_name,u.app_name) AS app_name FROM memory_evidence e JOIN memory_nodes n ON n.id=e.node_id LEFT JOIN frames f ON e.source_type='frame' AND f.id=e.source_id LEFT JOIN ui_events u ON e.source_type='ui_event' AND u.id=e.source_id WHERE n.day_key=? LIMIT 12001").bind(&day).fetch_all(&mut *tx).await?;
        if refs.len() > MAX_DAY_EVIDENCE {
            return Err(invalid("memory_day_evidence_budget_exceeded"));
        }
        for row in refs {
            if let Some(n) = nodes.get_mut(&row.try_get::<String, _>("node_id")?) {
                n.refs.push(SourceRef {
                    kind: row.try_get("source_type")?,
                    id: row.try_get("source_id")?,
                    at: row.try_get("occurred_at")?,
                    digest: row.try_get("content_fingerprint")?,
                    app: row.try_get("app_name")?,
                });
            }
        }
        let marks=sqlx::query("SELECT nm.*,m.source_timestamp,m.source_app FROM memory_node_marks nm JOIN memory_nodes n ON n.id=nm.node_id JOIN explicit_memory_marks m ON m.id=nm.mark_id WHERE n.day_key=? LIMIT 1001").bind(&day).fetch_all(&mut *tx).await?;
        if marks.len() > 1000 {
            return Err(invalid("memory_day_mark_budget_exceeded"));
        }
        for row in marks {
            if let Some(n) = nodes.get_mut(&row.try_get::<String, _>("node_id")?) {
                let id: String = row.try_get("mark_id")?;
                n.marks.push((id.clone(), row.try_get("mark_revision")?));
                n.refs.push(SourceRef {
                    kind: "mark".into(),
                    id: 0,
                    at: row.try_get("source_timestamp")?,
                    digest: id,
                    app: row.try_get("source_app")?,
                });
            }
        }
        tx.commit().await?;
        drop(conn);
        recompute(&mut nodes, expected)?;
        let mut target_ids = vec![id.to_string()];
        target_ids.extend(body.node_ids.iter().cloned());
        target_ids.sort();
        target_ids.dedup();
        if let Some(index) = target_ids.iter().position(|n| n == id) {
            target_ids.swap(0, index);
        }
        let mut targets = Vec::new();
        for target in target_ids {
            let n = nodes.get(&target).ok_or(ExplicitMemoryError::NotFound)?;
            if cutoff.is_some_and(|cutoff| time(&n.node.start_time).ok() < time(cutoff).ok()) {
                return Err(ExplicitMemoryError::NotFound);
            }
            if operation == "merge"
                && (n.node.kind != node.kind || n.node.parent_id != node.parent_id)
            {
                return Err(invalid("merge_requires_sibling_nodes"));
            }
            targets.push(Anchor {
                id: target,
                kind: n.node.kind.clone(),
                source: n
                    .refs
                    .iter()
                    .find(|r| r.kind != "mark")
                    .map(|r| (r.kind.clone(), r.id)),
                mark: n.marks.first().map(|m| m.0.clone()),
                start_time: n.node.start_time.clone(),
                end_time: n.node.end_time.clone(),
                canonical_label: hash(&n.node.label),
            });
        }
        let action_id = uuid::Uuid::new_v4().to_string();
        let payload = Override {
            targets,
            label: body.label.as_ref().map(|l| l.trim().into()),
            at: body.at.clone(),
        };
        apply_override(&mut nodes, &action_id, operation, &payload)?;
        recompute(&mut nodes, expected)?;
        // The containing day revision is compared inside the writer as well:
        // any concurrent sibling edit or reconciliation conflicts atomically.
        let result_id = match operation {
            "merge" => format!("u:{}", hash(&action_id)),
            "split" => format!("u:{}", hash(format!("{action_id}:left"))),
            _ => id.to_string(),
        };
        let result = nodes
            .get(&result_id)
            .ok_or(ExplicitMemoryError::NotFound)?
            .node
            .clone();
        persist(
            self,
            &day,
            offset,
            &nodes,
            expected,
            day_revision,
            Some((&action_id, operation, id, &payload)),
        )
        .await?;
        let fresh = self
            .get_memory_node(&result.id, cutoff, 1)
            .await?
            .ok_or(ExplicitMemoryError::NotFound)?;
        Ok(fresh.node)
    }
}
