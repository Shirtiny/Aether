"""Admin performance stats routes."""

from __future__ import annotations

from datetime import date, timezone
from typing import Any

from fastapi import APIRouter, Depends, Query, Request
from sqlalchemy.orm import Session

from src.api.base.admin_adapter import AdminApiAdapter
from src.api.base.context import ApiRequestContext
from src.config.constants import CacheTTL
from src.database import get_db
from src.models.database import StatsDaily
from src.services.system.stats_aggregator import StatsAggregatorService
from src.services.system.time_range import TimeRangeParams
from src.utils.cache_decorator import cache_result

from .common import _apply_admin_default_range, _build_time_range_params, pipeline

router = APIRouter()


class AdminPercentilesAdapter(AdminApiAdapter):
    def __init__(self, time_range: TimeRangeParams | None) -> None:
        self.time_range = _apply_admin_default_range(time_range)

    @cache_result(
        key_prefix="admin:stats:performance:percentiles",
        ttl=CacheTTL.ADMIN_USAGE_AGGREGATION,
        user_specific=False,
        vary_by=[
            "time_range.start_date",
            "time_range.end_date",
            "time_range.preset",
            "time_range.timezone",
            "time_range.tz_offset_minutes",
        ],
    )
    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        if not self.time_range:
            return []

        time_range = self.time_range
        is_utc = (time_range.timezone in {None, "UTC"}) and time_range.tz_offset_minutes == 0

        if is_utc:
            start_utc, end_utc = time_range.to_utc_datetime_range()
            rows = (
                context.db.query(StatsDaily)
                .filter(StatsDaily.date >= start_utc, StatsDaily.date < end_utc)
                .order_by(StatsDaily.date.asc())
                .all()
            )
            result = []
            for row in rows:
                date_str = (
                    row.date.astimezone(timezone.utc).date().isoformat()
                    if row.date.tzinfo
                    else row.date.date().isoformat()
                )
                result.append(
                    {
                        "date": date_str,
                        "p50_response_time_ms": row.p50_response_time_ms,
                        "p90_response_time_ms": row.p90_response_time_ms,
                        "p99_response_time_ms": row.p99_response_time_ms,
                        "p50_first_byte_time_ms": row.p50_first_byte_time_ms,
                        "p90_first_byte_time_ms": row.p90_first_byte_time_ms,
                        "p99_first_byte_time_ms": row.p99_first_byte_time_ms,
                    }
                )
            return result

        return StatsAggregatorService.compute_percentiles_by_local_day(context.db, time_range)


@router.get("/performance/percentiles")
async def get_percentiles(
    request: Request,
    db: Session = Depends(get_db),
    start_date: date | None = Query(None),
    end_date: date | None = Query(None),
    preset: str | None = Query(None),
    timezone_name: str | None = Query(None, alias="timezone"),
    tz_offset_minutes: int | None = Query(0),
) -> Any:
    time_range = _build_time_range_params(
        start_date, end_date, preset, timezone_name, tz_offset_minutes
    )
    adapter = AdminPercentilesAdapter(time_range=time_range)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)
