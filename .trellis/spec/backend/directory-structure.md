# Directory Structure

> How backend code is organized in this project.

---

## Overview

The backend is a modular FastAPI application centered on `src/main.py` as the composition root.
Routing, business logic, persistence, and cross-cutting concerns are separated by package.

---

## Directory Layout

```text
src/
├── main.py                 # FastAPI app assembly, middleware, global handlers, router registration
├── api/                    # HTTP layer (grouped by domain)
│   ├── admin/
│   ├── public/
│   ├── auth/
│   ├── dashboard/
│   ├── monitoring/
│   └── user_me/
├── services/               # Business logic (grouped by domain)
│   ├── auth/
│   ├── provider/
│   ├── usage/
│   ├── proxy_node/
│   └── ...
├── models/                 # SQLAlchemy models and enums used by DB layer
├── database/               # Engine/session bootstrap and get_db()
├── middleware/             # ASGI middleware (plugin middleware, transaction lifecycle)
├── core/                   # Shared foundation (exceptions, logger, enums, modules)
├── plugins/                # Extensible plugin mechanisms (rate limit, hooks)
├── clients/                # External client wrappers
├── modules/                # Feature module registry/integration points
└── utils/                  # Shared utility functions
```

---

## Module Organization

1. **API layer (`src/api/*`)**
   - Keep endpoint parsing/response orchestration in routes.
   - Each major domain has an `__init__.py` that aggregates child routers.

2. **Service layer (`src/services/*`)**
   - Place domain business logic here.
   - Services are grouped by capability/domain (auth, billing, provider, usage, task, etc.).

3. **Core and infrastructure**
   - `src/core/*` for global concerns (errors, logging, enums).
   - `src/database/*` + `src/models/*` for persistence.
   - `src/middleware/*` for request lifecycle and cross-cutting behavior.

4. **Composition root**
   - `src/main.py` wires middleware, exception handlers, and top-level routers.

---

## Naming Conventions

- Use `snake_case` for files and folders.
- Router aggregation files expose a single `router` symbol in `__all__`.
- Keep API package names domain-oriented (`admin`, `public`, `user_me`) instead of technical layers.
- Keep service packages domain-oriented as well (`provider`, `usage`, `rate_limit`, `scheduling`).

---

## Examples

### Example 1: App-level router composition
`src/main.py`

```python
app.include_router(auth_router)
app.include_router(admin_router)
app.include_router(me_router)
app.include_router(announcement_router)
app.include_router(dashboard_router)
app.include_router(public_router)
app.include_router(monitoring_router)
```

### Example 2: Domain router aggregation + order-sensitive registration
`src/api/public/__init__.py`

```python
router = APIRouter()
router.include_router(videos_router, tags=["Video Generation"])
router.include_router(models_router)
router.include_router(claude_router, tags=["Claude API"])
...
```

### Example 3: Admin domain router aggregation
`src/api/admin/__init__.py`

```python
router = APIRouter()
router.include_router(system_router)
router.include_router(users_router)
router.include_router(providers_router)
...
```
