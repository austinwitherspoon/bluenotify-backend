use diesel::{
    backend::Backend,
    deserialize::{FromSql, FromSqlRow},
    expression::AsExpression,
    serialize::{Output, ToSql},
    sql_types::Timestamp,
};
use serde::Serialize;

/// A wrapper around `chrono::NaiveDateTime` that can be serialized to a string by serde
#[derive(Debug, Clone, Copy, AsExpression, FromSqlRow, PartialEq, Eq)]
#[diesel(sql_type = Timestamp)]
pub struct SerializableTimestamp {
    timestamp: chrono::NaiveDateTime,
}

impl SerializableTimestamp {
    pub fn now() -> Self {
        let timestamp = chrono::Utc::now().naive_utc();
        SerializableTimestamp { timestamp }
    }
}

impl From<chrono::NaiveDateTime> for SerializableTimestamp {
    fn from(timestamp: chrono::NaiveDateTime) -> Self {
        SerializableTimestamp { timestamp }
    }
}
impl Serialize for SerializableTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.timestamp.to_string())
    }
}
impl<DB: Backend> ToSql<Timestamp, DB> for SerializableTimestamp
where
    chrono::NaiveDateTime: ToSql<Timestamp, DB>,
{
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, DB>) -> diesel::serialize::Result {
        self.timestamp.to_sql(out)
    }
}
impl<DB> FromSql<Timestamp, DB> for SerializableTimestamp
where
    DB: Backend,
    chrono::NaiveDateTime: FromSql<Timestamp, DB>,
{
    fn from_sql(bytes: DB::RawValue<'_>) -> diesel::deserialize::Result<Self> {
        let timestamp = chrono::NaiveDateTime::from_sql(bytes)?;
        Ok(SerializableTimestamp { timestamp })
    }
}
