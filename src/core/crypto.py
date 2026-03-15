"""
加密工具模块
提供API密钥的加密和解密功能

安全说明:
- 生产环境必须设置独立的 ENCRYPTION_KEY
- 加密密钥应独立于 JWT_SECRET_KEY，避免密钥轮换问题
- 使用 PBKDF2 派生密钥时会使用应用级 salt
"""

from __future__ import annotations

import base64
import hashlib
import threading
import time
from collections import OrderedDict
from typing import TYPE_CHECKING, Any, cast

from src.core.logger import logger
from src.utils.perf import PerfRecorder

from ..config import config
from ..core.exceptions import DecryptionException

if TYPE_CHECKING:
    from cryptography.fernet import Fernet


class CryptoService:
    """
    加密服务

    提供对称加密功能，用于保护 Provider API Key 等敏感数据。
    使用 Fernet（AES-128-CBC + HMAC-SHA256）确保数据机密性和完整性。
    """

    _instance: CryptoService | None = None
    _instance_lock = threading.Lock()
    _cipher: Fernet | None = None
    _key_source: str = "unknown"  # 记录密钥来源，用于调试

    # 应用级 salt（基于应用名称生成，比硬编码更安全）
    # 注意：更改此值会导致所有已加密数据无法解密
    APP_SALT = hashlib.sha256(b"aether-v1").digest()[:16]

    def __new__(cls) -> CryptoService:
        if cls._instance is None:
            with cls._instance_lock:
                if cls._instance is None:
                    inst = super().__new__(cls)
                    inst._initialize()
                    cls._instance = inst
        return cls._instance

    def _initialize(self) -> None:
        """初始化加密服务"""
        from cryptography.fernet import Fernet

        logger.info("初始化加密服务")

        encryption_key = config.encryption_key

        if not encryption_key:
            if config.environment == "production":
                raise ValueError(
                    "ENCRYPTION_KEY must be set in production! "
                    "Use 'python generate_keys.py' to generate a secure key."
                )
            # 开发环境：使用固定的开发密钥
            logger.warning("[DEV] 未设置 ENCRYPTION_KEY，使用开发环境默认密钥。")
            encryption_key = "dev-encryption-key-do-not-use-in-production"
            self._key_source = "development_default"
        else:
            self._key_source = "environment_variable"

        # 派生 Fernet 密钥
        key = self._derive_fernet_key(encryption_key)

        self._cipher = Fernet(key)
        logger.info(f"加密服务初始化成功 (key_source={self._key_source})")

        # 解密缓存配置（使用实例变量，避免测试场景下缓存跨实例持久化）
        self._decrypt_cache: OrderedDict[str, tuple[str, float]] = OrderedDict()
        self._decrypt_cache_lock = threading.Lock()
        self._decrypt_cache_enabled = bool(getattr(config, "crypto_decrypt_cache_enabled", False))
        self._decrypt_cache_size = int(getattr(config, "crypto_decrypt_cache_size", 0) or 0)
        self._decrypt_cache_ttl_seconds = float(
            getattr(config, "crypto_decrypt_cache_ttl_seconds", 0.0) or 0.0
        )
        if self._decrypt_cache_enabled and self._decrypt_cache_size > 0:
            logger.info(
                "解密缓存已启用 (size={}, ttl={}s)",
                self._decrypt_cache_size,
                self._decrypt_cache_ttl_seconds,
            )

    def _derive_fernet_key(self, encryption_key: str) -> bytes:
        """
        从密码/密钥派生 Fernet 兼容的密钥

        Args:
            encryption_key: 原始密钥字符串

        Returns:
            Fernet 兼容的 base64 编码密钥
        """
        from cryptography.fernet import Fernet
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.kdf.pbkdf2 import PBKDF2HMAC

        # 首先尝试直接作为 Fernet 密钥使用
        try:
            key_bytes = (
                encryption_key.encode() if isinstance(encryption_key, str) else encryption_key
            )
            # 验证是否为有效的 Fernet 密钥（32 字节 base64 编码）
            Fernet(key_bytes)
            return key_bytes
        except Exception:
            pass

        # 不是有效的 Fernet 密钥，使用 PBKDF2 派生
        kdf = PBKDF2HMAC(
            algorithm=hashes.SHA256(),
            length=32,
            salt=self.APP_SALT,
            iterations=100000,
        )
        derived_key = kdf.derive(encryption_key.encode())
        return base64.urlsafe_b64encode(derived_key)

    def encrypt(self, plaintext: str) -> str:
        """
        加密字符串

        Args:
            plaintext: 明文字符串

        Returns:
            加密后的字符串（base64编码）
        """
        if not plaintext:
            return plaintext

        try:
            encrypted = self._cipher.encrypt(plaintext.encode())
            return base64.urlsafe_b64encode(encrypted).decode()
        except Exception as e:
            logger.error(f"Encryption failed: {e}")
            raise ValueError("Failed to encrypt data")

    def decrypt(self, ciphertext: str, silent: bool = False) -> str:
        """
        解密字符串

        Args:
            ciphertext: 加密的字符串（base64编码）
            silent: 是否静默模式（失败时不打印错误日志）

        Returns:
            解密后的明文字符串

        Raises:
            DecryptionException: 解密失败时抛出异常
        """
        if not ciphertext:
            return ciphertext

        cached = self._get_cached_decrypt(ciphertext)
        if cached is not None:
            PerfRecorder.record_counter("crypto_decrypt_cache_hits_total", 1)
            return cached

        PerfRecorder.record_counter("crypto_decrypt_cache_misses_total", 1)
        start = PerfRecorder.start()
        try:
            encrypted = base64.urlsafe_b64decode(ciphertext.encode())
            decrypted = self._cipher.decrypt(encrypted)
            plaintext = decrypted.decode()
            self._set_cached_decrypt(ciphertext, plaintext)
            PerfRecorder.stop(start, "crypto_decrypt")
            return plaintext
        except Exception as e:
            PerfRecorder.stop(start, "crypto_decrypt")
            if not silent:
                logger.error(f"Decryption failed: {e}")
            # 抛出自定义异常，方便在上层通过类型判断是否需要打印堆栈
            raise DecryptionException(
                message=f"解密失败: {str(e)}。可能原因: ENCRYPTION_KEY 已改变或数据已损坏。解决方案: 请在管理面板重新设置 Provider API Key。",
                details={"original_error": str(e), "key_source": self._key_source},
            )

    def hash_api_key(self, api_key: str) -> str:
        """
        对API密钥进行哈希（用于查找）

        Args:
            api_key: API密钥明文

        Returns:
            哈希后的值
        """
        return hashlib.sha256(api_key.encode()).hexdigest()

    def _cache_key(self, ciphertext: str) -> str:
        """生成缓存 key（使用密文 hash，避免内存中保留完整密文）"""
        return hashlib.sha256(ciphertext.encode()).hexdigest()[:32]

    def _get_cached_decrypt(self, ciphertext: str) -> str | None:
        if not self._decrypt_cache_enabled:
            return None
        if not ciphertext:
            return None
        if self._decrypt_cache_size <= 0:
            return None
        cache_key = self._cache_key(ciphertext)
        with self._decrypt_cache_lock:
            entry = self._decrypt_cache.get(cache_key)
            if not entry:
                return None
            value, expires_at = entry
            if expires_at <= time.time():
                self._decrypt_cache.pop(cache_key, None)
                return None
            # 维护 LRU 顺序
            self._decrypt_cache.move_to_end(cache_key)
            return value

    def _set_cached_decrypt(self, ciphertext: str, plaintext: str) -> None:
        if not self._decrypt_cache_enabled:
            return
        if not ciphertext:
            return
        if self._decrypt_cache_size <= 0:
            return
        if self._decrypt_cache_ttl_seconds <= 0:
            return
        cache_key = self._cache_key(ciphertext)
        expires_at = time.time() + self._decrypt_cache_ttl_seconds
        with self._decrypt_cache_lock:
            self._decrypt_cache[cache_key] = (plaintext, expires_at)
            self._decrypt_cache.move_to_end(cache_key)
            while len(self._decrypt_cache) > self._decrypt_cache_size:
                self._decrypt_cache.popitem(last=False)


def get_crypto_service() -> CryptoService:
    """获取加密服务单例（首次使用时才会初始化）。"""
    return CryptoService()


class _LazyCryptoServiceProxy:
    """延迟代理，避免 import 阶段触发 cryptography 重载。"""

    def __getattr__(self, name: str) -> Any:
        return getattr(get_crypto_service(), name)


if TYPE_CHECKING:
    crypto_service = CryptoService()
else:
    crypto_service = cast(CryptoService, _LazyCryptoServiceProxy())
