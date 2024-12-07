"""Server that listens to the Bluesky Jetstream and forwards posts from interesting users to the notification server."""

import asyncio
import datetime
import json
import logging
import os
from urllib.parse import urlparse

import httpx
from async_utils import retry, schedule_task
from atproto_client import AsyncClient  # type: ignore
from atproto_client.exceptions import BadRequestError  # type: ignore
from atproto_client.models.app.bsky.feed.defs import PostView  # type: ignore
from atproto_client.models.app.bsky.feed.post import Record as PostRecord  # type: ignore
from atproto_client.models.app.bsky.feed.repost import Record as RepostRecord  # type: ignore
from bluesky_utils import parse_created_at
from custom_types import DISCONNECT_ERRORS, BlueskyDid, parse_uri
from websockets.asyncio.client import connect

from .prometheus import CYCLE_THROUGH_ALL_WATCHED_USERS_TIME, FORWARD_POST_TIME, HANDLED_MESSAGES

logger = logging.getLogger(__name__)

# Users we're interested in posts from
INTERESTING_USERS: set[BlueskyDid] = set()

# The server to get users from and send posts to
NOTIFIER_SERVER = os.getenv("NOTIFIER_SERVER", "http://localhost:8000")

# max age of a post before we stop forwarding it
MAX_NOTIFICATION_TIME = datetime.timedelta(hours=1)

# Track the last post timestamp we've sent for each user
LAST_POSTS: dict[BlueskyDid, datetime.datetime] = {}

# number of requests to run in parallel
MAX_REQUESTS = 10

# min time between cycles through all watched users
MIN_CYCLE_TIME = datetime.timedelta(minutes=5)

# Minimum cooldown after looping through all watched users
MIN_COOLDOWN = datetime.timedelta(seconds=30)

# How long to wait for the server to come online before giving up
SERVER_ONLINE_TIMEOUT = 60 * 10

http_client = httpx.AsyncClient(timeout=httpx.Timeout(60.0))

bluesky_client = AsyncClient(base_url="https://public.api.bsky.app/")

# override the default 5 second timeout in atproto's httpx client
bluesky_httpx = bluesky_client.request._client
bluesky_httpx._timeout = httpx.Timeout(30.0)


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
        except DISCONNECT_ERRORS:
            logger.warning("user watch TimeoutError, retrying...")
            await asyncio.sleep(1)
        except Exception as e:
            logger.exception(e)
            await asyncio.sleep(1)


@retry(12, 5)
async def forward_post(post: PostView):
    """Forward the post to our notification server."""
    record = post.record
    if not isinstance(record, (PostRecord, RepostRecord)):
        return

    for _ in range(SERVER_ONLINE_TIMEOUT):
        try:
            await http_client.get(NOTIFIER_SERVER)
            break
        except Exception as e:
            logger.warning(f"Server not online: {e}")
            await asyncio.sleep(1)

    did, rkey = parse_uri(post.uri)  # type: ignore

    with FORWARD_POST_TIME.time():
        if isinstance(record, PostRecord):
            await http_client.post(f"{NOTIFIER_SERVER}/post/{did}/{rkey}")
        elif isinstance(record, RepostRecord):
            await http_client.post(f"{NOTIFIER_SERVER}/repost/{did}/{rkey}")

    HANDLED_MESSAGES.inc()


async def cycle_through_all_watched_users():
    """Cycle through all watched users and forward their posts."""
    with CYCLE_THROUGH_ALL_WATCHED_USERS_TIME.time():
        tasks = set()
        for user in INTERESTING_USERS.copy():
            if len(tasks) >= MAX_REQUESTS:
                try:
                    await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
                except Exception as e:
                    logger.exception(e)
            task = asyncio.create_task(check_user_posts(user))
            tasks.add(task)
            task.add_done_callback(lambda task=task: tasks.remove(task))
        try:
            await asyncio.wait(tasks)
        except Exception as e:
            logger.exception(e)


async def check_user_posts(user: BlueskyDid):  # noqa: C901
    """Check for recent posts for the given user and forward them."""
    logger.info(f"Checking for new posts for user: {user}")
    last_post = LAST_POSTS.get(user)
    if not last_post:
        last_post = datetime.datetime.now(tz=datetime.timezone.utc)
        LAST_POSTS[user] = last_post
        return

    cursor = None

    while True:
        try:
            posts = await bluesky_client.get_author_feed(user, cursor=cursor, limit=1)
        except BadRequestError as e:
            if e.response:
                response_content = e.response.content
                if response_content:
                    message = getattr(response_content, "message", None)
                    if message == "Profile not found":
                        logger.info(f"User not found: {user}")
                        return
            raise
        cursor = posts.cursor
        any_posts = False
        for post in posts.feed:
            record = post.post.record
            created_at = getattr(record, "created_at", None)
            if not created_at:
                continue
            created_at_time = parse_created_at(created_at)

            if created_at_time < last_post:
                continue
            if datetime.datetime.now(tz=datetime.timezone.utc) - created_at_time > MAX_NOTIFICATION_TIME:
                continue
            last_post = max(created_at_time, last_post)
            any_posts = True
            logger.info(f"Forwarding post from {user}: {post.post.uri}")
            schedule_task(forward_post(post.post))

        if not any_posts:
            break

        LAST_POSTS[user] = last_post


async def main() -> None:
    """Main coroutine to monitor the jetstream servers for posts."""
    schedule_task(stream_users_to_watch())

    users = set((await http_client.get(NOTIFIER_SERVER + "/users_to_watch")).json())
    logger.info(f"Initial users to watch: {len(users)}")
    INTERESTING_USERS.update(users)
    for user in INTERESTING_USERS:
        LAST_POSTS[user] = datetime.datetime.now(tz=datetime.timezone.utc)

    while True:
        logger.info("Cycling through all watched users...")
        start_time = datetime.datetime.now()
        await cycle_through_all_watched_users()
        elapsed = datetime.datetime.now() - start_time
        logger.info(f"Cycle through all watched users took {elapsed}")
        sleep_time = max(MIN_CYCLE_TIME - elapsed, MIN_COOLDOWN)
        await asyncio.sleep(sleep_time.total_seconds())
