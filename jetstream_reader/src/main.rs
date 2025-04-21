use async_nats::jetstream::kv::Store;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{routing::get, Router};
use bluesky_utils::get_following;
use bluesky_utils::parse_created_at;
use database_schema::{account_follows, run_migrations, AccountFollow, DBPool, NewAccountFollow};
use diesel::dsl::exists;
use diesel::dsl::not;
use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use futures::TryStreamExt;
use futures_util::StreamExt;
use lazy_static::lazy_static;
use prometheus::{
    self, register_gauge, register_int_counter, Encoder, Gauge, IntCounter, TextEncoder,
};
use rand::rng;
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tracing::debug;
use tracing::{error, info, warn};

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

#[derive(Clone)]
struct AxumState {
    pool: Pool<AsyncPgConnection>,
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
#[derive(Debug, Clone, Deserialize, Serialize)]
struct JetstreamFollow {
    did: String,
    time_us: u64,
    kind: String,
    commit: FollowCommit,
}

impl JetstreamFollow {
    fn subject(&self) -> String {
        self.commit.record.subject.clone()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FollowCommit {
    rev: String,
    operation: String,
    collection: String,
    rkey: String,
    record: FollowRecord,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FollowRecord {
    #[serde(alias = "$type")]
    commit_type: String,
    subject: String,
    #[serde(alias = "createdAt")]
    created_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JetstreamUnfollow {
    did: String,
    time_us: u64,
    kind: String,
    commit: UnfollowCommit,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
struct UnfollowCommit {
    rev: String,
    operation: String,
    collection: String,
    rkey: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum JetstreamFollowEvent {
    Follow(JetstreamFollow),
    Unfollow(JetstreamUnfollow),
}

impl JetstreamFollowEvent {
    fn did(&self) -> String {
        match self {
            JetstreamFollowEvent::Follow(follow) => follow.did.clone(),
            JetstreamFollowEvent::Unfollow(unfollow) => unfollow.did.clone(),
        }
    }
    fn action(&self) -> String {
        match self {
            JetstreamFollowEvent::Follow(_) => "follow".to_string(),
            JetstreamFollowEvent::Unfollow(_) => "unfollow".to_string(),
        }
    }

    fn from_value(value: Value) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        match value["commit"]["operation"].as_str() {
            Some("create") => {
                let follow: JetstreamFollow = serde_json::from_value(value)?;
                Ok(JetstreamFollowEvent::Follow(follow))
            }
            Some("delete") => {
                let unfollow: JetstreamUnfollow = serde_json::from_value(value)?;
                Ok(JetstreamFollowEvent::Unfollow(unfollow))
            }
            _ => Err("Unknown operation".into()),
        }
    }
}

async fn get_last_event_time(kv_store: Store) -> Option<String> {
    let result = kv_store.get("last_jetstream_time").await;
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

async fn rescan_user_follows(did: String, pg_pool: DBPool) {
    info!("Rescanning follows for {}", did);
    let follows = get_following(&did, None, Some(std::time::Duration::from_millis(50))).await;
    if follows.is_err() {
        error!("Error getting follows: {:?}", follows);
        return;
    }
    let follows = follows.unwrap();
    let mut conn = {
        let conn = pg_pool.get().await;
        if conn.is_err() {
            error!("Error getting PG from pool!");
            return;
        }
        conn.unwrap()
    };

    let mut existing_follows = HashSet::new();
    let existing = account_follows::table
        .filter(account_follows::account_did.eq(did.clone()))
        .load::<AccountFollow>(&mut conn)
        .await;
    if existing.is_err() {
        error!("Error getting existing follows: {:?}", existing);
        return;
    }
    let existing = existing.unwrap();
    for follow in existing {
        existing_follows.insert(follow.follow_did);
    }

    let to_remove = existing_follows
        .difference(&follows)
        .cloned()
        .collect::<Vec<String>>();

    let to_add = follows
        .difference(&existing_follows)
        .cloned()
        .collect::<Vec<String>>();

    diesel::delete(
        account_follows::table
            .filter(account_follows::account_did.eq(did.clone()))
            .filter(account_follows::follow_did.eq_any(to_remove)),
    )
    .execute(&mut conn)
    .await
    .unwrap();

    let new_follows = to_add
        .iter()
        .map(|follow| NewAccountFollow {
            account_did: did.clone(),
            follow_did: follow.clone(),
        })
        .collect::<Vec<NewAccountFollow>>();

    if new_follows.is_empty() {
        return;
    }
    diesel::insert_into(account_follows::table)
        .values(new_follows)
        .execute(&mut conn)
        .await
        .unwrap();
}

async fn handle_follow_event(event: JetstreamFollowEvent, pg_pool: DBPool) {
    // try forever until it works! We don't want to miss any follow updates
    loop {
        info!("Handling {} event for: {:?}", event.action(), event.did());
        let pg_pool = pg_pool.clone();
        match event.clone() {
            JetstreamFollowEvent::Follow(follow_event) => {
                // On a follow event, we can just insert the follow into the database
                let mut conn = {
                    let conn = pg_pool.get().await;
                    if conn.is_err() {
                        error!("Error getting PG from pool!");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    conn.unwrap()
                };

                // first check if it already exists
                let exists = account_follows::table
                    .filter(account_follows::account_did.eq(follow_event.did.clone()))
                    .filter(account_follows::follow_did.eq(follow_event.subject()))
                    .first::<AccountFollow>(&mut conn)
                    .await;

                if exists.is_ok() {
                    info!("Already exists, skipping: {:?}", follow_event);
                    return;
                }

                let follow = NewAccountFollow {
                    account_did: follow_event.did.clone(),
                    follow_did: follow_event.subject(),
                };
                let result = diesel::insert_into(account_follows::table)
                    .values(follow)
                    .execute(&mut conn)
                    .await;

                if result.is_err() {
                    error!("Error inserting follow: {:?}", result);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                return;
            }
            JetstreamFollowEvent::Unfollow(unfollow) => {
                // on an unfollow, we can't know what user was unfollowed, so unfortunately have to
                // rescan the entire list of users!
                rescan_user_follows(unfollow.did, pg_pool).await;
                return;
            }
        }
    }
}

async fn fill_in_missing_follows(
    pg_pool: DBPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = {
        let conn = pg_pool.get().await;
        if conn.is_err() {
            error!("Error getting PG from pool!");
            return Err("Error getting PG from pool!".into());
        }
        conn.unwrap()
    };

    let users_to_fill = database_schema::schema::accounts::table
        .select(database_schema::schema::accounts::dsl::account_did)
        .filter(not(exists(
            database_schema::schema::account_follows::table.filter(
                database_schema::schema::account_follows::dsl::account_did
                    .eq(database_schema::schema::accounts::dsl::account_did),
            ),
        )))
        .distinct()
        .load::<String>(&mut conn)
        .await?;

    info!(
        "Filling in missing follows for {} users",
        users_to_fill.len()
    );

    for user in users_to_fill {
        rescan_user_follows(user, pg_pool.clone()).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

async fn connect_and_listen(
    base_url: &str,
    watched_users: SharedWatchedUsers,
    nats_js: Arc<RwLock<async_nats::jetstream::Context>>,
    kv_store: Store,
    pg_pool: DBPool,
) {
    let mut url = format!(
        "{}?wantedCollections=app.bsky.feed.post&wantedCollections=app.bsky.feed.repost&wantedCollections=app.bsky.graph.follow",
        base_url
    );
    let last_event_time = get_last_event_time(kv_store.clone()).await;
    info!("Last event time: {:?}", last_event_time);
    if let Some(last_event_time) = last_event_time {
        url.push_str(&format!("&cursor={}", last_event_time));
    }

    info!("Connecting to {}", url);
    let (ws_stream, _) = connect_async(&url).await.expect("Failed to connect");
    let (_write, mut read) = ws_stream.split();
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
        if text.is_empty() {
            continue;
        }
        ALL_MESSAGES_COUNTER.inc();
        let did = text
            .split_once("\"did\":\"")
            .unwrap_or(("", ""))
            .1
            .split_once("\"")
            .unwrap_or(("", ""))
            .0;
        if did.is_empty() {
            debug!("No did found in message: {:?}", text);
            error!("Error getting did: {:?}", did);
            continue;
        }
        let is_watched = watched_users.read().await.watched_users.contains(did);
        if !is_watched {
            continue;
        }
        let raw: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if raw.is_null() {
            error!("Error parsing JSON: {:?}", text);
            continue;
        }
        let collection = raw["commit"]["collection"].as_str().unwrap_or("");
        if collection == "app.bsky.graph.follow" {
            let event: Result<JetstreamFollowEvent, _> = JetstreamFollowEvent::from_value(raw);
            if event.is_err() {
                error!("Error deserializing follow: {:?}", event);
                continue;
            }
            let event = event.unwrap();
            let pg_pool = pg_pool.clone();

            tokio::spawn(async move {
                handle_follow_event(event, pg_pool).await;
            });
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
            .put("last_jetstream_time", post.time_us.to_string().into())
            .await;
    }
}

async fn listen_to_jetstream_forever(
    watched_users: SharedWatchedUsers,
    nats_js: async_nats::jetstream::Context,
    kv_store: Store,
    pg_pool: DBPool,
) {
    let shared_js: Arc<RwLock<async_nats::jetstream::Context>> = Arc::new(RwLock::new(nats_js));
    loop {
        let url = URLS.choose(&mut rng()).unwrap();
        let watched_users = watched_users.clone();
        connect_and_listen(
            url,
            watched_users,
            shared_js.clone(),
            kv_store.clone(),
            pg_pool.clone(),
        )
        .await;
        info!("Retrying connection...");
    }
}

async fn update_watched_users(
    watched_users: SharedWatchedUsers,
    pg_pool: DBPool,
) -> Result<HashSet<String>, Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = {
        let conn = pg_pool.get().await;
        if conn.is_err() {
            error!("Error getting PG from pool!");
            return Err("Error getting PG from pool!".into());
        }
        conn.unwrap()
    };

    let actual_watched_users = database_schema::schema::notification_settings::table
        .select(database_schema::schema::notification_settings::dsl::following_did)
        .distinct()
        .load::<String>(&mut conn)
        .await?;

    let bluenotify_users = database_schema::schema::accounts::table
        .select(database_schema::schema::accounts::dsl::account_did)
        .distinct()
        .load::<String>(&mut conn)
        .await?;

    let combined = actual_watched_users
        .into_iter()
        .chain(bluenotify_users.into_iter())
        .collect::<HashSet<String>>();

    let mut watched_users = watched_users.write().await;
    watched_users.watched_users = combined.clone();
    info!(
        "Updated watched users: Len {:?}",
        watched_users.watched_users.len()
    );

    Ok(combined)
}

/// Listen to the NATS jetstream for updates, and replace the watched users list
async fn listen_to_watched_forever(
    kv_store: Store,
    watched_users: SharedWatchedUsers,
    pg_pool: DBPool,
) {
    _ = update_watched_users(watched_users.clone(), pg_pool.clone()).await;
    loop {
        let messages = kv_store.watch("watched_users").await;
        if messages.is_err() {
            error!("Error watching watched_users");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let mut messages = messages.unwrap();
        while let Ok(Some(_message)) = messages.try_next().await {
            _ = update_watched_users(watched_users.clone(), pg_pool.clone()).await;
        }
    }
}

#[axum::debug_handler]
async fn rescan_user_web_request(
    State(AxumState { pool: pg_pool }): State<AxumState>,
    Path(did): Path<String>,
) -> Result<Response, StatusCode> {
    if did.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    rescan_user_follows(did, pg_pool).await;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body("Rescanned user".into())
        .unwrap())
}

#[axum::debug_handler]
async fn rescan_missing(
    State(AxumState { pool: pg_pool }): State<AxumState>,
) -> Result<Response, StatusCode> {
    tokio::spawn(async move {
        fill_in_missing_follows(pg_pool.clone()).await.unwrap();
    });
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body("Started rescan job.".into())
        .unwrap())
}

async fn _main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    info!("Starting up Bluesky Jetstream Reader.");

    info!("Getting DB");
    let pg_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pg_config = AsyncDieselConnectionManager::<diesel_async::AsyncPgConnection>::new(&pg_url);
    let pg_pool = Pool::builder(pg_config).build()?;
    info!("Got DB");

    run_migrations()?;

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

    let shared_watched_users = Arc::new(RwLock::new(WatchedUsers {
        watched_users: HashSet::new(),
    }));

    info!("Starting up metrics server on {}", axum_url);
    let axum_app = Router::new()
        .route("/", get(|| async { "Jetstream_reader server online." }))
        .route("/metrics", get(metrics))
        .route("/rescan/{did}", get(rescan_user_web_request))
        .route("/rescan_missing", get(rescan_missing))
        .with_state(AxumState {
            pool: pg_pool.clone(),
        });
    let axum_listener = tokio::net::TcpListener::bind(axum_url).await.unwrap();

    let mut tasks: JoinSet<_> = JoinSet::new();
    let watched_copy = shared_watched_users.clone();
    tasks.spawn(listen_to_jetstream_forever(
        watched_copy,
        nats_js,
        kv_store.clone(),
        pg_pool.clone(),
    ));
    let watched_copy = shared_watched_users.clone();
    tasks.spawn(listen_to_watched_forever(
        kv_store,
        watched_copy,
        pg_pool.clone(),
    ));
    tasks.spawn(async move {
        axum::serve(axum_listener, axum_app).await.unwrap();
    });

    // fill in missing follows
    tasks.spawn(async move {
        fill_in_missing_follows(pg_pool.clone()).await.unwrap();
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
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(_main());

        if let Err(e) = result {
            warn!("Shutting down due to error: {:?}", e);
            std::process::exit(1);
        }
        info!("Jetstream reader shutting down.");
    } else {
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(_main());

        if let Err(e) = result {
            warn!("Shutting down due to error: {:?}", e);
            std::process::exit(1);
        }
        info!("Jetstream reader shutting down.");
    }
}

async fn metrics() -> String {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
