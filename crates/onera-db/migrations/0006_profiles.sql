-- Milestone 3: game-scoped profiles and their desired members.
-- This migration deliberately depends only on v1-v4 catalogue/deployment data.

CREATE TABLE profiles (
    id            TEXT PRIMARY KEY,
    local_game_id TEXT NOT NULL REFERENCES local_game_installs(id) ON DELETE CASCADE,
    name          TEXT NOT NULL CHECK (length(trim(name)) > 0),
    description   TEXT,
    is_active     INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1)),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX profiles_name_per_game
    ON profiles (local_game_id, name COLLATE NOCASE);
CREATE UNIQUE INDEX profiles_one_active_per_game
    ON profiles (local_game_id) WHERE is_active = 1;
CREATE INDEX profiles_deterministic_list
    ON profiles (local_game_id, is_active DESC, name COLLATE NOCASE, id);

CREATE TABLE profile_members (
    id                     TEXT PRIMARY KEY,
    profile_id             TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    mod_id                 TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    provider_id            TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    provider_mod_id        TEXT NOT NULL,
    provider_file_id       TEXT,
    provider_version_id    TEXT,
    provider_file_group_id TEXT,
    installation_id        TEXT REFERENCES installations(id) ON DELETE SET NULL,
    desired                TEXT NOT NULL CHECK (desired IN ('enabled', 'disabled')),
    pinned                 INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    pinned_at              TEXT,
    pin_reason             TEXT,
    priority               INTEGER NOT NULL,
    added_at               TEXT NOT NULL,
    UNIQUE (profile_id, mod_id),
    CHECK ((pinned = 0 AND pinned_at IS NULL AND pin_reason IS NULL)
        OR (pinned = 1 AND pinned_at IS NOT NULL))
) STRICT;

CREATE INDEX profile_members_priority
    ON profile_members (profile_id, priority, id);
CREATE INDEX profile_members_installation
    ON profile_members (installation_id) WHERE installation_id IS NOT NULL;

-- Cross-row ownership checks need triggers: a member's retained artifact must
-- belong to both the profile's concrete game and the same mod lineage.
CREATE TRIGGER profile_members_installation_scope_insert
BEFORE INSERT ON profile_members
WHEN NEW.installation_id IS NOT NULL
 AND NOT EXISTS (
     SELECT 1 FROM installations i JOIN profiles p ON p.id = NEW.profile_id
     WHERE i.id = NEW.installation_id
       AND i.local_game_id = p.local_game_id AND i.mod_id = NEW.mod_id
 )
BEGIN
    SELECT RAISE(ABORT, 'profile member installation belongs to another game or mod');
END;

CREATE TRIGGER profile_members_installation_scope_update
BEFORE UPDATE OF profile_id, mod_id, installation_id ON profile_members
WHEN NEW.installation_id IS NOT NULL
 AND NOT EXISTS (
     SELECT 1 FROM installations i JOIN profiles p ON p.id = NEW.profile_id
     WHERE i.id = NEW.installation_id
       AND i.local_game_id = p.local_game_id AND i.mod_id = NEW.mod_id
 )
BEGIN
    SELECT RAISE(ABORT, 'profile member installation belongs to another game or mod');
END;

CREATE TABLE profile_activation_history (
    id              TEXT PRIMARY KEY,
    from_profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL,
    to_profile_id   TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    operation_id    TEXT REFERENCES operations(id) ON DELETE SET NULL,
    state           TEXT NOT NULL CHECK (state IN
                        ('preparing', 'applying', 'applied', 'rolled_back', 'failed')),
    started_at      TEXT NOT NULL,
    finished_at     TEXT,
    error           TEXT,
    UNIQUE (to_profile_id, started_at)
) STRICT;

CREATE INDEX profile_activation_history_target
    ON profile_activation_history (to_profile_id, started_at DESC, id DESC);
CREATE INDEX profile_activation_history_operation
    ON profile_activation_history (operation_id) WHERE operation_id IS NOT NULL;

-- Existing rules remain legacy rows. New profile-aware conflict rules have a
-- profile in their uniqueness scope and therefore cannot leak between sets.
ALTER TABLE scoped_rules RENAME TO scoped_rules_v5;

CREATE TABLE scoped_rules (
    id          TEXT PRIMARY KEY,
    profile_id  TEXT REFERENCES profiles(id) ON DELETE CASCADE,
    mod_id      TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    root_key    TEXT NOT NULL,
    path_prefix TEXT NOT NULL,
    choice      TEXT NOT NULL,
    created_at  TEXT NOT NULL
) STRICT;

INSERT INTO scoped_rules (id, profile_id, mod_id, root_key, path_prefix, choice, created_at)
SELECT id, NULL, mod_id, root_key, path_prefix, choice, created_at FROM scoped_rules_v5;
DROP TABLE scoped_rules_v5;

CREATE UNIQUE INDEX scoped_rules_legacy_scope
    ON scoped_rules (mod_id, root_key, path_prefix) WHERE profile_id IS NULL;
CREATE UNIQUE INDEX scoped_rules_profile_scope
    ON scoped_rules (profile_id, mod_id, root_key, path_prefix)
    WHERE profile_id IS NOT NULL;

-- Backfill exactly one active Default for every already-registered local game.
INSERT INTO profiles
    (id, local_game_id, name, description, is_active, created_at, updated_at)
SELECT lower(hex(randomblob(16))), l.id, 'Default', NULL, 1, l.created_at, l.created_at
FROM local_game_installs l
WHERE l.confirmed = 1
  AND NOT EXISTS (SELECT 1 FROM profiles p WHERE p.local_game_id = l.id);

-- Only active installations enter desired state. Opaque file/version/group
-- identifiers are retained when an archive-to-provider-file link exists.
INSERT INTO profile_members
    (id, profile_id, mod_id, provider_id, provider_mod_id,
     provider_file_id, provider_version_id, provider_file_group_id,
     installation_id, desired, pinned, pinned_at, pin_reason, priority, added_at)
SELECT lower(hex(randomblob(16))), ranked.profile_id, ranked.mod_id,
       ranked.provider_id, ranked.provider_mod_id, ranked.provider_file_id,
       ranked.provider_version_id, ranked.provider_file_group_id,
       ranked.installation_id, 'enabled', 0, NULL, NULL,
       ranked.priority, ranked.installed_at
FROM (
    SELECT p.id AS profile_id, i.id AS installation_id, i.mod_id,
           m.provider_id, m.provider_mod_id, i.installed_at,
           chosen.provider_file_id, chosen.provider_version_id,
           chosen.provider_file_group_id,
           10 * row_number() OVER (
               PARTITION BY p.id ORDER BY i.installed_at, i.id
           ) AS priority
    FROM profiles p
    JOIN installations i
      ON i.local_game_id = p.local_game_id AND i.active = 1
    JOIN mods m ON m.id = i.mod_id
    LEFT JOIN provider_files chosen ON chosen.id = (
        SELECT pf.id
        FROM archive_provider_files apf
        JOIN provider_files pf ON pf.id = apf.provider_file_id
        WHERE apf.archive_id = i.archive_id
          AND pf.release_id = i.release_id AND pf.provider_id = m.provider_id
        ORDER BY pf.provider_file_id, pf.id LIMIT 1
    )
    WHERE p.is_active = 1
) AS ranked;

UPDATE schema_meta SET value = '6' WHERE key = 'schema_version';
