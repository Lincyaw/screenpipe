// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! Incremental projection of an accepted ledger slice. Source bodies remain in
//! their original storage. A durable tuple cursor and small atomic writes keep
//! processing cost independent of the total number of sources in a day.

use super::*;
use serde_json::json;

const PAGE: i64 = 128;

struct Input {
    kind: String,
    id: i64,
    at: DateTime<Utc>,
    stop: DateTime<Utc>,
    app: String,
    title: String,
    url: Option<String>,
    path: Option<String>,
    digest: String,
}

struct Delta {
    node: MemoryNode,
    day: String,
    canonical: String,
    expected_revision: Option<i64>,
    sources: Map<(String, i64), (String, String)>,
    marks: Map<String, i64>,
    dirty: bool,
}

fn node(
    id: String,
    parent: Option<String>,
    kind: &str,
    label: String,
    view: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> MemoryNode {
    MemoryNode {
        id,
        parent_id: parent,
        kind: kind.into(),
        label,
        label_origin: "deterministic".into(),
        start_time: iso(start),
        end_time: iso(end),
        provisional: end > Utc::now() - chrono::Duration::minutes(5),
        evidence_count: 0,
        mark_count: 0,
        apps: Vec::new(),
        revision: 1,
        fingerprint: String::new(),
        view: view.into(),
        app_name: None,
        location_type: None,
        location_key: None,
        location_label: None,
        children: Vec::new(),
    }
}

fn day(
    at: DateTime<Utc>,
    offset: i32,
    view: &str,
) -> (String, DateTime<Utc>, DateTime<Utc>, String) {
    let date = (at + chrono::Duration::minutes(i64::from(offset))).date_naive();
    let start =
        date.and_hms_opt(0, 0, 0).unwrap().and_utc() - chrono::Duration::minutes(i64::from(offset));
    (
        format!(
            "{}:{offset}:{date}",
            if view == "app" { "app-day" } else { "day" }
        ),
        start,
        start + chrono::Duration::days(1),
        date.to_string(),
    )
}

fn location(input: &Input) -> (String, String, String) {
    if let Some(url) = input.url.as_deref().filter(|url| !url.trim().is_empty()) {
        let key = url.split('#').next().unwrap_or(url).trim().to_string();
        let host = key
            .split("://")
            .nth(1)
            .unwrap_or(&key)
            .split('/')
            .next()
            .unwrap_or(&key);
        return (
            "url".into(),
            key.clone(),
            bounded(
                if input.title.is_empty() {
                    host
                } else {
                    &input.title
                },
                180,
            ),
        );
    }
    if let Some(path) = input.path.as_deref().filter(|path| !path.trim().is_empty()) {
        let label = path
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(path);
        return ("file".into(), path.to_string(), bounded(label, 180));
    }
    if !input.title.is_empty() {
        return (
            "window".into(),
            input.title.clone(),
            bounded(&input.title, 180),
        );
    }
    (
        "unknown".into(),
        "unknown".into(),
        "Unclassified activity".into(),
    )
}

async fn load_delta(
    conn: &mut sqlx::SqliteConnection,
    nodes: &mut Map<String, Delta>,
    fresh: MemoryNode,
    day: &str,
    canonical: String,
    epoch: Option<i64>,
) -> Result<(), ExplicitMemoryError> {
    if nodes.contains_key(&fresh.id) {
        return Ok(());
    }
    let existing = sqlx::query("SELECT * FROM memory_nodes WHERE id=?")
        .bind(&fresh.id)
        .fetch_optional(&mut *conn)
        .await?;
    let mut current = fresh;
    let mut expected_revision = None;
    let mut dirty = true;
    if let Some(row) = existing {
        let old = row_node(&row)?;
        expected_revision = Some(old.revision);
        let released = row.try_get::<Option<i64>, _>("privacy_epoch")? == epoch;
        if released && old.label_origin == "user" && !old.label.is_empty() {
            current.label = old.label.clone();
            current.label_origin = old.label_origin.clone();
        }
        if released {
            current.start_time = old.start_time.clone().min(current.start_time);
            current.end_time = old.end_time.clone().max(current.end_time);
        }
        current.provisional = time(&current.end_time)? > Utc::now() - chrono::Duration::minutes(5);
        current.revision = old.revision;
        current.fingerprint = old.fingerprint;
        if released {
            current.evidence_count = old.evidence_count;
            current.mark_count = old.mark_count;
            current.apps = old.apps;
            if row.try_get::<bool, _>("invalidated")? {
                let sql=format!("{} SELECT count(*) FROM (SELECT DISTINCT e.source_type,e.source_id FROM memory_evidence e JOIN descendants d ON d.id=e.node_id WHERE e.privacy_epoch IS ?2 AND julianday(e.occurred_at)>=julianday(d.lower_bound) AND julianday(e.occurred_at)<julianday(d.upper_bound))",query::DESCENDANTS);
                current.evidence_count = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                    .bind(&current.id)
                    .bind(epoch)
                    .fetch_one(&mut *conn)
                    .await?;
                let sql=format!("{} SELECT count(DISTINCT m.id) FROM memory_node_marks nm JOIN descendants d ON d.id=nm.node_id JOIN explicit_memory_marks m ON m.id=nm.mark_id WHERE m.privacy_epoch IS ?2 AND julianday(m.source_timestamp)>=julianday(d.lower_bound) AND julianday(m.source_timestamp)<julianday(d.upper_bound)",query::DESCENDANTS);
                current.mark_count = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                    .bind(&current.id)
                    .bind(epoch)
                    .fetch_one(&mut *conn)
                    .await?;
            }
        }
        dirty = !released
            || row.try_get::<bool, _>("invalidated")?
            || current.start_time != old.start_time
            || current.end_time != old.end_time
            || current.provisional != old.provisional;
    }
    nodes.insert(
        current.id.clone(),
        Delta {
            node: current,
            day: day.into(),
            canonical,
            expected_revision,
            sources: Map::new(),
            marks: Map::new(),
            dirty,
        },
    );
    Ok(())
}

async fn route(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    at: DateTime<Utc>,
) -> Result<String, ExplicitMemoryError> {
    let mut current = id.to_string();
    for _ in 0..32 {
        let next: Option<String> = sqlx::query_scalar("SELECT target_node_id FROM memory_node_routes WHERE source_node_id=? AND julianday(start_time)<=julianday(?) AND julianday(end_time)>julianday(?) ORDER BY start_time,target_node_id LIMIT 1")
            .bind(&current).bind(iso(at)).bind(iso(at)).fetch_optional(&mut *conn).await?;
        let Some(next) = next else {
            return Ok(current);
        };
        if next == current {
            break;
        }
        current = next;
    }
    Err(invalid("memory_correction_route_depth"))
}

async fn legacy_corrected_target(
    conn: &mut sqlx::SqliteConnection,
    day: &str,
    parent: &str,
    key: &str,
    at: DateTime<Utc>,
) -> Result<Option<String>, ExplicitMemoryError> {
    // Original authored overrides remain the durable record. Bootstrap their
    // canonical identity without changing a renamed/merged node or its ID.
    let rows = sqlx::query("SELECT id,operation,payload_json FROM memory_overrides WHERE day_key=? ORDER BY created_at DESC,id DESC LIMIT 1000")
        .bind(day).fetch_all(&mut *conn).await?;
    for row in rows {
        let Ok(payload) =
            serde_json::from_str::<serde_json::Value>(&row.try_get::<String, _>("payload_json")?)
        else {
            continue;
        };
        let matches = payload["targets"].as_array().is_some_and(|targets| {
            targets.iter().any(|target| {
                target["kind"] == "task" && target["canonical_label"].as_str() == Some(key)
            })
        });
        if !matches {
            continue;
        }
        let action: String = row.try_get("id")?;
        let op: String = row.try_get("operation")?;
        let target = match op.as_str() {
            "merge" => format!("u:{}", hash(&action)),
            "split" => {
                let Some(pivot) = payload["at"].as_str().and_then(|value| time(value).ok()) else {
                    continue;
                };
                format!(
                    "u:{}",
                    hash(format!(
                        "{action}:{}",
                        if at < pivot { "left" } else { "right" }
                    ))
                )
            }
            "label" => payload["targets"][0]["id"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            _ => continue,
        };
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM memory_nodes WHERE id=? AND parent_id=? AND superseded=0)",
        )
        .bind(&target)
        .bind(parent)
        .fetch_one(&mut *conn)
        .await?;
        if exists {
            return Ok(Some(target));
        }
    }
    Ok(None)
}

