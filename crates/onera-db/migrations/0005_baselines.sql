-- Milestone 2: what "clean" means for one concrete installation.
--
-- Three rules shape this schema:
--
--   * A captured baseline and its file records are immutable. A recapture is a
--     new row that supersedes the old one, so history survives a game update
--     and a stale baseline can still be inspected.
--   * At most one baseline per installation is `current`. The partial unique
--     index is what enforces that, rather than application discipline.
--   * A scan run is progress, not a verdict. It records how far a scan got, how
--     thoroughly it looked, and why it stopped — including when it was
--     cancelled or failed, which must never be mistaken for a clean result.

CREATE TABLE game_baselines (
    id                TEXT PRIMARY KEY,
    local_game_id     TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    source            TEXT NOT NULL CHECK (source IN
                          ('store_verified_capture', 'local_snapshot', 'store_manifest')),
    -- The serialized `StoreBuildIdentity`, or NULL when the store exposed none.
    -- NULL is "we could not tell", never "nothing changed": the freshness rules
    -- in the domain turn it into `Unknown` rather than `Fresh`.
    build_identity    TEXT,
    adapter_id        TEXT NOT NULL,
    reported_version  TEXT,
    status            TEXT NOT NULL CHECK (status IN
                          ('capturing', 'current', 'superseded', 'failed')),
    captured_at       TEXT NOT NULL,
    scope_fingerprint TEXT NOT NULL,
    file_count        INTEGER NOT NULL,
    total_bytes       INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX game_baselines_one_current_per_game
    ON game_baselines (local_game_id) WHERE status = 'current';
CREATE INDEX game_baselines_by_game
    ON game_baselines (local_game_id, captured_at DESC, id DESC);

-- Rejects an in-place edit of a captured baseline. `status` is deliberately not
-- in the column list: superseding is a lifecycle change, not a rewrite of what
-- was observed.
CREATE TRIGGER game_baselines_are_immutable
BEFORE UPDATE OF id, local_game_id, source, build_identity, adapter_id,
                 reported_version, captured_at, scope_fingerprint,
                 file_count, total_bytes
ON game_baselines
BEGIN
    SELECT RAISE(ABORT,
        'a captured baseline is immutable; capture a new one to supersede it');
END;

CREATE TABLE baseline_files (
    id          TEXT PRIMARY KEY,
    baseline_id TEXT NOT NULL REFERENCES game_baselines(id) ON DELETE CASCADE,
    root_key    TEXT NOT NULL,
    rel_path    TEXT NOT NULL,
    hash        TEXT NOT NULL,
    size        INTEGER NOT NULL,
    -- Unix mode when the platform reported one. Recorded so a lost executable
    -- bit is visible; never an integrity decision on its own.
    mode        INTEGER,
    UNIQUE (baseline_id, root_key, rel_path)
) STRICT;

CREATE INDEX baseline_files_by_baseline
    ON baseline_files (baseline_id, root_key, rel_path);

CREATE TRIGGER baseline_files_are_immutable
BEFORE UPDATE ON baseline_files
BEGIN
    SELECT RAISE(ABORT, 'baseline file records are immutable');
END;

CREATE TABLE baseline_scan_runs (
    id             TEXT PRIMARY KEY,
    local_game_id  TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    -- The baseline being verified, or the one a capture ultimately produced.
    -- NULL while a capture has not decided yet.
    baseline_id    TEXT REFERENCES game_baselines(id) ON DELETE CASCADE,
    purpose        TEXT NOT NULL CHECK (purpose IN ('capture', 'verify', 'clean_restore')),
    state          TEXT NOT NULL CHECK (state IN
                       ('running', 'completed', 'cancelled', 'failed')),
    evidence       TEXT NOT NULL CHECK (evidence IN ('content_hashed', 'metadata_only')),
    started_at     TEXT NOT NULL,
    finished_at    TEXT,
    files_scanned  INTEGER NOT NULL DEFAULT 0,
    bytes_hashed   INTEGER NOT NULL DEFAULT 0,
    count_matching      INTEGER NOT NULL DEFAULT 0,
    count_modified      INTEGER NOT NULL DEFAULT 0,
    count_missing       INTEGER NOT NULL DEFAULT 0,
    count_extra_managed INTEGER NOT NULL DEFAULT 0,
    count_extra_unknown INTEGER NOT NULL DEFAULT 0,
    count_unreadable    INTEGER NOT NULL DEFAULT 0,
    count_special       INTEGER NOT NULL DEFAULT 0,
    error          TEXT,
    -- A run that has not finished cannot claim a terminal state, and a terminal
    -- run must say when it stopped.
    CHECK ((state = 'running') = (finished_at IS NULL))
) STRICT;

CREATE INDEX baseline_scan_runs_by_game
    ON baseline_scan_runs (local_game_id, started_at DESC, id DESC);
CREATE INDEX baseline_scan_runs_by_baseline
    ON baseline_scan_runs (baseline_id, started_at DESC, id DESC);

CREATE TABLE baseline_scan_findings (
    id             TEXT PRIMARY KEY,
    scan_run_id    TEXT NOT NULL REFERENCES baseline_scan_runs(id) ON DELETE CASCADE,
    -- Position in the scan's own deterministic ordering, so findings come back
    -- exactly as the scanner produced them.
    seq            INTEGER NOT NULL,
    root_key       TEXT NOT NULL,
    rel_path       TEXT NOT NULL,
    classification TEXT NOT NULL CHECK (classification IN
                       ('matching', 'modified', 'missing', 'extra_managed',
                        'extra_unknown', 'unreadable', 'special_file')),
    expected_hash  TEXT,
    observed_hash  TEXT,
    detail         TEXT,
    UNIQUE (scan_run_id, seq)
) STRICT;

CREATE INDEX baseline_scan_findings_by_run
    ON baseline_scan_findings (scan_run_id, root_key, rel_path);

UPDATE schema_meta SET value = '5' WHERE key = 'schema_version';
