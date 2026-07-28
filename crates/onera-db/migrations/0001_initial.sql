-- Onera initial schema.
--
-- Conventions:
--   * Identifiers are UUID text so rows can be created offline.
--   * Hashes are stored as `algorithm:hex` in a single column.
--   * Timestamps are RFC 3339 UTC text, which sorts correctly as text.
--   * Provider-specific identifiers live only in provider-scoped tables; the
--     deployment tables below never reference them.
--
-- See docs/database-schema.md for the narrative version.

CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- Records which adapter version last wrote a game's deployment state, so a
-- future adapter can detect state it does not understand.
CREATE TABLE adapter_versions (
    adapter_id TEXT PRIMARY KEY,
    version    INTEGER NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE providers (
    id         TEXT PRIMARY KEY,       -- slug, e.g. 'nexus'
    name       TEXT NOT NULL,
    api_base   TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

-- Accounts never store a credential. The secret lives only in the platform
-- secret store; this table records who the stored credential belongs to.
CREATE TABLE accounts (
    id               TEXT PRIMARY KEY,
    provider_id      TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    provider_user_id TEXT NOT NULL,
    username         TEXT NOT NULL,
    premium          INTEGER,
    created_at       TEXT NOT NULL,
    UNIQUE (provider_id, provider_user_id)
) STRICT;

CREATE TABLE games (
    id            TEXT PRIMARY KEY,
    provider_id   TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    provider_slug TEXT NOT NULL,
    name          TEXT NOT NULL,
    steam_app_id  INTEGER,
    cached_at     TEXT NOT NULL,
    UNIQUE (provider_id, provider_slug)
) STRICT;

CREATE TABLE local_game_installs (
    id              TEXT PRIMARY KEY,
    game_id         TEXT NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    adapter_id      TEXT NOT NULL,
    source          TEXT NOT NULL CHECK (source IN ('steam_native', 'steam_flatpak', 'manual')),
    install_root    TEXT NOT NULL UNIQUE,
    compat_prefix   TEXT,
    user_data_roots TEXT NOT NULL DEFAULT '[]',   -- JSON array
    confirmed       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL
) STRICT;

CREATE TABLE deploy_roots (
    id            TEXT PRIMARY KEY,
    local_game_id TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    root_key      TEXT NOT NULL,
    kind          TEXT NOT NULL,
    path          TEXT NOT NULL,
    UNIQUE (local_game_id, root_key)
) STRICT;

CREATE TABLE mods (
    id              TEXT PRIMARY KEY,
    provider_id     TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    provider_mod_id TEXT NOT NULL,
    game_slug       TEXT NOT NULL,
    name            TEXT NOT NULL,
    author          TEXT,
    updated_at      TEXT NOT NULL,
    UNIQUE (provider_id, game_slug, provider_mod_id)
) STRICT;

-- `version` is stored exactly as the provider reported it and is never parsed.
-- Ordering uses `published_at`; see docs/nexus-api-assumptions.md.
CREATE TABLE releases (
    id           TEXT PRIMARY KEY,
    mod_id       TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    version      TEXT NOT NULL,
    published_at TEXT,
    metadata     TEXT NOT NULL DEFAULT '{}',      -- provider-specific JSON
    UNIQUE (mod_id, version, published_at)
) STRICT;

CREATE TABLE provider_files (
    id               TEXT PRIMARY KEY,
    release_id       TEXT NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    provider_id      TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    provider_file_id TEXT NOT NULL,
    name             TEXT NOT NULL,
    size_bytes       INTEGER,
    category         TEXT NOT NULL,
    published_hash   TEXT,                        -- provider-supplied, advisory
    uploaded_at      TEXT,
    is_primary       INTEGER NOT NULL DEFAULT 0,
    UNIQUE (provider_id, provider_file_id)
) STRICT;

-- Content-addressed archive storage. `hash` is the BLAKE3 of the archive file
-- and is what the on-disk path is derived from; `original_filename` preserves
-- what the provider called it.
CREATE TABLE archives (
    id                TEXT PRIMARY KEY,
    hash              TEXT NOT NULL UNIQUE,
    size              INTEGER NOT NULL,
    original_filename TEXT NOT NULL,
    format            TEXT NOT NULL,
    stored_path       TEXT NOT NULL,
    created_at        TEXT NOT NULL
) STRICT;

CREATE TABLE archive_provider_files (
    archive_id       TEXT NOT NULL REFERENCES archives(id) ON DELETE CASCADE,
    provider_file_id TEXT NOT NULL REFERENCES provider_files(id) ON DELETE CASCADE,
    PRIMARY KEY (archive_id, provider_file_id)
) STRICT;

CREATE TABLE archive_entries (
    id         TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives(id) ON DELETE CASCADE,
    path       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    hash       TEXT NOT NULL,
    executable INTEGER NOT NULL DEFAULT 0,
    UNIQUE (archive_id, path)
) STRICT;

CREATE TABLE installations (
    id                TEXT PRIMARY KEY,
    local_game_id     TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    mod_id            TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    release_id        TEXT NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    archive_id        TEXT NOT NULL REFERENCES archives(id) ON DELETE CASCADE,
    state             TEXT NOT NULL DEFAULT 'installed',
    layout_rationale  TEXT,
    installed_at      TEXT NOT NULL
) STRICT;

CREATE INDEX installations_by_game_mod ON installations (local_game_id, mod_id);

-- A copy of a file Onera was about to overwrite. Unmanaged originals are backed
-- up here so `restore` can put the pre-Onera state back byte for byte.
CREATE TABLE backups (
    id            TEXT PRIMARY KEY,
    local_game_id TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    root_key      TEXT NOT NULL,
    rel_path      TEXT NOT NULL,
    hash          TEXT NOT NULL,
    size          INTEGER NOT NULL,
    stored_path   TEXT NOT NULL,
    created_at    TEXT NOT NULL
) STRICT;

-- One row per deployed relative path per game.
CREATE TABLE deployed_files (
    id            TEXT PRIMARY KEY,
    local_game_id TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    root_key      TEXT NOT NULL,
    rel_path      TEXT NOT NULL,
    current_hash  TEXT NOT NULL,
    size          INTEGER NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (local_game_id, root_key, rel_path)
) STRICT;

-- The file-provider stack: `position` 0 is the bottom (oldest provider), the
-- highest position is what is currently on disk. Removing the top row restores
-- the row beneath it. Exactly one of installation_id / backup_id is set.
CREATE TABLE deployed_file_providers (
    id               TEXT PRIMARY KEY,
    deployed_file_id TEXT NOT NULL REFERENCES deployed_files(id) ON DELETE CASCADE,
    position         INTEGER NOT NULL,
    provider_kind    TEXT NOT NULL CHECK (provider_kind IN ('installation', 'unmanaged')),
    installation_id  TEXT REFERENCES installations(id) ON DELETE CASCADE,
    backup_id        TEXT REFERENCES backups(id) ON DELETE SET NULL,
    hash             TEXT NOT NULL,
    size             INTEGER NOT NULL,
    recorded_at      TEXT NOT NULL,
    UNIQUE (deployed_file_id, position),
    CHECK (
        (provider_kind = 'installation' AND installation_id IS NOT NULL AND backup_id IS NULL)
        OR
        (provider_kind = 'unmanaged' AND backup_id IS NOT NULL AND installation_id IS NULL)
    )
) STRICT;

CREATE INDEX providers_by_installation ON deployed_file_providers (installation_id);

-- Which files an installation contributed, and from which archive entry.
CREATE TABLE installation_files (
    installation_id  TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    deployed_file_id TEXT NOT NULL REFERENCES deployed_files(id) ON DELETE CASCADE,
    source_path      TEXT NOT NULL,
    source_hash      TEXT NOT NULL,
    action           TEXT NOT NULL,
    PRIMARY KEY (installation_id, deployed_file_id)
) STRICT;

-- Directories Onera created while deploying. Removal only deletes directories
-- listed here, so a game's own empty directories survive uninstalling mods.
CREATE TABLE created_directories (
    id              TEXT PRIMARY KEY,
    local_game_id   TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    root_key        TEXT NOT NULL,
    rel_path        TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    UNIQUE (installation_id, root_key, rel_path)
) STRICT;

-- Append-only audit trail of every ownership change for a path.
CREATE TABLE file_provider_history (
    id               TEXT PRIMARY KEY,
    deployed_file_id TEXT NOT NULL REFERENCES deployed_files(id) ON DELETE CASCADE,
    operation_id     TEXT,
    event            TEXT NOT NULL,
    installation_id  TEXT,
    hash             TEXT,
    at               TEXT NOT NULL
) STRICT;

CREATE INDEX history_by_file ON file_provider_history (deployed_file_id, at);

CREATE TABLE operations (
    id            TEXT PRIMARY KEY,
    local_game_id TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('install', 'remove', 'repair')),
    state         TEXT NOT NULL CHECK (state IN (
                      'planned', 'prepared', 'committing',
                      'complete', 'rolling_back', 'rolled_back', 'failed')),
    plan          TEXT NOT NULL,       -- the full InstallPlan as JSON
    error         TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE INDEX operations_by_state ON operations (state);

-- One row per file in an operation, written before the file is touched and
-- updated after each atomic step. This is what makes recovery possible.
CREATE TABLE operation_files (
    id            TEXT PRIMARY KEY,
    operation_id  TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    seq           INTEGER NOT NULL,
    root_key      TEXT NOT NULL,
    rel_path      TEXT NOT NULL,
    abs_path      TEXT NOT NULL,
    source_hash   TEXT NOT NULL,
    previous_hash TEXT,
    backup_id     TEXT REFERENCES backups(id) ON DELETE SET NULL,
    temp_path     TEXT,
    status        TEXT NOT NULL CHECK (status IN
                      ('pending', 'staged', 'committed', 'skipped', 'rolled_back')),
    updated_at    TEXT NOT NULL,
    UNIQUE (operation_id, seq)
) STRICT;

CREATE TABLE conflicts (
    id           TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    root_key     TEXT NOT NULL,
    rel_path     TEXT NOT NULL,
    classification TEXT NOT NULL,
    choice       TEXT,
    scope        TEXT,
    decided_at   TEXT
) STRICT;

-- Deliberately narrow: a rule is always scoped to one mod, one deployment root
-- and one path prefix. There is no global "always replace".
CREATE TABLE scoped_rules (
    id          TEXT PRIMARY KEY,
    mod_id      TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    root_key    TEXT NOT NULL,
    path_prefix TEXT NOT NULL,
    choice      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (mod_id, root_key, path_prefix)
) STRICT;

CREATE TABLE download_jobs (
    id               TEXT PRIMARY KEY,
    provider_id      TEXT NOT NULL,
    provider_file_id TEXT NOT NULL,
    filename         TEXT NOT NULL,
    expected_size    INTEGER,
    expected_hash    TEXT,
    bytes_downloaded INTEGER NOT NULL DEFAULT 0,
    temp_path        TEXT,
    state            TEXT NOT NULL CHECK (state IN
                         ('queued', 'running', 'paused', 'complete', 'failed', 'cancelled')),
    attempts         INTEGER NOT NULL DEFAULT 0,
    error            TEXT,
    archive_id       TEXT REFERENCES archives(id) ON DELETE SET NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
) STRICT;

CREATE INDEX download_jobs_by_state ON download_jobs (state);

INSERT INTO schema_meta (key, value) VALUES ('schema_version', '1');
