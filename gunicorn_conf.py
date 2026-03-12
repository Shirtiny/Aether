# Gunicorn configuration file
from __future__ import annotations

import faulthandler
import gc
import os
import signal
import sys
import traceback
from typing import Any

# worker 心跳超时（秒）：异步 worker 在此时间内必须向 arbiter 发送心跳
# 对于 UvicornWorker，事件循环偶发阻塞（GC、同步 IO）可能延迟心跳
# 默认 300 秒，通过 GUNICORN_TIMEOUT 环境变量可调整
timeout = int(os.getenv("GUNICORN_TIMEOUT", "300"))

# max-requests 触发 worker 轮换时，旧 worker 的最大存活时间（秒）
# 超过此时间后旧 worker 会被 SIGKILL 强制回收，防止因长 streaming 连接导致僵尸进程
graceful_timeout = int(os.getenv("GUNICORN_GRACEFUL_TIMEOUT", "120"))


def _log_current_rss(log: Any, message: str) -> None:
    try:
        import resource

        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        log.info(f"{message}: {rss} KB")
    except ImportError:
        pass


def _enable_fault_handler(log: Any) -> None:
    try:
        faulthandler.enable(file=sys.stderr, all_threads=True)
    except Exception as exc:
        log.warning(f"Failed to enable faulthandler: {exc}")
        return

    sigusr2 = getattr(signal, "SIGUSR2", None)
    if sigusr2 is None:
        return

    try:
        faulthandler.unregister(sigusr2)
    except Exception:
        pass

    try:
        faulthandler.register(sigusr2, file=sys.stderr, all_threads=True, chain=False)
        log.info("Registered faulthandler stack dump on SIGUSR2")
    except Exception as exc:
        log.warning(f"Failed to register faulthandler SIGUSR2 hook: {exc}")


def _dump_all_thread_traces(log: Any, reason: str) -> None:
    pid = os.getpid()
    log.critical(f"===== Python stack dump begin: reason={reason}, pid={pid} =====")
    _log_current_rss(log, f"Worker {pid} RSS before traceback dump")

    try:
        faulthandler.dump_traceback(file=sys.stderr, all_threads=True)
    except Exception as exc:
        log.warning(f"faulthandler.dump_traceback failed: {exc}")

    try:
        current_frames = sys._current_frames()
        for thread_id, frame in current_frames.items():
            stack = "".join(traceback.format_stack(frame))
            log.critical(
                f"--- thread_id={thread_id} stack begin ---\n{stack}--- thread_id={thread_id} stack end ---"
            )
    except Exception as exc:
        log.warning(f"Failed to dump Python frames via sys._current_frames(): {exc}")

    log.critical(f"===== Python stack dump end: reason={reason}, pid={pid} =====")


def when_ready(server: Any) -> None:
    """
    Called just after the server is started.
    Freeze GC before forking workers to optimize Copy-on-Write memory sharing.
    """
    collected = gc.collect()
    gc.freeze()
    server.log.info(f"GC collected {collected} unreachable objects before freeze")
    server.log.info("GC frozen for Copy-on-Write optimization")
    server.log.info(f"Objects in permanent generation: {gc.get_freeze_count()}")


def post_fork(server: Any, worker: Any) -> None:
    _log_current_rss(server.log, f"Worker {worker.pid} RSS after fork")


def post_worker_init(worker: Any) -> None:
    _log_current_rss(worker.log, f"Worker {worker.pid} RSS after app init")
    _enable_fault_handler(worker.log)


def worker_abort(worker: Any) -> None:
    _dump_all_thread_traces(worker.log, "gunicorn worker timeout / SIGABRT")
