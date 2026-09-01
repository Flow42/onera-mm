-- Milestone 0: persisted browser inbox and enough download context to resume a
-- job by resolving a fresh signed URL after restart.

ALTER TABLE download_jobs ADD COLUMN game_slug TEXT NOT NULL DEFAULT '';
ALTER TABLE download_jobs ADD COLUMN provider_mod_id TEXT NOT NULL DEFAULT '';

CREATE TABLE inbox_requests (
    id              TEXT PRIMARY KEY,
    request_kind    TEXT NOT NULL CHECK (request_kind IN
                        ('add_mod', 'download', 'download_and_install')),
    provider_id     TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    game_slug       TEXT NOT NULL,
    provider_mod_id TEXT NOT NULL,
    provider_file_id TEXT,
    state           TEXT NOT NULL CHECK (state IN
                        ('queued', 'waiting_for_user', 'complete', 'failed', 'dismissed')),
    error           TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;

CREATE INDEX inbox_requests_by_state ON inbox_requests (state, created_at);
CREATE INDEX releases_by_mod_published ON releases (mod_id, published_at);
CREATE INDEX installations_by_game_installed ON installations (local_game_id, installed_at);

UPDATE schema_meta SET value = '2' WHERE key = 'schema_version';
