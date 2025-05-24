use chrono::{DateTime, TimeZone, Utc};
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;
use std::{collections::HashSet, error::Error};
use tracing::{debug, error, info, warn};

pub fn bluesky_browseable_url(handle: &str, rkey: &str) -> String {
    format!("https://bsky.app/profile/{}/post/{}", handle, rkey)
}

pub fn parse_uri(uri: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = uri.split('/').collect();
    if parts.len() < 5 {
        return None;
    }
    let handle = parts[2].to_string();
    let rkey = parts[4].to_string();
    Some((handle, rkey))
}

lazy_static! {
    static ref CREATED_AT_REGEX: Regex = Regex::new(
        r"(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{1,2})T(?P<hour>\d{2}):(?P<minute>\d{2}):(?P<second>\d{2})(?:\.(?P<microsecond>\d+))?(?:(?P<offset>[+-]\d+:\d+))?[Zz]?"
    ).unwrap();
}

#[derive(Debug)]
struct CreatedAtMatch<'a> {
    year: &'a str,
    month: &'a str,
    day: &'a str,
    hour: &'a str,
    minute: &'a str,
    second: &'a str,
    offset: Option<&'a str>,
}

pub fn parse_created_at(time: &str) -> Result<DateTime<Utc>, Box<dyn Error>> {
    // formats:
    // 2024-12-07T05:48:21.260Z
    // 2024-12-07T05:43:53.0557218Z
    // 2024-12-07T05:48:07+00:00
    // 2024-12-04T10:54:13-06:00

    let captures = CREATED_AT_REGEX
        .captures(time)
        .ok_or_else(|| format!("Could not parse time: {}", time))?;

    let groups = CreatedAtMatch {
        year: captures.name("year").unwrap().as_str(),
        month: captures.name("month").unwrap().as_str(),
        day: captures.name("day").unwrap().as_str(),
        hour: captures.name("hour").unwrap().as_str(),
        minute: captures.name("minute").unwrap().as_str(),
        second: captures.name("second").unwrap().as_str(),
        offset: captures.name("offset").map(|m| m.as_str()),
    };

    let year = groups.year.parse::<i32>()?;
    let month = groups.month.parse::<u32>()?;
    let day = groups.day.parse::<u32>()?;
    let hour = groups.hour.parse::<u32>()?;
    let minute = groups.minute.parse::<u32>()?;
    let second = groups.second.parse::<u32>()?;

    // Base datetime in UTC
    let mut datetime = Utc
        .with_ymd_and_hms(year, month, day, hour, minute, second)
        .unwrap();

    // Handle offset if present
    if let Some(raw_offset) = groups.offset {
        let add = raw_offset.starts_with('+');
        let parts: Vec<&str> = raw_offset[1..].split(':').collect();
        let hours: i64 = parts[0].parse()?;
        let minutes: i64 = parts[1].parse()?;

        let offset_seconds = (hours * 3600 + minutes * 60) * if add { -1 } else { 1 };
        datetime = datetime + chrono::Duration::seconds(offset_seconds);
    }

    Ok(datetime)
}


pub enum GetFollowsError {
    RequestError(reqwest::Error),
    StatusError(reqwest::StatusCode),
    TooManyFollows(u64),
    DisabledAccount,
}

impl std::fmt::Display for GetFollowsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GetFollowsError::RequestError(err) => write!(f, "Request error: {}", err),
            GetFollowsError::StatusError(status) => write!(f, "Status error: {}", status),
            GetFollowsError::TooManyFollows(count) => {
                write!(f, "Too many follows: {}", count)
            }
            GetFollowsError::DisabledAccount => write!(f, "Account is disabled"),
        }
    }
}

impl std::fmt::Debug for GetFollowsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GetFollowsError::RequestError(err) => write!(f, "Request error: {:?}", err),
            GetFollowsError::StatusError(status) => write!(f, "Status error: {:?}", status),
            GetFollowsError::TooManyFollows(count) => {
                write!(f, "Too many follows: {:?}", count)
            }
            GetFollowsError::DisabledAccount => write!(f, "Account is disabled"),
        }
    }
}

