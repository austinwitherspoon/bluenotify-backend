ALTER TABLE account_follows DROP COLUMN id;

ALTER TABLE account_follows ADD PRIMARY KEY (account_did, follow_did);

ALTER TABLE accounts ADD COLUMN too_many_follows BOOLEAN NOT NULL DEFAULT FALSE;
