CREATE TYPE post_notification_type AS ENUM (
    'post',
    'repost',
    'reply',
    'replyToFriend'
);

CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    fcm_token TEXT NOT NULL UNIQUE,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at TIMESTAMP DEFAULT NULL
);
CREATE INDEX idx_users_fcm_token ON users(fcm_token);

CREATE TABLE accounts (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    account_did TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,

    PRIMARY KEY (account_did, user_id)
);

CREATE TABLE notification_settings (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_account_did TEXT NOT NULL,
    following_did TEXT NOT NULL,
    post_type post_notification_type ARRAY NOT NULL check (array_position(post_type, null) is null),
    word_allow_list TEXT ARRAY check (array_position(word_allow_list, null) is null) DEFAULT NULL,
    word_block_list TEXT ARRAY check (array_position(word_block_list, null) is null) DEFAULT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,

    PRIMARY KEY (user_id, user_account_did, following_did),
    FOREIGN KEY (user_account_did, user_id) REFERENCES accounts(account_did, user_id) ON DELETE CASCADE
);

CREATE INDEX idx_notification_settings_user_id ON notification_settings(user_id);
CREATE INDEX idx_notification_settings_user_account_did ON notification_settings(user_account_did);
CREATE INDEX idx_notification_settings_following_did ON notification_settings(following_did);
