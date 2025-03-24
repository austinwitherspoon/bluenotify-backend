use diesel::prelude::*;
use serde::Serialize;
use crate::timestamp::SerializableTimestamp;

#[derive(Queryable, Selectable, Serialize, Debug)]
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
