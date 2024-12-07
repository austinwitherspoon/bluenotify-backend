from enum import Enum
from typing import NewType

from websockets import ConnectionClosedError

FirebaseToken = NewType("FirebaseToken", str)
BlueskyDid = NewType("BlueskyDid", str)
BlueskyCid = NewType("BlueskyCid", str)
BlueskyRKey = NewType("BlueskyRKey", str)
BlueskyUri = NewType("BlueskyUri", str)


def bluesky_uri(did: BlueskyDid, rkey: BlueskyRKey) -> BlueskyUri:
    """Build the URI for a bluesky object."""
    return f"at://{did}/app.bsky.feed.post/{rkey}"  # type: ignore


def parse_uri(uri: BlueskyUri) -> tuple[BlueskyDid, BlueskyRKey]:
    """Parse a bluesky URI."""
    _, _, did, _, rkey = uri.split("/")
    return BlueskyDid(did), BlueskyRKey(rkey)


def bluesky_browseable_url(handle: str, rkey: BlueskyRKey) -> str:
    """Build a URL to view a post on Bluesky."""
    return f"https://bsky.app/profile/{handle}/post/{rkey}"


class PostType(Enum):
    """The type of post."""

    POST = "post"
    REPOST = "repost"
    QUOTE_POST = "quote_post"
    REPLY = "reply"


DISCONNECT_ERRORS = (TimeoutError, ConnectionClosedError, ConnectionRefusedError, EOFError)
