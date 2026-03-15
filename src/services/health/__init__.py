"""
健康监控服务模块

包含健康监控相关功能：
- HealthMonitor: 健康监控类
- get_health_monitor: 健康度监控单例获取函数（懒加载）
"""

from .monitor import HealthMonitor, get_health_monitor

__all__ = [
    "HealthMonitor",
    "get_health_monitor",
]
