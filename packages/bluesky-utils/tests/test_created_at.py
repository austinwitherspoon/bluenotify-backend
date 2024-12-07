import datetime

import pytest
from bluesky_utils import parse_created_at  # type: ignore


@pytest.mark.parametrize(
    "time,expected",
    [
        ("2024-12-07T05:48:21.260Z", datetime.datetime(2024, 12, 7, 5, 48, 21, tzinfo=datetime.timezone.utc)),
        (
            "2024-12-07T05:43:53.0557218Z",
            datetime.datetime(2024, 12, 7, 5, 43, 53, tzinfo=datetime.timezone.utc),
        ),
        ("2024-12-07T05:48:07+00:00", datetime.datetime(2024, 12, 7, 5, 48, 7, tzinfo=datetime.timezone.utc)),
        (
            "2024-12-07T18:55:09.507030+00:00",
            datetime.datetime(2024, 12, 7, 18, 55, 9, tzinfo=datetime.timezone.utc),
        ),
        (
            "2024-12-07T18:57:06.701504+00:00",
            datetime.datetime(2024, 12, 7, 18, 57, 6, tzinfo=datetime.timezone.utc),
        ),
        ("2024-12-07T19:06:18Z", datetime.datetime(2024, 12, 7, 19, 6, 18, tzinfo=datetime.timezone.utc)),
        (
            "2024-12-07T14:50:38.1051195Z",
            datetime.datetime(2024, 12, 7, 14, 50, 38, tzinfo=datetime.timezone.utc),
        ),
        ("2024-12-04T10:54:13-06:00", datetime.datetime(2024, 12, 4, 16, 54, 13, tzinfo=datetime.timezone.utc)),
        ("2024-12-06T17:17:18-05:00", datetime.datetime(2024, 12, 6, 22, 17, 18, tzinfo=datetime.timezone.utc)),
    ],
)
def test_parse_created_at(time: str, expected: datetime.datetime):
    parsed = parse_created_at(time)
    assert parsed == expected
