from __future__ import annotations

import json
from types import SimpleNamespace

import pytest

from src.api.admin import provider_oauth as module
from src.core.exceptions import InvalidRequestException


def test_parse_standard_oauth_import_entries_keeps_codex_hints() -> None:
    entries = module._parse_standard_oauth_import_entries(
        '[{"refresh_token":"rt_1","accountId":"acc-1","chatgptAccountUserId":"u-1__acc-1","planType":"TEAM","userId":"u-1","email":"u@example.com"}]'
    )

    assert entries == [
        {
            "refresh_token": "rt_1",
            "account_id": "acc-1",
            "account_user_id": "u-1__acc-1",
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
            "account_user_id": "u-1__acc-1",
            "account_id": "acc-1",
            "plan_type": "plus",
            "user_id": "user-1",
            "email": "u@example.com",
        },
    )

    assert auth_config["account_id"] == "existing-account"
    assert auth_config["account_user_id"] == "u-1__acc-1"
    assert auth_config["plan_type"] == "plus"
    assert auth_config["user_id"] == "user-1"
    assert auth_config["email"] == "u@example.com"


class _DummyQuery:
    def __init__(self, keys: list[SimpleNamespace]) -> None:
        self._keys = keys

    def filter(self, *_args: object, **_kwargs: object) -> "_DummyQuery":
        return self

    def all(self) -> list[SimpleNamespace]:
        return self._keys


class _DummyDB:
    def __init__(self, keys: list[SimpleNamespace]) -> None:
        self._keys = keys

    def query(self, _model: object) -> _DummyQuery:
        return _DummyQuery(self._keys)


def _make_oauth_key(*, key_id: str, name: str, auth_config: dict[str, object]) -> SimpleNamespace:
    return SimpleNamespace(
        id=key_id,
        name=name,
        provider_id="provider-1",
        auth_type="oauth",
        auth_config=json.dumps(auth_config),
        is_active=True,
    )


def test_check_duplicate_oauth_account_codex_allows_same_user_different_account_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(module.crypto_service, "decrypt", lambda value, silent=True: value)

    existing_key = _make_oauth_key(
        key_id="key-1",
        name="existing",
        auth_config={
            "provider_type": "codex",
            "email": "u@example.com",
            "user_id": "user-1",
            "account_id": "acc-1",
            "account_user_id": "user-1__acc-1",
            "plan_type": "team",
        },
    )
    db = _DummyDB([existing_key])

    result = module._check_duplicate_oauth_account(
        db,  # type: ignore[arg-type]
        "provider-1",
        {
            "provider_type": "codex",
            "email": "u@example.com",
            "user_id": "user-1",
            "account_id": "acc-2",
            "account_user_id": "user-1__acc-2",
            "plan_type": "team",
        },
    )

    assert result is None


def test_check_duplicate_oauth_account_codex_rejects_same_account_user_identity(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(module.crypto_service, "decrypt", lambda value, silent=True: value)

    existing_key = _make_oauth_key(
        key_id="key-1",
        name="existing",
        auth_config={
            "provider_type": "codex",
            "email": "u@example.com",
            "user_id": "user-1",
            "account_id": "acc-1",
            "account_user_id": "user-1__acc-1",
            "plan_type": "team",
        },
    )
    db = _DummyDB([existing_key])

    with pytest.raises(InvalidRequestException, match="已存在"):
        module._check_duplicate_oauth_account(
            db,  # type: ignore[arg-type]
            "provider-1",
            {
                "provider_type": "codex",
                "email": "u@example.com",
                "user_id": "user-1",
                "account_id": "acc-1",
                "account_user_id": "user-1__acc-1",
                "plan_type": "team",
            },
        )
