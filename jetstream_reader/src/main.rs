use async_nats::jetstream::kv::Store;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{routing::get, Router};
use bluesky_utils::{get_following, parse_created_at, GetFollowsError};
use database_schema::{account_follows, run_migrations, AccountFollow, DBPool};
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
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;
use url::Url;

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
    .expect("Failed to register all_messages counter");
    static ref HANDLED_MESSAGES_COUNTER: IntCounter = register_int_counter!(
        "handled_messages",
        "The number of messages handled by the server"
    )
    .expect("Failed to register handled_messages counter");
    static ref JETSTREAM_LAG: Gauge = register_gauge!(
        "jetstream_lag_seconds",
        "The lag between the Jetstream events and the notifier server"
    )
    .expect("Failed to register jetstream_lag gauge");
}

type SharedWatchedUsers = Arc<RwLock<WatchedUsers>>;

struct WatchedUsers {
    watched_users: HashSet<String>,
    bluenotify_users: HashSet<String>,
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
        match parse_created_at(&self.commit.as_ref()?.record.created_at) {
            Ok(time) => Some(time),
            Err(e) => {
                error!("Error parsing created_at: {:?}", e);
                return None;
            }
        }
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
    match kv_store.get("last_jetstream_time").await {
        Ok(Some(message)) => {
            let string_message =
                std::str::from_utf8(&message).expect("Failed to parse message as utf8");
            Some(string_message.to_string())
        }
        Ok(None) => None,
        Err(e) => {
            warn!("Error getting last event time: {:?}", e);
            None
        }
    }
}

async fn rescan_user_follows(did: String, pg_pool: DBPool) {
    info!("Rescanning follows for {}", did);
    let follows = match get_following(&did, None, Some(std::time::Duration::from_millis(50))).await
    {
        Ok(follows) => follows,
        Err(error) => match error {
            GetFollowsError::TooManyFollows(count) => {
                let mut conn = match pg_pool.get().await {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("Error getting PG from pool: {:?}", e);
                        return;
                    }
                };
                let result = diesel::update(database_schema::schema::accounts::table)
                    .filter(database_schema::schema::accounts::dsl::account_did.eq(did.clone()))
                    .set(database_schema::schema::accounts::dsl::too_many_follows.eq(true))
                    .execute(&mut conn)
                    .await;
                if let Err(e) = result {
                    error!("Error updating too_many_follows: {:?}", e);
                    return;
                }
                warn!("Too many follows for {}: {}", did, count);
                return;
            }
            GetFollowsError::DisabledAccount => {
                info!("User disabled, deleting account: {}", did);
                let mut conn = match pg_pool.get().await {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("Error getting PG from pool: {:?}", e);
                        return;
                    }
                };
                let result = diesel::delete(database_schema::schema::accounts::table)
                    .filter(database_schema::schema::accounts::dsl::account_did.eq(did.clone()))
                    .execute(&mut conn)
                    .await;
                if let Err(e) = result {
                    error!("Error deleting account: {:?}", e);
                    return;
                }
                return;
            }
            _ => {
                error!("Error getting follows: {:?}", error);
                return;
            }
        },
    };
    let mut conn = {
        let conn = pg_pool.get().await;
        match conn {
            Ok(conn) => conn,
            Err(e) => {
                error!("Error getting PG from pool: {:?}", e);
                return;
            }
        }
    };

    let retry = again::RetryPolicy::fixed(Duration::from_millis(2000))
        .with_max_retries(5)
        .with_jitter(false);

    let mut existing_follows = HashSet::new();
    let existing = match account_follows::table
        .filter(account_follows::account_did.eq(did.clone()))
        .load::<AccountFollow>(&mut conn)
        .await {
        Ok(existing) => existing,
        Err(e) => {
            error!("Error getting existing follows: {:?}", e);
            return;
        }
    };
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

    let result = retry
        .retry(|| {
            diesel::delete(
                account_follows::table
                    .filter(account_follows::account_did.eq(did.clone()))
                    .filter(account_follows::follow_did.eq_any(to_remove.clone())),
            )
            .execute(&mut conn)
        })
        .await;

    match result {
        Ok(result) => result,
        Err(e) => {
            error!("Error deleting follows: {:?}", e);
            return;
        }
    };

    let new_follows = to_add
        .iter()
        .map(|follow| AccountFollow {
            account_did: did.clone(),
            follow_did: follow.clone(),
        })
        .collect::<Vec<AccountFollow>>();

    if new_follows.is_empty() {
        return;
    }
    let result = retry
        .retry(|| {
            diesel::insert_into(account_follows::table)
                .values(new_follows.clone())
                .on_conflict_do_nothing()
                .execute(&mut conn)
        })
        .await;
    match result {
        Ok(result) => result,
        Err(e) => {
            error!("Error inserting follows: {:?}", e);
            return;
        }
    };
}