async fn task_identity(
    conn: &mut sqlx::SqliteConnection,
    input: &Input,
    day: &str,
    offset: i32,
    current: Option<i64>,
) -> Result<(MemoryNode, MemoryNode, String), ExplicitMemoryError> {
    let key = projection::topic(&input.title, input.url.as_deref(), input.path.as_deref());
    let canonical = hash(&key);
    let existing = sqlx::query("SELECT n.* FROM memory_evidence e JOIN memory_nodes n ON n.id=e.node_id WHERE e.source_type=? AND e.source_id=? AND n.day_key=? AND n.view='task' AND n.kind='task' ORDER BY n.superseded,n.id LIMIT 1")
        .bind(&input.kind).bind(input.id).bind(day).fetch_optional(&mut *conn).await?;
    if let Some(old) = existing {
        let id = route(conn, &old.try_get::<String, _>("id")?, input.at).await?;
        if let Some(target) = sqlx::query("SELECT * FROM memory_nodes WHERE id=? AND superseded=0")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
        {
            let mut task = row_node(&target)?;
            let released = target.try_get::<Option<i64>, _>("privacy_epoch")? == current
                && !target.try_get::<bool, _>("invalidated")?;
            if !released {
                task.label = key.clone();
                task.label_origin = "deterministic".into();
                task.start_time = iso(input.at);
                task.end_time = iso(input.stop);
                task.apps.clear();
            }
            if let Some(parent) = task.parent_id.as_deref() {
                if let Some(parent) = sqlx::query("SELECT * FROM memory_nodes WHERE id=?")
                    .bind(parent)
                    .fetch_optional(&mut *conn)
                    .await?
                {
                    let mut session = row_node(&parent)?;
                    if parent.try_get::<Option<i64>, _>("privacy_epoch")? != current
                        || parent.try_get::<bool, _>("invalidated")?
                    {
                        session.label = format!(
                            "Work session {}",
                            (input.at + chrono::Duration::minutes(i64::from(offset)))
                                .format("%H:%M")
                        );
                        session.label_origin = "deterministic".into();
                        session.start_time = iso(input.at);
                        session.end_time = iso(input.stop);
                        session.apps.clear();
                    }
                    return Ok((session, task, canonical));
                }
            }
        }
    }
    let session = sqlx::query("SELECT * FROM memory_nodes WHERE day_key=? AND view='task' AND kind='session' AND superseded=0 AND julianday(start_time)<=julianday(?)+20.0/1440.0 AND julianday(end_time)>=julianday(?)-20.0/1440.0 AND julianday(?)-julianday(start_time)<=4.0/24.0 ORDER BY CASE WHEN julianday(start_time)<=julianday(?) AND julianday(end_time)>julianday(?) THEN 0 ELSE 1 END,abs(julianday(end_time)-julianday(?)),id LIMIT 1")
        .bind(day).bind(iso(input.at)).bind(iso(input.at)).bind(iso(input.at)).bind(iso(input.at)).bind(iso(input.at)).bind(iso(input.at)).fetch_optional(&mut *conn).await?;
    let session = if let Some(row) = session {
        let mut node = row_node(&row)?;
        if row.try_get::<Option<i64>, _>("privacy_epoch")? != current
            || row.try_get::<bool, _>("invalidated")?
        {
            node.label = format!(
                "Work session {}",
                (input.at + chrono::Duration::minutes(i64::from(offset))).format("%H:%M")
            );
            node.label_origin = "deterministic".into();
            node.start_time = iso(input.at);
            node.end_time = iso(input.stop);
            node.apps.clear();
        }
        node
    } else {
        let id = format!("s:{}", hash(format!("{day}:{}:{}", input.kind, input.id)));
        node(
            id,
            Some(day.into()),
            "session",
            format!(
                "Work session {}",
                (input.at + chrono::Duration::minutes(i64::from(offset))).format("%H:%M")
            ),
            "task",
            input.at,
            input.stop,
        )
    };
    let found = sqlx::query("SELECT * FROM memory_nodes WHERE parent_id=? AND view='task' AND kind='task' AND (canonical_key=? OR (canonical_key='' AND label=?)) ORDER BY superseded,id LIMIT 1")
        .bind(&session.id).bind(&canonical).bind(&key).fetch_optional(&mut *conn).await?;
    let found = if let Some(row) = found {
        let routed = route(conn, &row.try_get::<String, _>("id")?, input.at).await?;
        sqlx::query("SELECT * FROM memory_nodes WHERE id=? AND superseded=0")
            .bind(routed)
            .fetch_optional(&mut *conn)
            .await?
    } else if let Some(target) =
        legacy_corrected_target(conn, day, &session.id, &canonical, input.at).await?
    {
        sqlx::query("SELECT * FROM memory_nodes WHERE id=?")
            .bind(target)
            .fetch_optional(&mut *conn)
            .await?
    } else {
        None
    };
    let task = if let Some(row) = found {
        let mut node = row_node(&row)?;
        if row.try_get::<Option<i64>, _>("privacy_epoch")? != current
            || row.try_get::<bool, _>("invalidated")?
        {
            node.label = key.clone();
            node.label_origin = "deterministic".into();
            node.start_time = iso(input.at);
            node.end_time = iso(input.stop);
            node.apps.clear();
        }
        node
    } else {
        node(
            format!("t:{}", hash(format!("{}:{key}", session.id))),
            Some(session.id.clone()),
            "task",
            key,
            "task",
            input.at,
            input.stop,
        )
    };
    Ok((session, task, canonical))
}

