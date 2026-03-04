from __future__ import annotations

from src.api.admin import provider_oauth as module


def test_parse_standard_oauth_import_entries_keeps_codex_hints() -> None:
    entries = module._parse_standard_oauth_import_entries(
        '[{"refresh_token":"rt_1","accountId":"acc-1","planType":"TEAM","userId":"u-1","email":"u@example.com"}]'
    )

    assert entries == [
        {
            "refresh_token": "rt_1",
            "account_id": "acc-1",
            "plan_type": "team",
            "user_id": "u-1",
            "email": "u@example.com",
        }
    ]


def test_parse_tokens_input_compatibility_wrapper() -> None:
    tokens = module._parse_tokens_input("token_a\ntoken_b")
    assert tokens == ["token_a", "token_b"]


def test_apply_codex_import_hints_only_fills_missing_fields() -> None:
    auth_config = {
        "account_id": "existing-account",
        "plan_type": "",
    }
    module._apply_codex_import_hints(
        auth_config,
        {
            "account_id": "acc-1",
            "plan_type": "plus",
            "user_id": "user-1",
            "email": "u@example.com",
        },
    )

    assert auth_config["account_id"] == "existing-account"
    assert auth_config["plan_type"] == "plus"
    assert auth_config["user_id"] == "user-1"
    assert auth_config["email"] == "u@example.com"
