import logging

import firebase_admin  # type: ignore
from src.sentry import init_sentry
from src.server import app

if __name__ == "__main__":
    import uvicorn

    root_logger = logging.getLogger()
    root_logger.setLevel(logging.INFO)
    handler = logging.StreamHandler()
    root_logger.addHandler(handler)
    handler.setFormatter(
        logging.Formatter("%(asctime)s - %(name)s - %(levelname)s - %(message)s"),
    )

    init_sentry()

    firebase_admin.initialize_app()

    uvicorn.run(app, host="0.0.0.0", port=8000)
