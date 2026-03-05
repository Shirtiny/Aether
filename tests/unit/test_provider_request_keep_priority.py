from src.models.admin_requests import CreateProviderRequest, UpdateProviderRequest


def test_create_provider_request_accepts_keep_priority_on_conversion() -> None:
    req = CreateProviderRequest.model_validate(
        {"name": "Provider Keep Priority", "keep_priority_on_conversion": True}
    )
    assert req.keep_priority_on_conversion is True


def test_update_provider_request_preserves_keep_priority_on_conversion() -> None:
    req = UpdateProviderRequest.model_validate({"keep_priority_on_conversion": True})
    data = req.model_dump(exclude_unset=True)
    assert data["keep_priority_on_conversion"] is True
