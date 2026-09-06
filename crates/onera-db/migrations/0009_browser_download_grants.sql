-- The browser handing over a download the provider's website authorised.

-- The identifier the provider's download endpoint uses for a file, when it is
-- not the one Onera keys on. Nexus has two id spaces for one file version --
-- the global id every API call takes, and the game-scoped number in the site's
-- own URLs -- and a request arriving from a "Mod manager download" click names
-- the second. Recording it is what lets that request be matched to a known
-- file instead of being reported as a file that does not exist.
ALTER TABLE provider_files ADD COLUMN provider_download_id TEXT;

-- A permission the website minted for one file. It is not a credential: it
-- authorises a single download for a few minutes and is worthless afterwards,
-- which is why it may live here at all -- the API key never does. It is stored
-- because the handoff is durable: the short-lived native host writes the
-- request and a desktop process that may not even be running yet spends it.
--
-- An expired grant is left in place rather than cleaned up; the code that
-- spends one checks the timestamp and says what to do about it, and a row
-- carrying a dead nonce is not worth a background job.
ALTER TABLE inbox_requests ADD COLUMN download_key TEXT;
ALTER TABLE inbox_requests ADD COLUMN download_expires_at TEXT;

UPDATE schema_meta SET value = '9' WHERE key = 'schema_version';
