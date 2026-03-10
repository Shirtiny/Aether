# Gunicorn configuration file

import gc
import os

# worker 心跳超时（秒）：异步 worker 在此时间内必须向 arbiter 发送心跳
# 对于 UvicornWorker，事件循环偶发阻塞（GC、同步 IO）可能延迟心跳
# 默认 300 秒，通过 GUNICORN_TIMEOUT 环境变量可调整
timeout = int(os.getenv("GUNICORN_TIMEOUT", "300"))

# max-requests 触发 worker 轮换时，旧 worker 的最大存活时间（秒）
# 超过此时间后旧 worker 会被 SIGKILL 强制回收，防止因长 streaming 连接导致僵尸进程
graceful_timeout = int(os.getenv("GUNICORN_GRACEFUL_TIMEOUT", "120"))


def when_ready(server):
    """
    Called just after the server is started.
    Freeze GC before forking workers to optimize Copy-on-Write memory sharing.
    """
    gc.freeze()
    server.log.info("GC frozen for Copy-on-Write optimization")
    server.log.info(f"Objects in permanent generation: {gc.get_freeze_count()}")


def post_fork(server, worker):
    try:
        import resource

        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        server.log.info(f"Worker {worker.pid} RSS after fork: {rss} KB")
    except ImportError:
        pass  # Windows 不支持 resource 模块
