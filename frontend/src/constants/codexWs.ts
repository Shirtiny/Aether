export const CODEX_WS_PROFILE_ID = 'codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1'

export const CODEX_WS_MANIFEST = {
  schema_version: 3,
  profile_id: CODEX_WS_PROFILE_ID,
  codex_commit: '1f0566d3f59298d1bb88820a0d35294f1eeb07ea',
  tokio_tungstenite_rev: '0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186',
  tungstenite_rev: '4fffad30fe373adbdcffab9545e9e9bf4f2fc19f',
  tungstenite_patch_id: 'aether-tungstenite-0.27-out-buffer-retention-v1',
  write_buffer_size_bytes: 131072,
  max_write_buffer_size_bytes: 17825792,
  max_retained_write_buffer_capacity_bytes: 262144,
  crypto_provider: 'aws-lc-rs',
} as const
