# Error Handling

> How errors are handled in this project.

---

## Overview

The project uses a centralized exception system in `src/core/exceptions.py`.
Domain errors inherit from `ProxyException`, and FastAPI global handlers in `src/main.py` convert exceptions into a unified JSON response shape via `ErrorResponse`.

---

## Error Types

1. **Base domain error**: `ProxyException(HTTPException)` with `error_type`, `message`, and optional `details`.
2. **Specialized domain errors**: provider/network/auth/rate-limit/not-found/forbidden variants inherit from `ProxyException`.
3. **HTTP framework errors**: `HTTPException` converted into standardized error response.
4. **Unknown exceptions**: mapped to `internal_error` with environment-aware detail exposure.

### Example 1: Base exception type
`src/core/exceptions.py`

```python
class ProxyException(HTTPException):
    def __init__(
        self,
        status_code: int,
        error_type: str,
        message: str,
        details: dict[str, Any] | None = None,
    ):
        self.error_type = error_type
        self.message = message
        self.details = details or {}
        super().__init__(status_code=status_code, detail=message)
```

### Example 2: Specialized domain exception
`src/core/exceptions.py`

```python
class ProviderRateLimitException(ProviderException):
    def __init__(..., response_headers: dict[str, str] | None = None, retry_after: int | None = None):
        self.response_headers = response_headers or {}
        self.retry_after = retry_after
        ...
```

---

## Error Handling Patterns

1. Route/service code raises typed domain exceptions where possible.
2. `ErrorResponse.from_exception()` is the central mapping function.
3. Generic unknown errors are always logged with generated short `error_id`.
4. Production vs development error detail differs by `config.environment`.

### Example 3: Central exception-to-response mapping
`src/core/exceptions.py`

```python
if isinstance(e, ProxyException):
    return ErrorResponse.create(...)
elif isinstance(e, HTTPException):
    return ErrorResponse.create(...)
else:
    error_id = str(uuid.uuid4())[:8]
    logger.error(f"[{error_id}] Unexpected error: {error_type_name}: {error_message}")
    ...
```

### Example 4: Global handler registration
`src/main.py`

```python
if not config.propagate_provider_exceptions:
    app.add_exception_handler(ProxyException, ExceptionHandlers.handle_proxy_exception)
app.add_exception_handler(Exception, ExceptionHandlers.handle_generic_exception)
app.add_exception_handler(HTTPException, ExceptionHandlers.handle_http_exception)
```

### Example 5: Generic handler fallback behavior
`src/core/exceptions.py`

```python
@staticmethod
async def handle_generic_exception(request: Request, exc: Exception) -> None:
    if isinstance(exc, HTTPException):
        return await ExceptionHandlers.handle_http_exception(request, exc)
    ...
    return ErrorResponse.from_exception(exc)
```

---

## API Error Responses

Standard shape:

```json
{
  "error": {
    "type": "error_type",
    "message": "human readable message",
    "details": {
      "...": "optional extra fields"
    }
  }
}
```

Response construction happens via `ErrorResponse.create()` and `ErrorResponse.from_exception()`.

### Example 6: Error response factory
`src/core/exceptions.py`

```python
error_body = {"error": {"type": error_type, "message": message}}
if details:
    error_body["error"]["details"] = details
return JSONResponse(status_code=status_code, content=error_body)
```

---

## Common Mistakes

- Raising raw `Exception` for business errors instead of a typed `ProxyException` subclass.
- Returning ad-hoc error JSON directly in routes instead of using centralized mapping.
- Leaking internal stack or sensitive details in production responses.
- Skipping context fields in `details` that are needed for troubleshooting (while still avoiding secret leakage).
