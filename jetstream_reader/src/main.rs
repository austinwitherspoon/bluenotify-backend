use async_nats::jetstream::kv::{self, Store};
use async_nats::jetstream::stream::Stream;
use axum::{routing::get, Router};
use bluesky_utils::parse_created_at;
use futures::TryStreamExt;
use futures_util::StreamExt;
use lazy_static::lazy_static;
use prometheus::{
    self, register_gauge, register_int_counter, Encoder, Gauge, IntCounter, TextEncoder,
};
use rand::rng;
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info};

const URLS: [&str; 4] = [
    "wss://jetstream1.us-west.bsky.network/subscribe",
    "wss://jetstream2.us-west.bsky.network/subscribe",
    "wss://jetstream1.us-east.bsky.network/subscribe",
    "wss://jetstream2.us-east.bsky.network/subscribe",
];

const OLDEST_POST_AGE: chrono::Duration = chrono::Duration::hours(2);

lazy_static! {
    static ref ALL_MESSAGES_COUNTER: IntCounter = register_int_counter!(
        "all_messages",
        "The number of messages received by the server"
    )
    .unwrap();
    static ref HANDLED_MESSAGES_COUNTER: IntCounter = register_int_counter!(
        "handled_messages",
        "The number of messages handled by the server"
    )
    .unwrap();
    static ref JETSTREAM_LAG: Gauge = register_gauge!(
        "jetstream_lag_seconds",
        "The lag between the Jetstream events and the notifier server"
    )
    .unwrap();
}

type SharedWatchedUsers = Arc<RwLock<WatchedUsers>>;

struct WatchedUsers {
    watched_users: HashSet<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JetstreamPost {
    did: String,
    time_us: u64,
    commit: Option<Commit>,
}

impl JetstreamPost {
    fn event_datetime(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::from_timestamp(
            (self.time_us / 1_000_000) as i64,
            ((self.time_us % 1_000_000) * 1000) as u32,
        )
    }

