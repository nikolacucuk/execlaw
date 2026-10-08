-- Advance an audience fence whenever execlaw observes a group roster change.
ALTER TABLE state_principal_groups
    ADD COLUMN membership_epoch INTEGER NOT NULL DEFAULT 0;

