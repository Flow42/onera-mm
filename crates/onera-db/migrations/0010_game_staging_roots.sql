-- Where one game's archives are extracted while an install is in flight.
--
-- Staging is a working area, not a store: an entry in it belongs to an
-- operation that has not finished, and the whole root is swept on startup.
-- Onera keeps one under $XDG_STATE_HOME by default, and this table records the
-- exception -- a user who put a game on a different disk and wants the
-- extraction to happen there rather than across a filesystem boundary.
--
-- A separate table rather than a column on local_game_installs: the install row
-- describes what the game *is*, as detected, and this is a preference about how
-- Onera works on it. A game with no row here uses the default, which is also
-- what removing the row means.
CREATE TABLE game_staging_roots (
    local_game_id TEXT PRIMARY KEY
                  REFERENCES local_game_installs(id) ON DELETE CASCADE,
    path          TEXT NOT NULL,
    created_at    TEXT NOT NULL
) STRICT;

UPDATE schema_meta SET value = '10' WHERE key = 'schema_version';