    fn post_datetime(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        if self.commit.is_none() {
            return None;
        }
        let result = parse_created_at(&self.commit.as_ref()?.record.created_at);
        if result.is_err() {
            return None;
        }
        Some(result.unwrap())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Commit {
    // rev: String,
    // operation: String,
    collection: String,
    rkey: String,
    cid: String,
    record: CommitRecord,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CommitRecord {
    #[serde(alias = "$type")]
    commit_type: String,
    #[serde(alias = "createdAt")]
    created_at: String,
}

async fn get_last_event_time(kv_store: Arc<RwLock<Store>>) -> Option<String> {
    let result = kv_store.read().await.get("last_jetstream_time").await;
    if result.is_err() {
        return None;
    }
    let result = result.unwrap();
    if result.is_none() {
        return None;
    }
    let result = result.unwrap();
    let string_message = std::str::from_utf8(&result).unwrap();
    Some(string_message.to_string())
}

async fn connect_and_listen(
    base_url: &str,
    watched_users: SharedWatchedUsers,
    nats_js: Arc<RwLock<async_nats::jetstream::Context>>,
    kv_store: Arc<RwLock<Store>>,
) {
    let mut url = format!(
        "{}?wantedCollections=app.bsky.feed.post&wantedCollections=app.bsky.feed.repost",
        base_url
    );
    let last_event_time = get_last_event_time(kv_store.clone()).await;
    info!("Last event time: {:?}", last_event_time);
    if let Some(last_event_time) = last_event_time {
        url.push_str(&format!("&cursor={}", last_event_time));
    }

    info!("Connecting to {}", url);
    let (ws_stream, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();
    loop {
        let frame = timeout(Duration::from_secs(10), read.next()).await;
        if frame.is_err() {
            error!("Frame timed out after 10 seconds: {:?}", frame);
            break;
        }
        let frame = frame.unwrap();
        if frame.is_none() {
            error!("Error receiving frame");
            break;
        }
        let frame = frame.unwrap();
        if frame.is_err() {
            error!("Error receiving frame: {:?}", frame);
            break;
        }
        let frame = frame.unwrap();
        let text = String::from_utf8_lossy(&frame.into_data()).into_owned();
        ALL_MESSAGES_COUNTER.inc();
        let did = text
            .split_once("\"did\":\"")
            .unwrap_or(("", ""))
            .1
            .split_once("\"")
            .unwrap_or(("", ""))
            .0;
        if did.is_empty() {
            error!("Error getting did: {:?}", did);
            continue;
        }
        let is_watched = watched_users.read().await.watched_users.contains(did);
        if !is_watched {
            continue;
        }
        let post: Result<JetstreamPost, _> = serde_json::from_str(&text);
        if post.is_err() {
            // ignore missing cid, this happens when you delete a post
            if format!("{:?}", post).contains("missing field `cid`") {
                continue;
            }
            error!("Error deserializing post: {:?}", post);
            continue;
        }
        let post = post.unwrap();
        let post_time = post.post_datetime();
        if post_time.is_none() {
            error!("Error getting time: {:?}", post_time);
            continue;
        }
        let post_time = post_time.unwrap();
        let event_time = post.event_datetime();
        if event_time.is_none() {
            error!("Error getting time: {:?}", event_time);
            continue;
        }
        let event_time = event_time.unwrap();
        JETSTREAM_LAG.set(
            (chrono::Utc::now() - event_time).num_nanoseconds().unwrap() as f64 / 1_000_000_000.0,
        );

        if post_time < chrono::Utc::now() - OLDEST_POST_AGE {
            info!("Old post, ignoring: {:?}", post_time);
            continue;
        }
        HANDLED_MESSAGES_COUNTER.inc();

        if post.commit.is_none() {
            error!("No commit: {:?}", post);
            continue;
        }

        let commit = post.commit.as_ref().unwrap();
        if commit.collection != "app.bsky.feed.post" && commit.collection != "app.bsky.feed.repost"
        {
            error!("Not a post: {:?}", post);
            continue;
        }

        let mut headers = async_nats::HeaderMap::new();
        headers.append("Nats-Msg-Id", format!("{}.{}", post.did, commit.rkey));

        let nats_js = nats_js.write().await;

        // Publish this post
        info!("Publishing post: {:?}", post);
        nats_js
            .publish_with_headers(
                format!("watched_posts.{}", post.did),
                headers,
                text.to_owned().into(),
            )
            .await
            .unwrap();

        // Publish the last jetstream time
        _ = kv_store
            .write()
            .await
            .put("last_jetstream_time", post.time_us.to_string().into())
            .await;
    }
}

async fn listen_to_jetstream_forever(
    watched_users: SharedWatchedUsers,
    nats_js: async_nats::jetstream::Context,
    kv_store: Arc<RwLock<Store>>,
) {
    let shared_js: Arc<RwLock<async_nats::jetstream::Context>> = Arc::new(RwLock::new(nats_js));
    loop {
        let url = URLS.choose(&mut rng()).unwrap();
        let watched_users = watched_users.clone();
        connect_and_listen(url, watched_users, shared_js.clone(), kv_store.clone()).await;
        info!("Retrying connection...");
    }
}

/// Listen to the NATS jetstream for updates, and replace the watched users list
async fn listen_to_watched_forever(kv_store: Store, watched_users: SharedWatchedUsers) {
    let watched = kv_store.get("watched_users").await;
    if watched.is_err() {
        error!("Error getting watched users: {:?}", watched);
    } else {
        let watched = watched.unwrap();
        if watched.is_some() {
            let watched: Result<HashSet<String>, _> =
                serde_json::from_slice(watched.unwrap().as_ref());
            if watched.is_err() {
                error!("Error deserializing watched users: {:?}", watched);
            } else {
                let mut watched_users = watched_users.write().await;
                watched_users.watched_users = watched.unwrap();
                info!(
                    "Updated watched users: Len {:?}",
                    watched_users.watched_users.len()
                );
            }
        }
    }
    loop {
        let mut messages = kv_store.watch("watched_users").await;
        if messages.is_err() {
            error!("Error watching watched_users");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let mut messages = messages.unwrap();
        while let Ok(Some(message)) = messages.try_next().await {
            type WatchedJson = HashSet<String>;
            let watched: Result<WatchedJson, _> = serde_json::from_slice(message.value.as_ref());
            if watched.is_err() {
                error!("Error deserializing watched users: {:?}", watched);
                continue;
            }
            let mut watched_users = watched_users.write().await;
            watched_users.watched_users = watched.unwrap();
            info!(
                "Updated watched users: len {:?}",
                watched_users.watched_users.len()
            );
        }
    }
}

async fn _main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    info!("Starting up Bluesky Jetstream Reader.");

    let mut nats_host = std::env::var("NATS_HOST").unwrap_or("localhost".to_string());
    if !nats_host.contains(':') {
        nats_host.push_str(":4222");
    }

    let axum_url = std::env::var("BIND_JETSTREAM_READER").unwrap_or("0.0.0.0:8002".to_string());

    info!("Connecting to NATS at {}", nats_host);
    let nats_client = async_nats::connect(nats_host).await?;
    let nats_js = async_nats::jetstream::new(nats_client);

    // Create a watched post stream that we can publish to
    nats_js
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "watched_posts".to_string(),
            subjects: vec!["watched_posts.*".to_string()],
            max_messages: 100_000,
            duplicate_window: OLDEST_POST_AGE.to_std().unwrap(),
            ..Default::default()
        })
        .await?;

    _ = nats_js
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "bluenotify_kv_store".to_string(),
            history: 1,
            ..Default::default()
        })
        .await;

    let kv_store = nats_js.get_key_value("bluenotify_kv_store").await.unwrap();
    let kv_store_2 = nats_js.get_key_value("bluenotify_kv_store").await.unwrap();

    let shared_watched_users = Arc::new(RwLock::new(WatchedUsers {
        watched_users: HashSet::new(),
    }));

    let shared_kv: Arc<RwLock<Store>> = Arc::new(RwLock::new(kv_store));

    info!("Starting up metrics server on {}", axum_url);
    let axum_app = Router::new()
        .route("/", get(|| async { "Jetstream_reader server online." }))
        .route("/metrics", get(metrics));
    let axum_listener = tokio::net::TcpListener::bind(axum_url).await.unwrap();

    let mut tasks: JoinSet<_> = JoinSet::new();
    let watched_copy = shared_watched_users.clone();
    tasks.spawn(listen_to_jetstream_forever(
        watched_copy,
        nats_js,
        shared_kv.clone(),
    ));
    let watched_copy = shared_watched_users.clone();
    tasks.spawn(listen_to_watched_forever(kv_store_2, watched_copy));
    tasks.spawn(async move {
        axum::serve(axum_listener, axum_app).await.unwrap();
    });

    tasks.join_all().await;
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
    }

    _ = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(_main());
}

async fn metrics() -> String {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
