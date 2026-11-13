-- Projects, artifact provenance, file change ledger, and dependencies.
--
-- Generalizes Onera from a game-specific mod manager into a universal
-- workspace folder project and file provenance engine.

CREATE TABLE projects (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    root_path           TEXT NOT NULL,
    kind                TEXT NOT NULL,
    kind_metadata       TEXT,
    deployment_strategy TEXT NOT NULL DEFAULT 'symlink'
                        CHECK (deployment_strategy IN ('symlink', 'hardlink', 'reflink', 'copy')),
    staging_root        TEXT,
    created_at          TEXT NOT NULL
) STRICT;

CREATE TABLE provenance_uris (
    id             TEXT PRIMARY KEY,
    artifact_id    TEXT NOT NULL,
    uri            TEXT NOT NULL,
    provider_id    TEXT,
    referrer_url   TEXT,
    etag           TEXT,
    content_hash   TEXT NOT NULL,
    downloaded_at  TEXT NOT NULL
) STRICT;

CREATE TABLE file_change_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path           TEXT NOT NULL,
    event_type     TEXT NOT NULL CHECK (event_type IN ('created', 'modified', 'deleted', 'renamed')),
    source         TEXT NOT NULL CHECK (source IN ('managed', 'external_drift')),
    previous_hash  TEXT,
    new_hash       TEXT,
    observed_at    TEXT NOT NULL
) STRICT;

CREATE TABLE artifact_dependencies (
    artifact_id         TEXT NOT NULL,
    depends_on_artifact TEXT NOT NULL,
    version_constraint  TEXT,
    kind                TEXT NOT NULL DEFAULT 'required'
                        CHECK (kind IN ('required', 'recommended', 'conflicts')),
    PRIMARY KEY (artifact_id, depends_on_artifact)
) STRICT;

-- Populate projects from existing local_game_installs
INSERT INTO projects (id, name, root_path, kind, kind_metadata, deployment_strategy, staging_root, created_at)
SELECT
    l.id,
    COALESCE(g.name, l.adapter_id),
    l.install_root,
    'game',
    printf('{"game_id":"%s","local_game_id":"%s","adapter_id":"%s"}', l.game_id, l.id, l.adapter_id),
    'symlink',
    s.path,
    datetime('now')
FROM local_game_installs l
LEFT JOIN games g ON g.id = l.game_id
LEFT JOIN game_staging_roots s ON s.local_game_id = l.id;

UPDATE schema_meta SET value = '12' WHERE key = 'schema_version';
