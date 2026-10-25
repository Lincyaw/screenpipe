-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- Older bounded ledger replacements could discard evidence after their end.
-- Revisit accepted derived jobs; immutable captures, authored marks, and user
-- correction overrides are retained. Existing node-change triggers clear all
-- generated summary text/citations and its FTS entries in this transaction.
UPDATE memory_index_state SET generation=max(generation,3) WHERE id=1;
UPDATE memory_nodes SET invalidated=1,label='',apps_json='[]',
    fingerprint=fingerprint||':bounded-suffix-v3',revision=revision+1,
    generation=(SELECT generation FROM memory_index_state WHERE id=1);
UPDATE memory_index_jobs SET state='pending',cursor=start_time,attempts=0,
    next_attempt_at=0,last_error=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now');
