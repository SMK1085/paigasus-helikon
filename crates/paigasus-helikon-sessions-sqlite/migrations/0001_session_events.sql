CREATE TABLE session_events (
    session_id  TEXT    NOT NULL,
    sequence    INTEGER NOT NULL,
    ts_nanos    INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    PRIMARY KEY (session_id, sequence)
);

-- This index is not used by the SDK itself; it backs ad-hoc operator
-- queries against the audit log (e.g., "events in a wall-clock window"
-- via the sqlite shell). Drop it if your deployment never runs such
-- queries and the per-INSERT write cost matters.
CREATE INDEX idx_session_events_session_ts
    ON session_events (session_id, ts_nanos);
