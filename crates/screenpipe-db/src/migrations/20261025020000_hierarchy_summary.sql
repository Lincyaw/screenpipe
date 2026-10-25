-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

CREATE TABLE memory_summary_config (
    id INTEGER PRIMARY KEY CHECK(id=1),
    config_json TEXT NOT NULL CHECK(json_valid(config_json)),
    updated_at INTEGER NOT NULL
);

CREATE TABLE memory_node_summaries (
    node_id TEXT PRIMARY KEY REFERENCES memory_nodes(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK(status IN ('pending','ready','stale','failed')),
    text TEXT,
    provider TEXT NOT NULL DEFAULT 'ollama_local',
    model TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    input_fingerprint TEXT NOT NULL,
    node_fingerprint TEXT NOT NULL,
    node_revision INTEGER NOT NULL,
    privacy_epoch INTEGER,
    generated_at TEXT,
    citations_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(citations_json)),
    statements_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(statements_json)),
    coverage_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(coverage_json)),
    error_code TEXT,
    updated_at INTEGER NOT NULL
);

CREATE TABLE memory_summary_jobs (
    id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES memory_nodes(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK(status IN ('pending','running','ready','stale','failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    lease_until INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    error_code TEXT
);
CREATE UNIQUE INDEX memory_summary_jobs_active ON memory_summary_jobs(node_id) WHERE status IN ('pending','running');
CREATE INDEX memory_summary_jobs_due ON memory_summary_jobs(status,next_attempt_at,created_at);
CREATE TABLE memory_summary_attempts (
    id INTEGER PRIMARY KEY,
    started_at INTEGER NOT NULL
);
CREATE INDEX memory_summary_attempts_time ON memory_summary_attempts(started_at);
-- Mark-time fallback first finds calendar days, then seeks the existing day_key
-- index. Historical notes cannot scan every task/session in the entire archive.
CREATE INDEX memory_summary_day_bounds ON memory_nodes(kind,julianday(end_time),julianday(start_time),day_key);
CREATE VIRTUAL TABLE memory_summary_fts USING fts5(node_id UNINDEXED,text,tokenize='unicode61');
CREATE TRIGGER memory_summary_fts_insert AFTER INSERT ON memory_node_summaries WHEN NEW.status='ready' BEGIN
    INSERT INTO memory_summary_fts(rowid,node_id,text) VALUES(NEW.rowid,NEW.node_id,NEW.text);
END;
CREATE TRIGGER memory_summary_fts_update AFTER UPDATE ON memory_node_summaries BEGIN
    DELETE FROM memory_summary_fts WHERE rowid=OLD.rowid;
    INSERT INTO memory_summary_fts(rowid,node_id,text) SELECT NEW.rowid,NEW.node_id,NEW.text WHERE NEW.status='ready';
END;
CREATE TRIGGER memory_summary_fts_delete AFTER DELETE ON memory_node_summaries BEGIN
    DELETE FROM memory_summary_fts WHERE rowid=OLD.rowid;
END;

-- Indexer corrections and raw-source deletion update the node fingerprint in
-- the same coordinated transaction. Never expose a previous generated body.
CREATE TRIGGER memory_summary_node_changed AFTER UPDATE OF fingerprint,invalidated ON memory_nodes
WHEN OLD.fingerprint!=NEW.fingerprint OR NEW.invalidated!=0 BEGIN
    UPDATE memory_node_summaries SET status='stale',text=NULL,citations_json='[]',statements_json='[]',error_code='input_changed',updated_at=unixepoch() WHERE node_id=NEW.id;
    UPDATE memory_summary_jobs SET status='stale',lease_until=NULL,error_code='input_changed',updated_at=unixepoch() WHERE node_id=NEW.id AND status IN ('pending','running');
END;

-- A mark is an attributed interpretation, not a capture mutation. Invalidate
-- immediately on user edits, before the background hierarchy next reconciles.
CREATE TRIGGER memory_summary_mark_insert AFTER INSERT ON explicit_memory_marks BEGIN
    UPDATE memory_node_summaries SET status='stale',text=NULL,citations_json='[]',statements_json='[]',error_code='user_note_changed',updated_at=unixepoch()
    WHERE node_id IN (WITH RECURSIVE affected(id) AS (SELECT node_id FROM memory_evidence WHERE source_type='frame' AND source_id=NEW.source_frame_id UNION SELECT node_id FROM memory_node_marks WHERE mark_id=NEW.id UNION SELECT id FROM memory_nodes WHERE day_key IN (SELECT day_key FROM memory_nodes WHERE kind='day' AND julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time)) AND julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) UNION SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id WHERE n.parent_id IS NOT NULL) SELECT id FROM affected);
    UPDATE memory_summary_jobs SET status='stale',lease_until=NULL,error_code='user_note_changed',updated_at=unixepoch()
    WHERE status IN ('pending','running') AND node_id IN (WITH RECURSIVE affected(id) AS (SELECT node_id FROM memory_evidence WHERE source_type='frame' AND source_id=NEW.source_frame_id UNION SELECT node_id FROM memory_node_marks WHERE mark_id=NEW.id UNION SELECT id FROM memory_nodes WHERE day_key IN (SELECT day_key FROM memory_nodes WHERE kind='day' AND julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time)) AND julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) UNION SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id WHERE n.parent_id IS NOT NULL) SELECT id FROM affected);
