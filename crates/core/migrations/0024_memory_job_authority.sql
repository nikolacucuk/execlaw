ALTER TABLE memory_jobs ADD COLUMN authority_scope TEXT;
ALTER TABLE memory_jobs ADD COLUMN authority_trust_class TEXT;

CREATE TRIGGER memory_extract_requires_authority_insert
BEFORE INSERT ON memory_jobs
WHEN NEW.kind = 'memory_extract'
BEGIN
    SELECT CASE WHEN NEW.authority_scope IS NULL
                     OR trim(NEW.authority_scope) = ''
                     OR NEW.authority_trust_class IS NULL
                     OR NEW.authority_trust_class NOT IN (
                         'Controller', 'Delegated', 'KnownTrusted',
                         'KnownLimited', 'UnknownPending', 'Blocked'
                     )
        THEN RAISE(ABORT, 'memory_extract job requires host authority') END;
END;
