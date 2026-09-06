-- Where downloaded archives are kept.
--
-- Onera's own directory under $XDG_DATA_HOME is the default. A user with a
-- small system disk and a large mod collection wants that somewhere else, and
-- someone who keeps one game's mods on the drive that game lives on wants it
-- per game -- so there are two levels of override, and the more specific one
-- wins.
--
-- Unlike a staging root, a download directory is never swept: it holds finished
-- archives that the database points at by absolute path. That is why an
-- existing directory with other things in it is perfectly acceptable here, and
-- why moving one has to rewrite `archives.stored_path` rather than just change
-- a setting.

-- Application-wide directories, keyed by what they are for. One table rather
-- than a column per directory: the next one to become configurable is a row,
-- not a migration that rewrites a settings table.
CREATE TABLE app_directories (
    kind       TEXT PRIMARY KEY CHECK (kind IN ('downloads')),
    path       TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

-- One game's override. No row means "use whatever the application-wide setting
-- says", which is also what deleting the row restores.
CREATE TABLE game_download_roots (
    local_game_id TEXT PRIMARY KEY
                  REFERENCES local_game_installs(id) ON DELETE CASCADE,
    path          TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

UPDATE schema_meta SET value = '11' WHERE key = 'schema_version';
