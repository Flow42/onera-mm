-- Milestone 6: a browser handoff the desktop can run unattended, and the
-- display metadata a mod list needs.

-- The provider's own artwork address. Only the address lives here; the bytes
-- are cached under $XDG_CACHE_HOME so the database stays small and a cleared
-- cache costs nothing but a re-fetch.
ALTER TABLE mods ADD COLUMN thumbnail_url TEXT;

-- The page the request came from. Reconstructing it from a slug and an id
-- would bake one provider's URL shape into Onera; recording what the extension
-- actually saw keeps "open on Nexus" honest for a mod whose page has moved.
ALTER TABLE inbox_requests ADD COLUMN page_url TEXT;

-- Which game the extension believed the request targeted, and whether the
-- desktop may act on it without asking first. A request that predates this
-- column is not auto-runnable: the user queued it under the old rules, where
-- nothing ran until they clicked.
ALTER TABLE inbox_requests ADD COLUMN local_game_id TEXT
    REFERENCES local_game_installs(id) ON DELETE SET NULL;
ALTER TABLE inbox_requests ADD COLUMN auto_run INTEGER NOT NULL DEFAULT 0
    CHECK (auto_run IN (0, 1));

-- When the desktop last picked this request up. The state column's CHECK
-- constraint cannot be widened in place, so a lease timestamp carries the
-- "someone is working on it" fact instead: a request whose lease has expired is
-- retried, and one that is being worked on right now is not started twice.
ALTER TABLE inbox_requests ADD COLUMN started_at TEXT;

UPDATE schema_meta SET value = '8' WHERE key = 'schema_version';
