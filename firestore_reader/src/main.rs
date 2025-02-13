use axum::{extract::State, routing::get, Router};
use firestore::*;
use prometheus::{self, register_int_gauge, Encoder, IntGauge, TextEncoder};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use futures::stream::BoxStream;
use futures::StreamExt;
use lazy_static::lazy_static;
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
        let mut state = state.write().await;
        for setting in all_settings {
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
                                        state
                                            .all_settings
                                            .insert(settings.fcm_token.clone(), settings);

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
        let _guard = sentry::init((sentry_dsn.ok(), sentry::ClientOptions {
            release: sentry::release_name!(),
            ..Default::default()
        }));
    }
    
    _ = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(_main());
}
