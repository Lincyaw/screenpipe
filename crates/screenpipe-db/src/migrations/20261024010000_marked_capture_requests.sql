-- screenpipe — AI that knows everything you've seen, said, or heard
-- https://screenpipe.com

-- User-request journal only. Ambient capture has no new insert/update work.
CREATE TABLE marked_capture_requests (
    request_id TEXT PRIMARY KEY,
    selector INTEGER,
    monitor_id INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','completed','failed','blocked','cancelled','interrupted')),
    code TEXT,
    frame_id INTEGER REFERENCES frames(id) ON DELETE SET NULL,
    timestamp TEXT,
    app_name TEXT,
    image_width INTEGER,
    image_height INTEGER,
    privacy_epoch INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    CHECK((image_width IS NULL) = (image_height IS NULL))
);
CREATE INDEX marked_capture_requests_state_expiry ON marked_capture_requests(state,expires_at);
CREATE INDEX marked_capture_requests_expiry ON marked_capture_requests(expires_at,request_id);
CREATE INDEX marked_capture_requests_frame ON marked_capture_requests(frame_id);

-- A source deletion invalidates this short-lived receipt's copied metadata,
-- without touching any independent authored mark or its retention choice.
CREATE TRIGGER marked_capture_source_deleted BEFORE DELETE ON frames BEGIN
    UPDATE marked_capture_requests SET frame_id=NULL,timestamp=NULL,app_name=NULL,
        image_width=NULL,image_height=NULL,code='source_unavailable'
        WHERE frame_id=OLD.id;
END;
