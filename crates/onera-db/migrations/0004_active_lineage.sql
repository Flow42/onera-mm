-- Finish the desired-state invariant without changing the checksum of schema
-- v3, which development databases may already have applied.

-- Retain the newest artifact as active if an older database happened to contain
-- more than one installed release for the same mod lineage.
UPDATE installations
SET active = 0, state = 'artifact', deactivated_at = installed_at
WHERE id IN (
    SELECT older.id
    FROM installations older
    WHERE EXISTS (
        SELECT 1 FROM installations newer
        WHERE newer.local_game_id = older.local_game_id
          AND newer.mod_id = older.mod_id
          AND (newer.installed_at > older.installed_at
               OR (newer.installed_at = older.installed_at AND newer.id > older.id))
    )
);

DROP INDEX installations_active_by_game_mod;
CREATE UNIQUE INDEX installations_one_active_by_game_mod
    ON installations (local_game_id, mod_id) WHERE active = 1;

UPDATE schema_meta SET value = '4' WHERE key = 'schema_version';