END;
CREATE TRIGGER memory_summary_mark_update AFTER UPDATE OF title,note,importance,revision,source_frame_id,source_timestamp ON explicit_memory_marks BEGIN
    UPDATE memory_node_summaries SET status='stale',text=NULL,citations_json='[]',statements_json='[]',error_code='user_note_changed',updated_at=unixepoch()
    WHERE node_id IN (WITH RECURSIVE affected(id) AS (SELECT node_id FROM memory_evidence WHERE source_type='frame' AND source_id IN (OLD.source_frame_id,NEW.source_frame_id) UNION SELECT node_id FROM memory_node_marks WHERE mark_id IN (OLD.id,NEW.id) UNION SELECT id FROM memory_nodes WHERE day_key IN (SELECT day_key FROM memory_nodes WHERE kind='day' AND (julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) OR julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time))) AND (julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) OR julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time)) UNION SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id WHERE n.parent_id IS NOT NULL) SELECT id FROM affected);
    UPDATE memory_summary_jobs SET status='stale',lease_until=NULL,error_code='user_note_changed',updated_at=unixepoch()
    WHERE status IN ('pending','running') AND node_id IN (WITH RECURSIVE affected(id) AS (SELECT node_id FROM memory_evidence WHERE source_type='frame' AND source_id IN (OLD.source_frame_id,NEW.source_frame_id) UNION SELECT node_id FROM memory_node_marks WHERE mark_id IN (OLD.id,NEW.id) UNION SELECT id FROM memory_nodes WHERE day_key IN (SELECT day_key FROM memory_nodes WHERE kind='day' AND (julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) OR julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time))) AND (julianday(NEW.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) OR julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time)) UNION SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id WHERE n.parent_id IS NOT NULL) SELECT id FROM affected);
END;
CREATE TRIGGER memory_summary_mark_delete AFTER DELETE ON explicit_memory_marks BEGIN
    UPDATE memory_node_summaries SET status='stale',text=NULL,citations_json='[]',statements_json='[]',error_code='user_note_changed',updated_at=unixepoch()
    WHERE node_id IN (WITH RECURSIVE affected(id) AS (SELECT node_id FROM memory_evidence WHERE source_type='frame' AND source_id=OLD.source_frame_id UNION SELECT node_id FROM memory_node_marks WHERE mark_id=OLD.id UNION SELECT id FROM memory_nodes WHERE day_key IN (SELECT day_key FROM memory_nodes WHERE kind='day' AND julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time)) AND julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) UNION SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id WHERE n.parent_id IS NOT NULL) SELECT id FROM affected);
    UPDATE memory_summary_jobs SET status='stale',lease_until=NULL,error_code='user_note_changed',updated_at=unixepoch()
    WHERE status IN ('pending','running') AND node_id IN (WITH RECURSIVE affected(id) AS (SELECT node_id FROM memory_evidence WHERE source_type='frame' AND source_id=OLD.source_frame_id UNION SELECT node_id FROM memory_node_marks WHERE mark_id=OLD.id UNION SELECT id FROM memory_nodes WHERE day_key IN (SELECT day_key FROM memory_nodes WHERE kind='day' AND julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time)) AND julianday(OLD.source_timestamp) BETWEEN julianday(start_time) AND julianday(end_time) UNION SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id WHERE n.parent_id IS NOT NULL) SELECT id FROM affected);
END;
