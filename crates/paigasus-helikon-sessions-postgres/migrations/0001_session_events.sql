CREATE TABLE IF NOT EXISTS session_events (
    session_id TEXT   NOT NULL,
    sequence   BIGINT NOT NULL,
    ts_nanos   BIGINT NOT NULL,
    kind       TEXT   NOT NULL,
    payload    JSONB  NOT NULL,
    PRIMARY KEY (session_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_session_events_session_ts
    ON session_events (session_id, ts_nanos);
