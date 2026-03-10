from __future__ import annotations

from unittest.mock import MagicMock

from src.core.enums import UserRole
from src.services.user.service import UserService


def test_list_users_orders_by_created_at_desc_then_id_desc() -> None:
    ordered_query = MagicMock()
    ordered_query.offset.return_value = ordered_query
    ordered_query.limit.return_value = ordered_query
    ordered_query.all.return_value = ["user-1"]

    query = MagicMock()
    query.filter.return_value = query
    query.order_by.return_value = ordered_query

    db = MagicMock()
    db.query.return_value = query

    result = UserService.list_users(
        db,
        skip=5,
        limit=10,
        role=UserRole.ADMIN,
        is_active=True,
    )

    assert result == ["user-1"]
    order_args = query.order_by.call_args.args
    assert len(order_args) == 2
    assert str(order_args[0]) == "users.created_at DESC"
    assert str(order_args[1]) == "users.id DESC"
    ordered_query.offset.assert_called_once_with(5)
    ordered_query.limit.assert_called_once_with(10)
