use axum::{extract::State, routing::get, Router};
use core::panic;
use firestore::*;
use futures::StreamExt;
use lazy_static::lazy_static;
use prometheus::{self, register_int_gauge, Encoder, IntGauge, TextEncoder};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    vec,
};
use tokio::sync::RwLock;
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

type SharedState = Arc<RwLock<Settings>>;

struct Settings {
    all_settings: HashMap<String, UserSettings>,
    kv: async_nats::jetstream::kv::Store,
    db_pool: DBPool,
}

async fn send_user_settings_to_db(
    db_pool: &DBPool,
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
                word_allow_list: Some(Vec::new()),
                word_block_list: Some(Vec::new()),
                created_at: now,
            };
            notification_settings.push(new_settings);
        }
    }

    diesel::insert_into(database_schema::schema::notification_settings::table)
        .values(&notification_settings)
        .execute(&mut conn)
        .await?;

    Ok(())
}

async fn remove_user_settings_from_db(
    db_pool: &DBPool,
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
    Ok(())
}

// The IDs of targets - must be different for different listener targets/listeners in case you have many instances
const TEST_TARGET_ID_BY_QUERY: FirestoreListenerTarget = FirestoreListenerTarget::new(42_u32);

pub fn config_env_var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|e| format!("{}: {}", name, e))
}

fn watched_users(settings: &HashMap<String, UserSettings>) -> HashSet<String> {
    let mut watched_users = HashSet::new();
    for (_, setting) in settings.iter() {
        for (account, _) in setting.settings.iter() {
            watched_users.insert(account.clone());
        }
    }
    watched_users
}

async fn metrics() -> String {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

async fn get_settings(state: State<SharedState>) -> String {
    let settings = &state.read().await.all_settings;
    serde_json::to_string(&settings).unwrap()
}

async fn get_watched_users(state: State<SharedState>) -> String {
    let settings = &state.read().await.all_settings;
    let watched_users = watched_users(settings);
    serde_json::to_string(&watched_users).unwrap()
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

    let state = Arc::new(RwLock::new(Settings {
        kv: kv_store,
        all_settings: HashMap::new(),
        db_pool: db_pool.clone(),
    }));

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
        let mut state = state.write().await;
        info!("Deserialized settings: {all_settings:?}");
        for setting in all_settings {
            info!("Deserialized settings: {setting:?}");
            let setting_copy = setting.clone();
            let result = send_user_settings_to_db(&state.db_pool, &setting_copy).await;
            if let Err(e) = result {
                panic!("Error sending settings to DB: {e:?}",);
            } else {
                info!("Settings sent to DB successfully.");
            }
            state
                .all_settings
                .insert(setting.fcm_token.clone(), setting);
        }
    }

    println!(
        "Initial settings: len {:?}",
        &state.read().await.all_settings.len()
    );

    let current_settings = serde_json::to_string(&state.read().await.all_settings).unwrap();
    let current_watched_users =
        serde_json::to_string(&watched_users(&state.read().await.all_settings)).unwrap();
    SUBSCRIBED_USERS_COUNTER.set(state.read().await.all_settings.len() as i64);
    WATCHED_USERS.set(watched_users(&state.read().await.all_settings).len() as i64);
    _ = state
        .write()
        .await
        .kv
        .put("user_settings", current_settings.into())
        .await;
    _ = state
        .write()
        .await
        .kv
        .put("watched_users", current_watched_users.into())
        .await;

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
            let state = Arc::clone(&state);
            move |event| {
                let state = Arc::clone(&state);
                async move {
                    match event {
                        FirestoreListenEvent::DocumentChange(ref doc_change) => {
                            if let Some(doc) = &doc_change.document {
                                let result = FirestoreDb::deserialize_doc_to::<UserSettings>(doc);
                                match result {
                                    Ok(settings) => {
                                        debug!("Deserialized settings: {settings:?}");
                                        let mut state = state.write().await;
                                        let previous_settings =
                                            state.all_settings.get(&settings.fcm_token);
                                        if let Some(previous_settings) = previous_settings {
                                            if previous_settings == &settings {
                                                info!("No changes in settings.");
                                                return Ok(());
                                            }
                                        }
                                        let settings_copy = (&settings).clone();
                                        state
                                            .all_settings
                                            .insert(settings.fcm_token.clone(), settings);
                                        let result = send_user_settings_to_db(
                                            &state.db_pool,
                                            &settings_copy,
                                        )
                                        .await;
                                        if let Err(e) = result {
                                            error!("Error sending settings to DB: {e:?}");
                                        } else {
                                            info!("Settings sent to DB successfully.");
                                        }

                                        let watched_users = watched_users(&state.all_settings);
                                        let watched_users_json =
                                            serde_json::to_string(&watched_users).unwrap();
                                        let all_users_json =
                                            serde_json::to_string(&state.all_settings).unwrap();
                                        WATCHED_USERS.set(watched_users.len() as i64);
                                        SUBSCRIBED_USERS_COUNTER
                                            .set(state.all_settings.len() as i64);
                                        _ = state
                                            .kv
                                            .put("user_settings", all_users_json.into())
                                            .await;
                                        _ = state
                                            .kv
                                            .put("watched_users", watched_users_json.into())
                                            .await;
                                    }
                                    Err(e) => {
                                        error!("Error deserializing settings: {e:?}");
                                    }
                                }
                            }
                        }
                        FirestoreListenEvent::DocumentDelete(ref doc_delete) => {
                            debug!("Doc deleted: {doc_delete:?}");
                            let mut state = state.write().await;
                            let id = doc_delete.document.split('/').last().unwrap().to_string();
                            state.all_settings.remove(&id);
                            let result = remove_user_settings_from_db(&state.db_pool, &id).await;
                            if let Err(e) = result {
                                error!("Error removing settings from DB: {e:?}");
                            } else {
                                info!("Settings removed from DB successfully.");
                            }
                            let watched_users = watched_users(&state.all_settings);
                            let watched_users_json = serde_json::to_string(&watched_users).unwrap();
                            let all_users_json =
                                serde_json::to_string(&state.all_settings).unwrap();
                            WATCHED_USERS.set(watched_users.len() as i64);
                            SUBSCRIBED_USERS_COUNTER.set(state.all_settings.len() as i64);
                            _ = state.kv.put("user_settings", all_users_json.into()).await;
                            _ = state
                                .kv
                                .put("watched_users", watched_users_json.into())
                                .await;
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
        .route("/metrics", get(metrics))
        .route("/settings", get(get_settings))
        .route("/watched_users", get(get_watched_users))
        .with_state(Arc::clone(&state));

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
