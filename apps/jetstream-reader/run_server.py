import asyncio
import logging

from src.prometheus import start_prometheus_server
from src.sentry import init_sentry
from src.server import main

if __name__ == "__main__":
    root_logger = logging.getLogger()
    root_logger.setLevel(logging.INFO)
    handler = logging.StreamHandler()
    root_logger.addHandler(handler)
    handler.setFormatter(logging.Formatter("%(asctime)s - %(name)s - %(levelname)s - %(message)s"))

    init_sentry()

    start_prometheus_server(port=9000)

    asyncio.run(main())
