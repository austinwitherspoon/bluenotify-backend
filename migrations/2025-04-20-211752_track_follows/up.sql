CREATE TABLE account_follows (
    id SERIAL PRIMARY KEY,
    account_did TEXT NOT NULL,
    follow_did TEXT NOT NULL
);

CREATE INDEX idx_account_follows_account_did ON account_follows(account_did);
