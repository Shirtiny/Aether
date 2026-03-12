from __future__ import annotations

from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from fastapi import HTTPException
from fastapi.security import HTTPAuthorizationCredentials

from src.models.database import UserRole
from src.utils.auth_utils import get_current_user, get_current_user_from_header


class TestAuthUtilsManagementToken:
    @staticmethod
    def _make_request() -> Any:
        return MagicMock(
            headers={},
            client=MagicMock(host="127.0.0.1"),
            state=MagicMock(spec=[]),
        )

    @pytest.mark.asyncio
    async def test_get_current_user_accepts_management_token(self) -> None:
        request = self._make_request()
        credentials = HTTPAuthorizationCredentials(scheme="Bearer", credentials="ae_valid_token")
        db = MagicMock()
        user = MagicMock()
        user.id = "admin-123"
        user.role = UserRole.ADMIN
        management_token = MagicMock()
        management_token.id = "mt-123"

        with patch(
            "src.utils.auth_utils.AuthService.authenticate_management_token",
            new_callable=AsyncMock,
            return_value=(user, management_token),
        ) as mock_authenticate:
            result = await get_current_user(request, credentials, db)

        assert result == user
        assert request.state.user_id == "admin-123"
        assert request.state.management_token_id == "mt-123"
        mock_authenticate.assert_awaited_once_with(db, "ae_valid_token", "127.0.0.1")

    @pytest.mark.asyncio
    async def test_get_current_user_from_header_accepts_management_token(self) -> None:
        request = self._make_request()
        db = MagicMock()
        user = MagicMock()
        user.id = "admin-123"
        user.role = UserRole.ADMIN
        management_token = MagicMock()
        management_token.id = "mt-123"

        with patch(
            "src.utils.auth_utils.AuthService.authenticate_management_token",
            new_callable=AsyncMock,
            return_value=(user, management_token),
        ) as mock_authenticate:
            result = await get_current_user_from_header(
                request,
                authorization="Bearer ae_valid_token",
                db=db,
            )

        assert result == user
        assert request.state.user_id == "admin-123"
        assert request.state.management_token_id == "mt-123"
        mock_authenticate.assert_awaited_once_with(db, "ae_valid_token", "127.0.0.1")

    @pytest.mark.asyncio
    async def test_get_current_user_invalid_management_token_raises_401(self) -> None:
        request = self._make_request()
        credentials = HTTPAuthorizationCredentials(scheme="Bearer", credentials="ae_invalid_token")
        db = MagicMock()

        with patch(
            "src.utils.auth_utils.AuthService.authenticate_management_token",
            new_callable=AsyncMock,
            return_value=None,
        ):
            with pytest.raises(HTTPException) as exc_info:
                await get_current_user(request, credentials, db)

        assert exc_info.value.status_code == 401
        assert exc_info.value.detail == "无效的Token"
