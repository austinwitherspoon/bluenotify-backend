from typing import Callable, Coroutine, ParamSpec, TypeVar

from prometheus_client import Counter, Gauge, Summary

RECEIVED_MESSAGES = Counter("received_messages", "The number of messages received by the server")
NOTIFICATIONS_SENT = Counter("notifications_sent", "The number of notifications sent by the server")
SUBSCRIBED_USERS = Gauge("subscribed_users", "The number of users subscribed to the server")
WATCHED_USERS = Gauge("watched_users", "The number of users watched by the server")
POST_HANDLE_TIME = Summary("post_handle_time_seconds", "Time spent handling a post")
BACKLOG = Gauge("backlog", "The number of posts in the backlog")
PROCESSED_POST_SIZE = Gauge("processed_post_size", "The size of the handled posts timestamps")

Param = ParamSpec("Param")
RetType = TypeVar("RetType")


def track_backlog(
    func: Callable[Param, Coroutine[None, None, RetType]],
) -> Callable[Param, Coroutine[None, None, RetType]]:
    """Decorator to track the backlog of async functions."""

    async def wrapper(*args: Param.args, **kwargs: Param.kwargs) -> RetType:
        BACKLOG.inc()
        try:
            return await func(*args, **kwargs)
        finally:
            BACKLOG.dec()

    return wrapper
