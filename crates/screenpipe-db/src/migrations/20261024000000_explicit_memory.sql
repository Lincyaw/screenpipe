-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- Authored notes and immutable source provenance are independent of raw-frame retention.
CREATE TABLE explicit_memory_marks (
    id TEXT PRIMARY KEY,
    frame_id INTEGER REFERENCES frames(id) ON DELETE SET NULL,
    source_frame_id INTEGER NOT NULL,
    source_timestamp TEXT NOT NULL,
    source_app TEXT,
    source_window TEXT,
    source_url TEXT,
    image_width INTEGER CHECK(image_width BETWEEN 1 AND 32768),
    image_height INTEGER CHECK(image_height BETWEEN 1 AND 32768),
    title TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    importance REAL NOT NULL DEFAULT 0.5 CHECK(importance >= 0 AND importance <= 1),
    revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
    privacy_epoch INTEGER,
    request_fingerprint TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK((image_width IS NULL) = (image_height IS NULL))
);
CREATE INDEX explicit_memory_marks_source ON explicit_memory_marks(source_frame_id);
-- Foreign-key cleanup must seek only marks referencing the expired frame.
CREATE INDEX explicit_memory_marks_frame ON explicit_memory_marks(frame_id);
CREATE INDEX explicit_memory_marks_timestamp ON explicit_memory_marks(source_timestamp);
CREATE INDEX explicit_memory_marks_time_value ON explicit_memory_marks(julianday(source_timestamp));
CREATE INDEX explicit_memory_marks_order ON explicit_memory_marks(created_at DESC, id DESC);
CREATE INDEX explicit_memory_marks_privacy_epoch ON explicit_memory_marks(privacy_epoch);

CREATE TABLE explicit_memory_anchors (
    id TEXT NOT NULL,
    mark_id TEXT NOT NULL REFERENCES explicit_memory_marks(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    anchor_json TEXT NOT NULL CHECK(json_valid(anchor_json)),
    PRIMARY KEY(mark_id,id),
    UNIQUE(mark_id,ordinal)
);
CREATE TABLE explicit_memory_mark_tags (
    mark_id TEXT NOT NULL REFERENCES explicit_memory_marks(id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY(mark_id,tag_id)
);
CREATE INDEX explicit_memory_mark_tags_tag ON explicit_memory_mark_tags(tag_id,mark_id);
-- Only tag names introduced by explicit memory are eligible for orphan cleanup.
CREATE TABLE explicit_memory_owned_tags (
    tag_id INTEGER PRIMARY KEY REFERENCES tags(id) ON DELETE CASCADE
);
CREATE TRIGGER explicit_memory_tag_cleanup AFTER DELETE ON explicit_memory_mark_tags BEGIN
    DELETE FROM tags WHERE id=OLD.tag_id
      AND EXISTS(SELECT 1 FROM explicit_memory_owned_tags WHERE tag_id=OLD.tag_id)
      AND NOT EXISTS(SELECT 1 FROM explicit_memory_mark_tags WHERE tag_id=OLD.tag_id)
      AND NOT EXISTS(SELECT 1 FROM vision_tags WHERE tag_id=OLD.tag_id)
      AND NOT EXISTS(SELECT 1 FROM audio_tags WHERE tag_id=OLD.tag_id);
END;

-- Bounded user-saved images share the configured database's storage boundary.
-- Their lifetime is transactional: no independently orphanable image files.
CREATE TABLE explicit_memory_assets (
    mark_id TEXT PRIMARY KEY REFERENCES explicit_memory_marks(id) ON DELETE CASCADE,
    bytes BLOB NOT NULL CHECK(length(bytes) BETWEEN 1 AND 8388608),
    mime_type TEXT NOT NULL CHECK(mime_type IN ('image/jpeg','image/png','image/webp')),
    width INTEGER NOT NULL CHECK(width BETWEEN 1 AND 32768),
    height INTEGER NOT NULL CHECK(height BETWEEN 1 AND 32768)
);
CREATE TABLE explicit_memory_search_docs (
    rowid INTEGER PRIMARY KEY,
    mark_id TEXT NOT NULL UNIQUE REFERENCES explicit_memory_marks(id) ON DELETE CASCADE,
    text TEXT NOT NULL
);
CREATE VIRTUAL TABLE explicit_memory_fts USING fts5(
    text,content='explicit_memory_search_docs',content_rowid='rowid',tokenize='unicode61'
);
CREATE TRIGGER explicit_memory_search_insert AFTER INSERT ON explicit_memory_search_docs BEGIN
    INSERT INTO explicit_memory_fts(rowid,text) VALUES(NEW.rowid,NEW.text);
END;
CREATE TRIGGER explicit_memory_search_delete AFTER DELETE ON explicit_memory_search_docs BEGIN
    INSERT INTO explicit_memory_fts(explicit_memory_fts,rowid,text) VALUES('delete',OLD.rowid,OLD.text);
END;
CREATE TRIGGER explicit_memory_search_update AFTER UPDATE OF text ON explicit_memory_search_docs BEGIN
    INSERT INTO explicit_memory_fts(explicit_memory_fts,rowid,text) VALUES('delete',OLD.rowid,OLD.text);
    INSERT INTO explicit_memory_fts(rowid,text) VALUES(NEW.rowid,NEW.text);
END;
