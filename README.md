# BlueNotify Backend Servers

This is a collection of servers that are used to run my app BlueNotify's notification system.

All servers are currently written in Python 3.13 using FastAPI.

## Core Servers

### Notifier Server
This server is responsible for listening to firebase for user notification settings updates, and sending notifications
to users.

It provides a websocket endpoint that other servers can listen to for live changes in users the server is interested
in events for.

It also tracks post CIDs so we can avoid duplicating notifications if posts are forwarded to the server multiple times,
so running multiple redundant copies of the Jetstream Reader is totally okay.

### Jetstream Reader
This server listens to the Bluesky Jetstream for posts from users we're interested in events for, and forwards them to
our notifier server.

### Manual Polling
This server/cron job is used for a lower bandwidth alternative to the Jetstream as a backup system for redundancy.
If the Jetstream Reader goes down (or the Bluesky Firehose/Jetstream have issues, which isn't uncommon!), this system
will still be able to send posts to the notification server.

This system should use less bandwidth because we don't need to care about *all* of bluesky's posts like we do with the 
firehose, but it also won't be as low-latency.


## Development
This package is built with [UV](https://docs.astral.sh/uv/), so if you already have uv installed you can just run 
`uv sync --all-packages` in the root of this workspace and you're ready to go!

You can also run `docker compose up` to spin up all servers at once for testing.

For testing against the live firebase account, make sure to have environment variables set:
- `GOOGLE_CLOUD_PROJECT`
- `GOOGLE_APPLICATION_CREDENTIALS`
