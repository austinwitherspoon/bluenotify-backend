// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "post_notification_type"))]
    pub struct PostNotificationType;
}

diesel::table! {
    account_follows (account_did, follow_did) {
        account_did -> Text,
        follow_did -> Text,
    }
}

diesel::table! {
    accounts (account_did, user_id) {
        user_id -> Int4,
        account_did -> Text,
        created_at -> Timestamp,
        too_many_follows -> Bool,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PostNotificationType;

    notification_settings (user_id, user_account_did, following_did) {
        user_id -> Int4,
        user_account_did -> Text,
        following_did -> Text,
        post_type -> Array<PostNotificationType>,
        word_allow_list -> Nullable<Array<Text>>,
        word_block_list -> Nullable<Array<Text>>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    notifications (id) {
        id -> Int4,
        user_id -> Text,
        created_at -> Timestamp,
        title -> Text,
        body -> Text,
        url -> Text,
        image -> Nullable<Text>,
    }
}

diesel::table! {
    users (id) {
        id -> Int4,
        fcm_token -> Text,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        deleted_at -> Nullable<Timestamp>,
    }
}

diesel::joinable!(accounts -> users (user_id));
diesel::joinable!(notification_settings -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    account_follows,
    accounts,
    notification_settings,
    notifications,
    users,
);
