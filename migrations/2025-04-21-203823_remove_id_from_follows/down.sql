-- This file should undo anything in `up.sql`

ALTER TABLE account_follows DROP CONSTRAINT account_follows_pkey;

ALTER TABLE account_follows ADD COLUMN id SERIAL PRIMARY KEY;

ALTER TABLE accounts DROP COLUMN too_many_follows;
