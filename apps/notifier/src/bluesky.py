"""Module for getting data from Bluesky."""

import datetime

import httpx
from async_utils import cache, retry
from atproto import AsyncClient  # type: ignore
from atproto_client.models.app.bsky.actor.defs import ProfileViewDetailed  # type: ignore
from atproto_client.models.app.bsky.feed.post import GetRecordResponse as PostResponse  # type: ignore
from atproto_client.models.app.bsky.feed.repost import GetRecordResponse as RepostResponse  # type: ignore
from custom_types import BlueskyDid, BlueskyRKey

client = AsyncClient(base_url="https://public.api.bsky.app/")

# override the default 5 second timeout in atproto's httpx client
bluesky_httpx = client.request._client
bluesky_httpx._timeout = httpx.Timeout(30.0)


ProfileCache: dict[BlueskyDid, tuple[datetime.datetime,]] = {}

@retry()
async def get_post(did: BlueskyDid, rkey: BlueskyRKey, repost: bool = False) -> PostResponse | RepostResponse:
    """Get a post from Bluesky."""
    if repost:
        return await client.app.bsky.feed.repost.get(did, rkey)
    return await client.get_post(rkey, did)


@retry()
@cache(expire=datetime.timedelta(hours=2))
async def get_user(did: BlueskyDid) -> ProfileViewDetailed:
    """Get a user from Bluesky, using the cache if available."""
    return await client.get_profile(did)


@retry()
@cache(expire=datetime.timedelta(hours=1))
async def get_following(did: BlueskyDid) -> set[BlueskyDid]:
    """Get the users that a user is following."""
    cursor = None
    results: set[BlueskyDid] = set()
    while True:
        results_length = len(results)
        follow_data = await client.get_follows(did, cursor=cursor, limit=50)
        if not follow_data.follows:
            break
        results.update({follow.did for follow in follow_data.follows})  # type: ignore
        cursor = follow_data.cursor
        if len(results) == results_length:
            break

    return results


__all__ = ["get_post", "get_user", "get_following", "ProfileViewDetailed", "PostResponse", "RepostResponse"]
