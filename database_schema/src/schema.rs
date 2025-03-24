// @generated automatically by Diesel CLI.

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
