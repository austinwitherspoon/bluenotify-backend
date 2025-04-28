use again::RetryPolicy;
use async_nats::jetstream::kv::Store;
use async_nats::jetstream::stream::Stream;
use async_nats::jetstream::Message;
use axum::{routing::get, Router};
use bluesky_utils::{bluesky_browseable_url, parse_created_at, parse_uri};
use diesel::dsl::exists;
use futures::{StreamExt, TryStreamExt};
use lazy_static::lazy_static;
use prometheus::{
    self, register_histogram, register_int_counter, register_int_gauge, Encoder, Histogram,
    IntCounter, IntGauge, TextEncoder,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::{debug, error, info, span, warn, Instrument, Level};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;
mod fcm;
use crate::fcm::FcmClient;
use database_schema::{get_pool, notifications, run_migrations, DBPool, NewNotification};
use url::Url;

use database_schema::diesel::prelude::*;
use database_schema::diesel_async::RunQueryDsl;

const MAX_NOTIFICATION_AGE: i64 = 30;
const MAX_USER_NOTIFICATIONS: usize = 100;

const NSFW_LABELS: [&str; 7] = [
    "!hide",
    "!warn",
    "!no-unauthenticated",
    "porn",
    "sexual",
    "graphic-media",
    "nudity",
];

const OLDEST_POST_AGE: chrono::Duration = chrono::Duration::hours(2);
lazy_static! {
    static ref RECEIVED_MESSAGES_COUNTER: IntCounter = register_int_counter!(
        "received_messages",
        "The number of messages received by the server"
    )
    .unwrap();
    static ref NOTIFICATIONS_SENT_COUNTER: IntCounter = register_int_counter!(
        "notifications_sent",
        "The number of notifications sent by the server"
    )
    .unwrap();
    static ref POST_HANDLE_TIME: Histogram = register_histogram!(
        "post_handle_time_seconds",
        "Time spent handling a post",
        vec![
            0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 12.5, 15.0, 17.5, 20.0, 30.0,
            40.0, 50.0, 60.0, 120.0, 180.0, 240.0, 300.0, 360.0
        ]
    )
    .unwrap();
    static ref TOKIO_ALIVE_TASKS: IntGauge = register_int_gauge!(
        "tokio_alive_tasks",
        "The number of living tasks in the tokio runtime"
    )
    .unwrap();
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JetstreamPost {
    did: String,
    time_us: u64, // microseconds timestamp
    commit: Commit,
    raw_json: Value,
}

impl JetstreamPost {
    fn post_datetime(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let record = &self.commit.record;
        let created_at = match record {
            Record::Post(post) => &post.created_at,
            Record::Repost(repost) => &repost.created_at,
        };
        let result = parse_created_at(created_at);
        if result.is_err() {
            return None;
        }
        Some(result.unwrap())
    }

    fn event_datetime(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::from_timestamp_micros(self.time_us as i64)
    }

    fn post_id(&self) -> String {
        let rkey = &self.commit.rkey;
        let did = &self.did;
        format!("{}:{}", did, rkey)
    }

    fn parse_raw_json(
        json_string: &str,
    ) -> Result<JetstreamPost, Box<dyn std::error::Error + Send + Sync>> {
        let json: Value = serde_json::from_str(json_string)?;
        Self::parse_value(json)
    }

    fn parse_value(json: Value) -> Result<JetstreamPost, Box<dyn std::error::Error + Send + Sync>> {
        let did = json["did"].as_str();
        if did.is_none() {
            return Err("did is missing".into());
        }
        let did = did.unwrap().to_string();
        let time_us = json["time_us"].as_u64();
        if time_us.is_none() {
            return Err("time_us is missing".into());
        }
        let time_us = time_us.unwrap();
        let rkey = json["commit"]["rkey"].as_str();
        if rkey.is_none() {
            return Err("rkey is missing".into());
        }
        let rkey = rkey.unwrap().to_string();
        let raw_record = json["commit"]["record"].clone();

        let record = if raw_record["$type"] == "app.bsky.feed.post" {
            Record::Post(serde_json::from_value(raw_record)?)
        } else if raw_record["$type"] == "app.bsky.feed.repost" {
            Record::Repost(serde_json::from_value(raw_record)?)
        } else {
            return Err("unknown record type".into());
        };
        Ok(JetstreamPost {
            did,
            time_us,
            commit: Commit { rkey, record },
            raw_json: json,
        })
    }

    fn get_embed_headline(&self) -> Option<String> {
        return self.raw_json["commit"]["record"]["embed"]["external"]["title"]
            .as_str()
            .map(|s| s.to_string());
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
enum PostType {
    Post,
    Repost,
    Reply,
    Quote,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Commit {
    // rev: String,
    // operation: String,
    // collection: String,
    rkey: String,
    // cid: String,
    record: Record,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
enum Record {
    Post(PostRecord),
    Repost(RepostRecord),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PostRecord {
    #[serde(alias = "$type")]
    record_type: String,
    #[serde(alias = "createdAt")]
    created_at: String,
    text: String,
    embed: Option<Embed>,
    reply: Option<ReplyData>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RepostRecord {
    #[serde(alias = "$type")]
    record_type: String,
    #[serde(alias = "createdAt")]
    created_at: String,
    subject: RecordReference,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RecordReference {
    cid: String,
    uri: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Embed {
    #[serde(alias = "$type")]
    embed_type: String,
    record: Option<Value>,
    media: Option<Media>,
    external: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Media {
    #[serde(alias = "$type")]
    media_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReplyData {
    parent: RecordReference,
}

async fn get_bluesky_display_name_and_handle(
    did: &str,
    client: Option<reqwest::Client>,
    cache: Option<Store>,
) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    let cache_key = format!("display_name.{}", did.replace(":", "_"));

    if let Some(cache) = cache.clone() {
        debug!(
            "Checking cache for handle for {:?}: Key: {:?}",
            did, cache_key
        );
        let cached = cache.get(&cache_key).await;
        if let Ok(cached) = cached {
            if let Some(cached) = cached {
                let cached = String::from_utf8(cached.into());
                if cached.is_err() {
                    let msg: String = cached.unwrap_err().to_string();
                    error!("Error getting handle, corrupt cache: {}", msg);
                } else {
                    debug!("Using cached handle for {:?}", did);
                    let (display, handle): (String, String) =
                        serde_json::from_str(&cached.unwrap()).unwrap();
                    return Ok((display, handle));
                }
            } else {
                debug!("No cached handle for {:?}", did);
            }
        } else {
            let msg: String = cached.unwrap_err().to_string();
            error!("Error getting handle from cache: {}", msg);
        }
    }

    debug!("Getting handle for {:?}", did);
    let start = chrono::Utc::now();
    let url = format!(
        "https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor={}",
        did
    );
    let client = client.unwrap_or_else(|| reqwest::Client::new());
    let response = client.get(&url).send().await;
    if response.is_err() {
        let msg: String = response.unwrap_err().to_string();
        error!("Error getting handle: {}", msg);
        return Err(msg.into());
    }
    let response = response.unwrap();
    if !response.status().is_success() {
        let msg: String = response.error_for_status().unwrap_err().to_string();
        error!("Error getting handle: {}", msg);
        return Err(msg.into());
    }
    let json: Value = response.json().await.unwrap();
    let handle = json["handle"].as_str().unwrap_or("[No handle]");
    let mut display = json["displayName"].as_str().unwrap_or("");

    if display.is_empty() {
        display = handle;
    }

    let end = chrono::Utc::now();
    let duration = end - start;
    debug!("Got handle for {:?} in {:?}", did, duration);

    let result = (display.to_string(), handle.to_string());

    if let Some(cache) = cache {
        _ = cache
            .put(&cache_key, serde_json::to_string(&result).unwrap().into())
            .await;
    }

    Ok(result)
}

async fn load_bluesky_post(
    uri: &str,
    client: Option<reqwest::Client>,
) -> Result<JetstreamPost, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "https://public.api.bsky.app/xrpc/app.bsky.feed.getPosts?uris={}",
        uri
    );
    let client = client.unwrap_or_else(|| reqwest::Client::new());
    let response = client.get(&url).send().await;
    if response.is_err() {
        let msg: String = response.unwrap_err().to_string();
        error!("Error getting post: {}", msg);
        return Err(msg.into());
    }
    let response = response?;
    if !response.status().is_success() {
        let msg: String = response.error_for_status().unwrap_err().to_string();
        error!("Error getting post: {}", msg);
        return Err(msg.into());
    }
    let raw_json_response: Value = response.json().await?;

    if raw_json_response["posts"][0]["record"].is_null() {
        return Err("Post not found.".into());
    }

    let record: Result<PostRecord, _> =
        serde_json::from_value(raw_json_response["posts"][0]["record"].clone());
    if record.is_err() {
        error!("Error deserializing post: {:?}", record);
        return Err(format!("Error deserializing post: {:?}", record).into());
    }
    let (did, rkey) = parse_uri(uri).unwrap_or(("None".to_string(), "None".to_string()));

    let post = JetstreamPost {
        did: did.clone(),
        time_us: 0,
        commit: Commit {
            rkey: rkey.clone(),
            record: Record::Post(record.unwrap()),
        },
        raw_json: serde_json::json!({
            "did": did,
            "time_us": 0,
            "commit": {
                "rkey": rkey,
                "record": raw_json_response["posts"][0]["record"].clone()
            }
        }),
    };

    Ok(post)
}

async fn load_post_image(
    uri: &str,
    client: Option<reqwest::Client>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!(
        "https://public.api.bsky.app/xrpc/app.bsky.feed.getPosts?uris={}",
        uri
    );
    let client = client.unwrap_or_else(|| reqwest::Client::new());
    let response = client.get(&url).send().await;
    if response.is_err() {
        let msg: String = response.unwrap_err().to_string();
        warn!("Error getting post: {}", msg);
        return Err(msg.into());
    }
    let response = response?;
    if !response.status().is_success() {
        let msg: String = response.error_for_status().unwrap_err().to_string();
        warn!("Error getting post: {}", msg);
        return Err(msg.into());
    }
    let raw_json_response: Value = response.json().await?;

    let labels = raw_json_response["posts"][0]["labels"]
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .map(|label| label["val"].as_str().unwrap_or(""))
                .collect::<Vec<&str>>()
        });
    if let Some(labels) = labels {
        for label in labels {
            if NSFW_LABELS.contains(&label) {
                return Ok(None);
            }
        }
    }

    let image = raw_json_response["posts"][0]["embed"]["images"][0]["thumb"].as_str();
    if let Some(image) = image {
        return Ok(Some(image.to_string()));
    }
    let image = raw_json_response["posts"][0]["embed"]["media"]["images"][0]["thumb"].as_str();
    if let Some(image) = image {
        return Ok(Some(image.to_string()));
    }

    // try to get embedded media thumbnail
    let image = raw_json_response["posts"][0]["embed"]["external"]["thumb"].as_str();
    if let Some(image) = image {
        return Ok(Some(image.to_string()));
    }

    Ok(None)
}

async fn process_post(
    post: JetstreamPost,
    nats_message: Option<Message>,
    fcm_client: Option<Arc<FcmClient>>,
    cache: Store,
    pg: DBPool,
) {
    info!("Processing post: {:?}", post.post_id());
    let poster_did = post.did.clone();
    let mut source_post = post.clone();

    let client = reqwest::Client::new();

    let retry = RetryPolicy::fixed(Duration::from_millis(5000))
        .with_max_retries(10)
        .with_jitter(false);

    let post_type = match &post.commit.record {
        Record::Post(record) => {
            if record.reply.is_some() {
                PostType::Reply
            } else if record.embed.is_some()
                && record
                    .embed
                    .as_ref()
                    .unwrap()
                    .embed_type
                    .contains("app.bsky.embed.record")
            {
                PostType::Quote
            } else {
                PostType::Post
            }
        }
        Record::Repost(record) => {
            source_post = match retry
                .retry(|| {
                    timeout(
                        Duration::from_secs(60 * 1),
                        load_bluesky_post(&record.subject.uri, Some(client.clone())),
                    )
                })
                .await
            {
                Ok(post) => match post {
                    Ok(post) => post,
                    Err(e) => {
                        if format!("{:?}", e).contains("Post not found.") {
                            warn!("Post not found: {:?}", e);
                            return;
                        }
                        error!("Error loading Repost: {:?}", e);
                        return;
                    }
                },
                Err(e) => {
                    error!("Loading Repost timed out: {:?}", e);
                    return;
                }
            };
            PostType::Repost
        }
    };

    let source_post_record = match &source_post.commit.record {
        Record::Post(record) => record,
        Record::Repost(_) => return, // Shouldn't happen, can't repost a repost, and we should have loaded the source
    };

    info!("Post type: {:?}", post_type);

    // check if this post has any interested listeners BEFORE downloading anything else
    let fcm_recipients: HashSet<String> = {
        let con = pg.get().await;
        if con.is_err() {
            error!("Error getting connection");
            return;
        }
        let mut con = con.unwrap();

        use database_schema::schema::{notification_settings, users};

        let relevant_users = match post_type {
            PostType::Post | PostType::Quote | PostType::Repost => {
                let poster_did = poster_did.clone();
                let db_post_type = match post_type {
                    PostType::Post => vec![database_schema::models::NotificationType::Post],
                    PostType::Quote => vec![database_schema::models::NotificationType::Post],
                    PostType::Repost => vec![database_schema::models::NotificationType::Repost],
                    _ => unreachable!(),
                };
                let results = notification_settings::table
                    .inner_join(users::table)
                    .filter(notification_settings::following_did.eq(poster_did))
                    .filter(notification_settings::post_type.overlaps_with(db_post_type))
                    .filter(users::deleted_at.is_null())
                    .select((
                        users::fcm_token,
                        <database_schema::models::UserSetting>::as_select(),
                    ))
                    .load::<(String, database_schema::models::UserSetting)>(&mut con)
                    .await;
                if results.is_err() {
                    error!("Error loading users: {:?}", results);
                    return;
                }
                results.unwrap()
            }
            PostType::Reply => {
                let poster_did = poster_did.clone();
                let parent_poster_did = match &post.commit.record {
                    Record::Post(record) => {
                        let uri = record.reply.as_ref().unwrap().parent.uri.clone();
                        parse_uri(&uri).unwrap().0
                    }
                    _ => return,
                };

                let results = notification_settings::table
                    .inner_join(users::table)
                    .filter(notification_settings::following_did.eq(poster_did))
                    .filter(
                        notification_settings::post_type.overlaps_with(
                                vec![
                                    database_schema::models::NotificationType::Reply,
                                ],
                            ).or(
                            notification_settings::post_type.overlaps_with(
                                vec![
                                    database_schema::models::NotificationType::ReplyToFriend,
                                ],
                            ).and(exists(
                                database_schema::schema::account_follows::table.filter(
                                    database_schema::schema::account_follows::dsl::account_did
                                        .eq(database_schema::schema::notification_settings::dsl::user_account_did),
                                ).filter(
                                    database_schema::schema::account_follows::dsl::follow_did
                                        .eq(parent_poster_did),
                                ),
                            )))
                    )
                    .filter(users::deleted_at.is_null())
                    .select((
                        users::fcm_token,
                        <database_schema::models::UserSetting>::as_select(),
                    ))
                    .load::<(String, database_schema::models::UserSetting)>(&mut con)
                    .await;
                if results.is_err() {
                    error!("Error loading users: {:?}", results);
                    return;
                }
                results.unwrap()
            }
        };

        let mut results = HashSet::new();
        for (fcm_token, user_settings) in relevant_users.iter() {
            if let Some(allowed_words) = &user_settings.word_allow_list {
                if !allowed_words.is_empty() {
                    let mut found = false;
                    for word in allowed_words.iter() {
                        if source_post_record
                            .text
                            .to_lowercase()
                            .contains(&word.to_lowercase())
                        {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        continue;
                    }
                }
            }

            if let Some(blocked_words) = &user_settings.word_block_list {
                let mut found = false;
                for word in blocked_words.iter() {
                    if source_post_record
                        .text
                        .to_lowercase()
                        .contains(&word.to_lowercase())
                    {
                        found = true;
                        break;
                    }
                }
                if found {
                    continue;
                }
            }
            results.insert(fcm_token.clone());
        }
        results
    };

    if fcm_recipients.is_empty() {
        info!("No interested listeners for this post");
        if nats_message.is_some() {
            let ack = nats_message.unwrap().ack().await;
            if ack.is_err() {
                error!("Error acknowledging message: {:?}", ack);
                return;
            }
        }
        return;
    }
    info!("Interested listeners found: {:?}", fcm_recipients);

    let user_name = retry
        .retry(|| {
            timeout(
                Duration::from_secs(60 * 1),
                get_bluesky_display_name_and_handle(
                    &poster_did,
                    Some(client.clone()),
                    Some(cache.clone()),
                ),
            )
        })
        .await
        .unwrap_or(Ok(("[error loading user]".to_string(), "".to_string())))
        .unwrap_or(("[error loading user]".to_string(), "".to_string()))
        .0;

    let mut notification_title;

    match post_type {
        PostType::Post => {
            notification_title = format!("{} posted:", user_name);
        }
        PostType::Repost => {
            notification_title = format!("{} reposted:", user_name);
        }
        PostType::Quote => {
            notification_title = format!("{} quote posted:", user_name);
        }
        PostType::Reply => {
            // fallback title
            notification_title = format!("{} replied:", user_name);
            match &source_post.commit.record {
                Record::Post(record) => {
                    let reply_data = record.reply.as_ref().unwrap();
                    let uri = &reply_data.parent.uri;
                    let source_did = parse_uri(uri).unwrap().0;
                    let other_username = retry
                        .retry(|| {
                            timeout(
                                Duration::from_secs(60 * 1),
                                get_bluesky_display_name_and_handle(
                                    &source_did,
                                    Some(client.clone()),
                                    Some(cache.clone()),
                                ),
                            )
                        })
                        .await
                        .unwrap_or(Ok(("[error loading user]".to_string(), "".to_string())))
                        .unwrap_or(("[error loading user]".to_string(), "".to_string()))
                        .0;
                    notification_title = format!("{} replied to {}:", user_name, other_username);
                }
                _ => {}
            }
        }
    }

    let source_post_record = match &source_post.commit.record {
        Record::Post(record) => record,
        Record::Repost(_) => return, // Shouldn't happen, can't repost a repost, and we should have loaded the source
    };

    let mut notification_body = source_post_record.text.clone();

    let image: Option<String> = {
        info!("Loading post image..");
        let post_uri = format!(
            "at://{}/app.bsky.feed.post/{}",
            source_post.did, source_post.commit.rkey
        );
        retry
            .retry(|| timeout(Duration::from_secs(20), load_post_image(&post_uri, None)))
            .await
            .unwrap_or(Ok(None))
            .unwrap_or(None)
    };

    let has_external_link = source_post_record
        .embed
        .as_ref()
        .map_or(false, |embed| embed.embed_type.contains("external"));

    let is_gif = (source_post_record
        .embed)
        .as_ref()
        .map_or(false, |embed| embed.external.as_ref().map_or(false, |external| {
            external["uri"].as_str().map_or(false, |uri| {
                uri.contains(".gif")
            })
        }));

    if has_external_link {
        if is_gif {
            notification_body = "[gif]".to_string();
        } else if let Some(headline) = source_post.get_embed_headline() {
            let headline_text = format!("Link: {}", headline);
            if notification_body.is_empty() {
                notification_body = headline_text;
            } else {
                notification_body = format!("{}\n{}", notification_body, headline_text);
            }
        }
    }

    if notification_body.is_empty() && source_post_record.embed.is_some() {
        let mut media_type = source_post_record
            .embed
            .as_ref()
            .unwrap()
            .embed_type
            .clone();
        // if the user is quoting a post, embed is nested
        if source_post_record.embed.as_ref().unwrap().media.is_some() {
            media_type = source_post_record
                .embed
                .as_ref()
                .unwrap()
                .media
                .as_ref()
                .unwrap()
                .media_type
                .clone();
        }
        if media_type.contains("image") {
            if image.is_none() {
                notification_body = "[image]".to_string();
            }
        } else if media_type.contains("video") {
            notification_body = "[video]".to_string();
        } else if media_type.contains("external") {
            notification_body = "[link]".to_string();
        }
    }

    let post_owner_handle = retry
        .retry(|| {
            timeout(
                Duration::from_secs(60 * 1),
                get_bluesky_display_name_and_handle(
                    &source_post.did,
                    Some(client.clone()),
                    Some(cache.clone()),
                ),
            )
        })
        .await
        .unwrap_or(Ok(("".to_string(), (&source_post.did.clone()).to_owned())))
        .unwrap_or(("".to_string(), (&source_post.did.clone()).to_owned()))
        .1;

    let rkey = source_post.commit.rkey.clone();

    let url = bluesky_browseable_url(&post_owner_handle, &rkey);

    if nats_message.is_some() {
        let ack = nats_message.unwrap().ack().await;
        if ack.is_err() {
            error!("Error acknowledging message: {:?}", ack);
            return;
        }
    }
    NOTIFICATIONS_SENT_COUNTER.inc_by(fcm_recipients.len() as u64);

    let post_time = post.event_datetime();
    if let Some(post_time) = post_time {
        let handle_time = (chrono::Utc::now() - post_time).num_milliseconds() as f64 / 1000.0;
        info!("Post handle time: {:?}", handle_time);
        POST_HANDLE_TIME.observe(handle_time);
    }

    send_notification(
        fcm_client,
        fcm_recipients,
        notification_title,
        notification_body,
        image,
        url,
        pg,
    )
    .await;
}

async fn update_db_notifications(
    fcm_recipients: HashSet<String>,
    title: String,
    body: String,
    image: Option<String>,
    url: String,
    pg_pool: DBPool,
) {
    let pool = pg_pool.get().await;
    if pool.is_err() {
        error!("Error getting pg connection!");
    } else {
        let mut new_notifications = Vec::new();
        for fcm_token in &fcm_recipients {
            new_notifications.push(NewNotification {
                user_id: fcm_token.clone(),
                title: title.clone(),
                body: body.clone(),
                url: url.clone(),
                image: image.clone(),
            });
        }

        let mut con = pool.unwrap();
        let res = diesel::insert_into(notifications::table)
            .values(&new_notifications)
            .execute(&mut con)
            .await;
        if res.is_err() {
            error!("Error inserting notifications: {:?}", res);
        }
        // remove notifications older than 30 days
        let res = diesel::delete(
            notifications::table.filter(
                notifications::dsl::created_at
                    .lt(chrono::Utc::now().naive_utc()
                        - chrono::Duration::days(MAX_NOTIFICATION_AGE)),
            ),
        )
        .execute(&mut con)
        .await;
        if res.is_err() {
            error!("Error deleting old notifications: {:?}", res);
        }
        for fcm_token in &fcm_recipients {
            let notifications_inner = diesel::alias!(notifications as notifs_inner);

            let result = diesel::delete(
                notifications::table.filter(
                    notifications::dsl::user_id.eq(fcm_token.clone()).and(
                        notifications::dsl::id.ne_all(
                            notifications_inner
                                .select(notifications_inner.field(notifications::dsl::id))
                                .filter(
                                    notifications_inner
                                        .field(notifications::dsl::user_id)
                                        .eq(fcm_token.clone()),
                                )
                                .order_by(
                                    notifications_inner
                                        .field(notifications::dsl::created_at)
                                        .desc(),
                                )
                                .limit(MAX_USER_NOTIFICATIONS as i64),
                        ),
                    ),
                ),
            )
            .execute(&mut con)
            .await;
            if result.is_err() {
                error!("Error deleting old notifications: {:?}", result);
            }
        }
    }
}

async fn send_notification(
    fcm_client: Option<Arc<FcmClient>>,
    fcm_recipients: HashSet<String>,
    title: String,
    body: String,
    image: Option<String>,
    url: String,
    pg: DBPool,
) {
    info!("Sending notification to {} users", fcm_recipients.len());
    info!("Title: {}", title);
    info!("Body: {}", body);
    info!("URL: {}", url);
    info!("Image: {:?}", image);
    let mock = std::env::var("MOCK")
        .unwrap_or("false".to_string())
        .to_ascii_lowercase()
        == "true";
    if mock || fcm_client.is_none() {
        warn!("Mock mode, will not send notification!");
        return;
    }
    {
        let fcm_recipients = fcm_recipients.clone();
        let title = title.clone();
        let body = body.clone();
        let image = image.clone();
        let url = url.clone();
        let pg = pg.clone();
        tokio::spawn(async move {
            update_db_notifications(fcm_recipients, title, body, image, url, pg).await;
        });
    }
    let fcm_client = fcm_client.unwrap();
    let mut message = serde_json::json!({
        "validate_only": false,
        "message": {
            "notification": {
                "title": title,
                "body": body
            },
            "data": {
                "url": url,
                "type": "post",
                "text": body,
                "post_user_did": "n/a",
                "post_user_handle": "n/a",
                "post_id": "n/a",
            },
            "android": {
                "priority": "high",
                "notification": {
                    "proxy": "DENY",
                }
            },
            "apns": {
                "payload": {
                    "aps": {
                        "alert": {
                            "title": title,
                            "body": body
                        },
                        "sound": "default"
                    }
                }
            }
        }
    });
    if let Some(image) = image {
        message["message"]["notification"]["image"] = Value::String(image);
    }

    let futures = fcm_recipients.into_iter().map(|fcm_token| {
        let fcm_client = fcm_client.clone();
        let pg = pg.clone();
        let fcm_token = fcm_token.clone();

        let mut message = message.clone();
        message["message"]["token"] = Value::String(fcm_token.clone());

        async move {
            match again::retry(|| {
                send_single_notification(
                    fcm_token.clone(),
                    message.clone(),
                    fcm_client.clone(),
                    pg.clone(),
                )
            })
            .await
            {
                Ok(_) => {
                    info!("Notification sent successfully to {:?}", fcm_token);
                }
                Err(error) => {
                    error!("Error sending notification: {:?}", error);
                }
            }
        }
    });
    let stream = futures::stream::iter(futures).buffer_unordered(10);
    stream.collect::<Vec<_>>().await;
}

async fn send_single_notification(
    fcm_token: String,
    message: serde_json::Value,
    fcm_client: Arc<FcmClient>,
    pg: DBPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let response = fcm_client.send(message.clone()).await;
    match response {
        Ok(_) => {
            info!("Notification sent successfully to {:?}", fcm_token);
        }
        Err(error) => match &error {
            fcm::FcmError::ResponseError(e) => {
                if e.error.code == 404 {
                    warn!("FCM token not found, user deleted. Removing from database.");
                    let mut con = pg.get().await.unwrap();
                    let result = diesel::update(database_schema::schema::users::table.filter(
                        database_schema::schema::users::dsl::fcm_token.eq(fcm_token.clone()),
                    ))
                    .set(
                        database_schema::schema::users::dsl::deleted_at
                            .eq(chrono::Utc::now().naive_utc()),
                    )
                    .execute(&mut con)
                    .await;
                    if result.is_err() {
                        error!("Error deleting notifications: {:?}", result);
                    }
                } else {
                    warn!("Token {:?}", fcm_token);
                    warn!("Payload: {:?}", message);
                    warn!("Error sending notification: {:?}", e);
                    return Err(error.into());
                }
            }
            _ => {
                warn!("Token {:?}", fcm_token);
                warn!("Payload: {:?}", message);
                warn!("Unknown Error sending notification: {:?}", error);
                return Err(error.into());
            }
        },
    }

    Ok(())
}

async fn listen_to_posts(
    posts_stream: Stream,
    fcm_client: Arc<FcmClient>,
    cache: Store,
    pg: DBPool,
) {
    let consumer = posts_stream
        .get_or_create_consumer(
            "watched_posts",
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("notifier_post_listener".to_string()),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
                ack_wait: Duration::from_secs(60 * 5),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let metrics = Handle::current().metrics();

    loop {
        let mut messages = consumer.messages().await.unwrap();
        while let Ok(Some(message)) = messages.try_next().await {
            let json_string = String::from_utf8_lossy(message.payload.as_ref());
            let post: Result<JetstreamPost, _> = JetstreamPost::parse_raw_json(&json_string);
            if post.is_err() {
                warn!(
                    "Failed to deserialize post: {:?}\nData: {}",
                    post, json_string
                );
                _ = message.ack().await;
                continue;
            }
            info!("Received post: {:?}", post);
            RECEIVED_MESSAGES_COUNTER.inc();

            let post = post.unwrap();
            let post_datetime = post.post_datetime();
            if post_datetime.is_none() {
                warn!("Error parsing post datetime");
                _ = message.ack().await;
                continue;
            }
            let now = chrono::Utc::now();
            if now - post_datetime.unwrap() > OLDEST_POST_AGE {
                // this post is too old, ignore it
                info!("Post is too old, ignoring");
                _ = message.ack().await;
                continue;
            }
            let post_id = post.post_id();
            tokio::spawn({
                let fcm_client = fcm_client.clone();
                let cache = cache.clone();
                let pg = pg.clone();
                debug!("Spawning process_post");
                async move {
                    let result = timeout(
                        Duration::from_secs(60 * 5),
                        process_post(post, Some(message), Some(fcm_client), cache, pg)
                            .instrument(span!(Level::INFO, "process", post_id = post_id.as_str())),
                    )
                    .await;
                    if result.is_err() {
                        error!("Processing timed out after 5 minutes: {:?}", result);
                    }
                }
            });

            TOKIO_ALIVE_TASKS.set(metrics.num_alive_tasks() as i64);
        }
    }
}

async fn metrics() -> String {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

async fn _main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let loki_url = std::env::var("LOKI_URL");
    if let Ok(loki_url) = loki_url {
        let environment = std::env::var("ENVIRONMENT").unwrap_or("dev".to_string());
        let (layer, task) = tracing_loki::builder()
            .label("environment", environment)?
            .label("service_name", "notifier")?
            .extra_field("pid", format!("{}", std::process::id()))?
            .build_url(Url::parse(&loki_url).unwrap())?;

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
        tracing::info!("Notifier starting, loki tracing enabled.");
    } else {
        warn!("LOKI_URL not set, will not send logs to Loki");
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(sentry_tracing::layer())
            .init();
    }

    TOKIO_ALIVE_TASKS.set(0);
    RECEIVED_MESSAGES_COUNTER.reset();
    NOTIFICATIONS_SENT_COUNTER.reset();

    run_migrations()?;

    let pg_pool = get_pool()?;

    let mut nats_host = std::env::var("NATS_HOST").unwrap_or("localhost".to_string());
    if !nats_host.contains(':') {
        nats_host.push_str(":4222");
    }

    let service_account_path = "./cert.json";
    // Create a new FCM client
    let fcm_client = Arc::new(FcmClient::new(service_account_path).await?);

    let axum_url = std::env::var("BIND_NOTIFIER").unwrap_or("0.0.0.0:8003".to_string());

    info!("Connecting to NATS at {}", nats_host);
    let nats_client = async_nats::connect(nats_host).await?;
    let nats_js = async_nats::jetstream::new(nats_client);

    // Get key/value store
    _ = nats_js
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "bluenotify_kv_store".to_string(),
            history: 1,
            ..Default::default()
        })
        .await;

    // Get cache
    _ = nats_js
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "bluenotify_cache".to_string(),
            history: 1,
            max_age: Duration::from_secs(60 * 60 * 1),
            ..Default::default()
        })
        .await;
    let cache = nats_js.get_key_value("bluenotify_cache").await.unwrap();

    let posts_stream = nats_js.get_stream("watched_posts").await?;

    let axum_app = Router::new()
        .route("/", get(|| async { "Notifier server online." }))
        .route("/metrics", get(metrics));
    info!("Listening for HTTP requests on {}", axum_url);
    let axum_listener = tokio::net::TcpListener::bind(axum_url).await.unwrap();

    let mut tasks: JoinSet<_> = JoinSet::new();
    tasks.spawn(async move {
        axum::serve(axum_listener, axum_app).await.unwrap();
    });
    tasks.spawn(async move {
        listen_to_posts(posts_stream, fcm_client.clone(), cache, pg_pool).await;
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
                attach_stacktrace: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_username() {
        let did = "did:plc:jpkjnmydclkafjyicv3s6hcx";
        let handle = get_bluesky_display_name_and_handle(did, None, None).await;
        assert_eq!(handle.unwrap().1, "austinwitherspoon.com");
    }
    #[tokio::test]
    async fn test_get_post() {
        let uri = "at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lhl4k52fek22";
        let post = load_bluesky_post(uri, None).await;
        println!("{:?}", post);
        assert!(post.is_ok());
    }

    #[tokio::test]
    async fn test_get_image() {
        let uri = "at://did:plc:vhwscbpufmtoekc5hyz73vpa/app.bsky.feed.post/3lcowibggzk2n";
        let image_url = load_post_image(uri, None).await.unwrap().unwrap();
        println!("{:?}", image_url);
        assert!(image_url.contains("https://cdn.bsky.app/img/feed_thumbnail/plain/"));

        let uri = "at://did:plc:vhwscbpufmtoekc5hyz73vpa/app.bsky.feed.post/3lcowy446uk27";
        let image_url = load_post_image(uri, None).await.unwrap().unwrap();
        println!("{:?}", image_url);
        assert!(image_url.contains("https://cdn.bsky.app/img/feed_thumbnail/plain/"));

        let uri = "at://did:plc:vhwscbpufmtoekc5hyz73vpa/app.bsky.feed.post/3lkrr752sxc26";
        let image_url = load_post_image(uri, None).await.unwrap().unwrap();
        println!("{:?}", image_url);
        assert!(image_url.contains("https://cdn.bsky.app/img/feed_thumbnail/plain/"));

        let nsfw_uri = "at://did:plc:kxrs3pexsaohn2j3d5thkr7r/app.bsky.feed.post/3ljuk522j7s23";
        let image_url = load_post_image(nsfw_uri, None).await.unwrap();
        println!("{:?}", image_url);
        assert!(image_url.is_none());

        let embedded_gif = "at://did:plc:kqbyr4gqt6p2l57htlsa4nha/app.bsky.feed.post/3lnuz6fqttk2g";
        let image_url = load_post_image(embedded_gif, None).await.unwrap().unwrap();
        println!("{:?}", image_url);
        assert!(image_url.contains("https://cdn.bsky.app/img/feed_thumbnail/plain/"));

    }

    #[test]
    fn test_models() {
        let inputs = vec![
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739074582960891,"kind":"commit","commit":{"rev":"3lhprhfyklu24","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhprhfeijk27","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T04:16:22.163Z","embed":{"$type":"app.bsky.embed.recordWithMedia","media":{"$type":"app.bsky.embed.images","images":[{"alt":"","aspectRatio":{"height":670,"width":739},"image":{"$type":"blob","ref":{"$link":"bafkreifyetr44wuxhi7gjzrcnvjpligg2awcvycml37tktvptfh6ylua4q"},"mimeType":"image/jpeg","size":392305}}]},"record":{"$type":"app.bsky.embed.record","record":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}}},"langs":["en"],"text":""},"cid":"bafyreigxjzf7hiygzsh4cnpndpfjoy5sbai3m5dsejnil2zq6ebqexlioy"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739075366266743,"kind":"commit","commit":{"rev":"3lhps6qxmsu2e","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhps6qvn3k27","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T04:29:26.015Z","embed":{"$type":"app.bsky.embed.record","record":{"cid":"bafyreib72cfeelnufyrp6ok373i5mcdgypq3ye4qu625p5q6m6grlzioyu","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacy7ydylc22"}},"langs":["en"],"reply":{"parent":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"},"root":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}},"text":""},"cid":"bafyreihbp6dtgxyg4q77tor3lyvgxy57c2s3zozuvbg3semjfr45hdbkji"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739070925289753,"kind":"commit","commit":{"rev":"3lhpo2fqaxw2q","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhpo2fqeik2q","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T03:15:25.122Z","embed":{"$type":"app.bsky.embed.record","record":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}},"langs":["en"],"text":"Quote Post"},"cid":"bafyreibw7kojbp3hwxvnzueihuz4draatigiius2axg5vhablylifstlbu"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739120673659627,"kind":"commit","commit":{"rev":"3lhr4ezinfv2q","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhr4ezad322p","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T17:04:33.236Z","langs":["en"],"text":"test"},"cid":"bafyreih5742ytclvqqmxtspocjnb7xrp3afxp6x4pwlm72fp6nb4wgw3te"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739074582960891,"kind":"commit","commit":{"rev":"3lhprhfyklu24","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhprhfeijk27","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T04:16:22.163Z","embed":{"$type":"app.bsky.embed.recordWithMedia","media":{"$type":"app.bsky.embed.images","images":[{"alt":"","aspectRatio":{"height":670,"width":739},"image":{"$type":"blob","ref":{"$link":"bafkreifyetr44wuxhi7gjzrcnvjpligg2awcvycml37tktvptfh6ylua4q"},"mimeType":"image/jpeg","size":392305}}]},"record":{"$type":"app.bsky.embed.record","record":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}}},"langs":["en"],"text":""},"cid":"bafyreigxjzf7hiygzsh4cnpndpfjoy5sbai3m5dsejnil2zq6ebqexlioy"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739070925289753,"kind":"commit","commit":{"rev":"3lhpo2fqaxw2q","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhpo2fqeik2q","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T03:15:25.122Z","embed":{"$type":"app.bsky.embed.record","record":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}},"langs":["en"],"text":"Quote Post"},"cid":"bafyreibw7kojbp3hwxvnzueihuz4draatigiius2axg5vhablylifstlbu"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739074582960891,"kind":"commit","commit":{"rev":"3lhprhfyklu24","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhprhfeijk27","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T04:16:22.163Z","embed":{"$type":"app.bsky.embed.recordWithMedia","media":{"$type":"app.bsky.embed.images","images":[{"alt":"","aspectRatio":{"height":670,"width":739},"image":{"$type":"blob","ref":{"$link":"bafkreifyetr44wuxhi7gjzrcnvjpligg2awcvycml37tktvptfh6ylua4q"},"mimeType":"image/jpeg","size":392305}}]},"record":{"$type":"app.bsky.embed.record","record":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}}},"langs":["en"],"text":""},"cid":"bafyreigxjzf7hiygzsh4cnpndpfjoy5sbai3m5dsejnil2zq6ebqexlioy"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739070946025127,"kind":"commit","commit":{"rev":"3lhpo2zhudg2n","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhpo2zhmos2q","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T03:15:45.808Z","langs":["en"],"reply":{"parent":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"},"root":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}},"text":"test reply"},"cid":"bafyreib6l7rltqha76gbrcyezukpskh7yf5gtqpdxru72xcjuqabe5ckte"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739074686768901,"kind":"commit","commit":{"rev":"3lhprkiymb72t","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhprkidbgs27","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T04:18:05.931Z","embed":{"$type":"app.bsky.embed.images","images":[{"alt":"","aspectRatio":{"height":670,"width":739},"image":{"$type":"blob","ref":{"$link":"bafkreifyetr44wuxhi7gjzrcnvjpligg2awcvycml37tktvptfh6ylua4q"},"mimeType":"image/jpeg","size":392305}}]},"langs":["en"],"reply":{"parent":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"},"root":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}},"text":""},"cid":"bafyreihlqnfjcmph2w3prstvc35lbrmte6yu4kd5b2war5s6qu6efxplq4"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739075366266743,"kind":"commit","commit":{"rev":"3lhps6qxmsu2e","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhps6qvn3k27","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T04:29:26.015Z","embed":{"$type":"app.bsky.embed.record","record":{"cid":"bafyreib72cfeelnufyrp6ok373i5mcdgypq3ye4qu625p5q6m6grlzioyu","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacy7ydylc22"}},"langs":["en"],"reply":{"parent":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"},"root":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}},"text":""},"cid":"bafyreihbp6dtgxyg4q77tor3lyvgxy57c2s3zozuvbg3semjfr45hdbkji"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1739075494566375,"kind":"commit","commit":{"rev":"3lhpsckuobb2q","operation":"create","collection":"app.bsky.feed.post","rkey":"3lhpscjtnvs27","record":{"$type":"app.bsky.feed.post","createdAt":"2025-02-09T04:31:32.828Z","embed":{"$type":"app.bsky.embed.external","external":{"description":"ALT: a baby in a striped shirt is covering his face with his hand and says `` dear lort '' .","thumb":{"$type":"blob","ref":{"$link":"bafkreifl5e5bn4gxxnmwuhx6vuhjvppyyi6mg4vryrhtpxxdptkjvyzyx4"},"mimeType":"image/jpeg","size":403665},"title":"a baby in a striped shirt is covering his face with his hand and says `` dear lort '' .","uri":"https://media.tenor.com/rjjtn8dd0tgAAAAC/baby-facepalm.gif?hh=498&ww=498"}},"langs":["en"],"reply":{"parent":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"},"root":{"cid":"bafyreibbg2leljyfcurxfnscsozon7h7ta4hetheopctcejdsmpnfooy4q","uri":"at://did:plc:jpkjnmydclkafjyicv3s6hcx/app.bsky.feed.post/3lacsvjm7o62x"}},"text":""},"cid":"bafyreig4ckxykhrryffns2am7zbe7rzo4dxt6j427cucxy4ig4iczzrk4y"}}"#,
            r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1742444562673094,"kind":"commit","commit":{"rev":"3lkrtymntgd2s","operation":"create","collection":"app.bsky.feed.post","rkey":"3lkrtyku3mc2e","record":{"$type":"app.bsky.feed.post","createdAt":"2025-03-20T04:22:40.190Z","embed":{"$type":"app.bsky.embed.external","external":{"description":"Russian and Chinese channels only the 'tip of the iceberg', boosted by 'an extensive covert network' of state-linked channels","thumb":{"$type":"blob","ref":{"$link":"bafkreihxojdpg3wlvz3blhkk7kp7mluyu6kshc6wgleaqam2j2np3zlvzu"},"mimeType":"image/jpeg","size":589090},"title":"Disinformation by hostile states a 'threat to democracies'","uri":"https://www.irishexaminer.com/news/arid-41596729.html?utm_source=dlvr.it\u0026utm_medium=bluesky"}},"langs":["en"],"text":""},"cid":"bafyreihmrr5e634igohte23imabmsvuqmo6ukpmtz4tlpchp2dl6rqxioi"}}"#,
        ];

        for input in inputs {
            println!("Json Input: {}", input);
            let post: JetstreamPost = JetstreamPost::parse_raw_json(input).unwrap();
            println!("{:?}\n\n", post);
        }
    }

    #[tokio::test]
    async fn test_get_embed_headline() {
        let input = r#"{"did":"did:plc:vhwscbpufmtoekc5hyz73vpa","time_us":1742444562673094,"kind":"commit","commit":{"rev":"3lkrtymntgd2s","operation":"create","collection":"app.bsky.feed.post","rkey":"3lkrtyku3mc2e","record":{"$type":"app.bsky.feed.post","createdAt":"2025-03-20T04:22:40.190Z","embed":{"$type":"app.bsky.embed.external","external":{"description":"Russian and Chinese channels only the 'tip of the iceberg', boosted by 'an extensive covert network' of state-linked channels","thumb":{"$type":"blob","ref":{"$link":"bafkreihxojdpg3wlvz3blhkk7kp7mluyu6kshc6wgleaqam2j2np3zlvzu"},"mimeType":"image/jpeg","size":589090},"title":"Disinformation by hostile states a 'threat to democracies'","uri":"https://www.irishexaminer.com/news/arid-41596729.html?utm_source=dlvr.it\u0026utm_medium=bluesky"}},"langs":["en"],"text":""},"cid":"bafyreihmrr5e634igohte23imabmsvuqmo6ukpmtz4tlpchp2dl6rqxioi"}}"#;
        println!("Json Input: {}", input);
        let post: JetstreamPost = JetstreamPost::parse_raw_json(input).unwrap();
        println!("{:?}\n\n", post);
        let headline = post.get_embed_headline();
        println!("Headline: {:?}", headline);
        assert_eq!(
            headline,
            Some("Disinformation by hostile states a 'threat to democracies'".to_string())
        );

        let post: JetstreamPost = load_bluesky_post(
            "at://did:plc:annmh2aamt3ctc2gubsvi6dj/app.bsky.feed.post/3lkritvwbkg2t",
            None,
        )
        .await
        .unwrap();
        println!("{:?}\n\n", post);
        let headline = post.get_embed_headline();
        println!("Headline: {:?}", headline);
        assert_eq!(
            headline,
            Some("Disinformation by hostile states a 'threat to democracies'".to_string())
        );
    }
}
