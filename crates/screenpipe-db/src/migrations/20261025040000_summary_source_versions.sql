-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- A Hybrid payload replacement can revoke previously copied text without a
-- privacy-policy change. Bind cached summaries to exact input generations.
ALTER TABLE memory_node_summaries ADD COLUMN source_versions_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(source_versions_json));
CREATE TRIGGER memory_summary_source_versions_clear AFTER UPDATE ON memory_node_summaries
WHEN NEW.status!='ready' AND NEW.source_versions_json!='[]' BEGIN
    UPDATE memory_node_summaries SET source_versions_json='[]' WHERE node_id=NEW.node_id;
END;
UPDATE memory_node_summaries SET status='stale',text=NULL,citations_json='[]',statements_json='[]',source_versions_json='[]',error_code='source_versions_required',updated_at=unixepoch();
