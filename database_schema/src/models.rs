use diesel::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Queryable, Selectable, Serialize)]
#[diesel(table_name = crate::schema::notifications)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Notification {
    pub id: i32,
    pub user_id: String,
    pub is_read: bool,
    pub title: String,
    pub body: String,
    pub url: String,
    pub image: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::notifications)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewNotification {
    pub user_id: String,
    pub is_read: bool,
    pub title: String,
    pub body: String,
    pub url: String,
    pub image: Option<String>,
}
