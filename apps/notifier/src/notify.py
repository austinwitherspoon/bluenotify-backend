import asyncio
import concurrent.futures
import logging
import os
import traceback

from async_utils import prometheus_time
from atproto_client.models.app.bsky.embed.external import Main as EmbedExternal  # type: ignore
from atproto_client.models.app.bsky.embed.images import Main as EmbedImage  # type: ignore
from atproto_client.models.app.bsky.embed.record import Main as EmbedRecord  # type: ignore
from atproto_client.models.app.bsky.embed.record_with_media import Main as EmbedRecordWithMedia  # type: ignore
from atproto_client.models.app.bsky.embed.video import Main as EmbedVideo  # type: ignore
from custom_types import FirebaseToken, PostType, bluesky_browseable_url, parse_uri
from firebase_admin import messaging  # type: ignore

from .bluesky import PostResponse, RepostResponse, get_following, get_post, get_user
from .firestore import AllFollowSettings, FirestorePostType
from .prometheus import NOTIFICATIONS_SENT, POST_HANDLE_TIME, track_backlog

logger = logging.getLogger(__name__)
thread_pool = concurrent.futures.ThreadPoolExecutor(max_workers=4)


FIRESTORE_POST_TYPE_MAP = {
    PostType.POST: {FirestorePostType.POST},
    PostType.QUOTE_POST: {FirestorePostType.POST},
    PostType.REPOST: {FirestorePostType.REPOST},
    PostType.REPLY: {FirestorePostType.REPLY, FirestorePostType.REPLY_TO_FRIEND},
}

MOCK = os.getenv("MOCK", "false").lower().strip() == "true"

@track_backlog
@prometheus_time(POST_HANDLE_TIME)
async def process_post(post: PostResponse | RepostResponse, settings: AllFollowSettings) -> None:  # noqa: C901
    """Process a post."""
    post_logger = logger.getChild(post.uri)
    post_logger.info(f"Processing post: {post.uri}")

    did, rkey = parse_uri(post.uri)  # type: ignore

    source_post: PostResponse = post  # type: ignore

    post_type = PostType.POST
    if isinstance(post, RepostResponse):
        post_type = PostType.REPOST
        source_did, source_rkey = parse_uri(post.value.subject.uri)  # type: ignore
        source_post = await get_post(source_did, source_rkey)  # type: ignore
    elif post.value.reply is not None:
        post_type = PostType.REPLY
    elif isinstance(post.value.embed, (EmbedRecord, EmbedRecordWithMedia)):
        post_type = PostType.QUOTE_POST

    users_to_notify = settings.users_to_subscribers.get(did, set())

    firestore_post_types = FIRESTORE_POST_TYPE_MAP[post_type]

    for user in users_to_notify.copy():
        user_settings = settings.get(user)
        if user_settings is None:
            continue
        settings_for_this_account = user_settings.settings.get(did)
        if settings_for_this_account is None:
            continue

        applicable_types = settings_for_this_account.types.intersection(firestore_post_types)

        # remove users who aren't interested in this type of post
        if not applicable_types:
            users_to_notify.remove(user)
            continue

        if applicable_types == {FirestorePostType.REPLY_TO_FRIEND}:
            # only notify if the post is a reply to the user
            following = set()
            for user_did in user_settings.accounts:
                following.update(await get_following(user_did))

            other_user_did, _ = parse_uri(post.value.reply.parent.uri)  # type: ignore

            if other_user_did not in following:
                users_to_notify.remove(user)
                continue

    if not users_to_notify:
        post_logger.info("No users to notify")
        return

    post_logger.info(f"Users to notify: {users_to_notify}")

    poster = await get_user(did)
    poster_name = poster.display_name or poster.handle

    match post_type:
        case PostType.POST:
            title = f"{poster_name} posted:"
        case PostType.REPOST:
            title = f"{poster_name} reposted:"
        case PostType.REPLY:
            assert source_post.value.reply is not None
            other_user_did, _ = parse_uri(post.value.reply.parent.uri)  # type: ignore
            other_user = await get_user(other_user_did)
            other_user_name = other_user.display_name or other_user.handle
            title = f"{poster_name} replied to {other_user_name}:"
        case PostType.QUOTE_POST:
            title = f"{poster_name} quote posted:"

    source_poster = poster

    text = source_post.value.text or None

    media_type = None

    # if the user is quoting a post, media is stored in a weird nested way
    embed = None
    if source_post.value.embed and isinstance(source_post.value.embed, EmbedRecordWithMedia):
        embed = source_post.value.embed.media
    elif source_post.value.embed:
        embed = source_post.value.embed

    if embed and isinstance(embed, EmbedImage):
        media_type = "image"
    elif embed and isinstance(embed, EmbedVideo):
        media_type = "video"
    elif embed and isinstance(embed, EmbedExternal):
        media_type = "external link"

    if text is None and media_type:
        text = f"[{media_type}]"

    source_post_did, source_post_rkey = parse_uri(source_post.uri)  # type: ignore
    source_poster_handle = source_poster.handle
    if post_type == PostType.REPOST:
        source_poster = await get_user(source_post_did)
        source_poster_handle = source_poster.handle

    url = bluesky_browseable_url(source_poster_handle, source_post_rkey)

    data = {
        "url": url,
        "type": post_type.value,
        "text": text,
        "post_user_did": did,
        "post_user_handle": poster.handle,
        "post_id": rkey,
    }

    post_logger.info(f"Sending message to {len(users_to_notify)} users.")
    post_logger.info(f"Title: {title}")
    post_logger.info(f"Text: {text}")
    post_logger.info(f"Url: {url}")
    await send_message(
        users_to_notify,
        title,
        text or "",
        data,
        all_settings=settings,
        mock=MOCK,
    )

    # remove the logger so we don't keep it forever
    logging.getLogger().manager.loggerDict.pop(post_logger.name, None)


async def send_message(
    registration_tokens: set[FirebaseToken],
    title: str,
    body: str,
    data: dict[str, str],
    all_settings: AllFollowSettings,
    mock: bool = False,
):
    """Send a notification to a user."""
    loop = asyncio.get_event_loop()
    return await loop.run_in_executor(
        thread_pool,
        _send_message_sync,
        registration_tokens,
        title,
        body,
        data,
        all_settings,
        mock,
    )


def _send_message_sync(
    registration_tokens: set[FirebaseToken],
    title: str,
    body: str,
    data: dict[str, str],
    all_settings: AllFollowSettings,
    mock: bool = False,
) -> None:
    try:
        if not registration_tokens:
            logger.info("No registration tokens provided")
            return None
        notification = messaging.Notification(
            title=title,
            body=body,
        )
        messages: list[messaging.Message] = []
        for token in registration_tokens:
            messages.append(
                messaging.Message(
                    notification=notification,
                    data=data,
                    token=token,
                    android=messaging.AndroidConfig(
                        priority="high",
                    ),
                ),
            )
        logger.info(f"Sending {len(messages)} messages")
        for message in messages:
            try:
                if not mock:
                    result = messaging.send(message, dry_run=mock)
                    logger.info(f"Send to {message.token} result: {result}")
                else:
                    logger.info(f"Mock send to {message.token}")
                NOTIFICATIONS_SENT.inc()
            except messaging.UnregisteredError:
                logger.info(f"Unregistered token: {message.token}, removing")
                all_settings.remove_setting_from_firestore(message.token)  # type: ignore
            except Exception as e:
                logger.warning(f"Error sending {e}")
    except Exception as e:
        logger.exception(f"Error sending message {e}")
        traceback.print_exc()
        raise e
