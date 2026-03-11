from __future__ import annotations

from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any
from unittest.mock import MagicMock

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.admin.users.routes import router as admin_users_router
from src.database import get_db


def _build_admin_users_app(db: MagicMock, monkeypatch: pytest.MonkeyPatch) -> TestClient:
    app = FastAPI()
    app.include_router(admin_users_router)
    app.dependency_overrides[get_db] = lambda: db

    async def _fake_pipeline_run(
        *, adapter: Any, http_request: object, db: MagicMock, mode: object
    ) -> Any:
        _ = http_request, mode
        context = SimpleNamespace(
            db=db,
            request=SimpleNamespace(state=SimpleNamespace()),
            user=SimpleNamespace(id="admin-1"),
            ensure_json_body=lambda: {},
            add_audit_metadata=lambda **_: None,
        )
        return await adapter.handle(context)

    monkeypatch.setattr("src.api.admin.users.routes.pipeline.run", _fake_pipeline_run)
    return TestClient(app)


def test_list_users_uses_wallet_batch_lookup(monkeypatch: pytest.MonkeyPatch) -> None:
    db = MagicMock()
    client = _build_admin_users_app(db, monkeypatch)
    now = datetime.now(timezone.utc)
    users = [
        SimpleNamespace(
            id="user-1",
            email="u1@example.com",
            username="user1",
            role=SimpleNamespace(value="user"),
            allowed_providers=None,
            allowed_api_formats=None,
            allowed_models=None,
            is_active=True,
            created_at=now,
            updated_at=now,
            last_login_at=None,
        ),
        SimpleNamespace(
            id="user-2",
            email="u2@example.com",
            username="user2",
            role=SimpleNamespace(value="admin"),
            allowed_providers=None,
            allowed_api_formats=None,
            allowed_models=None,
            is_active=True,
            created_at=now,
            updated_at=None,
            last_login_at=None,
        ),
    ]
    wallets_by_user_id = {
        "user-1": SimpleNamespace(limit_mode="unlimited"),
    }
    batch_getter = MagicMock(return_value=wallets_by_user_id)

    monkeypatch.setattr(
        "src.api.admin.users.routes.UserService.list_users", lambda *_a, **_k: users
    )
    monkeypatch.setattr(
        "src.api.admin.users.routes.WalletService.get_wallets_by_user_ids",
        batch_getter,
    )
    monkeypatch.setattr(
        "src.api.admin.users.routes.WalletService.get_wallet",
        lambda *_a, **_k: (_ for _ in ()).throw(AssertionError("不应回退到逐个钱包查询")),
    )

    response = client.get("/api/admin/users")

    assert response.status_code == 200
    assert response.json()[0]["unlimited"] is True
    assert response.json()[1]["unlimited"] is False
    batch_getter.assert_called_once()
    assert batch_getter.call_args.args[1] == ["user-1", "user-2"]
