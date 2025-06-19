ALTER TABLE users ADD COLUMN device_uuid TEXT UNIQUE;
ALTER TABLE users ADD COLUMN last_token_refresh TIMESTAMP;
ALTER TABLE users ADD COLUMN last_interaction TIMESTAMP;

CREATE INDEX idx_users_device_uuid ON users(device_uuid);
