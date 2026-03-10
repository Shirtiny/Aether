"""Admin quota usage stats routes."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from typing import Any

from fastapi import APIRouter, Depends, Request
from sqlalchemy.orm import Session

from src.api.base.admin_adapter import AdminApiAdapter
from src.api.base.context import ApiRequestContext
from src.config.constants import CacheTTL
from src.core.enums import ProviderBillingType
from src.database import get_db
from src.models.database import Provider
from src.utils.cache_decorator import cache_result

from .common import pipeline

router = APIRouter()


class AdminQuotaUsageAdapter(AdminApiAdapter):
    @cache_result(
        key_prefix="admin:stats:providers:quota_usage",
        ttl=CacheTTL.ADMIN_USAGE_AGGREGATION,
        user_specific=False,
    )
    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        db = context.db
        providers = (
            db.query(Provider)
            .filter(
                (Provider.billing_type == ProviderBillingType.MONTHLY_QUOTA)
                | (Provider.monthly_quota_usd.isnot(None))
            )
            .all()
        )
        now = datetime.now(timezone.utc)

        result = []
        for provider in providers:
            quota = float(provider.monthly_quota_usd or 0)
            used = float(provider.monthly_used_usd or 0)
            remaining = max(quota - used, 0.0)
            usage_percent = round((used / quota) * 100, 2) if quota > 0 else 0.0

            reset_at = provider.quota_last_reset_at
            if reset_at:
                days_elapsed = max(1, (now - reset_at).days)
            else:
                days_elapsed = max(1, now.day - 1)

            daily_rate = used / days_elapsed if used > 0 else 0.0
            estimated_exhaust_at = None
            if daily_rate > 0 and remaining > 0:
                estimated_exhaust_at = now + timedelta(days=remaining / daily_rate)
            if provider.quota_expires_at:
                if not estimated_exhaust_at or provider.quota_expires_at < estimated_exhaust_at:
                    estimated_exhaust_at = provider.quota_expires_at

            result.append(
                {
                    "id": provider.id,
                    "name": provider.name,
                    "quota_usd": float(quota),
                    "used_usd": float(used),
                    "remaining_usd": float(remaining),
                    "usage_percent": usage_percent,
                    "quota_expires_at": (
                        provider.quota_expires_at.isoformat() if provider.quota_expires_at else None
                    ),
                    "estimated_exhaust_at": (
                        estimated_exhaust_at.isoformat() if estimated_exhaust_at else None
                    ),
                }
            )

        result.sort(key=lambda x: x["usage_percent"], reverse=True)
        return {"providers": result}


@router.get("/providers/quota-usage")
async def get_quota_usage(
    request: Request,
    db: Session = Depends(get_db),
) -> Any:
    adapter = AdminQuotaUsageAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)
