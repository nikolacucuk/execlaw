ALTER TABLE config_general
ADD COLUMN https_only_session_cookies INTEGER NOT NULL DEFAULT 1
CHECK (https_only_session_cookies IN (0, 1));
