# Logging Guidelines

> How logging is done in this project.

---

## Overview

The project uses **Loguru** with centralized configuration in `src/core/logger.py`.
Logging behavior differs by environment (Docker/production vs local development), with both console and file sinks configured at startup/import time.

---

## Log Levels

Use levels by intent:

- `DEBUG`: detailed execution state, internal troubleshooting details.
- `INFO`: key lifecycle and business milestones (startup, initialization completed, major state changes).
- `WARNING`: potentially problematic but non-fatal conditions.
- `ERROR`: failures and exceptions requiring attention.

### Example 1: Documented level strategy
`src/core/logger.py`

```python
日志级别策略:
- DEBUG: 开发调试，详细执行流程、变量值、缓存操作
- INFO:  生产环境，关键业务操作、状态变更、请求处理
- WARNING: 潜在问题、降级处理、资源警告
- ERROR: 异常错误、需要关注的故障
```

### Example 2: Runtime level from environment
`src/core/logger.py`

```python
LOG_LEVEL = os.getenv("LOG_LEVEL", "DEBUG" if not IS_DOCKER else "INFO").upper()
```

---

## Structured Logging

- Console and file use explicit formats.
- File logs include source location (`{name}:{function}:{line}`).
- Main app log captures all levels from DEBUG; separate error log captures ERROR+ only.

### Example 3: File log format and sinks
`src/core/logger.py`

```python
FILE_FORMAT = "{time:YYYY-MM-DD HH:mm:ss.SSS} | {level: <8} | {name}:{function}:{line} | {message}"
logger.add(log_dir / "app.log", level="DEBUG", **file_log_config)
logger.add(log_dir / "error.log", level="ERROR", **error_log_config)
```

### Example 4: App startup logging
`src/main.py`

```python
logger.info("初始化插件系统...")
...
logger.info(f"插件初始化完成: {successful}/{len(init_results)} 个插件成功启动")
```

---

## What to Log

- Application lifecycle milestones (startup, subsystem init, shutdown).
- Important transactional outcomes (commit failures, rollback paths).
- Error responses and unexpected exceptions (with correlation info like `error_id`).
- Infrastructure warnings (pool capacity, CORS not configured, etc.).

### Example 5: Infrastructure warning
`src/database/database.py`

```python
logger.warning(
    "数据库连接池总需求可能超过 PostgreSQL 限制: {} > {} ...",
    total_estimated,
    safe_limit,
)
```

### Example 6: Unexpected exception correlation
`src/core/exceptions.py`

```python
error_id = str(uuid.uuid4())[:8]
logger.error(f"[{error_id}] Unexpected error: {error_type_name}: {error_message}")
```

---

## What NOT to Log

- Secrets, credentials, API keys, tokens, raw authorization headers.
- Sensitive payload fields that can identify users unless masked/minimized.
- Excessive third-party noise logs that reduce signal.

### Example 7: Third-party noise suppression
`src/core/logger.py`

```python
logging.getLogger("httpx").setLevel(logging.WARNING)
logging.getLogger("httpcore").setLevel(logging.WARNING)
logging.getLogger("uvicorn.access").setLevel(logging.WARNING)
```

---

## Common Mistakes

- Using `print()` instead of centralized logger.
- Logging at `ERROR` for expected business validation failures.
- Missing contextual fields (operation/entity IDs) in important logs.
- Accidentally logging full upstream request/response bodies containing sensitive data.
