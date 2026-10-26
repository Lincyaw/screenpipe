-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- Keep the original node table, CHECK constraints, rowids, foreign keys, FTS,
-- summaries and correction IDs. App projection levels reuse physical
-- day/session/task kinds; the typed API maps them to day/app/location.
ALTER TABLE memory_nodes ADD COLUMN view TEXT NOT NULL DEFAULT 'task' CHECK(view IN ('task','app'));
ALTER TABLE memory_nodes ADD COLUMN app_name TEXT;
ALTER TABLE memory_nodes ADD COLUMN location_type TEXT;
ALTER TABLE memory_nodes ADD COLUMN location_key TEXT;
ALTER TABLE memory_nodes ADD COLUMN location_label TEXT;
ALTER TABLE memory_nodes ADD COLUMN canonical_key TEXT NOT NULL DEFAULT '';
ALTER TABLE memory_nodes ADD COLUMN superseded INTEGER NOT NULL DEFAULT 0;
CREATE INDEX memory_nodes_projection_children ON memory_nodes(view,parent_id,superseded,start_time,id);
CREATE INDEX memory_nodes_projection_identity ON memory_nodes(day_key,view,parent_id,canonical_key,superseded);
ALTER TABLE memory_evidence ADD COLUMN privacy_epoch INTEGER;
UPDATE memory_evidence SET privacy_epoch=(SELECT privacy_epoch FROM memory_nodes WHERE id=node_id);

-- A durable cursor belongs to one already-accepted ledger slice. Restart and
-- bounded retries resume the source tuple, never rewrite the entire day.
CREATE TABLE memory_projection_jobs (
    id TEXT PRIMARY KEY,
    parent_job_id TEXT NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    timezone_offset_minutes INTEGER NOT NULL,
    privacy_epoch INTEGER,
    phase TEXT NOT NULL DEFAULT 'sources',
    cursor_time TEXT NOT NULL,
    cursor_type TEXT NOT NULL DEFAULT '',
    cursor_id INTEGER NOT NULL DEFAULT -1,
    cursor_mark TEXT NOT NULL DEFAULT '',
    processed_sources INTEGER NOT NULL DEFAULT 0,
    page_limit INTEGER NOT NULL DEFAULT 128,
    state TEXT NOT NULL DEFAULT 'pending',
    updated_at TEXT NOT NULL
);
CREATE INDEX memory_projection_jobs_parent ON memory_projection_jobs(parent_job_id,start_time,state);
CREATE INDEX memory_projection_jobs_cleanup ON memory_projection_jobs(state,updated_at);

-- Routing is metadata only. Source bytes remain exclusively in the capture
-- tables/Hybrid reader; both views reference the same immutable source IDs.
CREATE TABLE memory_source_assignments (
    day_key TEXT NOT NULL,
    view TEXT NOT NULL,
    source_type TEXT NOT NULL,
    source_id INTEGER NOT NULL,
    node_id TEXT NOT NULL REFERENCES memory_nodes(id) ON DELETE CASCADE,
    occurred_at TEXT NOT NULL,
    content_fingerprint TEXT NOT NULL,
    privacy_epoch INTEGER,
    PRIMARY KEY(day_key,view,source_type,source_id)
);
CREATE INDEX memory_source_assignments_node ON memory_source_assignments(node_id,source_type,source_id);

-- Correction routing preserves the old source links without a large copy in
-- the writer. Only exceptional user corrections add these time-bounded edges.
CREATE TABLE memory_node_routes (
    source_node_id TEXT NOT NULL REFERENCES memory_nodes(id) ON DELETE CASCADE,
    target_node_id TEXT NOT NULL REFERENCES memory_nodes(id) ON DELETE CASCADE,
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    PRIMARY KEY(source_node_id,target_node_id,start_time)
);
CREATE INDEX memory_node_routes_target ON memory_node_routes(target_node_id,source_node_id);

-- A processing-budget failure never invalidates good existing projections.
UPDATE memory_index_jobs SET state='pending',cursor=start_time,attempts=0,
    next_attempt_at=0,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE state IN ('pending','failed') AND last_error IN (
    'memory_day_interval_budget_exceeded','memory_day_evidence_budget_exceeded',
    'memory_day_node_budget_exceeded','memory_day_mark_budget_exceeded');
-- Populate the additional view from only the already-accepted seven-day
-- background history. Existing task nodes remain readable during replay.
UPDATE memory_index_jobs SET state='pending',cursor=start_time,attempts=0,
    next_attempt_at=0,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE state='ready' AND priority=0 AND julianday(end_time)>=julianday('now','-7 days');

CREATE TABLE memory_ingest_watermarks(kind TEXT PRIMARY KEY,source_id INTEGER NOT NULL);
INSERT INTO memory_ingest_watermarks VALUES('frame',(SELECT COALESCE(max(id),0) FROM frames));
INSERT INTO memory_ingest_watermarks VALUES('ui_event',(SELECT COALESCE(max(id),0) FROM ui_events));
INSERT INTO memory_ingest_watermarks VALUES('audio',(SELECT COALESCE(max(id),0) FROM audio_transcriptions));
