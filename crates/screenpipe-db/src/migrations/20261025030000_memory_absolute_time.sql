-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- Capture timestamps can carry different UTC offsets. Index their absolute
-- value so bounded background reads cannot silently select another time zone
-- or replace a range lookup with a complete capture-history scan. These are
-- ordinary SQLite metadata indexes, maintained by the existing sole writer.
CREATE INDEX IF NOT EXISTS memory_frames_absolute_time ON frames(julianday(timestamp),id);
CREATE INDEX IF NOT EXISTS memory_events_absolute_time ON ui_events(julianday(timestamp),id);
CREATE INDEX IF NOT EXISTS memory_audio_absolute_time ON audio_transcriptions(julianday(timestamp),id);
CREATE INDEX IF NOT EXISTS memory_activity_absolute_bounds ON activity_intervals(julianday(end_at),julianday(start_at),id);
CREATE INDEX IF NOT EXISTS memory_activity_evidence_absolute_time ON activity_evidence(julianday(occurred_at),id,interval_id);

-- Revisit earlier apparently completed windows as well as failed ones: a
-- lexicographic UTC-vs-offset comparison could have reported empty success.
-- Only already accepted derived jobs are reset; captures, authored marks and
-- user correction actions remain untouched.
UPDATE memory_index_state SET generation=generation+1 WHERE id=1;
-- Do not serve a v1 projection as if it had passed the absolute-time scan.
-- Existing node-change triggers also clear summary bodies, citations and FTS.
UPDATE memory_nodes SET invalidated=1,label='',apps_json='[]',
    fingerprint=fingerprint||':absolute-time-v2',revision=revision+1,
    generation=(SELECT generation FROM memory_index_state WHERE id=1);
UPDATE memory_index_jobs SET state='pending',cursor=start_time,attempts=0,
    next_attempt_at=0,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now');
