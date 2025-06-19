use std::fmt::Display;

use crate::timestamp::SerializableTimestamp;
use diesel::{prelude::*, sql_types::Nullable};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, diesel_derive_enum::DbEnum, PartialEq, Eq)]
#[db_enum(
    existing_type_path = "crate::schema::sql_types::PostNotificationType",
    value_style = "camelCase"
)]
pub enum NotificationType {
    Post,
    Repost,
    Reply,
    ReplyToFriend,
}

impl NotificationType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "post" => Some(NotificationType::Post),
            "repost" => Some(NotificationType::Repost),
            "reply" => Some(NotificationType::Reply),
            "replyToFriend" => Some(NotificationType::ReplyToFriend),
            _ => None,
        }
    }
}

impl Display for NotificationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotificationType::Post => write!(f, "post"),
            NotificationType::Repost => write!(f, "repost"),
            NotificationType::Reply => write!(f, "reply"),
            NotificationType::ReplyToFriend => write!(f, "replyToFriend"),
        }
    }
}

#[derive(Insertable, Debug, Queryable, Identifiable, Selectable, Clone, PartialEq, Eq, Serialize)]
#[diesel(table_name = crate::schema::notifications)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Notification {
    pub id: i32,
    pub user_id: String,
    pub created_at: SerializableTimestamp,
    pub title: String,
    pub body: String,
    pub url: String,
    pub image: Option<String>,
}

#[derive(Insertable, Debug)]
#[diesel(table_name = crate::schema::notifications)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewNotification {
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub image: Option<String>,
}

#[derive(Insertable, Debug, Queryable, Identifiable, Selectable, Clone, PartialEq, Eq, Serialize)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    pub id: i32,
    pub fcm_token: String,
    pub created_at: SerializableTimestamp,
    pub updated_at: SerializableTimestamp,
    pub deleted_at: Option<SerializableTimestamp>,
    pub device_uuid: Option<String>,
    pub last_token_refresh: Option<SerializableTimestamp>,
    pub last_interaction: Option<SerializableTimestamp>,
}

#[derive(Insertable, Debug)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewUser {
    pub fcm_token: String,
    pub created_at: SerializableTimestamp,
    pub updated_at: SerializableTimestamp,
}


#[derive(Insertable, Debug, Queryable, Identifiable, Associations, Selectable, Clone, PartialEq, Eq)]
#[diesel(belongs_to(User))]
#[diesel(primary_key(user_id, account_did))]
#[diesel(table_name = crate::schema::accounts)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct UserAccount {
    pub user_id: i32,
    pub account_did: String,
    pub created_at: SerializableTimestamp,
    pub too_many_follows: bool,
}


#[derive(Insertable, Debug, Queryable, Identifiable, Associations, Selectable, Clone, PartialEq, Eq)]
#[diesel(belongs_to(User))]
#[diesel(primary_key(user_id, user_account_did, following_did))]
#[diesel(table_name = crate::schema::notification_settings)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct UserSetting {
    pub user_id: i32,
    pub user_account_did: String,
    pub following_did: String,
    pub post_type: Vec<NotificationType>,
    pub word_allow_list: Option<Vec<String>>,
    pub word_block_list: Option<Vec<String>>,
    pub created_at: SerializableTimestamp,
}


#[derive(Insertable, Debug, Queryable, Identifiable, Selectable, Clone, PartialEq, Eq)]
#[diesel(primary_key(account_did, follow_did))]
#[diesel(table_name = crate::schema::account_follows)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AccountFollow {
    pub account_did: String,
    pub follow_did: String,
}
