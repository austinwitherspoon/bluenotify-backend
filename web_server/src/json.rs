use database_schema::diesel::prelude::*;
use database_schema::diesel_async::pooled_connection::deadpool::Object;
use database_schema::diesel_async::{AsyncPgConnection, RunQueryDsl};
use database_schema::models::{NotificationType, UserAccount, UserSetting};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserSettings {
    pub fcm_token: String,
    pub device_uuid: Option<String>, // <-- add this line
    pub accounts: Vec<Account>,
    pub notification_settings: Vec<NotificationSetting>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Account {
    pub account_did: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NotificationSetting {
    pub user_account_did: String,
    pub following_did: String,
    pub post_type: Vec<NotificationType>,
    pub word_allow_list: Option<Vec<String>>,
    pub word_block_list: Option<Vec<String>>,
}

impl UserSettings {
    pub async fn get(
        fcm_token: &str,
        mut conn: Object<AsyncPgConnection>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let user = database_schema::schema::users::table
            .filter(database_schema::schema::users::dsl::fcm_token.eq(fcm_token))
            .first::<database_schema::models::User>(&mut conn)
            .await?;

        let accounts = UserAccount::belonging_to(&user)
            .load::<UserAccount>(&mut conn)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|account| Account {
                account_did: account.account_did,
            })
            .collect::<Vec<_>>();

        let notification_settings = UserSetting::belonging_to(&user)
            .load::<UserSetting>(&mut conn)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|setting| NotificationSetting {
                user_account_did: setting.user_account_did,
                following_did: setting.following_did,
                post_type: setting.post_type,
                word_allow_list: setting.word_allow_list,
                word_block_list: setting.word_block_list,
            })
            .collect::<Vec<_>>();

        Ok(Self {
            fcm_token: fcm_token.to_string(),
            device_uuid: user.device_uuid,
            accounts,
            notification_settings,
        })
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SettingUpdateRequest {
    pub post_type: Vec<NotificationType>,
    pub word_allow_list: Option<Vec<String>>,
    pub word_block_list: Option<Vec<String>>,
}
