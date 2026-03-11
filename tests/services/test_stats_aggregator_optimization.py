from __future__ import annotations

from datetime import date, datetime, time, timedelta, timezone
from types import SimpleNamespace
from typing import Any, cast

import pytest

from src.models.database import StatsDaily, StatsUserDaily
from src.services.system.stats_aggregator import (
    AggregatedStats,
    StatsAggregatorService,
    query_stats_hybrid,
)
from src.services.system.time_range import TimeRangeParams


class _FakeQuery:
    def __init__(self, *, all_result: list[Any] | None = None) -> None:
        self._all_result = all_result if all_result is not None else []

    def filter(self, *_args: object, **_kwargs: object) -> _FakeQuery:
        return self

    def group_by(self, *_args: object, **_kwargs: object) -> _FakeQuery:
        return self

    def all(self) -> list[Any]:
        return self._all_result


class _HybridQuerySession:
    def __init__(self, stats_daily_rows: list[SimpleNamespace]) -> None:
        self._stats_daily_rows = stats_daily_rows
        self.stats_daily_query_count = 0

    def query(self, entity: object) -> _FakeQuery:
        if entity is StatsDaily:
            self.stats_daily_query_count += 1
            return _FakeQuery(all_result=self._stats_daily_rows)
        raise AssertionError(f"Unexpected query entity: {entity}")


class _BatchUserStatsSession:
    def __init__(
        self, existing_rows: list[StatsUserDaily], aggregated_rows: list[SimpleNamespace]
    ) -> None:
        self._responses: list[list[Any]] = [list(existing_rows), list(aggregated_rows)]
        self.added: list[StatsUserDaily] = []
        self.commit_count = 0

    def query(self, *_entities: object) -> _FakeQuery:
        if not self._responses:
            raise AssertionError("Unexpected extra query")
        return _FakeQuery(all_result=self._responses.pop(0))

    def add(self, row: StatsUserDaily) -> None:
        self.added.append(row)

    def commit(self) -> None:
        self.commit_count += 1


def test_query_stats_hybrid_batches_statsdaily_lookup_and_merges_realtime_ranges(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    today = datetime.now(timezone.utc).date()
    historical_cached_day = today - timedelta(days=4)
    historical_missing_day = today - timedelta(days=3)
    realtime_day = today

    cached_row = SimpleNamespace(
        date=datetime.combine(historical_cached_day, time.min, tzinfo=timezone.utc),
        total_requests=10,
        success_requests=9,
        error_requests=1,
        input_tokens=100,
        output_tokens=50,
        cache_creation_tokens=5,
        cache_read_tokens=3,
        cache_creation_cost=1.2,
        cache_read_cost=0.8,
        total_cost=3.5,
        actual_total_cost=3.0,
        avg_response_time_ms=200.0,
    )
    db = _HybridQuerySession(stats_daily_rows=[cached_row])

    calls: list[tuple[datetime, datetime]] = []

    def _fake_aggregate_usage_range(
        _db: object,
        start_utc: datetime,
        end_utc: datetime,
        filters: object | None = None,  # noqa: ARG001
    ) -> AggregatedStats:
        calls.append((start_utc, end_utc))
        return AggregatedStats(total_requests=1, success_requests=1)

    class _FakeParams:
        def get_complete_utc_dates(self) -> tuple[list[date], None, None]:
            return [historical_cached_day, historical_missing_day, realtime_day], None, None

    monkeypatch.setattr(
        "src.services.system.stats_aggregator.aggregate_usage_range",
        _fake_aggregate_usage_range,
    )

    result = query_stats_hybrid(cast(Any, db), cast(Any, _FakeParams()))

    assert db.stats_daily_query_count == 1
    assert calls == [
        (
            datetime.combine(historical_missing_day, time.min, tzinfo=timezone.utc),
            datetime.combine(
                historical_missing_day + timedelta(days=1), time.min, tzinfo=timezone.utc
            ),
        ),
        (
            datetime.combine(realtime_day, time.min, tzinfo=timezone.utc),
            datetime.combine(realtime_day + timedelta(days=1), time.min, tzinfo=timezone.utc),
        ),
    ]
    assert result.total_requests == 12
    assert result.success_requests == 11


def test_aggregate_user_daily_stats_batch_updates_all_users_in_two_queries() -> None:
    target_day = datetime(2026, 3, 1, tzinfo=timezone.utc)
    aggregated_rows = [
        SimpleNamespace(
            user_id="user-1",
            username="alice",
            total_requests=4,
            error_requests=1,
            input_tokens=20,
            output_tokens=8,
            cache_creation_tokens=2,
            cache_read_tokens=1,
            total_cost=1.5,
        )
    ]
    db = _BatchUserStatsSession(existing_rows=[], aggregated_rows=aggregated_rows)

    result = StatsAggregatorService.aggregate_user_daily_stats_batch(
        cast(Any, db),
        target_day,
        ["user-1", "user-2"],
        commit=True,
    )

    assert len(result) == 2
    assert db.commit_count == 1
    assert len(db.added) == 2

    user_one = next(row for row in result if row.user_id == "user-1")
    user_two = next(row for row in result if row.user_id == "user-2")

    assert user_one.username == "alice"
    assert user_one.total_requests == 4
    assert user_one.success_requests == 3
    assert user_one.total_cost == 1.5

    assert user_two.total_requests == 0
    assert user_two.success_requests == 0
    assert user_two.error_requests == 0
    assert user_two.total_cost == 0.0


def test_compute_percentiles_by_local_day_returns_sqlite_fallback_without_queries() -> None:
    db = SimpleNamespace(bind=SimpleNamespace(dialect=SimpleNamespace(name="sqlite")))
    time_range = TimeRangeParams(
        start_date=date(2026, 3, 1),
        end_date=date(2026, 3, 3),
        timezone="Asia/Singapore",
    )

    result = StatsAggregatorService.compute_percentiles_by_local_day(cast(Any, db), time_range)

    assert [row["date"] for row in result] == ["2026-03-01", "2026-03-02", "2026-03-03"]
    assert all(row["p50_response_time_ms"] is None for row in result)
    assert all(row["p50_first_byte_time_ms"] is None for row in result)
