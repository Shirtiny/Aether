# Database Guidelines

> Database patterns and conventions for this project.

---

## Overview

This project uses SQLAlchemy ORM + PostgreSQL, with Alembic for schema migrations.
Session/transaction handling follows a **mixed strategy**: middleware-managed for request flows, route-managed for explicit admin commits, and independent sessions for background tasks.

---

## Query Patterns

1. Use `Depends(get_db)` in API handlers for request-scoped DB access.
2. For read-heavy endpoints, explicitly reduce selected columns and prefetch relations (`load_only`, `selectinload`) to avoid over-fetching/N+1.
3. For non-request contexts (scripts/jobs), use `create_session()` or `get_db_context()` and manage lifecycle explicitly.

### Example 1: Request DB injection
`src/api/public/claude.py`

```python
@router.post("/v1/messages")
async def create_message(
    http_request: Request,
    db: Session = Depends(get_db),
) -> Any:
    ...
```

### Example 2: Optimized read query with loader options
`src/api/public/system_catalog.py`

```python
load_options = [
    load_only(Provider.id, Provider.name, Provider.is_active, Provider.provider_priority)
]
load_options.append(
    selectinload(Provider.models)
    .load_only(Model.id, Model.provider_model_name, Model.is_active, Model.global_model_id)
    .selectinload(Model.global_model)
    .load_only(GlobalModel.id, GlobalModel.name, GlobalModel.display_name)
)
```

### Example 3: Non-request session helpers
`src/database/database.py`

```python
def create_session() -> Session:
    _ensure_engine()
    assert _SessionLocal is not None
    return _SessionLocal()

@contextmanager
def get_db_context() -> Generator[Session, None, None]:
    ...
    try:
        yield db
        db.commit()
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()
```

---

## Transaction Strategy

The project’s default transaction behavior is documented in `get_db()`:

- **LLM request path**: middleware performs final commit/rollback; service layer prefers `flush()` when needed.
- **Admin or explicit route commits**: route may call `db.commit()` directly, then middleware still closes session.
- **Background jobs**: own session and full transaction lifecycle.

### Example 4: Route-level commit pattern
`src/api/admin/providers/routes.py`

```python
db.commit()
db.refresh(provider)
```

### Example 5: Middleware finalization behavior
`src/middleware/plugin_middleware.py`

```python
tx_committed_by_route = getattr(request.state, "tx_committed_by_route", False)
self._finalize_db_session(
    db,
    should_commit=not tx_committed_by_route and exception is None,
    should_rollback=exception is not None,
)
```

---

## Migrations

- Use Alembic under `alembic/versions/*.py` for all schema changes.
- Keep `target_metadata = Base.metadata` in Alembic env.
- Online migrations in PostgreSQL use an advisory lock to prevent concurrent migration races.

### Example 6: Alembic metadata + advisory lock
`alembic/env.py`

```python
target_metadata = Base.metadata
MIGRATION_ADVISORY_LOCK_ID = 582694137405821
...
connection.execute(
    text("SELECT pg_advisory_xact_lock(:lock_id)"),
    {"lock_id": MIGRATION_ADVISORY_LOCK_ID},
)
```

### Example 7: Defensive/idempotent migration logic
`alembic/versions/20260303_1730_5f1d2e3c4b5a_add_idx_usage_status_user_created.py`

```python
result = bind.execute(
    sa.text("SELECT 1 FROM pg_indexes WHERE indexname = :name"),
    {"name": INDEX_NAME},
).fetchone()
if result:
    return
op.create_index(INDEX_NAME, TABLE, COLUMNS)
```

---

## Naming Conventions

- Table names are lowercase snake_case plural (e.g., `users`, `providers`, `global_models`).
- PKs commonly use `id` with UUID string (`String(36)`) in ORM models.
- Enum names are explicitly named (`userrole`, `authsource`) and reused with `create_type=False`.
- Index names use explicit, descriptive names in migrations (e.g., `idx_usage_status_user_created`).

### Example 8: Model naming and column style
`src/models/database.py`

```python
class User(Base):
    __tablename__ = "users"
    id = Column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()), index=True)
    email = Column(String(255), unique=True, index=True, nullable=True)
```

---

## Common Mistakes

- Writing new schema changes directly in runtime code instead of Alembic migrations.
- Fetching full entities where `load_only` + relation preloading is required on list endpoints.
- Mixing session ownership (route + middleware + background) without clear lifecycle boundary.
- Assuming automatic commit behavior in request handlers; this codebase relies on explicit strategy by context.
