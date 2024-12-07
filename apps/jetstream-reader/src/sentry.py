import logging
import os

import sentry_sdk

logger = logging.getLogger(__name__)


def init_sentry():
    if os.getenv("SENTRY_DSN"):
        sentry_sdk.init(
            dsn=os.getenv("SENTRY_DSN"),
            # Set traces_sample_rate to 1.0 to capture 100%
            # of transactions for tracing.
            traces_sample_rate=0.1,
            _experiments={
                # Set continuous_profiling_auto_start to True
                # to automatically start the profiler on when
                # possible.
                "continuous_profiling_auto_start": True,
            },
        )
        logging.info("Sentry enabled")
    else:
        logging.warning("No SENTRY_DSN found, not enabling Sentry")
