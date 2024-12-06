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
    record: Record


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
        # format 2024-12-06T01:47:20.666416+00:00
        return datetime.datetime.strptime(self.commit.record.createdAt, "%Y-%m-%dT%H:%M:%S.%f%z")
