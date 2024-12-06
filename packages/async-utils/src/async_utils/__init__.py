import asyncio
import datetime
import typing
from typing import Any, Callable, Coroutine, ParamSpec, TypeVar

Param = ParamSpec("Param")
RetType = TypeVar("RetType")
# Asyncio tasks that we don't want to be garbage collected
TASKS = set()

CACHE_CLEAN_INTERVAL = 10


def schedule_task(coroutine: typing.Coroutine):
    """Schedule a coroutine to run later."""
    task = asyncio.create_task(coroutine)
    TASKS.add(task)
    task.add_done_callback(TASKS.remove)


def cache(
    expire: datetime.timedelta,
) -> Callable[[Callable[Param, Coroutine[None, None, RetType]]], Callable[Param, Coroutine[None, None, RetType]]]:
    """Cache the result of a coroutine."""

    def decorator(
        func: Callable[Param, Coroutine[None, None, RetType]],
    ) -> Callable[Param, Coroutine[None, None, RetType]]:
        cache: dict[Any, tuple[datetime.datetime, RetType]] = {}
        i = 0

        async def wrapper(*args: Param.args, **kwargs: Param.kwargs) -> RetType:
            key = args, frozenset(kwargs.items())
            if key in cache:
                time, value = cache[key]
                if datetime.datetime.now() - time < expire:
                    return value
            value = await func(*args, **kwargs)
            cache[key] = datetime.datetime.now(), value

            # clean out the cache every CACHE_CLEAN_INTERVAL calls
            nonlocal i
            if i == CACHE_CLEAN_INTERVAL:
                i = 0
                for key, (time, value) in list(cache.items()):
                    if datetime.datetime.now() - time > expire:
                        cache.pop(key, None)

            return value

        return wrapper

    return decorator