async fn append_source(
    conn: &mut sqlx::SqliteConnection,
    nodes: &mut Map<String, Delta>,
    input: &Input,
    offset: i32,
    current: Option<i64>,
) -> Result<Vec<String>, ExplicitMemoryError> {
    let mut leaves = Vec::new();
    for view in ["task", "app"] {
        let (day, start, end, label) = day(input.at, offset, view);
        let root = node(day.clone(), None, "day", label, view, start, end);
        let (middle, mut leaf, canonical) = if view == "task" {
            task_identity(conn, input, &day, offset, current).await?
        } else {
            let existing = sqlx::query("SELECT n.* FROM memory_evidence e JOIN memory_nodes n ON n.id=e.node_id WHERE e.source_type=? AND e.source_id=? AND n.day_key=? AND n.view='app' AND n.kind='task' AND n.superseded=0 AND n.invalidated=0 AND n.privacy_epoch IS ? LIMIT 1")
                .bind(&input.kind).bind(input.id).bind(&day).bind(current).fetch_optional(&mut *conn).await?;
            if input.digest.is_empty() {
                if let Some(row) = existing {
                    let leaf = row_node(&row)?;
                    if let Some(parent) = leaf.parent_id.as_deref() {
                        if let Some(row) = sqlx::query("SELECT * FROM memory_nodes WHERE id=?")
                            .bind(parent)
                            .fetch_optional(&mut *conn)
                            .await?
                        {
                            let middle = row_node(&row)?;
                            let ids = [root.id.clone(), middle.id.clone(), leaf.id.clone()];
                            load_delta(conn, nodes, root, &day, String::new(), current).await?;
                            load_delta(conn, nodes, middle, &day, String::new(), current).await?;
                            load_delta(conn, nodes, leaf, &day, String::new(), current).await?;
                            leaves.push(ids[2].clone());
                            continue;
                        }
                    }
                }
            }
            let app_id = format!("a:{}", hash(format!("{day}:{}", input.app.to_lowercase())));
            let mut middle = node(
                app_id.clone(),
                Some(day.clone()),
                "app",
                input.app.clone(),
                view,
                input.at,
                input.stop,
            );
            middle.app_name = Some(input.app.clone());
            let (kind, key, label) = location(input);
            let mut leaf = node(
                format!("l:{}", hash(format!("{app_id}:{kind}:{key}"))),
                Some(app_id),
                "location",
                label.clone(),
                view,
                input.at,
                input.stop,
            );
            leaf.app_name = Some(input.app.clone());
            leaf.location_type = Some(kind);
            leaf.location_key = Some(key.clone());
            leaf.location_label = Some(label);
            (middle, leaf, hash(key))
        };
        // Preserve canonical IDs selected earlier in this same page, before
        // their rows are committed. Sessions cannot split at page boundaries.
        if view == "task" && !nodes.contains_key(leaf.parent_id.as_deref().unwrap_or("")) {
            if let Some(prior) = nodes
                .values()
                .filter(|delta| {
                    delta.node.view == "task" && delta.node.kind == "session" && delta.day == day
                })
                .filter(|delta| {
                    time(&delta.node.end_time)
                        .is_ok_and(|at| input.at - at <= chrono::Duration::minutes(20))
                        && time(&delta.node.start_time)
                            .is_ok_and(|at| input.at - at <= chrono::Duration::hours(4))
                })
                .max_by_key(|delta| delta.node.end_time.clone())
            {
                if middle.revision == 1 && middle.fingerprint.is_empty() {
                    leaf.parent_id = Some(prior.node.id.clone());
                    leaf.id = format!("t:{}", hash(format!("{}:{}", prior.node.id, leaf.label)));
                }
            }
        }
        let middle = if view == "task" {
            nodes
                .get(leaf.parent_id.as_deref().unwrap_or(""))
                .map(|d| d.node.clone())
                .unwrap_or(middle)
        } else {
            middle
        };
        let ids = [root.id.clone(), middle.id.clone(), leaf.id.clone()];
        load_delta(conn, nodes, root, &day, String::new(), current).await?;
        load_delta(conn, nodes, middle, &day, String::new(), current).await?;
        load_delta(conn, nodes, leaf, &day, canonical, current).await?;
        for id in &ids {
            let delta = nodes.get_mut(id).unwrap();
            if delta.node.kind != "day" {
                let before = (delta.node.start_time.clone(), delta.node.end_time.clone());
                delta.node.start_time = delta.node.start_time.clone().min(iso(input.at));
                delta.node.end_time = delta.node.end_time.clone().max(iso(input.stop.min(end)));
                delta.dirty |=
                    before != (delta.node.start_time.clone(), delta.node.end_time.clone());
            }
            delta.sources.insert(
                (input.kind.clone(), input.id),
                (iso(input.at), input.digest.clone()),
            );
            if !delta.node.apps.contains(&input.app) {
                delta.node.apps.push(input.app.clone());
                delta.node.apps.sort();
                delta.dirty = true;
            }
        }
        leaves.push(ids[2].clone());
    }
    Ok(leaves)
}

