-- This file should undo anything in `up.sql`

DROP INDEX IF EXISTS idx_account_follows_account_did;
DROP TABLE IF EXISTS account_follows;
