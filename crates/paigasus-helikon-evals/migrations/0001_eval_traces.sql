CREATE TABLE eval_runs (
    run_id           TEXT PRIMARY KEY,
    dataset          TEXT NOT NULL,
    started_ts_nanos INTEGER NOT NULL
);

CREATE TABLE eval_cases (
    run_id       TEXT NOT NULL,
    case_id      TEXT NOT NULL,
    final_output TEXT NOT NULL,
    error        TEXT,
    scores       TEXT NOT NULL, -- JSON: [{evaluator, score:{value, outcome, detail}}]
    PRIMARY KEY (run_id, case_id)
);

CREATE TABLE eval_events (
    run_id   TEXT NOT NULL,
    case_id  TEXT NOT NULL,
    seq      INTEGER NOT NULL,
    kind     TEXT NOT NULL,
    ts_nanos INTEGER NOT NULL,
    payload  TEXT NOT NULL, -- SessionEvent JSON
    PRIMARY KEY (run_id, case_id, seq)
);
