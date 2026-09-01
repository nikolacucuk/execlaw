-- Migration 0014: operator-configurable default TTL for signed download URLs.
--
-- Defaults to 300 s (5 min) to match the hard-coded value previously used
-- in download_urls.rs::DEFAULT_TTL_SECS. Operator updates via
-- PATCH /api/admin/config.
ALTER TABLE config_general ADD COLUMN download_url_ttl_secs INTEGER NOT NULL DEFAULT 300;
