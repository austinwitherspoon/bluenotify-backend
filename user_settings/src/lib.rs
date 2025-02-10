use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    string,
};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash, Copy)]
pub enum PostType {
    #[serde(rename = "post")]
    Post,
    #[serde(rename = "repost")]
    Repost,
    #[serde(rename = "reply")]
    Reply,
    #[serde(rename = "replyToFriend")]
    ReplyToFriend,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountSetting {
    pub did: String,
    #[serde(alias = "postTypes")]
    pub post_types: HashSet<PostType>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserSettings {
    #[serde(alias = "_firestore_id")]
    pub doc_id: Option<String>,

    pub accounts: HashSet<String>,
    #[serde(alias = "fcmToken")]
    pub fcm_token: String,

    pub settings: HashMap<String, AccountSetting>,
}

impl UserSettings {
    fn stringified(&self) -> String {
        let mut stringified = "".to_string();
        stringified.push_str(&self.fcm_token);
        let mut settings: Vec<AccountSetting> = self.settings.values().cloned().collect();
        settings.sort_by(|a, b| a.did.cmp(&b.did));
        for account_setting in settings.iter() {
            stringified.push_str(&account_setting.did);
            let mut post_types: Vec<String> = account_setting
                .post_types
                .iter()
                .map(|post_type| format!("{:?}", post_type))
                .collect();
            post_types.sort();
            for post_type in post_types.iter() {
                stringified.push_str(post_type);
            }
        }
        let mut accounts: Vec<String> = self.accounts.iter().cloned().collect();
        accounts.sort();
        for account in accounts.iter() {
            stringified.push_str(account);
        }
        stringified
    }
}

impl PartialEq<UserSettings> for UserSettings {
    fn eq(&self, other: &UserSettings) -> bool {
        self.stringified() == other.stringified()
    }
}

pub type UserSettingsMap = HashMap<String, UserSettings>;
pub struct AllUserSettings {
    pub settings: UserSettingsMap,
    pub fcm_tokens_by_watched: HashMap<String, HashSet<String>>,
}

impl AllUserSettings {
    pub fn new(settings: UserSettingsMap) -> Self {
        let mut result = Self {
            settings,
            fcm_tokens_by_watched: HashMap::new(),
        };
        result.rebuild_cache();
        result
    }

    pub fn rebuild_cache(&mut self) {
        let mut fcm_tokens_by_watched = HashMap::new();
        for (fcm_token, user_settings) in self.settings.iter() {
            for (did, _) in user_settings.settings.iter() {
                fcm_tokens_by_watched
                    .entry(did.clone())
                    .or_insert_with(HashSet::new)
                    .insert(fcm_token.clone());
            }
        }
        self.fcm_tokens_by_watched = fcm_tokens_by_watched;
    }
}
