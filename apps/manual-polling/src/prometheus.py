from prometheus_client import Counter, Summary, start_http_server

FORWARD_POST_TIME = Summary("forward_post_processing_seconds", "Time spent forwarding a post to the notifier server")
CYCLE_THROUGH_ALL_WATCHED_USERS_TIME = Summary(
    "cycle_through_all_watched_users_seconds", "Time spent cycling through all watched users"
)
HANDLED_MESSAGES = Counter("handled_messages_total", "Number of messages handled by the server")


def start_prometheus_server(port: int = 9000) -> None:
    """Start the Prometheus server."""
    start_http_server(port)
    print(f"Prometheus server started on port {port}")
