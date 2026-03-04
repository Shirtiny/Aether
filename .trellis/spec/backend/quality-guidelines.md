# Quality Guidelines

> Code quality standards for backend development.

---

## Overview

Backend quality is enforced by a mix of tooling (Black/isort/mypy/pytest) and project patterns (typed exception flow, query optimization, layered organization).
The standard is: follow existing patterns in `src/` and avoid introducing parallel styles.

---

## Forbidden Patterns

1. **Bypassing centralized error model**
   - Do not return arbitrary ad-hoc error JSON in handlers when exception mapping exists.

2. **Over-fetching in list/read endpoints**
   - Avoid querying full object graphs where loader options are already standard.

3. **Unmanaged DB sessions**
   - Avoid creating sessions without clear ownership and close/rollback behavior.

4. **Inconsistent logging approach**
   - Do not mix `print()` debugging into backend runtime paths.

---

## Required Patterns

1. Use centralized router composition through package `__init__.py` and app-level `include_router`.
2. Keep domain logic in `src/services/*`, not deeply embedded in route handlers.
3. Use `Depends(get_db)` for request DB access.
4. Use project exception hierarchy (`ProxyException` and subclasses) for business errors.
5. Use query optimization options (`load_only`, `selectinload`, etc.) on list/detail APIs with optional expansions.

### Example 1: Route DB dependency pattern
`src/api/public/claude.py`

```python
async def create_message(http_request: Request, db: Session = Depends(get_db)) -> Any:
    ...
```

### Example 2: Router aggregation pattern
`src/api/admin/__init__.py`

```python
router = APIRouter()
router.include_router(system_router)
router.include_router(users_router)
router.include_router(providers_router)
...
```

### Example 3: Query optimization pattern
`src/api/public/system_catalog.py`

```python
base_query = db.query(Provider)
base_query = base_query.options(*load_options)
base_query = base_query.order_by(Provider.provider_priority.asc(), Provider.name.asc())
```

---

## Testing Requirements

- Use pytest as the standard test runner.
- Async behavior should be covered with `@pytest.mark.asyncio` where appropriate.
- Keep tests under `tests/`, naming by `test_*.py` conventions.
- Coverage reporting is enabled by default via pytest addopts.

### Example 4: Async pytest pattern
`tests/services/test_auth.py`

```python
@pytest.mark.asyncio
async def test_verify_valid_access_token(self) -> None:
    ...
```

### Tooling configuration references
`pyproject.toml`

- `black` line length: 100
- `isort` profile: black
- `mypy` enabled with project-specific overrides
- `pytest` configured with coverage reports (`--cov=src`)

---

## Code Review Checklist

- [ ] New code follows existing module boundaries (`api` / `services` / `core` / `database`).
- [ ] Error paths use standardized exception + response flow.
- [ ] DB queries avoid unnecessary eager payloads and N+1 patterns.
- [ ] Transaction ownership is explicit (route, middleware, or context manager).
- [ ] Logging uses project logger and appropriate log levels.
- [ ] Type hints are present for new/changed function signatures.
- [ ] Tests added or updated for non-trivial behavior changes.
