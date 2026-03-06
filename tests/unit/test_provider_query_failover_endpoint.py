from types import SimpleNamespace

from src.api.admin.provider_query import (
    _build_direct_test_candidates,
    _filter_test_candidates_by_endpoint,
)


def _build_provider() -> tuple[SimpleNamespace, SimpleNamespace, SimpleNamespace]:
    endpoint_a = SimpleNamespace(id="ep-a", api_format="openai:chat", is_active=True)
    endpoint_b = SimpleNamespace(id="ep-b", api_format="claude:cli", is_active=True)
    key_all = SimpleNamespace(
        id="key-all", is_active=True, api_formats=["openai:chat", "claude:cli"]
    )
    key_b = SimpleNamespace(id="key-b", is_active=True, api_formats=["claude:cli"])
    provider = SimpleNamespace(
        id="provider-1", endpoints=[endpoint_a, endpoint_b], api_keys=[key_all, key_b]
    )
    return provider, endpoint_a, endpoint_b


def test_build_direct_test_candidates_respects_endpoint_id() -> None:
    provider, _endpoint_a, endpoint_b = _build_provider()

    candidates = _build_direct_test_candidates(provider, endpoint_id=endpoint_b.id)  # type: ignore[arg-type]

    assert {candidate.endpoint.id for candidate in candidates} == {endpoint_b.id}
    assert {candidate.key.id for candidate in candidates} == {"key-all", "key-b"}


def test_filter_test_candidates_by_endpoint_keeps_matching_candidates() -> None:
    provider, endpoint_a, endpoint_b = _build_provider()
    candidates = _build_direct_test_candidates(provider)  # type: ignore[arg-type]

    filtered = _filter_test_candidates_by_endpoint(candidates, endpoint_a.id)

    assert {candidate.endpoint.id for candidate in filtered} == {endpoint_a.id}
    assert all(candidate.endpoint.id != endpoint_b.id for candidate in filtered)
