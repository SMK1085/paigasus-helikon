CREATE TABLE session_events (
    session_id  TEXT    NOT NULL,
    sequence    INTEGER NOT NULL,
    ts_nanos    INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    PRIMARY KEY (session_id, sequence)
);

CREATE INDEX idx_session_events_session_ts
    ON session_events (session_id, ts_nanos);
