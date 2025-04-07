use axum::{routing::get, Router};
use core::panic;
use firestore::*;
use futures::StreamExt;
use lazy_static::lazy_static;
use prometheus::{self, register_int_gauge, Encoder, IntGauge, TextEncoder};
use std::vec;
use tracing::{debug, error, info};

use database_schema::diesel_async::RunQueryDsl;
use database_schema::{
    diesel::{self, prelude::*},
    get_pool, run_migrations, DBPool,
};
use user_settings::UserSettings;

lazy_static! {
    static ref SUBSCRIBED_USERS_COUNTER: IntGauge = register_int_gauge!(
        "subscribed_users",
        "The number of users subscribed to bluenotify notifications"
    )
    .unwrap();
    static ref WATCHED_USERS: IntGauge = register_int_gauge!(
        "watched_users",
        "The number of bluesky users watched by the server for updates"
    )
    .unwrap();
}

async fn send_user_settings_to_db(
    db_pool: &DBPool,
    kv_store: &async_nats::jetstream::kv::Store,
    settings: &UserSettings,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = db_pool.get().await?;
    let now = database_schema::timestamp::SerializableTimestamp::now();

    let existing_user = database_schema::schema::users::table
        .filter(database_schema::schema::users::dsl::fcm_token.eq(settings.fcm_token.clone()))
        .first::<database_schema::models::User>(&mut conn)
        .await
        .ok();

    let user = match existing_user {
        Some(user) => {
            // update updated_at timestamp and set deleted_at to null
            diesel::update(database_schema::schema::users::table.filter(
                database_schema::schema::users::dsl::fcm_token.eq(settings.fcm_token.clone()),
            ))
            .set((
                database_schema::schema::users::dsl::updated_at.eq(now),
                database_schema::schema::users::dsl::deleted_at.eq(None::<chrono::NaiveDateTime>),
            ))
            .execute(&mut conn)
            .await?;
            user
        }
        None => {
            // if the user does not exist, create it
            let new_user = database_schema::models::NewUser {
                fcm_token: settings.fcm_token.clone(),
                created_at: now,
                updated_at: now,
            };
            diesel::insert_into(database_schema::schema::users::table)
                .values(&new_user)
                .get_result::<database_schema::models::User>(&mut conn)
                .await?
        }
    };

    // remove existing accounts for the user
    diesel::delete(
        database_schema::schema::accounts::table
            .filter(database_schema::schema::accounts::dsl::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await?;

    // create accounts
    let accounts = settings
        .accounts
        .iter()
        .map(|account| database_schema::models::NewUserAccount {
            user_id: user.id,
            account_did: account.clone(),
            created_at: now,
        })
        .collect::<Vec<_>>();
    diesel::insert_into(database_schema::schema::accounts::table)
        .values(&accounts)
        .execute(&mut conn)
        .await?;

    // remove existing notification settings for the user
    diesel::delete(
        database_schema::schema::notification_settings::table
            .filter(database_schema::schema::notification_settings::dsl::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await?;

    // create notification settings
    // for now, make a copy of the settings for every account
    let mut notification_settings = vec![];

    for account in settings.accounts.iter() {
        for (following_did, single_setting) in settings.settings.iter() {
            let new_settings = database_schema::models::UserSetting {
                user_id: user.id,
                user_account_did: account.clone(),
                following_did: following_did.clone(),
                post_type: single_setting
                    .post_types
                    .iter()
                    .map(|s| match s {
                        user_settings::PostType::Post => {
                            database_schema::models::NotificationType::Post
                        }
                        user_settings::PostType::Repost => {
                            database_schema::models::NotificationType::Repost
                        }
                        user_settings::PostType::Reply => {
                            database_schema::models::NotificationType::Reply
                        }
                        user_settings::PostType::ReplyToFriend => {
                            database_schema::models::NotificationType::ReplyToFriend
                        }
                    })
                    .collect(),
                word_allow_list: None,
                word_block_list: None,
                created_at: now,
            };
            notification_settings.push(new_settings);
        }
    }

    diesel::insert_into(database_schema::schema::notification_settings::table)
        .values(&notification_settings)
        .execute(&mut conn)
        .await?;

    update_watched_users(&db_pool, &kv_store).await?;

    Ok(())
}

async fn remove_user_settings_from_db(
    db_pool: &DBPool,
    kv_store: &async_nats::jetstream::kv::Store,
    id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = db_pool.get().await?;
    diesel::update(
        database_schema::schema::users::table
            .filter(database_schema::schema::users::dsl::fcm_token.eq(id)),
    )
    .set(database_schema::schema::users::dsl::deleted_at.eq(chrono::Utc::now().naive_utc()))
    .execute(&mut conn)
    .await?;
    update_watched_users(&db_pool, &kv_store).await?;
    Ok(())
}

async fn update_watched_users(
    db_pool: &DBPool,
    kv_store: &async_nats::jetstream::kv::Store,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = db_pool.get().await?;
    let users_count = database_schema::schema::users::table
        .filter(database_schema::schema::users::dsl::deleted_at.is_null())
        .count()
        .get_result::<i64>(&mut conn)
        .await?;
    SUBSCRIBED_USERS_COUNTER.set(users_count);

    //SELECT DISTINCT ON (following_did) * FROM notification_settings ORDER BY following_did
    let watched_users = database_schema::schema::notification_settings::table
        .select(database_schema::schema::notification_settings::dsl::following_did)
        .distinct()
        .load::<String>(&mut conn)
        .await?;

    WATCHED_USERS.set(watched_users.len() as i64);

    // Store the watched users in the kv store
    let watched_users_json = serde_json::to_string(&watched_users).unwrap();
    let result = kv_store
        .put("watched_users", watched_users_json.into())
        .await;
    if let Err(e) = result {
        error!("Error storing watched users in kv store: {e:?}");
    } else {
        info!("Watched users stored in kv store successfully.");
    }
    Ok(())
}

// The IDs of targets - must be different for different listener targets/listeners in case you have many instances
const TEST_TARGET_ID_BY_QUERY: FirestoreListenerTarget = FirestoreListenerTarget::new(42_u32);

pub fn config_env_var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|e| format!("{}: {}", name, e))
}

async fn metrics() -> String {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

async fn _main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Logging with debug enabled
    tracing_subscriber::fmt::init();

    info!("Starting up Google Firestore Listener.");

    run_migrations()?;
    let db_pool = get_pool()?;

    let mut nats_host = std::env::var("NATS_HOST").unwrap_or("localhost".to_string());
    if !nats_host.contains(':') {
        nats_host.push_str(":4222");
    }

    let axum_url = std::env::var("BIND_FIRESTORE_READER").unwrap_or("0.0.0.0:8001".to_string());

    info!("Connecting to NATS at {}", nats_host);
    let nats_client = async_nats::connect(nats_host).await?;
    let jetstream = async_nats::jetstream::new(nats_client);

    _ = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "bluenotify_kv_store".to_string(),
            history: 1,
            ..Default::default()
        })
        .await;

    let kv_store = jetstream
        .get_key_value("bluenotify_kv_store")
        .await
        .unwrap();

    let collection_name = config_env_var("SETTINGS_COLLECTION")?;

    let db = FirestoreDb::new(&config_env_var("GOOGLE_CLOUD_PROJECT")?)
        .await
        .unwrap();

    let all_settings_stream = db
        .fluent()
        .select()
        .from(collection_name.as_str())
        .obj()
        .stream_query()
        .await?;

    let all_settings: Vec<UserSettings> = all_settings_stream.collect().await;

    {
        info!("Deserializing settings from Firestore...");
        info!("Deserialized settings: {all_settings:?}");
        for setting in &all_settings {
            info!("Deserialized settings: {setting:?}");
            let setting_copy = setting.clone();
            let result =
                send_user_settings_to_db(&db_pool.clone(), &kv_store.clone(), &setting_copy).await;
            if let Err(e) = result {
                panic!("Error sending settings to DB: {e:?}",);
            } else {
                info!("Settings sent to DB successfully.");
            }
        }
    }

    println!("Initial settings: len {:?}", all_settings.len());

    let mut listener = db
        .create_listener(FirestoreTempFilesListenStateStorage::new())
        .await?;

    db.fluent()
        .select()
        .from(collection_name.as_str())
        .listen()
        .add_target(TEST_TARGET_ID_BY_QUERY, &mut listener)?;

    listener
        .start({
            move |event| {
                let db_pool = db_pool.clone();
                let kv_store = kv_store.clone();
                async move {
                    match event {
                        FirestoreListenEvent::DocumentChange(ref doc_change) => {
                            if let Some(doc) = &doc_change.document {
                                let result = FirestoreDb::deserialize_doc_to::<UserSettings>(doc);
                                match result {
                                    Ok(settings) => {
                                        debug!("Deserialized settings: {settings:?}");
                                        let settings_copy = (&settings).clone();
                                        let result = send_user_settings_to_db(
                                            &db_pool.clone(),
                                            &kv_store.clone(),
                                            &settings_copy,
                                        )
                                        .await;
                                        if let Err(e) = result {
                                            error!("Error sending settings to DB: {e:?}");
                                        } else {
                                            info!("Settings sent to DB successfully.");
                                        }
                                    }
                                    Err(e) => {
                                        error!("Error deserializing settings: {e:?}");
                                    }
                                }
                            }
                        }
                        FirestoreListenEvent::DocumentDelete(ref doc_delete) => {
                            debug!("Doc deleted: {doc_delete:?}");
                            let id = doc_delete.document.split('/').last().unwrap().to_string();
                            let result = remove_user_settings_from_db(
                                &db_pool.clone(),
                                &kv_store.clone(),
                                &id,
                            )
                            .await;
                            if let Err(e) = result {
                                error!("Error removing settings from DB: {e:?}");
                            } else {
                                info!("Settings removed from DB successfully.");
                            }
                        }
                        _ => {
                            info!("Received an unknown listen response event to handle: {event:?}");
                        }
                    }

                    Ok(())
                }
            }
        })
        .await?;

    let axum_app = Router::new()
        .route("/", get(|| async { "Server online." }))
        .route("/metrics", get(metrics));

    info!("HTTP server listening on {}", axum_url);
    let axum_listener = tokio::net::TcpListener::bind(axum_url).await.unwrap();
    axum::serve(axum_listener, axum_app).await.unwrap();

    listener.shutdown().await?;

    Ok(())
}

fn main() {
    let sentry_dsn = std::env::var("SENTRY_DSN");
    if sentry_dsn.is_ok() && !sentry_dsn.as_ref().unwrap().is_empty() {
        let _guard = sentry::init((
            sentry_dsn.ok(),
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        ));
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(_main());

        if let Err(e) = result {
            eprintln!("Error: {:?}", e);
        }
    } else {
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(_main());

        if let Err(e) = result {
            eprintln!("Error: {:?}", e);
        }
    }
}
