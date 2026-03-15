from src.clients import redis_client as redis_client_module
from src.clients.redis_client import RedisClientManager, RedisState


def _seed_open_circuit(manager: RedisClientManager) -> None:
    manager._circuit_open_until = 9999999999.0
    manager._consecutive_failures = 5
    manager._last_error = "boom"


def test_reset_redis_circuit_breaker_resets_both_clients() -> None:
    old_global = redis_client_module._redis_manager
    old_usage = redis_client_module._usage_queue_redis_manager
    try:
        global_manager = RedisClientManager(client_name="global")
        usage_manager = RedisClientManager(client_name="usage")
        _seed_open_circuit(global_manager)
        _seed_open_circuit(usage_manager)

        redis_client_module._redis_manager = global_manager
        redis_client_module._usage_queue_redis_manager = usage_manager

        assert redis_client_module.reset_redis_circuit_breaker() is True
        assert global_manager.get_state() == RedisState.NOT_INITIALIZED
        assert usage_manager.get_state() == RedisState.NOT_INITIALIZED
    finally:
        redis_client_module._redis_manager = old_global
        redis_client_module._usage_queue_redis_manager = old_usage


def test_reset_redis_circuit_breaker_returns_false_when_uninitialized() -> None:
    old_global = redis_client_module._redis_manager
    old_usage = redis_client_module._usage_queue_redis_manager
    try:
        redis_client_module._redis_manager = None
        redis_client_module._usage_queue_redis_manager = None

        assert redis_client_module.reset_redis_circuit_breaker() is False
    finally:
        redis_client_module._redis_manager = old_global
        redis_client_module._usage_queue_redis_manager = old_usage
