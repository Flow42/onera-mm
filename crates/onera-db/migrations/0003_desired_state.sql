-- Milestone 1: retain acquired artifacts independently from whether they are
-- currently deployed, and retain their resolved mappings for safe reactivation.

ALTER TABLE provider_files ADD COLUMN provider_version_id TEXT;
ALTER TABLE provider_files ADD COLUMN provider_file_group_id TEXT;

ALTER TABLE installations ADD COLUMN active INTEGER NOT NULL DEFAULT 1
    CHECK (active IN (0, 1));
ALTER TABLE installations ADD COLUMN deactivated_at TEXT;

UPDATE installations SET active = 1 WHERE state = 'installed';

CREATE TABLE installation_mappings (
    id              TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL REFERENCES installations(id) ON DELETE CASCADE,
    root_key        TEXT NOT NULL,
    rel_path        TEXT NOT NULL,
    source_path     TEXT NOT NULL,
    source_hash     TEXT NOT NULL,
    source_size     INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    UNIQUE (installation_id, root_key, rel_path)
) STRICT;

CREATE INDEX installation_mappings_by_installation
    ON installation_mappings (installation_id, root_key, rel_path);
CREATE INDEX installations_active_by_game_mod
    ON installations (local_game_id, mod_id, active);

-- Older installations that recorded their deployed source can be reactivated
-- without asking the game adapter to rediscover layout. Some legacy rows have
-- no `installation_files` record; they remain valid artifacts but require a
-- fresh layout resolution before reactivation.
INSERT INTO installation_mappings
    (id, installation_id, root_key, rel_path, source_path, source_hash, source_size, created_at)
SELECT lower(hex(randomblob(16))), f.installation_id, d.root_key, d.rel_path,
       f.source_path, f.source_hash, d.size, i.installed_at
FROM installation_files f
JOIN deployed_files d ON d.id = f.deployed_file_id
JOIN installations i ON i.id = f.installation_id
ON CONFLICT(installation_id, root_key, rel_path) DO NOTHING;

-- SQLite cannot alter a CHECK constraint. Rebuild the two journal tables while
-- preserving all rows and their foreign-key relationship.
PRAGMA foreign_keys = OFF;

DROP INDEX operations_by_state;
ALTER TABLE operation_files RENAME TO operation_files_v2;
ALTER TABLE operations RENAME TO operations_v2;

CREATE TABLE operations (
    id            TEXT PRIMARY KEY,
    local_game_id TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('install', 'remove', 'repair', 'reconcile', 'clean_restore')),
    state         TEXT NOT NULL CHECK (state IN (
                      'planned', 'prepared', 'committing',
                      'complete', 'rolling_back', 'rolled_back', 'failed')),
    plan          TEXT NOT NULL,
    error         TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE INDEX operations_by_state ON operations (state);

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

INSERT INTO operations
    (id, local_game_id, kind, state, plan, error, created_at, updated_at)
SELECT id, local_game_id, kind, state, plan, error, created_at, updated_at
FROM operations_v2;

INSERT INTO operation_files
    (id, operation_id, seq, root_key, rel_path, abs_path, source_hash,
     previous_hash, backup_id, temp_path, status, updated_at)
SELECT id, operation_id, seq, root_key, rel_path, abs_path, source_hash,
       previous_hash, backup_id, temp_path, status, updated_at
FROM operation_files_v2;

DROP TABLE operation_files_v2;
DROP TABLE operations_v2;

PRAGMA foreign_keys = ON;

UPDATE schema_meta SET value = '3' WHERE key = 'schema_version';