async fn handle_follow_event(event: JetstreamFollowEvent, pg_pool: DBPool) {
    // try forever until it works! We don't want to miss any follow updates
    loop {
        info!("Handling {} event for: {:?}", event.action(), event.did());
        let pg_pool = pg_pool.clone();

        {
            let mut conn = match pg_pool.get().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!("Error getting PG from pool: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    return;
                }
            };
            let too_many_follows = database_schema::schema::accounts::table
                .select(database_schema::schema::accounts::dsl::too_many_follows)
                .filter(database_schema::schema::accounts::dsl::account_did.eq(event.did()))
                .first::<bool>(&mut conn)
                .await;
            match too_many_follows {
                Ok(too_many_follows) => {
                    if too_many_follows {
                        return;
                    }
                }
                Err(_) => {
                    error!("Error getting too_many_follows: {:?}", too_many_follows);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
        }
        match event.clone() {
            JetstreamFollowEvent::Follow(follow_event) => {
                // On a follow event, we can just insert the follow into the database
                let mut conn = match pg_pool.get().await {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!("Error getting PG from pool: {:?}", e);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        return;
                    }
                };

                let follow = AccountFollow {
                    account_did: follow_event.did.clone(),
                    follow_did: follow_event.subject(),
                };
                let result = diesel::insert_into(account_follows::table)
                    .values(follow)
                    .on_conflict((account_follows::account_did, account_follows::follow_did))
                    .do_nothing()
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
    let users_to_fill = {
        let mut conn = match pg_pool.get().await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Error getting PG from pool: {:?}", e);
                return Err("Error getting PG from pool!".into());
            }
        };
        database_schema::schema::accounts::table
            .select(database_schema::schema::accounts::dsl::account_did)
            .filter(database_schema::schema::accounts::dsl::too_many_follows.eq(false))
            .filter(not(exists(
                database_schema::schema::account_follows::table.filter(
                    database_schema::schema::account_follows::dsl::account_did
                        .eq(database_schema::schema::accounts::dsl::account_did),
                ),
            )))
            .distinct()
            .load::<String>(&mut conn)
            .await?
    };

    info!(
        "Filling in missing follows for {} users",
        users_to_fill.len()
    );

    for user in users_to_fill {
        let result = timeout(
            Duration::from_secs(60 * 2),
            rescan_user_follows(user, pg_pool.clone()),
        )
        .await;
        if result.is_err() {
            error!("Error filling in missing follows: {:?}", result);
            continue;
        }
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

        let frame = match frame {
            Ok(Some(Ok(frame))) => frame,
            Ok(Some(Err(e))) => {
                error!("Error receiving frame: {:?}", e);
                break;
            }
            Ok(None) => {
                error!("Error receiving frame: None");
                break;
            }
            Err(_) => {
                error!("Error receiving frame: Timeout");
                break;
            }
        };
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
        let is_bluenotify = watched_users.read().await.bluenotify_users.contains(did);
        if !is_watched && !is_bluenotify {
            continue;
        }
        let raw: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if raw.is_null() {
            error!("Error parsing JSON: {:?}", text);
            continue;
        }
        let collection = raw["commit"]["collection"].as_str().unwrap_or("");
        if collection == "app.bsky.graph.follow" && is_bluenotify {
            let event = match JetstreamFollowEvent::from_value(raw) {
                Ok(event) => event,
                Err(e) => {
                    error!("Error parsing follow event: {:?}", e);
                    continue;
                }
            };
            let pg_pool = pg_pool.clone();

            tokio::spawn(async move {
                handle_follow_event(event, pg_pool).await;
            });
            continue;
        }
        if !is_watched || collection == "app.bsky.graph.follow" {
            continue;
        }
        let post: JetstreamPost = match serde_json::from_str(&text) {
            Ok(post) => post,
            Err(e) => {
                if format!("{:?}", e).contains("missing field `cid`") {
                    continue;
                }
                error!("Error deserializing post: {:?}", e);
                continue;
            }
        };
        if post.commit.is_none() {
            warn!("No commit, skipping: {:?}", post);
            continue;
        }
        let post_time = match post.post_datetime() {
            Some(time) => time,
            None => {
                error!("Error getting post_time: {:?}", post);
                continue;
            }
        };
        let event_time = match post.event_datetime() {
            Some(time) => time,
            None => {
                error!("Error getting event_time: {:?}", post);
                continue;
            }
        };
        JETSTREAM_LAG.set(
            (chrono::Utc::now() - event_time).num_nanoseconds().expect("i64 overflow!") as f64 / 1_000_000_000.0,
        );

        if post_time < chrono::Utc::now() - OLDEST_POST_AGE {
            info!("Old post, ignoring: {:?}", post_time);
            continue;
        }
        HANDLED_MESSAGES_COUNTER.inc();

        let commit = match &post.commit {
            Some(commit) => commit.clone(),
            None => {
                error!("No commit: {:?}", post);
                continue;
            }
        };
        if commit.collection != "app.bsky.feed.post" && commit.collection != "app.bsky.feed.repost"
        {
            error!("Not a post: {:?}", post);
            continue;
        }

        let mut headers = async_nats::HeaderMap::new();
        headers.append("Nats-Msg-Id", format!("{}.{}", post.did, commit.rkey));

        let nats_js = nats_js.write().await;

        info!("Publishing post: {:?}", post);
        let result = again::retry(|| {
            nats_js.publish_with_headers(
                format!("watched_posts.{}", post.did.clone()),
                headers.clone(),
                text.to_owned().into(),
            )
        })
        .await;
        if result.is_err() {
            info!("post: {:?}", post);
            error!("Error publishing post: {:?}", result);
            continue;
        }

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
        let url = URLS.choose(&mut rng()).expect("No URLs found");
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = {
        let conn = timeout(Duration::from_secs(30), pg_pool.get()).await;

        match conn {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => {
                error!("Error getting PG from pool: {:?}", e);
                return Err("Error getting PG from pool!".into());
            }
            Err(_) => {
                error!("Timeout getting PG from pool!");
                return Err("Timeout getting PG from pool!".into());
            }
        }
    };

    let actual_watched_users = timeout(
        Duration::from_secs(30),
        database_schema::schema::notification_settings::table
            .select(database_schema::schema::notification_settings::dsl::following_did)
            .distinct()
            .load::<String>(&mut conn),
    )
    .await;

    let actual_watched_users = match actual_watched_users {
        Ok(Ok(users)) => users,
        Ok(Err(e)) => {
            error!("Error getting watched users: {:?}", e);
            return Err("Error getting watched users!".into());
        }
        Err(_) => {
            error!("Timeout getting watched users!");
            return Err("Timeout getting watched users!".into());
        }
    };

    let bluenotify_users = timeout(
        Duration::from_secs(30),
        database_schema::schema::accounts::table
            .select(database_schema::schema::accounts::dsl::account_did)
            .distinct()
            .load::<String>(&mut conn),
    )
    .await;

    let bluenotify_users = match bluenotify_users {
        Ok(Ok(users)) => users,
        Ok(Err(e)) => {
            error!("Error getting bluenotify users: {:?}", e);
            return Err("Error getting bluenotify users!".into());
        }
        Err(_) => {
            error!("Timeout getting bluenotify users!");
            return Err("Timeout getting bluenotify users!".into());
        }
    };

    let watched_users = timeout(Duration::from_secs(30), watched_users.write()).await;

    let mut watched_users = match watched_users {
        Ok(watched_users) => watched_users,
        Err(_) => {
            error!("Error getting watched users lock!");
            return Err("Error getting watched users lock!".into());
        }
    };
    watched_users.watched_users = actual_watched_users.into_iter().collect();
    watched_users.bluenotify_users = bluenotify_users.into_iter().collect();

    info!(
        "Updated watched users: Len {:?}",
        watched_users.watched_users.len()
    );
    info!(
        "Updated bluenotify users: Len {:?}",
        watched_users.bluenotify_users.len()
    );

    tokio::spawn(async move {
        match fill_in_missing_follows(pg_pool.clone()).await {
            Ok(_) => {}
            Err(e) => {
                error!("Error filling in missing follows: {:?}", e);
            }
        }
    });

    Ok(())
}

/// Listen to the NATS jetstream for updates, and replace the watched users list
async fn listen_to_watched_forever(
    kv_store: Store,
    watched_users: SharedWatchedUsers,
    pg_pool: DBPool,
) {
    let result = timeout(
        Duration::from_secs(45),
        update_watched_users(watched_users.clone(), pg_pool.clone()),
    )
    .await;
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            error!("Error updating watched users on startup: {:?}", e);
        }
        Err(_) => {
            error!("Timeout waiting for watched users on startup.");
        }
    }
    loop {
        let mut messages = match kv_store.watch("watched_users").await {
            Ok(messages) => messages,
            Err(e) => {
                error!("Error watching watched_users: {:?}", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        while let Ok(Some(_message)) = messages.try_next().await {
            let result = timeout(
                Duration::from_secs(30),
                update_watched_users(watched_users.clone(), pg_pool.clone()),
            )
            .await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    error!("Error updating watched users: {:?}", e);
                }
                Err(_) => {
                    error!("Timeout waiting for watched users.");
                }
            }
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
        .expect("Failed to create response"))
}

#[axum::debug_handler]
async fn rescan_missing(
    State(AxumState { pool: pg_pool }): State<AxumState>,
) -> Result<Response, StatusCode> {
    tokio::spawn(async move {
        match fill_in_missing_follows(pg_pool.clone()).await {
            Ok(_) => {}
            Err(e) => {
                error!("Error filling in missing follows: {:?}", e);
            }
        }
    });
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body("Started rescan job.".into())
        .expect("Failed to create response"))
}

async fn _main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let loki_url = std::env::var("LOKI_URL");
    if let Ok(loki_url) = loki_url {
        let environment = std::env::var("ENVIRONMENT").unwrap_or("dev".to_string());
        let (layer, task) = tracing_loki::builder()
            .label("environment", environment)?
            .label("service_name", "jetstream")?
            .extra_field("pid", format!("{}", std::process::id()))?
            .build_url(Url::parse(&loki_url).expect("Invalid Loki URL"))?;

        tracing_subscriber::registry()
            .with(layer.with_filter(tracing_subscriber::filter::EnvFilter::from_default_env()))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stdout)
                    .with_filter(tracing_subscriber::filter::EnvFilter::from_default_env()),
            )
            .with(sentry_tracing::layer())
            .init();

        tokio::spawn(task);
        tracing::info!("jetstream starting, loki tracing enabled.");
    } else {
        warn!("LOKI_URL not set, will not send logs to Loki");
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stdout)
                    .with_filter(tracing_subscriber::filter::EnvFilter::from_default_env()),
            )
            .with(sentry_tracing::layer())
            .init();
    }

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
            duplicate_window: OLDEST_POST_AGE.to_std().expect("Could not convert OldestPostAge to std"),
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

    let kv_store = nats_js.get_key_value("bluenotify_kv_store").await.expect("Failed to get kv store");

    let shared_watched_users = Arc::new(RwLock::new(WatchedUsers {
        watched_users: HashSet::new(),
        bluenotify_users: HashSet::new(),
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
    let axum_listener = tokio::net::TcpListener::bind(axum_url).await.expect("Failed to bind to address");

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
        axum::serve(axum_listener, axum_app).await.expect("Failed to start server");
    });

    tasks.spawn(async move {
        fill_in_missing_follows(pg_pool.clone()).await.expect("Failed to fill in missing follows");
    });

    tasks.join_all().await;
    Ok(())
}

fn main() {
    let sentry_dsn = std::env::var("SENTRY_DSN");
    if sentry_dsn.is_ok() && !sentry_dsn.as_ref().expect("SENTRY_DSN").is_empty() {
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
            .expect("Failed to build tokio runtime")
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
            .expect("Failed to build tokio runtime")
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
    encoder.encode(&metric_families, &mut buffer).expect("Failed to encode metrics");
    String::from_utf8(buffer).expect("Failed to convert metrics to string")
}
