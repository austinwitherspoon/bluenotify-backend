"""Module for getting data from Bluesky."""

import datetime

from async_utils import cache
from atproto import AsyncClient  # type: ignore
from atproto_client.models.app.bsky.actor.defs import ProfileViewDetailed  # type: ignore
from atproto_client.models.app.bsky.feed.post import GetRecordResponse as PostResponse  # type: ignore
from atproto_client.models.app.bsky.feed.repost import GetRecordResponse as RepostResponse  # type: ignore
from custom_types import BlueskyDid, BlueskyRKey

client = AsyncClient(base_url="https://public.api.bsky.app/")


ProfileCache: dict[BlueskyDid, tuple[datetime.datetime,]] = {}


async def get_post(did: BlueskyDid, rkey: BlueskyRKey, repost: bool = False) -> PostResponse | RepostResponse:
    """Get a post from Bluesky."""
    if repost:
        return await client.app.bsky.feed.repost.get(did, rkey)
    return await client.get_post(rkey, did)


@cache(expire=datetime.timedelta(hours=2))
async def get_user(did: BlueskyDid) -> ProfileViewDetailed:
    """Get a user from Bluesky, using the cache if available."""
    return await client.get_profile(did)


@cache(expire=datetime.timedelta(hours=1))
async def get_following(did: BlueskyDid) -> set[BlueskyDid]:
    """Get the users that a user is following."""
    follow_data = await client.get_follows(did)
    return {follow.did for follow in follow_data.follows}  # type: ignore


__all__ = ["get_post", "get_user", "get_following", "ProfileViewDetailed", "PostResponse", "RepostResponse"]