impl DatabaseManager {
    /// Corrections move a bounded set of node identities, never every source
    /// in a dense day. Source links are exposed through timestamped routes.
    pub async fn correct_memory_node(
        &self,
        id: &str,
        operation: &str,
        body: &MemoryNodeCorrection,
        cutoff: Option<&str>,
    ) -> Result<MemoryNode, ExplicitMemoryError> {
        self.memory_reader_ready().await?;
        if body.node_ids.len() > 16
            || body
                .label
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.len() > 500)
        {
            return Err(invalid("invalid_memory_correction"));
        }
        let mut conn = self.acquire_read().await?;
        let mut read = conn.begin().await?;
        let current = epoch(&mut read).await?;
        let mut ids = vec![id.to_string()];
        for other in &body.node_ids {
            if !ids.contains(other) {
                ids.push(other.clone());
            }
        }
        let mut targets = Vec::new();
        let mut revisions = Vec::new();
        let mut day_id = String::new();
        let mut offset = 0_i32;
        let mut canonical = String::new();
        for target in &ids {
            let row=sqlx::query("SELECT * FROM memory_nodes WHERE id=? AND superseded=0 AND invalidated=0 AND privacy_epoch IS ?").bind(target).bind(current).fetch_optional(&mut *read).await?.ok_or(ExplicitMemoryError::NotFound)?;
            let node = row_node(&row)?;
            if cutoff.is_some_and(|c| time(&node.start_time).ok() < time(c).ok()) {
                return Err(ExplicitMemoryError::NotFound);
            }
            if target == id {
                if node.revision != body.revision {
                    return Err(ExplicitMemoryError::Conflict);
                }
                day_id = row.try_get("day_key")?;
                offset = row.try_get("timezone_offset_minutes")?;
                canonical = row.try_get("canonical_key")?;
                if canonical.is_empty() {
                    canonical = hash(&node.label);
                }
            } else if targets.first().is_some_and(|first: &MemoryNode| {
                first.parent_id != node.parent_id
                    || first.kind != node.kind
                    || first.view != node.view
            }) {
                return Err(invalid("merge_requires_sibling_nodes"));
            }
            revisions.push((target.clone(), Some(node.revision)));
            targets.push(node);
        }
        let original = &targets[0];
        if original.view == "app" && operation != "label" {
            return Err(invalid("directory_corrections_require_label"));
        }
        if operation == "label" && body.label.is_none() {
            return Err(invalid("memory_label_required"));
        }
        if operation == "merge" && (targets.len() < 2 || original.kind == "day") {
            return Err(invalid("merge_requires_sibling_nodes"));
        }
        let pivot = if operation == "split" {
            let at = time(
                body.at
                    .as_deref()
                    .ok_or_else(|| invalid("split_time_required"))?,
            )?;
            if original.kind != "task"
                || at <= time(&original.start_time)?
                || at >= time(&original.end_time)?
            {
                return Err(invalid("invalid_split_boundary"));
            }
            Some(at)
        } else {
            None
        };
        if !matches!(operation, "label" | "merge" | "split") {
            return Err(invalid("invalid_memory_correction"));
        }
        let action = uuid::Uuid::new_v4().to_string();
        let anchors=targets.iter().map(|node|json!({"id":node.id,"kind":node.kind,"source":null,"mark":null,"start_time":node.start_time,"end_time":node.end_time,"canonical_label":if node.id==id{canonical.clone()}else{hash(&node.label)}})).collect::<Vec<_>>();
        let payload =
            json!({"targets":anchors,"label":body.label.as_ref().map(|s|s.trim()),"at":body.at});
        let mut output = Vec::new();
        if operation == "label" {
            let mut edited = original.clone();
            edited.label = body.label.as_ref().unwrap().trim().into();
            edited.label_origin = "user".into();
            edited.revision += 1;
            edited.fingerprint = hash(format!("{}:{action}", edited.fingerprint));
            output.push(edited);
        } else {
            for side in if pivot.is_some() {
                vec!["left", "right"]
            } else {
                vec![""]
            } {
                let mut edited = original.clone();
                edited.id = format!(
                    "u:{}",
                    hash(if side.is_empty() {
                        action.clone()
                    } else {
                        format!("{action}:{side}")
                    })
                );
                edited.label_origin = "user".into();
                edited.revision = 1;
                edited.start_time = targets.iter().map(|n| n.start_time.clone()).min().unwrap();
                edited.end_time = targets.iter().map(|n| n.end_time.clone()).max().unwrap();
                if let Some(at) = pivot {
                    if side == "left" {
                        edited.end_time = iso(at);
                    } else {
                        edited.start_time = iso(at);
                    }
                }
                if (side == "right" || side.is_empty()) && body.label.is_some() {
                    edited.label = body.label.as_ref().unwrap().trim().into();
                }
                let roots = serde_json::to_string(&ids)
                    .map_err(|_| invalid("memory_projection_encoding"))?;
                let desc = query::DESCENDANTS.replace(
                    "WHERE id=?1",
                    "WHERE id IN (SELECT value FROM json_each(?1))",
                );
                let lower = if side == "right" {
                    iso(pivot.unwrap())
                } else {
                    "0001-01-01T00:00:00Z".into()
                };
                let upper = if side == "left" {
                    iso(pivot.unwrap())
                } else {
                    "9999-12-31T23:59:59Z".into()
                };
                let sql=format!("{desc} SELECT count(*) FROM (SELECT DISTINCT e.source_type,e.source_id FROM memory_evidence e JOIN descendants d ON d.id=e.node_id WHERE e.privacy_epoch IS ?2 AND julianday(e.occurred_at)>=julianday(?3) AND julianday(e.occurred_at)<julianday(?4) AND julianday(e.occurred_at)>=julianday(d.lower_bound) AND julianday(e.occurred_at)<julianday(d.upper_bound))");
                edited.evidence_count = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                    .bind(&roots)
                    .bind(current)
                    .bind(&lower)
                    .bind(&upper)
                    .fetch_one(&mut *read)
                    .await?;
                let sql=format!("{desc} SELECT count(DISTINCT m.id) FROM memory_node_marks nm JOIN descendants d ON d.id=nm.node_id JOIN explicit_memory_marks m ON m.id=nm.mark_id WHERE m.privacy_epoch IS ?2 AND julianday(m.source_timestamp)>=julianday(?3) AND julianday(m.source_timestamp)<julianday(?4) AND julianday(m.source_timestamp)>=julianday(d.lower_bound) AND julianday(m.source_timestamp)<julianday(d.upper_bound)");
                edited.mark_count = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                    .bind(&roots)
                    .bind(current)
                    .bind(lower)
                    .bind(upper)
                    .fetch_one(&mut *read)
                    .await?;
                edited.apps = targets
                    .iter()
                    .flat_map(|n| n.apps.clone())
                    .collect::<Set<_>>()
                    .into_iter()
                    .collect();
                edited.fingerprint = hash(
                    json!([
                        action,
                        side,
                        targets.iter().map(|n| &n.fingerprint).collect::<Vec<_>>()
                    ])
                    .to_string(),
                );
                output.push(edited);
            }
        }
        read.commit().await?;
        drop(conn);
        let mut tx = self.begin_immediate_with_retry().await?;
        let began = std::time::Instant::now();
        if epoch(&mut **tx.conn()).await? != current {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let revisions =
            serde_json::to_string(&revisions).map_err(|_| invalid("memory_projection_encoding"))?;
        let changed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM json_each(?) j LEFT JOIN memory_nodes n ON n.id=json_extract(j.value,'$[0]') WHERE n.revision IS NOT json_extract(j.value,'$[1]') OR n.superseded!=0)").bind(revisions).fetch_one(&mut **tx.conn()).await?;
        if changed {
            return Err(ExplicitMemoryError::Conflict);
        }
        sqlx::query("INSERT INTO memory_overrides(id,day_key,operation,target_id,payload_json,created_at) VALUES(?,?,?,?,?,?)").bind(&action).bind(&day_id).bind(operation).bind(id).bind(payload.to_string()).bind(iso(Utc::now())).execute(&mut **tx.conn()).await?;
        for edited in &output {
            if operation == "label" {
                sqlx::query("UPDATE memory_nodes SET label=?,label_origin='user',revision=?,fingerprint=?,canonical_key=?,updated_at=? WHERE id=?").bind(&edited.label).bind(edited.revision).bind(&edited.fingerprint).bind(&canonical).bind(iso(Utc::now())).bind(id).execute(&mut **tx.conn()).await?;
            } else {
                sqlx::query("INSERT INTO memory_nodes(id,parent_id,day_key,timezone_offset_minutes,kind,label,label_origin,start_time,end_time,provisional,evidence_count,mark_count,apps_json,revision,fingerprint,privacy_epoch,invalidated,generation,updated_at,view,canonical_key) VALUES(?,?,?,?,?,?,'user',?,?,?,?,?,?,?,?,?,0,(SELECT generation FROM memory_index_state WHERE id=1),?,?,?)")
                    .bind(&edited.id).bind(&edited.parent_id).bind(&day_id).bind(offset).bind(&edited.kind).bind(&edited.label).bind(&edited.start_time).bind(&edited.end_time).bind(edited.provisional).bind(edited.evidence_count).bind(edited.mark_count).bind(serde_json::to_string(&edited.apps).unwrap()).bind(edited.revision).bind(&edited.fingerprint).bind(current).bind(iso(Utc::now())).bind(&edited.view).bind(&canonical).execute(&mut **tx.conn()).await?;
                for old in &ids {
                    let lower = if pivot.is_some() && edited.start_time == iso(pivot.unwrap()) {
                        edited.start_time.clone()
                    } else {
                        "0001-01-01T00:00:00Z".into()
                    };
                    let upper = if pivot.is_some() && edited.end_time == iso(pivot.unwrap()) {
                        edited.end_time.clone()
                    } else {
                        "9999-12-31T23:59:59Z".into()
                    };
                    sqlx::query("INSERT INTO memory_node_routes(source_node_id,target_node_id,start_time,end_time) VALUES(?,?,?,?)").bind(old).bind(&edited.id).bind(lower).bind(upper).execute(&mut **tx.conn()).await?;
                }
            }
        }
        if operation != "label" {
            let encoded =
                serde_json::to_string(&ids).map_err(|_| invalid("memory_projection_encoding"))?;
            if original.kind == "session" {
                sqlx::query("UPDATE memory_nodes SET parent_id=?,revision=revision+1,fingerprint=fingerprint||? WHERE parent_id IN (SELECT value FROM json_each(?)) AND superseded=0").bind(&output[0].id).bind(format!(":{action}")).bind(&encoded).execute(&mut **tx.conn()).await?;
            }
            sqlx::query("UPDATE memory_nodes SET superseded=1,revision=revision+1,fingerprint=fingerprint||? WHERE id IN (SELECT value FROM json_each(?))").bind(format!(":{action}")).bind(encoded).execute(&mut **tx.conn()).await?;
        }
        sqlx::query("UPDATE memory_nodes SET revision=revision+1,fingerprint=fingerprint||? WHERE id=? OR id=?")
            .bind(format!(":{action}")).bind(&day_id).bind(&original.parent_id).execute(&mut **tx.conn()).await?;
        if began.elapsed() > std::time::Duration::from_millis(250) {
            return Err(invalid("memory_index_writer_budget"));
        }
        tx.commit().await?;
        self.get_memory_node(&output[0].id, cutoff, 1)
            .await?
            .map(|d| d.node)
            .ok_or(ExplicitMemoryError::NotFound)
    }

    pub async fn begin_memory_projection(
        &self,
        parent: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        offset: i32,
        marks_only: bool,
    ) -> Result<(), ExplicitMemoryError> {
        validate_offset(offset)?;
        let mut tx = self.begin_immediate_with_retry().await?;
        let current = epoch(&mut **tx.conn()).await?;
        let id = hash(format!("{parent}:{}", iso(start)));
        sqlx::query("INSERT INTO memory_projection_jobs(id,parent_job_id,start_time,end_time,timezone_offset_minutes,privacy_epoch,phase,cursor_time,updated_at) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING")
            .bind(id).bind(parent).bind(iso(start)).bind(iso(end)).bind(offset).bind(current).bind(if marks_only {"marks"}else{"sources"}).bind(iso(start)).bind(iso(Utc::now())).execute(&mut **tx.conn()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// None means the ledger slice has not yet been accepted. A pending page
    /// returns its fixed end with false; callers retain their source watermark.
    pub async fn memory_projection_step(
        &self,
        parent: &str,
        start: DateTime<Utc>,
    ) -> Result<Option<(DateTime<Utc>, bool)>, ExplicitMemoryError> {
        let result = self.memory_projection_page(parent, start).await;
        if matches!(&result, Err(ExplicitMemoryError::PrivacyChanged)) {
            let mut tx = self.begin_immediate_with_retry().await?;
            let current = epoch(&mut **tx.conn()).await?;
            sqlx::query("UPDATE memory_projection_jobs SET privacy_epoch=?,cursor_time=start_time,cursor_type='',cursor_id=-1,cursor_mark='',phase=?,state='pending',processed_sources=0,updated_at=? WHERE id=? AND privacy_epoch IS NOT ?")
                .bind(current).bind(if parent.starts_with("mark:"){"marks"}else{"sources"}).bind(iso(Utc::now())).bind(hash(format!("{parent}:{}",iso(start)))).bind(current).execute(&mut **tx.conn()).await?;
            tx.commit().await?;
        }
        if matches!(&result,Err(ExplicitMemoryError::Invalid(code)) if code=="memory_index_writer_budget")
        {
            let mut tx = self.begin_immediate_with_retry().await?;
            sqlx::query("UPDATE memory_projection_jobs SET page_limit=max(1,page_limit/2),updated_at=? WHERE id=? AND state='pending'")
                .bind(iso(Utc::now())).bind(hash(format!("{parent}:{}",iso(start)))).execute(&mut **tx.conn()).await?;
            tx.commit().await?;
        }
        result
    }

    async fn memory_projection_page(
        &self,
        parent: &str,
        start: DateTime<Utc>,
    ) -> Result<Option<(DateTime<Utc>, bool)>, ExplicitMemoryError> {
        self.memory_reader_ready().await?;
        let mut conn = self.acquire_read().await?;
        let mut read = conn.begin().await?;
        let current = epoch(&mut read).await?;
        let id = hash(format!("{parent}:{}", iso(start)));
        let Some(job) = sqlx::query("SELECT * FROM memory_projection_jobs WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *read)
            .await?
        else {
            return Ok(None);
        };
        let end = time(&job.try_get::<String, _>("end_time")?)?;
        if job.try_get::<Option<i64>, _>("privacy_epoch")? != current {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        if job.try_get::<String, _>("state")? == "ready" {
            return Ok(Some((end, true)));
        }
        let offset: i32 = job.try_get("timezone_offset_minutes")?;
        let page: i64 = job.try_get::<i64, _>("page_limit")?.clamp(1, PAGE);
        let old_cursor: String = job.try_get("cursor_time")?;
        let mut cursor = old_cursor.clone();
        let mut cursor_type: String = job.try_get("cursor_type")?;
        let mut cursor_id: i64 = job.try_get("cursor_id")?;
        let mut cursor_mark: String = job.try_get("cursor_mark")?;
        let phase: String = job.try_get("phase")?;
        let job_epoch: Option<i64> = job.try_get("privacy_epoch")?;
        if job_epoch != current {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let mut nodes = Map::new();
        let mut links = Vec::new();
        let mut mark_links = Vec::new();
        let mut processed = 0_i64;
        let mut next_phase = phase.clone();
        let mut ready = false;
        if phase == "sources" {
            let rows=sqlx::query("SELECT e.source_type,e.source_id,min(e.occurred_at) AS occurred_at,max(i.end_at) AS stop FROM activity_evidence e JOIN activity_intervals i ON i.id=e.interval_id WHERE julianday(e.occurred_at)>=julianday(?1) AND julianday(e.occurred_at)<julianday(?2) AND (julianday(e.occurred_at)>julianday(?3) OR (julianday(e.occurred_at)=julianday(?3) AND (e.source_type>?4 OR (e.source_type=?4 AND e.source_id>?5)))) GROUP BY e.source_type,e.source_id ORDER BY julianday(occurred_at),e.source_type,e.source_id LIMIT ?6")
                .bind(iso(start)).bind(iso(end)).bind(&cursor).bind(&cursor_type).bind(cursor_id).bind(page).fetch_all(&mut *read).await?;
            for row in &rows {
                let kind: String = row.try_get("source_type")?;
                let source_id: i64 = row.try_get("source_id")?;
                let at: DateTime<Utc> = row.try_get("occurred_at")?;
                cursor = at.to_rfc3339();
                cursor_type = kind.clone();
                cursor_id = source_id;
                processed += 1;
                let payload=match kind.as_str(){
                    "frame"=>"SELECT timestamp,app_name,window_name AS title,browser_url,document_path,content_hash AS version,NULL AS body FROM frames WHERE id=? AND (machine_id IS NULL OR machine_id='')",
                    "ui_event"=>"SELECT timestamp,app_name,window_title AS title,browser_url,NULL AS document_path,NULL AS version,substr(COALESCE(text_content,element_name,''),1,8000) AS body FROM ui_events WHERE id=?",
                    "audio"=>"SELECT timestamp,NULL AS app_name,NULL AS title,NULL AS browser_url,NULL AS document_path,NULL AS version,substr(transcription,1,8000) AS body FROM audio_transcriptions WHERE id=?",
                    _=>continue,
                };
                let Some(source) = sqlx::query(sqlx::AssertSqlSafe(payload))
                    .bind(source_id)
                    .fetch_optional(&mut *read)
                    .await?
                else {
                    continue;
                };
                let app = source
                    .try_get::<Option<String>, _>("app_name")?
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| {
                        if kind == "audio" {
                            "Audio".into()
                        } else {
                            "Unknown app".into()
                        }
                    });
                let title = source
                    .try_get::<Option<String>, _>("title")?
                    .unwrap_or_default();
                let url: Option<String> = source.try_get("browser_url")?;
                let path: Option<String> = source.try_get("document_path")?;
                let version: Option<i64> = source.try_get("version")?;
                let body: Option<String> = source.try_get("body")?;
                let digest = hash(
                    json!([
                        kind,
                        source_id,
                        iso(at),
                        app,
                        title,
                        url,
                        path,
                        version,
                        body,
                        current
                    ])
                    .to_string(),
                );
                let input = Input {
                    kind: kind.clone(),
                    id: source_id,
                    at,
                    stop: row
                        .try_get::<DateTime<Utc>, _>("stop")?
                        .max(at + chrono::Duration::milliseconds(1)),
                    app,
                    title,
                    url,
                    path,
                    digest: digest.clone(),
                };
                for leaf in append_source(&mut read, &mut nodes, &input, offset, current).await? {
                    links.push((leaf, kind.clone(), source_id, iso(at), digest.clone()));
                }
            }
            if rows.len() < page as usize {
                next_phase = "marks".into();
                cursor = iso(start);
                cursor_type.clear();
                cursor_id = -1;
            }
        } else {
            let rows=sqlx::query("SELECT id,source_timestamp,source_frame_id,source_app,revision FROM explicit_memory_marks WHERE julianday(source_timestamp)>=julianday(?) AND julianday(source_timestamp)<julianday(?) AND privacy_epoch IS ? AND (julianday(source_timestamp)>julianday(?) OR (julianday(source_timestamp)=julianday(?) AND id>?)) ORDER BY julianday(source_timestamp),id LIMIT ?")
                .bind(iso(start)).bind(iso(end)).bind(current).bind(&cursor).bind(&cursor).bind(&cursor_mark).bind(page).fetch_all(&mut *read).await?;
            for row in &rows {
                let mark: String = row.try_get("id")?;
                let at: DateTime<Utc> = row.try_get("source_timestamp")?;
                let frame: i64 = row.try_get("source_frame_id")?;
                let revision: i64 = row.try_get("revision")?;
                let source = sqlx::query(
                    "SELECT app_name,window_name,browser_url,document_path FROM frames WHERE id=?",
                )
                .bind(frame)
                .fetch_optional(&mut *read)
                .await?;
                let app = source
                    .as_ref()
                    .and_then(|r| r.try_get::<Option<String>, _>("app_name").ok().flatten())
                    .or(row.try_get::<Option<String>, _>("source_app")?)
                    .unwrap_or_else(|| "Unknown app".into());
                let title = source
                    .as_ref()
                    .and_then(|r| r.try_get::<Option<String>, _>("window_name").ok().flatten())
                    .unwrap_or_else(|| "Saved moment".into());
                let url = source
                    .as_ref()
                    .and_then(|r| r.try_get::<Option<String>, _>("browser_url").ok().flatten());
                let path = source.as_ref().and_then(|r| {
                    r.try_get::<Option<String>, _>("document_path")
                        .ok()
                        .flatten()
                });
                let input = Input {
                    kind: "frame".into(),
                    id: frame,
                    at,
                    stop: at + chrono::Duration::milliseconds(1),
                    app,
                    title,
                    url,
                    path,
                    digest: String::new(),
                };
                let leaves = append_source(&mut read, &mut nodes, &input, offset, current).await?;
                for delta in nodes.values_mut() {
                    delta.sources.remove(&("frame".into(), frame));
                }
                for leaf in leaves {
                    let mut path = Some(leaf.clone());
                    while let Some(id) = path {
                        let delta = nodes.get_mut(&id).unwrap();
                        delta.marks.insert(mark.clone(), revision);
                        path = delta.node.parent_id.clone();
                    }
                    mark_links.push((leaf, mark.clone(), revision));
                }
                cursor = at.to_rfc3339();
                cursor_mark = mark;
                processed += 1;
            }
            ready = rows.len() < page as usize;
        }
        // Count only source tuples in this page, using the source-key index.
        // There is no full-day read or read/modify/write of all historical refs.
        if nodes.len() > MAX_PROJECTION_NODES {
            return Err(invalid("memory_index_writer_budget"));
        }
        for delta in nodes.values_mut() {
            let tuples = serde_json::to_string(&delta.sources.keys().collect::<Vec<_>>())
                .map_err(|_| invalid("memory_projection_encoding"))?;
            let sql=format!("{} SELECT DISTINCT e.source_type,e.source_id,e.content_fingerprint FROM memory_evidence e JOIN descendants d ON e.node_id=d.id JOIN json_each(?2) keys ON e.source_type=json_extract(keys.value,'$[0]') AND e.source_id=json_extract(keys.value,'$[1]') WHERE e.privacy_epoch IS ?3 AND julianday(e.occurred_at)>=julianday(d.lower_bound) AND julianday(e.occurred_at)<julianday(d.upper_bound)",query::DESCENDANTS);
            let old = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(&delta.node.id)
                .bind(tuples)
                .bind(current)
                .fetch_all(&mut *read)
                .await?;
            let mut known = Map::new();
            for row in old {
                known.insert(
                    (
                        row.try_get::<String, _>("source_type")?,
                        row.try_get::<i64, _>("source_id")?,
                    ),
                    row.try_get::<String, _>("content_fingerprint")?,
                );
            }
            for (key, (_, digest)) in &delta.sources {
                if !known.contains_key(key) {
                    delta.node.evidence_count += 1;
                }
                if known.get(key) != Some(digest) {
                    delta.dirty = true;
                }
            }
            for (mark, revision) in &delta.marks {
                let sql=format!("{} SELECT max(nm.mark_revision) FROM memory_node_marks nm JOIN descendants d ON nm.node_id=d.id JOIN explicit_memory_marks m ON m.id=nm.mark_id WHERE nm.mark_id=?2 AND m.privacy_epoch IS ?3 AND julianday(m.source_timestamp)>=julianday(d.lower_bound) AND julianday(m.source_timestamp)<julianday(d.upper_bound)",query::DESCENDANTS);
                let old: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                    .bind(&delta.node.id)
                    .bind(mark)
                    .bind(current)
                    .fetch_one(&mut *read)
                    .await?;
                if old.is_none() {
                    delta.node.mark_count += 1;
                }
                if old != Some(*revision) {
                    delta.dirty = true;
                }
            }
            if delta.dirty {
                delta.node.provisional =
                    time(&delta.node.end_time)? > Utc::now() - chrono::Duration::minutes(5);
                delta.node.fingerprint = hash(
                    json!([
                        delta.node.fingerprint,
                        delta.node.label,
                        delta.node.start_time,
                        delta.node.end_time,
                        delta.sources.iter().collect::<Vec<_>>(),
                        delta.marks,
                        current
                    ])
                    .to_string(),
                );
                if delta.expected_revision.is_some() {
                    delta.node.revision += 1;
                }
            }
        }
        read.commit().await?;
        drop(conn);
        let mut tx = self.begin_immediate_with_retry().await?;
        let began = std::time::Instant::now();
        let budget = || {
            if began.elapsed() > std::time::Duration::from_millis(250) {
                Err(invalid("memory_index_writer_budget"))
            } else {
                Ok(())
            }
        };
        if epoch(&mut **tx.conn()).await? != current {
            return Err(ExplicitMemoryError::PrivacyChanged);
        }
        let unchanged:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_projection_jobs WHERE id=? AND cursor_time=? AND cursor_type=? AND cursor_id=? AND phase=? AND cursor_mark=?)")
            .bind(&id).bind(&old_cursor).bind(job.try_get::<String,_>("cursor_type")?).bind(job.try_get::<i64,_>("cursor_id")?).bind(&phase).bind(job.try_get::<String,_>("cursor_mark")?).fetch_one(&mut **tx.conn()).await?;
        if !unchanged {
            return Err(ExplicitMemoryError::Conflict);
        }
        let revisions = serde_json::to_string(
            &nodes
                .values()
                .map(|d| (&d.node.id, d.expected_revision))
                .collect::<Vec<_>>(),
        )
        .map_err(|_| invalid("memory_projection_encoding"))?;
        let changed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM json_each(?) j LEFT JOIN memory_nodes n ON n.id=json_extract(j.value,'$[0]') WHERE n.revision IS NOT json_extract(j.value,'$[1]'))").bind(revisions).fetch_one(&mut **tx.conn()).await?;
        if changed {
            return Err(ExplicitMemoryError::Conflict);
        }
        let generation: i64 =
            sqlx::query_scalar("SELECT generation FROM memory_index_state WHERE id=1")
                .fetch_one(&mut **tx.conn())
                .await?;
        for kind in ["day", "session", "app", "task", "location"] {
            let values: Vec<_> = nodes
                .values()
                .filter(|d| d.node.kind == kind && d.dirty)
                .collect();
            for chunk in values.chunks(32) {
                budget()?;
                let mut sql=sqlx::QueryBuilder::new("INSERT INTO memory_nodes(id,parent_id,day_key,timezone_offset_minutes,kind,label,label_origin,start_time,end_time,provisional,evidence_count,mark_count,apps_json,revision,fingerprint,privacy_epoch,invalidated,generation,updated_at,view,app_name,location_type,location_key,location_label,canonical_key) ");
                sql.push_values(chunk, |mut row, delta| {
                    let n = &delta.node;
                    row.push_bind(&n.id)
                        .push_bind(&n.parent_id)
                        .push_bind(&delta.day)
                        .push_bind(offset)
                        .push_bind(match n.kind.as_str() {
                            "app" => "session",
                            "location" => "task",
                            other => other,
                        })
                        .push_bind(&n.label)
                        .push_bind(&n.label_origin)
                        .push_bind(&n.start_time)
                        .push_bind(&n.end_time)
                        .push_bind(n.provisional)
                        .push_bind(n.evidence_count)
                        .push_bind(n.mark_count)
                        .push_bind(serde_json::to_string(&n.apps).unwrap())
                        .push_bind(n.revision)
                        .push_bind(&n.fingerprint)
                        .push_bind(current)
                        .push_bind(0)
                        .push_bind(generation)
                        .push_bind(iso(Utc::now()))
                        .push_bind(&n.view)
                        .push_bind(&n.app_name)
                        .push_bind(&n.location_type)
                        .push_bind(&n.location_key)
                        .push_bind(&n.location_label)
                        .push_bind(&delta.canonical);
                });
                sql.push(" ON CONFLICT(id) DO UPDATE SET label=excluded.label,label_origin=excluded.label_origin,start_time=excluded.start_time,end_time=excluded.end_time,provisional=excluded.provisional,evidence_count=excluded.evidence_count,mark_count=excluded.mark_count,apps_json=excluded.apps_json,revision=excluded.revision,fingerprint=excluded.fingerprint,privacy_epoch=excluded.privacy_epoch,invalidated=0,generation=excluded.generation,updated_at=excluded.updated_at,canonical_key=CASE WHEN excluded.canonical_key='' THEN memory_nodes.canonical_key ELSE excluded.canonical_key END");
                sql.build().execute(&mut **tx.conn()).await?;
            }
        }
        for chunk in links.chunks(128) {
            budget()?;
            let mut sql=sqlx::QueryBuilder::new("INSERT INTO memory_evidence(node_id,source_type,source_id,occurred_at,content_fingerprint,privacy_epoch) ");
            sql.push_values(chunk, |mut row, (node, kind, id, at, digest)| {
                row.push_bind(node)
                    .push_bind(kind)
                    .push_bind(id)
                    .push_bind(at)
                    .push_bind(digest)
                    .push_bind(current);
            });
            sql.push(" ON CONFLICT(node_id,source_type,source_id) DO UPDATE SET occurred_at=excluded.occurred_at,content_fingerprint=excluded.content_fingerprint,privacy_epoch=excluded.privacy_epoch");
            sql.build().execute(&mut **tx.conn()).await?;
        }
        for chunk in links.chunks(100) {
            budget()?;
            let mut sql=sqlx::QueryBuilder::new("INSERT INTO memory_source_assignments(day_key,view,source_type,source_id,node_id,occurred_at,content_fingerprint,privacy_epoch) ");
            sql.push_values(chunk, |mut row, (id, kind, source, at, digest)| {
                let d = &nodes[id];
                row.push_bind(&d.day)
                    .push_bind(&d.node.view)
                    .push_bind(kind)
                    .push_bind(source)
                    .push_bind(id)
                    .push_bind(at)
                    .push_bind(digest)
                    .push_bind(current);
            });
            sql.push(" ON CONFLICT(day_key,view,source_type,source_id) DO UPDATE SET node_id=excluded.node_id,occurred_at=excluded.occurred_at,content_fingerprint=excluded.content_fingerprint,privacy_epoch=excluded.privacy_epoch");
            sql.build().execute(&mut **tx.conn()).await?;
        }
        for chunk in mark_links.chunks(128) {
            budget()?;
            let mut sql = sqlx::QueryBuilder::new(
                "INSERT INTO memory_node_marks(node_id,mark_id,mark_revision) ",
            );
            sql.push_values(chunk, |mut row, (node, mark, revision)| {
                row.push_bind(node).push_bind(mark).push_bind(revision);
            });
            sql.push(
                " ON CONFLICT(node_id,mark_id) DO UPDATE SET mark_revision=excluded.mark_revision",
            );
            sql.build().execute(&mut **tx.conn()).await?;
        }
        budget()?;
        sqlx::query("UPDATE memory_projection_jobs SET cursor_time=?,cursor_type=?,cursor_id=?,cursor_mark=?,phase=?,state=?,processed_sources=processed_sources+?,updated_at=? WHERE id=?")
            .bind(&cursor).bind(&cursor_type).bind(cursor_id).bind(&cursor_mark).bind(next_phase).bind(if ready{"ready"}else{"pending"}).bind(processed).bind(iso(Utc::now())).bind(&id).execute(&mut **tx.conn()).await?;
        tx.commit().await?;
        Ok(Some((end, ready)))
    }
}
