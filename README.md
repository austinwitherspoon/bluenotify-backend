# BlueNotify Backend Servers

This is a collection of servers that are used to run my app BlueNotify's notification system.

Currently written in Rust. Previously these servers were built in python, but I ran into issues trying to run it
on affordable servers, so I took the opportunity to rewrite it and hopefully make it more robust.

## Core Servers

### Notifier Server
This server is responsible for processing posts and sending notifications.

It listens to NATS for changes in the user settings and posts from the jetstream, validates which users to send notifications to, and then sends the messages via FCM.

### Jetstream Reader
This server listens to the Bluesky Jetstream for posts from users we're interested in events for, and forwards them to
NATS.


### Firestore Reader
This server listens to the firestore and forwards all user settings changes into NATS.


## Development
This package is written in rust. You can run an individual tool using a command like `cargo run --package jetstream_reader`

You can also run `docker compose up` to spin up all servers at once for testing.

For testing against the live firebase account, make sure to have environment variables set:
- `GOOGLE_CLOUD_PROJECT`
- `GOOGLE_APPLICATION_CREDENTIALS`

As well as certificates for google cloud saved to `cert.json`