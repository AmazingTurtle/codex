CREATE TABLE account_limit_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    captured_at INTEGER NOT NULL,
    account_id TEXT NOT NULL,
    limit_id TEXT NOT NULL,
    window_seconds INTEGER NOT NULL DEFAULT -1,
    used_percent REAL,
    resets_at INTEGER,
    source TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_account_limit_observations_identity
    ON account_limit_observations(
        captured_at,
        account_id,
        limit_id,
        window_seconds,
        COALESCE(resets_at, -1),
        source
    );

CREATE INDEX idx_account_limit_observations_captured_at
    ON account_limit_observations(captured_at, account_id, limit_id, id);

CREATE TABLE usage_reset_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    consumed_at INTEGER NOT NULL,
    account_id TEXT NOT NULL,
    idempotency_key TEXT,
    limit_id TEXT
);

CREATE UNIQUE INDEX idx_usage_reset_events_idempotency_key
    ON usage_reset_events(account_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE INDEX idx_usage_reset_events_consumed_at
    ON usage_reset_events(consumed_at, account_id, id);

CREATE TABLE account_telemetry_poll_schedule (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_poll_at INTEGER NOT NULL
);
