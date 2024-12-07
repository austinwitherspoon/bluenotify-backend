import datetime
import re
from typing import TypedDict

CREATED_AT_REGEX = re.compile(
    r"(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{1,2})T(?P<hour>\d{2}):(?P<minute>\d{2}):(?P<second>\d{2})(\.(?P<microsecond>\d+))?((?P<offset>[+-]\d+:\d+))?[Zz]?"
)


class CreatedAtMatch(TypedDict):
    year: str
    month: str
    day: str
    hour: str
    minute: str
    second: str
    microsecond: str | None
    offset: str | None


def parse_created_at(time: str) -> datetime.datetime:
    """Parse the created_at time from the Bluesky API."""
    # formats:
    # 2024-12-07T05:48:21.260Z
    # 2024-12-07T05:43:53.0557218Z
    # 2024-12-07T05:48:07+00:00
    # 2024-12-04T10:54:13-06:00
    # WHY ARE THESE SO INCONSISTENT?!

    result = CREATED_AT_REGEX.search(time)
    if not result:
        raise ValueError(f"Could not parse time: {time}")
    groups: CreatedAtMatch = result.groupdict()  # type: ignore
    year = int(groups["year"])
    month = int(groups["month"])
    day = int(groups["day"])
    hour = int(groups["hour"])
    minute = int(groups["minute"])
    second = int(groups["second"])
    # we're going to ignore microseconds because they're a nightmare
    # raw_microsecond = groups["microsecond"]

    raw_offset = groups["offset"]

    offset = datetime.timedelta()
    if raw_offset:
        add = raw_offset[0] == "+"
        hours, minutes = map(int, raw_offset[1:].split(":"))
        offset = datetime.timedelta(hours=hours, minutes=minutes)
        if add:
            offset = -offset
    return datetime.datetime(year, month, day, hour, minute, second, tzinfo=datetime.timezone.utc) + offset
