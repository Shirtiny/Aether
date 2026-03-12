from __future__ import annotations

import json

import jwt

from src.core.provider_oauth_utils import parse_codex_id_token


def _encode_unsigned_jwt(payload: dict[str, object]) -> str:
    token = jwt.encode(payload, key="", algorithm="none")
    return token.decode("utf-8") if isinstance(token, bytes) else token


def test_parse_codex_id_token_extracts_auth_claim_fields() -> None:
    token = _encode_unsigned_jwt(
        {
            "email": "u@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc-1",
                "chatgpt_account_user_id": "user-1__acc-1",
                "chatgpt_plan_type": "team",
                "chatgpt_user_id": "user-1",
                "organizations": [{"id": "org-1", "title": "Personal", "is_default": True}],
            },
        }
    )

    parsed = parse_codex_id_token(token)

    assert parsed == {
        "email": "u@example.com",
        "account_id": "acc-1",
        "account_user_id": "user-1__acc-1",
        "plan_type": "team",
        "user_id": "user-1",
        "organizations": [{"id": "org-1", "title": "Personal", "is_default": True}],
    }


def test_parse_codex_id_token_accepts_json_payload_string() -> None:
    payload = {
        "email": "u@example.com",
        "chatgpt_account_id": "acc-2",
        "chatgpt_plan_type": "plus",
        "chatgpt_user_id": "user-2",
    }

    parsed = parse_codex_id_token(json.dumps(payload))

    assert parsed == {
        "email": "u@example.com",
        "account_id": "acc-2",
        "plan_type": "plus",
        "user_id": "user-2",
    }


def test_parse_codex_id_token_accepts_dict_payload() -> None:
    parsed = parse_codex_id_token(
        {
            "email": "u@example.com",
            "accountId": "acc-3",
            "planType": "enterprise",
            "userId": "user-3",
        }
    )

    assert parsed == {
        "email": "u@example.com",
        "account_id": "acc-3",
        "plan_type": "enterprise",
        "user_id": "user-3",
    }
