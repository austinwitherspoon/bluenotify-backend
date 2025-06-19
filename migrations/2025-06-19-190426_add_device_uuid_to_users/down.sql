DROP INDEX IF EXISTS idx_users_device_uuid;

ALTER TABLE users DROP COLUMN device_uuid;
ALTER TABLE users DROP COLUMN last_token_refresh;
ALTER TABLE users DROP COLUMN last_interaction;
