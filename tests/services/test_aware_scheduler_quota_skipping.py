from __future__ import annotations

from unittest.mock import MagicMock, patch

from src.services.scheduling.aware_scheduler import CacheAwareScheduler


def _make_key(
    *,
    upstream_metadata: dict,
    allowed_models: list[str] | None = None,
    capabilities: dict[str, bool] | None = None,
) -> MagicMock:
    key = MagicMock()
    key.id = "k1234567890"
    key.allowed_models = allowed_models
    key.capabilities = capabilities or {}
    key.upstream_metadata = upstream_metadata
    return key


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_kiro_quota_remaining_zero_skips(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()
    key = _make_key(upstream_metadata={"kiro": {"remaining": 0.0}})

    ok, reason, _mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="openai:chat",
        model_name="any-model",
        provider_type="kiro",
    )

    assert ok is False
    assert reason == "Kiro 账号配额剩余 0"


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_kiro_quota_remaining_positive_allows(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()
    key = _make_key(upstream_metadata={"kiro": {"remaining": 1.0}})

    ok, reason, _mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="openai:chat",
        model_name="any-model",
        provider_type="kiro",
    )

    assert ok is True
    assert reason is None


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_codex_weekly_quota_exhausted_skips(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()
    key = _make_key(
        upstream_metadata={
            "codex": {
                "primary_used_percent": 100.0,
                "secondary_used_percent": 10.0,
            }
        }
    )

    ok, reason, _mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="openai:cli",
        model_name="any-model",
        provider_type="codex",
    )

    assert ok is False
    assert reason == "Codex 周限额剩余 0%"


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_codex_5h_quota_exhausted_skips(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()
    key = _make_key(
        upstream_metadata={
            "codex": {
                "primary_used_percent": 10.0,
                "secondary_used_percent": 100.0,
            }
        }
    )

    ok, reason, _mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="openai:cli",
        model_name="any-model",
        provider_type="codex",
    )

    assert ok is False
    assert reason == "Codex 5H 限额剩余 0%"


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_codex_ignores_unrelated_metadata_fields(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()
    key = _make_key(
        upstream_metadata={
            "codex": {
                "primary_used_percent": 10.0,
                "secondary_used_percent": 20.0,
                "legacy_marker": "ignore-me",
            }
        }
    )

    ok, reason, _mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="openai:cli",
        model_name="any-model",
        provider_type="codex",
    )

    assert ok is True
    assert reason is None


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_antigravity_model_quota_exhausted_skips(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()
    key = _make_key(
        upstream_metadata={
            "antigravity": {
                "quota_by_model": {
                    "ag-model": {"remaining_fraction": 0.0, "used_percent": 100.0},
                    "other": {"remaining_fraction": 1.0, "used_percent": 0.0},
                }
            }
        },
    )

    ok, reason, _mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="gemini:chat",
        model_name="ag-model",
        provider_type="antigravity",
    )

    assert ok is False
    assert reason == "Antigravity 模型 ag-model 配额剩余 0%"


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_antigravity_other_model_not_exhausted_allows(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()
    key = _make_key(
        upstream_metadata={
            "antigravity": {
                "quota_by_model": {
                    "ag-model": {"remaining_fraction": 0.0, "used_percent": 100.0},
                    "other": {"remaining_fraction": 1.0, "used_percent": 0.0},
                }
            }
        },
    )

    ok, reason, _mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="gemini:chat",
        model_name="other",
        provider_type="antigravity",
    )

    assert ok is True
    assert reason is None


@patch("src.services.scheduling.candidate_builder.get_health_monitor")
def test_antigravity_quota_uses_mapping_matched_model(mock_get_health_monitor: MagicMock) -> None:
    mock_get_health_monitor.return_value.get_circuit_breaker_status.return_value = (True, None)
    scheduler = CacheAwareScheduler()

    # Request uses GlobalModel.name, but allowed_models only contains provider-side model id.
    key = _make_key(
        upstream_metadata={
            "antigravity": {
                "quota_by_model": {
                    "ag-model": {"remaining_fraction": 0.0, "used_percent": 100.0},
                }
            }
        },
        allowed_models=["ag-model"],
    )

    ok, reason, mapped = scheduler._candidate_builder._check_key_availability(
        key,
        api_format="gemini:chat",
        model_name="global-model",
        model_mappings=["ag-.*"],
        provider_type="antigravity",
    )

    assert mapped == "ag-model"
    assert ok is False
    assert reason == "Antigravity 模型 ag-model 配额剩余 0%"
