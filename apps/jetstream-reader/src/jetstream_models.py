from __future__ import annotations

import datetime

from custom_types import BlueskyCid, BlueskyDid
from pydantic import BaseModel, Field


class BlueskyBaseModel(BaseModel):
    class Config:
        extra = "allow"


class Record(BlueskyBaseModel):
    type: str = Field(..., alias="$type")  # type: ignore
    createdAt: str  # noqa: N815


class Commit(BlueskyBaseModel):
    rev: str
    operation: str
    collection: str
    rkey: str
    cid: BlueskyCid | None = None
    record: Record | None = None


class Post(BlueskyBaseModel):
    did: BlueskyDid
    time_us: int
    commit: Commit | None = None

    @property
    def event_datetime(self) -> datetime.datetime | None:
        return datetime.datetime.fromtimestamp(self.time_us / 1_000_000.0, datetime.UTC)

    @property
    def post_datetime(self) -> datetime.datetime | None:
        if not self.commit or not self.commit.record:
            return None
        return parse_created_at(self.commit.record.createdAt)


def parse_created_at(time: str) -> datetime.datetime:
    """Parse the created_at time from the Bluesky API."""
    # WHY ARE THESE SO INCONSISTENT?!
    # formats:
    # 2024-12-07T05:48:21.260Z
    # 2024-12-07T05:43:53.0557218Z
    # 2024-12-07T05:48:07+00:00
    time = time.replace("+00:00", "Z")
    if "+" in time:
        return datetime.datetime.strptime(time, "%Y-%m-%dT%H:%M:%S%z")
    try:
        return datetime.datetime.strptime(time, "%Y-%m-%dT%H:%M:%S.%f%z")
    except ValueError:
        year, month, day = time.split("T")[0].split("-")
        hour, minute, second = time.split("T")[1].split("+")[0].split(":")
        if "." in second:
            second, microsecond = second.split(".")
        else:
            microsecond = "0"
        second = "".join([c for c in second if c.isdigit()])
        microsecond = "".join([c for c in microsecond if c.isdigit()])
        return datetime.datetime(
            int(year),
            int(month),
            int(day),
            int(hour),
            int(minute),
            int(second),
            int(microsecond),
            tzinfo=datetime.timezone.utc,
        )
