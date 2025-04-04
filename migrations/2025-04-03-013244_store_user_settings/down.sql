-- Down Migration

-- Drop indexes first, as they depend on the tables
DROP INDEX IF EXISTS idx_notification_settings_following_did;
DROP INDEX IF EXISTS idx_notification_settings_user_account_did;
DROP INDEX IF EXISTS idx_notification_settings_user_fcm_token;
DROP INDEX IF EXISTS idx_users_fcm_token;

-- Drop tables in reverse order of creation, respecting foreign key dependencies
-- notification_settings has FKs to users and accounts, so drop it first.
DROP TABLE IF EXISTS notification_settings;

-- accounts has an FK to users, so drop it before users.
DROP TABLE IF EXISTS accounts;

-- users is referenced by accounts and notification_settings (which are now dropped).
DROP TABLE IF EXISTS users;

-- Drop the custom type last, as notification_settings depended on it.
DROP TYPE IF EXISTS post_notification_type;