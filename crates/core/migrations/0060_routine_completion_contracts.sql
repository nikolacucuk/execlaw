-- Freeze operator-authored completion requirements at routine-fire creation.
ALTER TABLE config_routines ADD COLUMN completion_contract_json TEXT;
ALTER TABLE state_routine_runs ADD COLUMN completion_contract_json TEXT;
