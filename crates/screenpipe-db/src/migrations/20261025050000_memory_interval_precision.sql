-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- Only retry the now-diagnosed millisecond CHECK failures. Preserve every
-- other diagnostic, accepted recording, authored mark and correction action.
UPDATE memory_index_jobs SET state='pending',cursor=start_time,attempts=0,
    next_attempt_at=0,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE state IN ('pending','failed') AND last_error='index_sqlite_275';
