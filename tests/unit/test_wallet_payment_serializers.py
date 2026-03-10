from __future__ import annotations

from decimal import Decimal
from types import SimpleNamespace

from src.api.serializers.wallet_payment import serialize_admin_wallet, serialize_wallet_transaction


def test_serialize_wallet_transaction_emits_split_balance_numbers() -> None:
    tx = SimpleNamespace(
        id="tx-1",
        category="adjust",
        reason_code="adjust_admin",
        amount=Decimal("1.25"),
        balance_before=Decimal("5.00"),
        balance_after=Decimal("6.25"),
        recharge_balance_before=Decimal("2.00"),
        recharge_balance_after=Decimal("3.25"),
        gift_balance_before=Decimal("3.00"),
        gift_balance_after=Decimal("3.00"),
        link_type="admin_action",
        link_id="wallet-1",
        operator_id="admin-1",
        description="test",
        created_at="2026-03-07T00:00:00Z",
    )

    payload = serialize_wallet_transaction(tx)

    assert payload["recharge_balance_before"] == 2.0
    assert payload["recharge_balance_after"] == 3.25
    assert payload["gift_balance_before"] == 3.0
    assert payload["gift_balance_after"] == 3.0


def test_serialize_admin_wallet_omits_version(monkeypatch) -> None:
    wallet = SimpleNamespace(
        id="wallet-1",
        user_id="user-1",
        api_key_id=None,
        user=SimpleNamespace(username="alice"),
        api_key=None,
        created_at="2026-03-07T00:00:00Z",
    )

    monkeypatch.setattr(
        "src.api.serializers.wallet_payment.WalletService.serialize_wallet_summary",
        lambda _wallet: {
            "balance": 10.0,
            "recharge_balance": 7.0,
            "gift_balance": 3.0,
            "refundable_balance": 7.0,
            "currency": "USD",
            "status": "active",
            "limit_mode": "finite",
            "unlimited": False,
            "total_recharged": 10.0,
            "total_consumed": 0.0,
            "total_refunded": 0.0,
            "total_adjusted": 0.0,
            "updated_at": "2026-03-07T00:00:00Z",
        },
    )

    payload = serialize_admin_wallet(wallet)

    assert "version" not in payload
    assert payload["owner_name"] == "alice"
