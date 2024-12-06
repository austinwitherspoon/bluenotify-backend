# Jetstream Reader

This server listens to Bluesky's Jetstream for posts.

It asks our Notifier server for a list of users to watch, and then forwards any posts from those users to it.

## Run locally
`uv run apps/jetstream_reader/reader.py`