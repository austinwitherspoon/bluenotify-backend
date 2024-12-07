"""Server that listens to the Bluesky Jetstream and forwards posts from interesting users to the notification server."""

import asyncio
import datetime
import json
import logging
import os
import random
from urllib.parse import urlparse

import httpx
from async_utils import retry, schedule_task
from custom_types import BlueskyDid
from websockets.asyncio.client import connect

from .jetstream_models import Post

logger = logging.getLogger(__name__)

# Users we're interested in posts from
INTERESTING_USERS: set[BlueskyDid] = set()

# The server to get users from and send posts to
NOTIFIER_SERVER = os.getenv("NOTIFIER_SERVER", "http://localhost:8000")


MAX_NOTIFICATION_TIME = datetime.timedelta(hours=1)

# All available bluesky-run public jetstream servers
JETSTREAM_SERVERS = {
    "jetstream1.us-west.bsky.network",
    "jetstream2.us-west.bsky.network",
    "jetstream1.us-east.bsky.network",
    "jetstream2.us-east.bsky.network",
}


async def stream_users_to_watch():
    """Stream changes to the users we're interested in posts from from the notifier server."""
    server_data = urlparse(NOTIFIER_SERVER)
    is_secure = server_data.scheme == "https"

    users_websocket = ("wss" if is_secure else "ws") + "://" + server_data.netloc + "/users_to_watch"
    logger.info(f"Connecting to notifier server: {users_websocket}")
    while True:
        INTERESTING_USERS.clear()
        try:
            async with connect(users_websocket) as websocket:
                logger.info("Connected to notifier server!")
                while message := await websocket.recv():
                    parsed = json.loads(message)
                    match parsed:
                        case {"action": "add", "user": user_did}:
                            INTERESTING_USERS.add(user_did)
                            logger.info(f"Adding user to listen list: {user_did}")
                        case {"action": "remove", "user": user_did}:
                            INTERESTING_USERS.discard(user_did)
                            logger.info(f"Removing user from listen list: {user_did}")
                        case _:
                            logger.error(f"Unknown message from notifier server: {message}")
        except Exception as e:
            logger.exception(e)
            await asyncio.sleep(1)

@retry(12, 5)
async def forward_post(post: Post):
    """Forward the post to our notification server."""
    if not post.commit:
        return
    async with httpx.AsyncClient() as client:
        if post.commit.collection == "app.bsky.feed.post":
            await client.post(f"{NOTIFIER_SERVER}/post/{post.did}/{post.commit.rkey}")
        elif post.commit.collection == "app.bsky.feed.repost":
            await client.post(f"{NOTIFIER_SERVER}/repost/{post.did}/{post.commit.rkey}")
        else:
            logger.error(f"Unknown post collection: {post.commit.collection}")


async def main() -> None:
    """Main coroutine to monitor the jetstream servers for posts."""
    schedule_task(stream_users_to_watch())

    # store the last update timestamp so we can resume from there if we disconnect
    last_update = None

    while True:
        jetstream_server = random.choice(list(JETSTREAM_SERVERS))
        websocket_url = (
            f"wss://{jetstream_server}/subscribe"
            "?wantedCollections=app.bsky.feed.post"
            "&wantedCollections=app.bsky.feed.repost"
        )
        if last_update:
            websocket_url += f"&since={last_update}"

        logger.info(f"Connecting to jetstream server: {websocket_url}")
        try:
            async with connect(websocket_url) as websocket:
                while message := await websocket.recv():  # type: ignore
                    assert isinstance(message, str)

                    # extract out the user DID from the message
                    did: BlueskyDid = message.split('"did":"')[1].split('"')[0]  # type: ignore

                    if did in INTERESTING_USERS:
                        logger.info(message)
                        model = Post.model_validate_json(message)
                        if not model.commit or not model.commit.record or model.commit.operation != "create":
                            continue
                        last_update = model.time_us

                        time = datetime.datetime.fromtimestamp(float(model.time_us) / 1_000_000.0, tz=datetime.UTC)
                        now = datetime.datetime.now(tz=datetime.UTC)
                        lag = now - time
                        logger.debug(f"{lag.total_seconds()} seconds lag.")

                        if not model.post_datetime:
                            continue

                        post_age = now - model.post_datetime
                        if post_age > MAX_NOTIFICATION_TIME:
                            logger.debug(f"Post is too old: {post_age}")
                            continue

                        schedule_task(forward_post(model))  # type: ignore

        except Exception as e:
            logger.exception(e)
            await asyncio.sleep(1)
