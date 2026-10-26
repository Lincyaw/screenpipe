-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- Marks-only replay cannot visit a leaf after its last mark is deleted. Recount
-- only the linked leaves, correction routes and their ancestors while the old
-- link still exists. No capture-table trigger or background writer is added.
-- A node's epoch is also the epoch used by the typed descendant-mark query;
-- stale nodes remain subject to the existing current-policy read gate.
CREATE TRIGGER memory_mark_projection_delete BEFORE DELETE ON explicit_memory_marks BEGIN
    UPDATE memory_nodes SET
        mark_count=(
            WITH RECURSIVE descendants(id,depth,lower_bound,upper_bound) AS (
                SELECT memory_nodes.id,0,'0001-01-01T00:00:00Z','9999-12-31T23:59:59Z'
                UNION ALL
                SELECT child.id,d.depth+1,d.lower_bound,d.upper_bound
                FROM memory_nodes child JOIN descendants d ON child.parent_id=d.id
                WHERE d.depth<32 AND child.superseded=0
                UNION ALL
                SELECT r.source_node_id,d.depth+1,
                    CASE WHEN julianday(r.start_time)>julianday(d.lower_bound) THEN r.start_time ELSE d.lower_bound END,
                    CASE WHEN julianday(r.end_time)<julianday(d.upper_bound) THEN r.end_time ELSE d.upper_bound END
                FROM memory_node_routes r JOIN descendants d ON r.target_node_id=d.id
                WHERE d.depth<32
            )
            SELECT count(DISTINCT m.id)
            FROM descendants d JOIN memory_node_marks nm ON nm.node_id=d.id
            JOIN explicit_memory_marks m ON m.id=nm.mark_id
            WHERE m.id!=OLD.id AND m.privacy_epoch IS memory_nodes.privacy_epoch
                AND julianday(m.source_timestamp)>=julianday(d.lower_bound)
                AND julianday(m.source_timestamp)<julianday(d.upper_bound)
        ),
        fingerprint=fingerprint||':mark-delete:'||OLD.id||':'||OLD.revision,
        revision=revision+1,
        updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE id IN (
        WITH RECURSIVE affected(id) AS (
            SELECT node_id FROM memory_node_marks WHERE mark_id=OLD.id
            UNION
            SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id
                WHERE n.parent_id IS NOT NULL
            UNION
            SELECT r.target_node_id FROM memory_node_routes r JOIN affected a ON r.source_node_id=a.id
                WHERE julianday(OLD.source_timestamp)>=julianday(r.start_time)
                    AND julianday(OLD.source_timestamp)<julianday(r.end_time)
        )
        SELECT id FROM affected
    );
END;

-- Author edits change the input fingerprint immediately, including correction
-- targets that only own a route. Retention-only epoch promotion does not change
-- an authored revision and therefore does not run this trigger. Existing node
-- fingerprint triggers invalidate summaries, FTS entries and active jobs.
CREATE TRIGGER memory_mark_projection_update AFTER UPDATE OF revision ON explicit_memory_marks
WHEN OLD.revision!=NEW.revision BEGIN
    UPDATE memory_nodes SET
        mark_count=(
            WITH RECURSIVE descendants(id,depth,lower_bound,upper_bound) AS (
                SELECT memory_nodes.id,0,'0001-01-01T00:00:00Z','9999-12-31T23:59:59Z'
                UNION ALL
                SELECT child.id,d.depth+1,d.lower_bound,d.upper_bound
                FROM memory_nodes child JOIN descendants d ON child.parent_id=d.id
                WHERE d.depth<32 AND child.superseded=0
                UNION ALL
                SELECT r.source_node_id,d.depth+1,
                    CASE WHEN julianday(r.start_time)>julianday(d.lower_bound) THEN r.start_time ELSE d.lower_bound END,
                    CASE WHEN julianday(r.end_time)<julianday(d.upper_bound) THEN r.end_time ELSE d.upper_bound END
                FROM memory_node_routes r JOIN descendants d ON r.target_node_id=d.id
                WHERE d.depth<32
            )
            SELECT count(DISTINCT m.id)
            FROM descendants d JOIN memory_node_marks nm ON nm.node_id=d.id
            JOIN explicit_memory_marks m ON m.id=nm.mark_id
            WHERE m.privacy_epoch IS memory_nodes.privacy_epoch
                AND julianday(m.source_timestamp)>=julianday(d.lower_bound)
                AND julianday(m.source_timestamp)<julianday(d.upper_bound)
        ),
        fingerprint=fingerprint||':mark-update:'||NEW.id||':'||NEW.revision,
        revision=revision+1,
        updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE id IN (
        WITH RECURSIVE affected(id) AS (
            SELECT node_id FROM memory_node_marks WHERE mark_id=NEW.id
            UNION
            SELECT n.parent_id FROM memory_nodes n JOIN affected a ON n.id=a.id
                WHERE n.parent_id IS NOT NULL
            UNION
            SELECT r.target_node_id FROM memory_node_routes r JOIN affected a ON r.source_node_id=a.id
                WHERE julianday(NEW.source_timestamp)>=julianday(r.start_time)
                    AND julianday(NEW.source_timestamp)<julianday(r.end_time)
        )
        SELECT id FROM affected
    );
END;
