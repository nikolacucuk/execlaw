-- 0031_chain_approval_effect_hash.sql
--
-- Approval of a chain run authorizes exactly the effectful plan steps that
-- were pending when the approval was requested. The canonical hash is checked
-- again before the run can transition from awaiting_approval to running.

ALTER TABLE state_chain_runs
    ADD COLUMN approval_effect_hash TEXT;
