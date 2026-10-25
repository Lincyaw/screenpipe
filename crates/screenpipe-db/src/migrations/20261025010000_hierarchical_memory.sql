-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- Derived, disposable index. No capture INSERT/UPDATE trigger, second writer,
-- or model work is introduced. Authored corrections live outside projections.
CREATE TABLE memory_nodes (
    id TEXT PRIMARY KEY,
    parent_id TEXT REFERENCES memory_nodes(id) ON DELETE CASCADE,
    day_key TEXT NOT NULL,
    timezone_offset_minutes INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('day','session','task')),
    label TEXT NOT NULL,
    label_origin TEXT NOT NULL DEFAULT 'deterministic',
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    provisional INTEGER NOT NULL,
    evidence_count INTEGER NOT NULL DEFAULT 0,
    mark_count INTEGER NOT NULL DEFAULT 0,
    apps_json TEXT NOT NULL DEFAULT '[]',
    revision INTEGER NOT NULL DEFAULT 1,
    fingerprint TEXT NOT NULL,
    privacy_epoch INTEGER,
    invalidated INTEGER NOT NULL DEFAULT 0,
    generation INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL
);
CREATE INDEX memory_nodes_parent_time ON memory_nodes(parent_id,start_time,id);
CREATE INDEX memory_nodes_day ON memory_nodes(day_key,id);
CREATE INDEX memory_nodes_epoch ON memory_nodes(privacy_epoch,invalidated,kind,start_time);
CREATE TABLE memory_evidence (
    node_id TEXT NOT NULL REFERENCES memory_nodes(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK(source_type IN ('frame','ui_event','audio')),
    source_id INTEGER NOT NULL,
    occurred_at TEXT NOT NULL,
    content_fingerprint TEXT NOT NULL,
    PRIMARY KEY(node_id,source_type,source_id)
);
CREATE INDEX memory_evidence_source ON memory_evidence(source_type,source_id,node_id);
CREATE INDEX memory_evidence_time ON memory_evidence(node_id,occurred_at);
CREATE TABLE memory_node_marks (
    node_id TEXT NOT NULL REFERENCES memory_nodes(id) ON DELETE CASCADE,
    mark_id TEXT NOT NULL REFERENCES explicit_memory_marks(id) ON DELETE CASCADE,
    mark_revision INTEGER NOT NULL,
    PRIMARY KEY(node_id,mark_id)
);
CREATE INDEX memory_node_marks_mark ON memory_node_marks(mark_id,node_id);
CREATE INDEX explicit_memory_source_time ON explicit_memory_marks(source_timestamp,id);
CREATE TABLE memory_overrides (
    id TEXT PRIMARY KEY,
    day_key TEXT NOT NULL,
    operation TEXT NOT NULL CHECK(operation IN ('label','merge','split')),
    target_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX memory_overrides_day ON memory_overrides(day_key,created_at,id);
CREATE TABLE memory_index_jobs (
    id TEXT PRIMARY KEY,
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    cursor TEXT NOT NULL,
    timezone_offset_minutes INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','running','ready','failed')),
    priority INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX memory_index_jobs_next ON memory_index_jobs(state,next_attempt_at,priority,created_at);
CREATE TABLE memory_index_state (
    id INTEGER PRIMARY KEY CHECK(id=1),
    generation INTEGER NOT NULL DEFAULT 1,
    cursor TEXT,
    last_success_at TEXT,
    timezone_offset_minutes INTEGER NOT NULL DEFAULT 0
);
INSERT INTO memory_index_state(id) VALUES(1);

-- Only rare authored mark mutations enqueue these tiny dirty-day jobs. Raw
-- recording has no index trigger. A running job is reset to pending so an edit
-- racing its snapshot cannot be acknowledged and then forgotten. One second
-- of observations is enough: the job rebuilds the whole existing day index and
-- associates all marks by their immutable source timestamp.
CREATE VIEW memory_mark_index_ranges AS
SELECT m.id AS mark_id,s.timezone_offset_minutes AS offset,
    'mark:'||s.timezone_offset_minutes||':'||strftime('%Y-%m-%d',m.source_timestamp,printf('%+d minutes',s.timezone_offset_minutes)) AS job_id,
    strftime('%Y-%m-%dT%H:%M:%fZ',m.source_timestamp,printf('%+d minutes',s.timezone_offset_minutes),'start of day',printf('%+d minutes',-s.timezone_offset_minutes)) AS start_time
FROM explicit_memory_marks m CROSS JOIN memory_index_state s WHERE s.id=1;
CREATE TRIGGER memory_mark_index_insert AFTER INSERT ON explicit_memory_marks BEGIN
    INSERT INTO memory_index_jobs(id,start_time,end_time,cursor,timezone_offset_minutes,state,priority,created_at,updated_at)
    SELECT job_id,start_time,strftime('%Y-%m-%dT%H:%M:%fZ',start_time,'+1 second'),start_time,offset,'pending',50,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now')
    FROM memory_mark_index_ranges WHERE mark_id=NEW.id
    ON CONFLICT(id) DO UPDATE SET state='pending',cursor=excluded.start_time,attempts=0,next_attempt_at=0,last_error=NULL,updated_at=excluded.updated_at;
END;
CREATE TRIGGER memory_mark_index_update AFTER UPDATE OF title,note,importance,revision ON explicit_memory_marks BEGIN
    INSERT INTO memory_index_jobs(id,start_time,end_time,cursor,timezone_offset_minutes,state,priority,created_at,updated_at)
    SELECT job_id,start_time,strftime('%Y-%m-%dT%H:%M:%fZ',start_time,'+1 second'),start_time,offset,'pending',50,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now')
    FROM memory_mark_index_ranges WHERE mark_id=NEW.id
    ON CONFLICT(id) DO UPDATE SET state='pending',cursor=excluded.start_time,attempts=0,next_attempt_at=0,last_error=NULL,updated_at=excluded.updated_at;
END;
CREATE TRIGGER memory_mark_index_delete BEFORE DELETE ON explicit_memory_marks BEGIN
    INSERT INTO memory_index_jobs(id,start_time,end_time,cursor,timezone_offset_minutes,state,priority,created_at,updated_at)
    SELECT job_id,start_time,strftime('%Y-%m-%dT%H:%M:%fZ',start_time,'+1 second'),start_time,offset,'pending',50,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now')
    FROM memory_mark_index_ranges WHERE mark_id=OLD.id
    ON CONFLICT(id) DO UPDATE SET state='pending',cursor=excluded.start_time,attempts=0,next_attempt_at=0,last_error=NULL,updated_at=excluded.updated_at;
END;
CREATE VIRTUAL TABLE memory_nodes_fts USING fts5(node_id UNINDEXED,label,tokenize='unicode61');
CREATE TRIGGER memory_nodes_ai AFTER INSERT ON memory_nodes BEGIN
    INSERT INTO memory_nodes_fts(rowid,node_id,label) VALUES(NEW.rowid,NEW.id,NEW.label);
END;
CREATE TRIGGER memory_nodes_au AFTER UPDATE OF label ON memory_nodes WHEN OLD.label!=NEW.label BEGIN
    DELETE FROM memory_nodes_fts WHERE rowid=OLD.rowid;
    INSERT INTO memory_nodes_fts(rowid,node_id,label) VALUES(NEW.rowid,NEW.id,NEW.label);
END;
CREATE TRIGGER memory_nodes_ad AFTER DELETE ON memory_nodes BEGIN
    DELETE FROM memory_nodes_fts WHERE rowid=OLD.rowid;
END;

-- Deletion is exceptional and already coordinated. Invalidate the entire
-- affected day before removing references: copied labels and ancestor summaries
-- must never survive erasure as readable evidence. The day index bounds work.
CREATE TRIGGER memory_frame_erased BEFORE DELETE ON frames BEGIN
    UPDATE memory_nodes SET invalidated=1,label='',apps_json='[]',fingerprint=fingerprint||':erased',revision=revision+1
      WHERE day_key IN (SELECT n.day_key FROM memory_nodes n JOIN memory_evidence e ON e.node_id=n.id WHERE e.source_type='frame' AND e.source_id=OLD.id)
        AND invalidated=0;
    DELETE FROM memory_evidence WHERE source_type='frame' AND source_id=OLD.id;
END;
CREATE TRIGGER memory_event_erased BEFORE DELETE ON ui_events BEGIN
    UPDATE memory_nodes SET invalidated=1,label='',apps_json='[]',fingerprint=fingerprint||':erased',revision=revision+1
      WHERE day_key IN (SELECT n.day_key FROM memory_nodes n JOIN memory_evidence e ON e.node_id=n.id WHERE e.source_type='ui_event' AND e.source_id=OLD.id)
        AND invalidated=0;
    DELETE FROM memory_evidence WHERE source_type='ui_event' AND source_id=OLD.id;
END;
CREATE TRIGGER memory_audio_erased BEFORE DELETE ON audio_transcriptions BEGIN
    UPDATE memory_nodes SET invalidated=1,label='',apps_json='[]',fingerprint=fingerprint||':erased',revision=revision+1
      WHERE day_key IN (SELECT n.day_key FROM memory_nodes n JOIN memory_evidence e ON e.node_id=n.id WHERE e.source_type='audio' AND e.source_id=OLD.id)
        AND invalidated=0;
    DELETE FROM memory_evidence WHERE source_type='audio' AND source_id=OLD.id;
END;
