use chrono::{DateTime, TimeZone, Utc};
use lazy_static::lazy_static;
use regex::Regex;
use std::error::Error;

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
}
