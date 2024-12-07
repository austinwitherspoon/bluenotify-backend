import datetime

import pytest
from src.jetstream_models import parse_created_at  # type: ignore


@pytest.mark.parametrize(
    "time,expected",
    [
        ("2024-12-07T05:48:21.260Z", datetime.datetime(2024, 12, 7, 5, 48, 21, 260000, tzinfo=datetime.timezone.utc)),
        (
            "2024-12-07T05:43:53.0557218Z",
            datetime.datetime(2024, 12, 7, 5, 43, 53, 557218, tzinfo=datetime.timezone.utc),
        ),
        ("2024-12-07T05:48:07+00:00", datetime.datetime(2024, 12, 7, 5, 48, 7, tzinfo=datetime.timezone.utc)),
    ],
)
def test_parse_created_at(time: str, expected: datetime.datetime):
    assert parse_created_at(time) == expected
