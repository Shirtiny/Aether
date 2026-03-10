from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True, slots=True)
class _Violation:
    file: Path
    line: int
    imported: str


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def _iter_python_files(root: Path) -> list[Path]:
    files: list[Path] = []
    for path in root.rglob("*.py"):
        if "__pycache__" in path.parts:
            continue
        files.append(path)
    return files


def _scan_imports(py_file: Path) -> list[tuple[int, str]]:
    """返回 (lineno, module_name) 列表。"""
    text = py_file.read_text(encoding="utf-8").lstrip("\ufeff")
    tree = ast.parse(text, filename=str(py_file))

    imports: list[tuple[int, str]] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                imports.append((int(getattr(node, "lineno", 0) or 0), str(alias.name)))
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                continue
            if node.module:
                imports.append((int(getattr(node, "lineno", 0) or 0), str(node.module)))
    return imports


def _scan_forbidden_imports(*, scope_dir: Path, forbidden_prefix: str) -> list[_Violation]:
    violations: list[_Violation] = []
    for py_file in _iter_python_files(scope_dir):
        for line, imported in _scan_imports(py_file):
            if imported == forbidden_prefix or imported.startswith(f"{forbidden_prefix}."):
                violations.append(_Violation(file=py_file, line=line, imported=imported))
    return violations


def _format_violations(title: str, violations: list[_Violation]) -> str:
    lines = [title, ""]
    for v in sorted(violations, key=lambda x: (str(x.file), x.line, x.imported)):
        rel = v.file.resolve().relative_to(_repo_root())
        lines.append(f"- {rel}:{v.line} -> {v.imported}")
    return "\n".join(lines)


def test_services_should_not_import_api() -> None:
    repo = _repo_root()
    violations = _scan_forbidden_imports(
        scope_dir=repo / "src" / "services",
        forbidden_prefix="src.api",
    )
    assert not violations, _format_violations("services 层禁止 import api 层：", violations)


def test_core_should_not_import_services() -> None:
    repo = _repo_root()
    violations = _scan_forbidden_imports(
        scope_dir=repo / "src" / "core",
        forbidden_prefix="src.services",
    )
    assert not violations, _format_violations("core 层禁止 import services 层：", violations)
