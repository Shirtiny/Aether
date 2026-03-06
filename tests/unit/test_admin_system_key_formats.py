from src.api.admin.system import AdminExportConfigAdapter, AdminImportConfigAdapter


def test_export_key_api_formats_falls_back_to_provider_endpoints_when_none() -> None:
    adapter = AdminExportConfigAdapter()

    result = adapter._resolve_export_key_api_formats(
        None,
        ["claude:chat", "openai:cli"],
    )

    assert result == ["claude:chat", "openai:cli"]


def test_export_key_api_formats_keeps_explicit_empty_list() -> None:
    adapter = AdminExportConfigAdapter()

    result = adapter._resolve_export_key_api_formats(
        [],
        ["openai:chat"],
    )

    assert result == []


def test_export_key_api_formats_normalizes_and_deduplicates() -> None:
    adapter = AdminExportConfigAdapter()

    result = adapter._resolve_export_key_api_formats(
        [" OPENAI:CHAT ", "openai:chat", "openai:cli", "bad-format"],
        ["claude:chat"],
    )

    assert result == ["openai:chat", "openai:cli"]


def test_import_key_api_formats_uses_supported_endpoints_alias() -> None:
    result = AdminImportConfigAdapter._extract_import_key_api_formats(
        {"supported_endpoints": ["openai:chat"]},
        {"openai:chat", "openai:cli"},
    )

    assert result == ["openai:chat"]


def test_import_key_api_formats_falls_back_to_provider_endpoints_when_none() -> None:
    result = AdminImportConfigAdapter._extract_import_key_api_formats(
        {"api_formats": None},
        {"openai:chat", "claude:cli"},
    )

    assert result == ["claude:cli", "openai:chat"]


def test_import_key_api_formats_keeps_explicit_empty_list() -> None:
    result = AdminImportConfigAdapter._extract_import_key_api_formats(
        {"api_formats": []},
        {"openai:chat"},
    )

    assert result == []
