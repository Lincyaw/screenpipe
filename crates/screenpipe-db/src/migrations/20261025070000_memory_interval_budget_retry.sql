-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- A normal recorded day can contain more than 2,000 activity intervals while
-- remaining inside the existing 12,000-source projection budget. Retry only
-- work deferred by that smaller interval limit; retain successful projections
-- and all unrelated diagnostics, captures, marks, and correction overrides.
UPDATE memory_index_jobs SET state='pending',cursor=start_time,attempts=0,
    next_attempt_at=0,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE state IN ('pending','failed') AND last_error='memory_day_interval_budget_exceeded';
