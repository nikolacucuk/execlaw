-- 0011_enable_skills_learning_loop_defaults.sql
--
-- Turn on the closed learning loop by default for existing installs.
-- New installs already have a singleton row in config_skills; this
-- migration flips the two enable flags in place.

UPDATE config_skills
SET auto_capture_enabled = 1,
    reuse_update_enabled = 1,
    updated_at = strftime('%s','now') * 1000
WHERE id = 1;
