-- Milestone 4: provider dependency snapshots, accepted-risk overrides, and
-- complete provider candidate identity.

-- These values are provider-owned and opaque. Existing catalogue rows stay
-- explicitly unresolved rather than deriving identity or ordering from names.
ALTER TABLE provider_files ADD COLUMN provider_position INTEGER;

CREATE TABLE dependency_snapshots (
    id                         TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    provider_id                TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    game_slug                  TEXT NOT NULL CHECK (length(trim(game_slug)) > 0),
    provider_mod_id            TEXT NOT NULL CHECK (length(trim(provider_mod_id)) > 0),
    provider_file_id           TEXT CHECK (provider_file_id IS NULL OR length(provider_file_id) > 0),
    provider_version_id        TEXT CHECK (provider_version_id IS NULL OR length(provider_version_id) > 0),
    availability_json          TEXT NOT NULL
        CHECK (json_valid(availability_json) AND json_type(availability_json) = 'object'),
    groups_json                TEXT NOT NULL
        CHECK (json_valid(groups_json) AND json_type(groups_json) = 'array'),
    dlc_json                   TEXT NOT NULL
        CHECK (json_valid(dlc_json) AND json_type(dlc_json) = 'array'),
    provider_revision          TEXT,
    fingerprint                TEXT NOT NULL
        CHECK (length(fingerprint) = 64 AND fingerprint NOT GLOB '*[^0-9a-f]*'),
    fetched_at                 TEXT NOT NULL CHECK (length(trim(fetched_at)) > 0),
    raw_json                   TEXT NOT NULL CHECK (json_valid(raw_json))
) STRICT;

-- SQLite considers NULL values distinct in ordinary UNIQUE constraints. The
-- empty-string sentinel is safe because present source identifiers are checked
-- non-empty above, giving each exact optional source identity one latest row.
CREATE UNIQUE INDEX dependency_snapshots_source
    ON dependency_snapshots (
        provider_id, game_slug, provider_mod_id,
        ifnull(provider_file_id, ''), ifnull(provider_version_id, '')
    );
CREATE INDEX dependency_snapshots_lookup
    ON dependency_snapshots (
        provider_id, game_slug, provider_mod_id,
        provider_file_id, provider_version_id
    );

CREATE TABLE dependency_overrides (
    profile_member_id TEXT NOT NULL
        REFERENCES profile_members(id) ON DELETE CASCADE,
    group_id          TEXT NOT NULL CHECK (length(trim(group_id)) > 0),
    fingerprint       TEXT NOT NULL
        CHECK (length(fingerprint) = 64 AND fingerprint NOT GLOB '*[^0-9a-f]*'),
    reason            TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    created_at        TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
    PRIMARY KEY (profile_member_id, group_id)
) STRICT;

CREATE INDEX dependency_overrides_member_fingerprint
    ON dependency_overrides (profile_member_id, fingerprint, group_id);

UPDATE schema_meta SET value = '7' WHERE key = 'schema_version';
