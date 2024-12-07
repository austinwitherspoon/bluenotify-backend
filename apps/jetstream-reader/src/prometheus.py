from prometheus_client import Counter, Gauge, Summary, start_http_server

FORWARD_POST_TIME = Summary("forward_post_processing_seconds", "Time spent forwarding a post to the notifier server")
JETSTREAM_LAG = Gauge("jetstream_lag_seconds", "The lag between the Jetstream events and the notifier server")
HANDLED_MESSAGES = Counter("handled_messages", "The number of messages handled by the server")
ALL_MESSAGES = Counter("all_messages", "The number of messages received by the server")


def start_prometheus_server(port: int = 9000) -> None:
    """Start the Prometheus server."""
    start_http_server(port)
    print(f"Prometheus server started on port {port}")
