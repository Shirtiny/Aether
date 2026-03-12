from __future__ import annotations

from pathlib import Path


def test_no_direct_access_token_verification_outside_pipeline() -> None:
    api_root = Path("src/api")
    allowed = {
        Path("src/api/base/pipeline.py"),
        Path("src/api/auth/routes.py"),
    }
    offenders: list[str] = []

    for path in api_root.rglob("*.py"):
        if path in allowed:
            continue
        text = path.read_text(encoding="utf-8")
        if "verify_token(" in text and 'token_type="access"' in text:
            offenders.append(str(path))

    assert (
        offenders == []
    ), "这些 API 文件仍在手写 access token 校验，应改为走 pipeline 或 auth_utils 统一入口: " + ", ".join(
        sorted(offenders)
    )
