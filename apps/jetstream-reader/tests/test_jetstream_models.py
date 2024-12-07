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
        (
            "2024-12-07T18:55:09.507030+00:00",
            datetime.datetime(2024, 12, 7, 18, 55, 9, 507030, tzinfo=datetime.timezone.utc),
        ),
        (
            "2024-12-07T18:57:06.701504+00:00",
            datetime.datetime(2024, 12, 7, 18, 57, 6, 701504, tzinfo=datetime.timezone.utc),
        ),
        ("2024-12-07T19:06:18Z", datetime.datetime(2024, 12, 7, 19, 6, 18, tzinfo=datetime.timezone.utc)),
    ],
)
def test_parse_created_at(time: str, expected: datetime.datetime):
    assert parse_created_at(time) == expected

