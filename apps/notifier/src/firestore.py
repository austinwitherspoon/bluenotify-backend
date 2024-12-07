"""Handle reading user notification settings from firebase, and listening for changes."""

import asyncio
import logging
import os
from collections.abc import MutableMapping
from dataclasses import dataclass
from enum import Enum
from typing import Any, Iterator, TypeVar, overload

from custom_types import BlueskyDid, FirebaseToken
from firebase_admin import firestore  # type: ignore
from google.cloud.firestore_v1.base_document import DocumentSnapshot  # type: ignore
from google.cloud.firestore_v1.watch import DocumentChange  # type: ignore

logger = logging.getLogger(__name__)

T = TypeVar("T")

SETTINGS_COLLECTION = os.getenv("SETTINGS_COLLECTION", "staging_subscriptions")


class FirestorePostType(Enum):
    POST = "post"
    REPOST = "repost"
    REPLY = "reply"
    REPLY_TO_FRIEND = "replyToFriend"


@dataclass(slots=True)
class SingleUserFollowSetting:
    did: str
    types: set[FirestorePostType]

    @classmethod
    def from_dict(cls, data: dict) -> "SingleUserFollowSetting":
        return cls(data["did"], {FirestorePostType(t) for t in data["postTypes"]})


@dataclass(slots=True)
class UserFollowSettings:
    token: FirebaseToken
    settings: dict[BlueskyDid, SingleUserFollowSetting]
    accounts: set[BlueskyDid]

    @classmethod
    def from_dict(cls, data: dict) -> "UserFollowSettings":
        settings = {}
        for did, setting in data.get("settings", {}).items():
            settings[did] = SingleUserFollowSetting.from_dict(setting)
        return cls(data["fcmToken"], settings, set(data.get("accounts", [])))


class AllFollowSettings(MutableMapping):
    def __init__(self) -> None:
        self._settings: dict[FirebaseToken, UserFollowSettings] = {}
        self._users_to_subscribers: dict[BlueskyDid, set[FirebaseToken]] = {}

        # Async event for when the settings change
        self.changed = asyncio.Event()

    @property
    def users_to_watch(self) -> set[BlueskyDid]:
        return set(self._users_to_subscribers.keys())

    @property
    def users_to_subscribers(self) -> dict[BlueskyDid, set[FirebaseToken]]:
        return self._users_to_subscribers

    def _update_users_to_watch(self) -> None:
        """Collect all bluesky users that we have settings for."""
        new_users_to_subscribers: dict[BlueskyDid, set[FirebaseToken]] = {}
        for settings in self._settings.values():
            for setting in settings.settings.values():
                if setting.types:
                    new_users_to_subscribers.setdefault(setting.did, set()).add(settings.token)  # type: ignore

        has_changed = set(new_users_to_subscribers.keys()) != set(self._users_to_subscribers.keys())

        self._users_to_subscribers = new_users_to_subscribers

        if has_changed:
            self.changed.set()
            self.changed.clear()

    @overload
    def get(self, key: FirebaseToken) -> UserFollowSettings: ...

    @overload
    def get(self, key: FirebaseToken, default: T) -> UserFollowSettings | T: ...

    def get(self, key: FirebaseToken, default: T = None) -> UserFollowSettings | T:  # type: ignore
        return self._settings.get(key, default)

    @overload
    def pop(self, key: FirebaseToken) -> UserFollowSettings: ...

    @overload
    def pop(self, key: FirebaseToken, default: T) -> UserFollowSettings | T: ...

    def pop(self, key: FirebaseToken, default: T = None) -> UserFollowSettings | T:  # type: ignore
        return self._settings.pop(key, default)

    def __getitem__(self, key: FirebaseToken) -> UserFollowSettings:
        return self._settings[key]

    def __setitem__(self, key: FirebaseToken, value: UserFollowSettings) -> None:
        self._settings[key] = value
        self._update_users_to_watch()

    def __delitem__(self, key: FirebaseToken) -> None:
        del self._settings[key]
        self._update_users_to_watch()

    def __iter__(self) -> Iterator[FirebaseToken]:
        return iter(self._settings)

    def __len__(self) -> int:
        return len(self._settings)

    def __repr__(self) -> str:
        return repr(self._settings)

    def listen_to_changes(self) -> None:
        """Start a listener to the firestore collection in another thread."""
        self.client = firestore.client()
        logger.info(f"Listening to changes in {SETTINGS_COLLECTION}")
        self.collection = self.client.collection(SETTINGS_COLLECTION)
        self.collection.on_snapshot(self._update_follow_settings)

    def _update_follow_settings(
        self,
        collection_snapshot: list[DocumentSnapshot],
        changes: list[DocumentChange],
        read_time: Any,
    ) -> None:
        for change in changes:
            doc: DocumentSnapshot = change.document
            fcm_token: FirebaseToken = doc.id
            logger.info(f"Change detected in {fcm_token}: {change.type.name}")
            if change.type.name == "REMOVED":
                self.pop(fcm_token, None)
                continue
            try:
                data = doc.to_dict()
                if not data:
                    continue
                settings = UserFollowSettings.from_dict(data)
                self[fcm_token] = settings
            except Exception as e:
                logger.info(data)
                logger.warning(f"Error parsing settings: {e}")