pub async fn get_following(
    did: &str,
    client: Option<reqwest::Client>,
    pause_between_requests: Option<std::time::Duration>,
) -> Result<HashSet<String>, GetFollowsError> {
    info!("Getting following for {:?}", did);
    let start = chrono::Utc::now();
    let client = client.unwrap_or_else(|| reqwest::Client::new());

    // First check and make sure they have less than 10,000 following, otherwise ignore
    // since this will choke the post in our system
    let profile_url = format!(
        "https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor={}",
        did
    );

    let response = client
        .get(&profile_url)
        .header("User-Agent", "BlueNotify Server")
        .send()
        .await;
    if response.is_err() {
        let err = response.unwrap_err();
        let msg: String = err.to_string();
        error!("Error getting profile: {}", msg);
        return Err(GetFollowsError::RequestError(err));
    }
    let response = response.unwrap();
    if !response.status().is_success() {
        
        let status = response.status();
        let response_json: Result<Value, _> = response.json().await;
        if let Ok(json) = response_json {
            let error = json["error"].as_str();
            let message = json["message"].as_str();
            if let Some(error_text) = error {
                if let Some(message_text) = message {
                    match error_text {
                        "AccountTakedown" | "AccountDeactivated" => {
                            return Err(GetFollowsError::DisabledAccount);
                        }
                        "InvalidRequest" => {
                            match message_text {
                                "Profile not found" => {
                                    return Err(GetFollowsError::DisabledAccount);
                                }
                                _ => {
                                    let msg: String = format!("Error getting profile: {} - {}", error_text, message_text);
                                    error!("{}", msg);
                                    return Err(GetFollowsError::StatusError(status));
                                }
                            }
                        }
                        _ => {
                            let msg: String = format!("Unknown Error getting profile: {} - {}", error_text, message_text);
                            error!("{}", msg);
                            return Err(GetFollowsError::StatusError(status));
                        }
                    }
                }
            }
            warn!("Failed Request Response JSON: {:?}", json);
        }
        error!("Error getting profile: {}", status);
        return Err(GetFollowsError::StatusError(status));
    }

    let json: Result<Value, _> = response.json().await;
    let mut following_count = 0;
    if let Ok(json) = json {
        following_count = json["followsCount"].as_u64().unwrap_or(0);
    }
    if following_count > 10_000 {
        return Err(GetFollowsError::TooManyFollows(following_count));
    }

    let mut cursor: Option<String> = None;
    let mut following = HashSet::new();
    loop {
        let url = format!(
            "https://public.api.bsky.app/xrpc/app.bsky.graph.getFollows?actor={}&limit=100&cursor={}",
            did,
            cursor.clone().unwrap_or_default()
        );
        debug!("Getting following from {:?}", url);
        let response = client
            .get(&url)
            .header("User-Agent", "BlueNotify Server")
            .send()
            .await;
        if response.is_err() {
            let err = response.unwrap_err();
            let msg: String = err.to_string();
            error!("Error getting following: {}", msg);
            return Err(GetFollowsError::RequestError(err));
        }
        let response = response.unwrap();
        if !response.status().is_success() {
            let status: String = response.error_for_status_ref().unwrap_err().to_string();
            error!("Error getting following: {}", status);
            return Err(GetFollowsError::StatusError(response.status()));
        }
        let json: Value = response.json().await.unwrap();
        for follower in json["follows"].as_array().unwrap() {
            following.insert(follower["did"].as_str().unwrap().to_string());
        }
        let cursor_value = json["cursor"].as_str();
        if cursor_value.is_none() {
            break;
        }
        cursor = Some(cursor_value.unwrap().to_string());
        if let Some(pause) = pause_between_requests {
            tokio::time::sleep(pause).await;
        }
    }

    let end = chrono::Utc::now();
    let duration = end - start;
    debug!("Got following for {:?} in {:?}", did, duration);

    Ok(following)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_parse_created_at() {
        let cases = vec![
            (
                "2024-12-07T05:48:21.260Z",
                Utc.with_ymd_and_hms(2024, 12, 7, 5, 48, 21).unwrap(),
            ),
            (
                "2024-12-07T05:43:53.0557218Z",
                Utc.with_ymd_and_hms(2024, 12, 7, 5, 43, 53).unwrap(),
            ),
            (
                "2024-12-07T05:48:07+00:00",
                Utc.with_ymd_and_hms(2024, 12, 7, 5, 48, 7).unwrap(),
            ),
            (
                "2024-12-07T18:55:09.507030+00:00",
                Utc.with_ymd_and_hms(2024, 12, 7, 18, 55, 9).unwrap(),
            ),
            (
                "2024-12-07T18:57:06.701504+00:00",
                Utc.with_ymd_and_hms(2024, 12, 7, 18, 57, 6).unwrap(),
            ),
            (
                "2024-12-07T19:06:18Z",
                Utc.with_ymd_and_hms(2024, 12, 7, 19, 6, 18).unwrap(),
            ),
            (
                "2024-12-07T14:50:38.1051195Z",
                Utc.with_ymd_and_hms(2024, 12, 7, 14, 50, 38).unwrap(),
            ),
            (
                "2024-12-04T10:54:13-06:00",
                Utc.with_ymd_and_hms(2024, 12, 4, 16, 54, 13).unwrap(),
            ),
            (
                "2024-12-06T17:17:18-05:00",
                Utc.with_ymd_and_hms(2024, 12, 6, 22, 17, 18).unwrap(),
            ),
        ];

        for (input, expected) in cases {
            let parsed = parse_created_at(input).unwrap();
            assert_eq!(parsed, expected);
        }
    }
    
    #[tokio::test]
    async fn test_follows() {
        let did = "did:plc:jpkjnmydclkafjyicv3s6hcx";
        let follows = get_following(did, None, None).await.expect("Failed to get following");
        println!("{:?}", follows);
        assert!(follows.len() > 60);

        // Test somebody with way too many following, ignore
        let did = "did:plc:w3xevyycvef7y4tqsojptrf5";
        let follows = get_following(did, None, None).await;
        println!("{:?}", follows);
        match follows {
            Err(GetFollowsError::TooManyFollows(count)) => {
                assert!(count > 10_000);
            }
            _ => {
                panic!("Expected TooManyFollows error, got {:?}", follows);
            }
        }

        // test somebody with taken down account
        let did = "did:plc:mxn56keus3cvwabpw4zr3f7h";
        let follows = get_following(did, None, None).await;
        println!("{:?}", follows);
        match follows {
            Err(GetFollowsError::DisabledAccount) => (),
            _ => {
                panic!("Expected DisabledAccount error, got {:?}", follows);
            }
        }

        // test somebody with deactivated account
        let did = "did:plc:znaukyuzxganntnzr5hgerzg";
        let follows = get_following(did, None, None).await;
        println!("{:?}", follows);
        match follows {
            Err(GetFollowsError::DisabledAccount) => (),
            _ => {
                panic!("Expected DisabledAccount error, got {:?}", follows);
            }
        }

        
        // non-existent account
        let did = "did:plc:aytdxezmiaweub6mek6zocmv";
        let follows = get_following(did, None, None).await;
        println!("{:?}", follows);
        match follows {
            Err(GetFollowsError::DisabledAccount) => (),
            _ => {
                panic!("Expected DisabledAccount error, got {:?}", follows);
            }
        }

    }
}
